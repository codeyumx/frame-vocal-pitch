//! Vulkan texture submission for overlays (SPEC §3.2).
//!
//! Own `VkInstance`/`VkDevice` with the extensions OpenVR requests, on the physical device named
//! by `IVRSystem::GetOutputDevice`. Each overlay gets two device-local RGBA8 images used
//! alternately plus a persistently mapped staging buffer. An upload copies the pixels into the
//! staging buffer, records `UNDEFINED → TRANSFER_DST` / copy / `TRANSFER_DST → TRANSFER_SRC`
//! (the layout OpenVR expects), waits for its fence and calls `SetOverlayTexture`.

use std::ffi::{c_void, CString};
use std::time::Duration;

use ash::vk::{self, Handle};

use crate::ffi::VRVulkanTextureData;
use crate::vr::{OverlayFlag, Vr};
use crate::{Error, OverlayHandle, Result};

/// Pixel format of overlay images (`VK_FORMAT_R8G8B8A8_UNORM`, gamma colour space).
const FORMAT: vk::Format = vk::Format::R8G8B8A8_UNORM;

/// Upper bound for one upload to complete on the GPU.
const UPLOAD_TIMEOUT: Duration = Duration::from_secs(2);

/// Submissions to the same overlay after which images of a previous size are freed.
const RETIRE_AFTER_SUBMITS: u8 = 2;

/// A CPU-rendered frame: tightly packed RGBA8 rows, top row first.
#[derive(Clone, Copy, Debug)]
pub struct FrameRef<'a> {
    pub width: u32,
    pub height: u32,
    pub rgba: &'a [u8],
    /// Colour channels are premultiplied by alpha (sets `VROverlayFlags_IsPremultiplied`).
    pub premultiplied: bool,
}

struct Image {
    image: vk::Image,
    memory: vk::DeviceMemory,
}

struct Staging {
    buffer: vk::Buffer,
    memory: vk::DeviceMemory,
    mapped: *mut u8,
    size: usize,
}

struct Target {
    overlay: OverlayHandle,
    width: u32,
    height: u32,
    images: [Image; 2],
    next: usize,
    staging: Staging,
}

struct Retired {
    overlay: OverlayHandle,
    submits_left: u8,
    target: Target,
}

fn vk_err(what: &'static str) -> impl Fn(vk::Result) -> Error {
    move |r| Error::Vulkan(format!("{what}: {r}"))
}

/// Chooses the physical device: the one OpenVR named, or, if it named none, the only device.
fn choose_physical_device(
    requested: u64,
    devices: &[vk::PhysicalDevice],
) -> Result<vk::PhysicalDevice> {
    if requested != 0 {
        return devices
            .iter()
            .copied()
            .find(|d| d.as_raw() == requested)
            .ok_or_else(|| {
                Error::Vulkan("GetOutputDevice returned an unknown VkPhysicalDevice".into())
            });
    }
    match devices {
        [only] => Ok(*only),
        [] => Err(Error::Vulkan("no Vulkan physical device".into())),
        _ => Err(Error::Vulkan(
            "GetOutputDevice named no device and several exist".into(),
        )),
    }
}

/// Index of a memory type allowed by `type_bits` with all of `flags`.
fn find_memory_type(
    props: &vk::PhysicalDeviceMemoryProperties,
    type_bits: u32,
    flags: vk::MemoryPropertyFlags,
) -> Option<u32> {
    (0..props.memory_type_count).find(|&i| {
        type_bits & (1 << i) != 0
            && props.memory_types[i as usize]
                .property_flags
                .contains(flags)
    })
}

fn to_cstrings(names: &[String]) -> Result<Vec<CString>> {
    names
        .iter()
        .map(|n| {
            CString::new(n.as_str()).map_err(|_| Error::InvalidString("Vulkan extension name"))
        })
        .collect()
}

/// Owns the Vulkan objects used to feed overlay textures. Create after [`Vr::init_overlay`];
/// destroy through [`Vr::shutdown`] so it is dropped after `VR_ShutdownInternal`.
pub struct TextureSubmitter {
    _entry: ash::Entry,
    instance: ash::Instance,
    physical: vk::PhysicalDevice,
    device: ash::Device,
    queue: vk::Queue,
    queue_family: u32,
    memory_props: vk::PhysicalDeviceMemoryProperties,
    pool: vk::CommandPool,
    cmd: vk::CommandBuffer,
    fence: vk::Fence,
    targets: Vec<Target>,
    retired: Vec<Retired>,
}

impl TextureSubmitter {
    /// Creates the instance with `IVRCompositor::GetVulkanInstanceExtensionsRequired`, picks the
    /// device from `IVRSystem::GetOutputDevice` and creates the logical device with
    /// `GetVulkanDeviceExtensionsRequired` for it.
    pub fn new(vr: &Vr) -> Result<Self> {
        Self::create(
            &vr.vulkan_instance_extensions(),
            |instance, devices| {
                choose_physical_device(vr.vulkan_output_device(instance.handle().as_raw()), devices)
            },
            |physical| vr.vulkan_device_extensions(physical.as_raw()),
        )
    }

    fn create(
        instance_extensions: &[String],
        choose: impl FnOnce(&ash::Instance, &[vk::PhysicalDevice]) -> Result<vk::PhysicalDevice>,
        device_extensions: impl FnOnce(vk::PhysicalDevice) -> Vec<String>,
    ) -> Result<Self> {
        // SAFETY: dlopens the system Vulkan loader (`libvulkan.so.1`); no preconditions.
        let entry = unsafe { ash::Entry::load() }
            .map_err(|e| Error::Vulkan(format!("loading libvulkan: {e}")))?;
        // SAFETY: valid entry.
        let loader_version = unsafe { entry.try_enumerate_instance_version() }
            .map_err(vk_err("vkEnumerateInstanceVersion"))?
            .unwrap_or(vk::API_VERSION_1_0);
        let api_version = loader_version.min(vk::API_VERSION_1_1);

        let app_name = c"frame-vocal-pitch";
        let app_info = vk::ApplicationInfo::default()
            .application_name(app_name)
            .application_version(0)
            .engine_name(app_name)
            .api_version(api_version);
        let ext_names = to_cstrings(instance_extensions)?;
        let ext_ptrs: Vec<_> = ext_names.iter().map(|c| c.as_ptr()).collect();
        let instance_info = vk::InstanceCreateInfo::default()
            .application_info(&app_info)
            .enabled_extension_names(&ext_ptrs);
        // SAFETY: create info and everything it points to outlive the call.
        let instance = unsafe { entry.create_instance(&instance_info, None) }
            .map_err(vk_err("vkCreateInstance"))?;

        match Self::create_device(&instance, choose, device_extensions) {
            Ok((physical, device, queue_family, memory_props)) => {
                // SAFETY: the family was created with one queue.
                let queue = unsafe { device.get_device_queue(queue_family, 0) };
                let mut this = TextureSubmitter {
                    _entry: entry,
                    instance,
                    physical,
                    device,
                    queue,
                    queue_family,
                    memory_props,
                    pool: vk::CommandPool::null(),
                    cmd: vk::CommandBuffer::null(),
                    fence: vk::Fence::null(),
                    targets: Vec::new(),
                    retired: Vec::new(),
                };
                // On failure `this` is dropped, which destroys whatever was created.
                this.create_sync_objects()?;
                Ok(this)
            }
            Err(e) => {
                // SAFETY: no child objects exist.
                unsafe { instance.destroy_instance(None) };
                Err(e)
            }
        }
    }

    fn create_device(
        instance: &ash::Instance,
        choose: impl FnOnce(&ash::Instance, &[vk::PhysicalDevice]) -> Result<vk::PhysicalDevice>,
        device_extensions: impl FnOnce(vk::PhysicalDevice) -> Vec<String>,
    ) -> Result<(
        vk::PhysicalDevice,
        ash::Device,
        u32,
        vk::PhysicalDeviceMemoryProperties,
    )> {
        // SAFETY: valid instance.
        let devices = unsafe { instance.enumerate_physical_devices() }
            .map_err(vk_err("vkEnumeratePhysicalDevices"))?;
        let physical = choose(instance, &devices)?;
        // SAFETY: `physical` belongs to `instance`.
        let families = unsafe { instance.get_physical_device_queue_family_properties(physical) };
        let queue_family = families
            .iter()
            .position(|f| f.queue_flags.contains(vk::QueueFlags::GRAPHICS) && f.queue_count > 0)
            .ok_or_else(|| Error::Vulkan("no graphics queue family".into()))?
            as u32;

        let ext_names = to_cstrings(&device_extensions(physical))?;
        let ext_ptrs: Vec<_> = ext_names.iter().map(|c| c.as_ptr()).collect();
        let priorities = [1.0f32];
        let queue_info = [vk::DeviceQueueCreateInfo::default()
            .queue_family_index(queue_family)
            .queue_priorities(&priorities)];
        let device_info = vk::DeviceCreateInfo::default()
            .queue_create_infos(&queue_info)
            .enabled_extension_names(&ext_ptrs);
        // SAFETY: create info and everything it points to outlive the call.
        let device = unsafe { instance.create_device(physical, &device_info, None) }
            .map_err(vk_err("vkCreateDevice"))?;
        // SAFETY: `physical` belongs to `instance`.
        let memory_props = unsafe { instance.get_physical_device_memory_properties(physical) };
        Ok((physical, device, queue_family, memory_props))
    }

    fn create_sync_objects(&mut self) -> Result<()> {
        let pool_info = vk::CommandPoolCreateInfo::default()
            .flags(vk::CommandPoolCreateFlags::TRANSIENT)
            .queue_family_index(self.queue_family);
        // SAFETY: valid device and create info.
        self.pool = unsafe { self.device.create_command_pool(&pool_info, None) }
            .map_err(vk_err("vkCreateCommandPool"))?;
        let alloc = vk::CommandBufferAllocateInfo::default()
            .command_pool(self.pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1);
        // SAFETY: valid pool.
        self.cmd = unsafe { self.device.allocate_command_buffers(&alloc) }
            .map_err(vk_err("vkAllocateCommandBuffers"))?[0];
        // SAFETY: valid device.
        self.fence = unsafe {
            self.device
                .create_fence(&vk::FenceCreateInfo::default(), None)
        }
        .map_err(vk_err("vkCreateFence"))?;
        Ok(())
    }

    /// Uploads `frame` into the overlay's next image and hands it to OpenVR with
    /// `SetOverlayTexture` (`TextureType_Vulkan`, `ColorSpace_Gamma`). Sets or clears
    /// `VROverlayFlags_IsPremultiplied` when `frame.premultiplied` differs from the last value.
    /// Call only when the pixels changed (SPEC §3.2).
    pub fn submit(
        &mut self,
        vr: &mut Vr,
        overlay: OverlayHandle,
        frame: &FrameRef<'_>,
    ) -> Result<()> {
        if vr.premultiplied(overlay) != Some(frame.premultiplied) {
            vr.set_flag(overlay, OverlayFlag::IsPremultiplied, frame.premultiplied)?;
        }
        let mut data = self.upload(overlay, frame)?;
        vr.set_overlay_texture(overlay, &mut data)?;
        self.age_retired(overlay);
        Ok(())
    }

    /// Frees the images of `overlay` (after it was destroyed or its texture cleared).
    pub fn release(&mut self, overlay: OverlayHandle) {
        // SAFETY: every upload waits for its fence, so no GPU work of ours references the images;
        // waiting for idle also covers work the runtime queued on our queue.
        unsafe { self.device.device_wait_idle() }.ok();
        let (gone, keep): (Vec<_>, Vec<_>) =
            self.targets.drain(..).partition(|t| t.overlay == overlay);
        self.targets = keep;
        for t in gone {
            self.destroy_target(t);
        }
        let (gone, keep): (Vec<_>, Vec<_>) =
            self.retired.drain(..).partition(|r| r.overlay == overlay);
        self.retired = keep;
        for r in gone {
            self.destroy_target(r.target);
        }
    }

    fn age_retired(&mut self, overlay: OverlayHandle) {
        let mut i = 0;
        while i < self.retired.len() {
            let r = &mut self.retired[i];
            if r.overlay == overlay {
                r.submits_left = r.submits_left.saturating_sub(1);
                if r.submits_left == 0 {
                    let r = self.retired.swap_remove(i);
                    self.destroy_target(r.target);
                    continue;
                }
            }
            i += 1;
        }
    }

    /// Copies `frame` into the overlay's next image and returns the OpenVR texture description.
    fn upload(
        &mut self,
        overlay: OverlayHandle,
        frame: &FrameRef<'_>,
    ) -> Result<VRVulkanTextureData> {
        let expected = frame.width as usize * frame.height as usize * 4;
        if frame.width == 0 || frame.height == 0 || frame.rgba.len() != expected {
            return Err(Error::Vulkan(format!(
                "frame {}x{} needs {expected} RGBA bytes, got {}",
                frame.width,
                frame.height,
                frame.rgba.len()
            )));
        }
        let index = match self.targets.iter().position(|t| t.overlay == overlay) {
            Some(i)
                if self.targets[i].width == frame.width
                    && self.targets[i].height == frame.height =>
            {
                i
            }
            existing => {
                let target = self.create_target(overlay, frame.width, frame.height)?;
                if let Some(i) = existing {
                    let old = std::mem::replace(&mut self.targets[i], target);
                    self.retired.push(Retired {
                        overlay,
                        submits_left: RETIRE_AFTER_SUBMITS,
                        target: old,
                    });
                    i
                } else {
                    self.targets.push(target);
                    self.targets.len() - 1
                }
            }
        };

        let target = &mut self.targets[index];
        let slot = target.next;
        target.next ^= 1;
        let image = target.images[slot].image;
        let buffer = target.staging.buffer;
        // SAFETY: `mapped` points to `size` == `expected` bytes of host-coherent memory that no
        // GPU work is using (the previous upload's fence was waited on).
        unsafe { std::slice::from_raw_parts_mut(target.staging.mapped, target.staging.size) }
            .copy_from_slice(frame.rgba);
        let (width, height) = (frame.width, frame.height);

        self.one_shot(|device, cmd| {
            let range = color_range();
            let to_dst = vk::ImageMemoryBarrier::default()
                .src_access_mask(vk::AccessFlags::empty())
                .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                .old_layout(vk::ImageLayout::UNDEFINED)
                .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(image)
                .subresource_range(range);
            let region = vk::BufferImageCopy::default()
                .image_subresource(color_layers())
                .image_extent(vk::Extent3D {
                    width,
                    height,
                    depth: 1,
                });
            let to_src = vk::ImageMemoryBarrier::default()
                .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                .dst_access_mask(vk::AccessFlags::TRANSFER_READ | vk::AccessFlags::SHADER_READ)
                .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .new_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(image)
                .subresource_range(range);
            // SAFETY: `cmd` is in the recording state; all handles are live.
            unsafe {
                device.cmd_pipeline_barrier(
                    cmd,
                    vk::PipelineStageFlags::ALL_COMMANDS,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::DependencyFlags::empty(),
                    &[],
                    &[],
                    &[to_dst],
                );
                device.cmd_copy_buffer_to_image(
                    cmd,
                    buffer,
                    image,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    &[region],
                );
                device.cmd_pipeline_barrier(
                    cmd,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::PipelineStageFlags::ALL_COMMANDS,
                    vk::DependencyFlags::empty(),
                    &[],
                    &[],
                    &[to_src],
                );
            }
        })?;

        Ok(VRVulkanTextureData {
            m_nImage: image.as_raw(),
            m_pDevice: self.device.handle().as_raw() as usize as *mut c_void,
            m_pPhysicalDevice: self.physical.as_raw() as usize as *mut c_void,
            m_pInstance: self.instance.handle().as_raw() as usize as *mut c_void,
            m_pQueue: self.queue.as_raw() as usize as *mut c_void,
            m_nQueueFamilyIndex: self.queue_family,
            m_nWidth: width,
            m_nHeight: height,
            m_nFormat: FORMAT.as_raw() as u32,
            m_nSampleCount: 1,
        })
    }

    /// Records commands with `record`, submits them and waits for completion.
    fn one_shot(&mut self, record: impl FnOnce(&ash::Device, vk::CommandBuffer)) -> Result<()> {
        let device = &self.device;
        // SAFETY: the pool's only command buffer is not pending (every submit is waited on).
        unsafe {
            device
                .reset_command_pool(self.pool, vk::CommandPoolResetFlags::empty())
                .map_err(vk_err("vkResetCommandPool"))?;
            device
                .begin_command_buffer(
                    self.cmd,
                    &vk::CommandBufferBeginInfo::default()
                        .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
                )
                .map_err(vk_err("vkBeginCommandBuffer"))?;
        }
        record(device, self.cmd);
        let cmds = [self.cmd];
        let submit = vk::SubmitInfo::default().command_buffers(&cmds);
        // SAFETY: recorded command buffer; unsignalled fence after reset; queue used only here.
        unsafe {
            device
                .end_command_buffer(self.cmd)
                .map_err(vk_err("vkEndCommandBuffer"))?;
            device
                .reset_fences(&[self.fence])
                .map_err(vk_err("vkResetFences"))?;
            device
                .queue_submit(self.queue, &[submit], self.fence)
                .map_err(vk_err("vkQueueSubmit"))?;
            device
                .wait_for_fences(&[self.fence], true, UPLOAD_TIMEOUT.as_nanos() as u64)
                .map_err(vk_err("vkWaitForFences"))?;
        }
        Ok(())
    }

    fn allocate(
        &self,
        requirements: vk::MemoryRequirements,
        preferred: vk::MemoryPropertyFlags,
        required: vk::MemoryPropertyFlags,
    ) -> Result<vk::DeviceMemory> {
        let type_index = find_memory_type(
            &self.memory_props,
            requirements.memory_type_bits,
            preferred | required,
        )
        .or_else(|| find_memory_type(&self.memory_props, requirements.memory_type_bits, required))
        .ok_or_else(|| Error::Vulkan("no suitable memory type".into()))?;
        let info = vk::MemoryAllocateInfo::default()
            .allocation_size(requirements.size)
            .memory_type_index(type_index);
        // SAFETY: valid device and allocate info.
        unsafe { self.device.allocate_memory(&info, None) }.map_err(vk_err("vkAllocateMemory"))
    }

    fn create_image(&self, width: u32, height: u32) -> Result<Image> {
        let info = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(FORMAT)
            .extent(vk::Extent3D {
                width,
                height,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(
                vk::ImageUsageFlags::TRANSFER_DST
                    | vk::ImageUsageFlags::TRANSFER_SRC
                    | vk::ImageUsageFlags::SAMPLED,
            )
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED);
        // SAFETY: valid device and create info.
        let image =
            unsafe { self.device.create_image(&info, None) }.map_err(vk_err("vkCreateImage"))?;
        // SAFETY: `image` is live.
        let req = unsafe { self.device.get_image_memory_requirements(image) };
        let bound = self
            .allocate(
                req,
                vk::MemoryPropertyFlags::DEVICE_LOCAL,
                vk::MemoryPropertyFlags::empty(),
            )
            .and_then(|memory| {
                // SAFETY: fresh image and memory satisfying its requirements.
                match unsafe { self.device.bind_image_memory(image, memory, 0) } {
                    Ok(()) => Ok(memory),
                    Err(e) => {
                        // SAFETY: unbound, unused memory.
                        unsafe { self.device.free_memory(memory, None) };
                        Err(vk_err("vkBindImageMemory")(e))
                    }
                }
            });
        match bound {
            Ok(memory) => Ok(Image { image, memory }),
            Err(e) => {
                // SAFETY: unused image.
                unsafe { self.device.destroy_image(image, None) };
                Err(e)
            }
        }
    }

    fn create_buffer(&self, size: usize, usage: vk::BufferUsageFlags) -> Result<Staging> {
        let info = vk::BufferCreateInfo::default()
            .size(size as u64)
            .usage(usage)
            .sharing_mode(vk::SharingMode::EXCLUSIVE);
        // SAFETY: valid device and create info.
        let buffer =
            unsafe { self.device.create_buffer(&info, None) }.map_err(vk_err("vkCreateBuffer"))?;
        // SAFETY: `buffer` is live.
        let req = unsafe { self.device.get_buffer_memory_requirements(buffer) };
        let host = vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT;
        let memory = match self.allocate(req, vk::MemoryPropertyFlags::empty(), host) {
            Ok(m) => m,
            Err(e) => {
                // SAFETY: unused buffer.
                unsafe { self.device.destroy_buffer(buffer, None) };
                return Err(e);
            }
        };
        // SAFETY: fresh buffer/memory; host-visible memory mapped once for its whole lifetime.
        let mapped = unsafe {
            self.device
                .bind_buffer_memory(buffer, memory, 0)
                .and_then(|()| {
                    self.device
                        .map_memory(memory, 0, vk::WHOLE_SIZE, vk::MemoryMapFlags::empty())
                })
        };
        match mapped {
            Ok(ptr) => Ok(Staging {
                buffer,
                memory,
                mapped: ptr.cast(),
                size,
            }),
            Err(e) => {
                // SAFETY: unused buffer and memory.
                unsafe {
                    self.device.destroy_buffer(buffer, None);
                    self.device.free_memory(memory, None);
                }
                Err(vk_err("vkBindBufferMemory/vkMapMemory")(e))
            }
        }
    }

    fn create_target(&self, overlay: OverlayHandle, width: u32, height: u32) -> Result<Target> {
        let size = width as usize * height as usize * 4;
        let a = self.create_image(width, height)?;
        let b = match self.create_image(width, height) {
            Ok(b) => b,
            Err(e) => {
                self.destroy_image(a);
                return Err(e);
            }
        };
        let staging = match self.create_buffer(size, vk::BufferUsageFlags::TRANSFER_SRC) {
            Ok(s) => s,
            Err(e) => {
                self.destroy_image(a);
                self.destroy_image(b);
                return Err(e);
            }
        };
        Ok(Target {
            overlay,
            width,
            height,
            images: [a, b],
            next: 0,
            staging,
        })
    }

    fn destroy_image(&self, image: Image) {
        // SAFETY: callers guarantee no pending GPU work uses the image.
        unsafe {
            self.device.destroy_image(image.image, None);
            self.device.free_memory(image.memory, None);
        }
    }

    fn destroy_staging(&self, staging: Staging) {
        // SAFETY: no pending GPU work uses the buffer (uploads are waited on).
        unsafe {
            self.device.unmap_memory(staging.memory);
            self.device.destroy_buffer(staging.buffer, None);
            self.device.free_memory(staging.memory, None);
        }
    }

    fn destroy_target(&self, target: Target) {
        let [a, b] = target.images;
        self.destroy_image(a);
        self.destroy_image(b);
        self.destroy_staging(target.staging);
    }

    /// Instance-only test constructor: explicit extension lists, first physical device.
    #[cfg(test)]
    fn with_extensions(instance_extensions: &[&str], device_extensions: &[&str]) -> Result<Self> {
        let owned = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let dev = owned(device_extensions);
        Self::create(
            &owned(instance_extensions),
            |_, devices| {
                devices
                    .first()
                    .copied()
                    .ok_or_else(|| Error::Vulkan("no Vulkan physical device".into()))
            },
            move |_| dev,
        )
    }

    /// Reads an overlay image back (test only).
    #[cfg(test)]
    fn read_back(&mut self, overlay: OverlayHandle, slot: usize) -> Result<Vec<u8>> {
        let t = self
            .targets
            .iter()
            .find(|t| t.overlay == overlay)
            .expect("target");
        let (image, width, height) = (t.images[slot].image, t.width, t.height);
        let size = width as usize * height as usize * 4;
        let readback = self.create_buffer(size, vk::BufferUsageFlags::TRANSFER_DST)?;
        let buffer = readback.buffer;
        let result = self.one_shot(|device, cmd| {
            let region = vk::BufferImageCopy::default()
                .image_subresource(color_layers())
                .image_extent(vk::Extent3D {
                    width,
                    height,
                    depth: 1,
                });
            // SAFETY: recording; image is in TRANSFER_SRC_OPTIMAL after an upload.
            unsafe {
                device.cmd_copy_image_to_buffer(
                    cmd,
                    image,
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    buffer,
                    &[region],
                )
            };
        });
        // SAFETY: the copy completed (fence waited); mapped range is `size` bytes.
        let out =
            result.map(|()| unsafe { std::slice::from_raw_parts(readback.mapped, size) }.to_vec());
        self.destroy_staging(readback);
        out
    }
}

fn color_range() -> vk::ImageSubresourceRange {
    vk::ImageSubresourceRange {
        aspect_mask: vk::ImageAspectFlags::COLOR,
        base_mip_level: 0,
        level_count: 1,
        base_array_layer: 0,
        layer_count: 1,
    }
}

fn color_layers() -> vk::ImageSubresourceLayers {
    vk::ImageSubresourceLayers {
        aspect_mask: vk::ImageAspectFlags::COLOR,
        mip_level: 0,
        base_array_layer: 0,
        layer_count: 1,
    }
}

impl Drop for TextureSubmitter {
    fn drop(&mut self) {
        // SAFETY: wait for all work, then destroy children before the device and instance.
        unsafe { self.device.device_wait_idle() }.ok();
        for t in std::mem::take(&mut self.targets) {
            self.destroy_target(t);
        }
        for r in std::mem::take(&mut self.retired) {
            self.destroy_target(r.target);
        }
        // SAFETY: idle device; null handles are valid no-ops for vkDestroy*.
        unsafe {
            self.device.destroy_fence(self.fence, None);
            self.device.destroy_command_pool(self.pool, None);
            self.device.destroy_device(None);
            self.instance.destroy_instance(None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn physical_device_choice() {
        let a = vk::PhysicalDevice::from_raw(0x10);
        let b = vk::PhysicalDevice::from_raw(0x20);
        assert_eq!(choose_physical_device(0x20, &[a, b]).unwrap(), b);
        assert!(choose_physical_device(0x30, &[a, b]).is_err());
        assert_eq!(choose_physical_device(0, &[a]).unwrap(), a);
        assert!(choose_physical_device(0, &[a, b]).is_err());
        assert!(choose_physical_device(0, &[]).is_err());
    }

    #[test]
    fn memory_type_selection() {
        let mut props = vk::PhysicalDeviceMemoryProperties {
            memory_type_count: 3,
            ..Default::default()
        };
        props.memory_types[0].property_flags = vk::MemoryPropertyFlags::DEVICE_LOCAL;
        props.memory_types[1].property_flags =
            vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT;
        props.memory_types[2].property_flags = vk::MemoryPropertyFlags::DEVICE_LOCAL
            | vk::MemoryPropertyFlags::HOST_VISIBLE
            | vk::MemoryPropertyFlags::HOST_COHERENT;
        let host = vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT;
        assert_eq!(
            find_memory_type(&props, 0b111, vk::MemoryPropertyFlags::DEVICE_LOCAL),
            Some(0)
        );
        assert_eq!(
            find_memory_type(&props, 0b110, vk::MemoryPropertyFlags::DEVICE_LOCAL),
            Some(2)
        );
        assert_eq!(find_memory_type(&props, 0b011, host), Some(1));
        assert_eq!(find_memory_type(&props, 0b001, host), None);
    }

    fn pattern(width: u32, height: u32, seed: u8) -> Vec<u8> {
        (0..width * height * 4)
            .map(|i| (i as u8).wrapping_mul(7).wrapping_add(seed))
            .collect()
    }

    /// Exercises instance/device creation, double-buffered staging uploads, resizing and
    /// teardown on the host GPU without SteamVR. Skips only when no Vulkan loader/device exists.
    #[test]
    fn host_vulkan_upload_round_trip() {
        let mut sub = match TextureSubmitter::with_extensions(&[], &[]) {
            Ok(s) => s,
            Err(Error::Vulkan(msg))
                if msg.starts_with("loading libvulkan") || msg == "no Vulkan physical device" =>
            {
                eprintln!("skipping: {msg}");
                return;
            }
            Err(e) => panic!("{e}"),
        };
        let ov = OverlayHandle(1);

        let f0 = pattern(64, 32, 0);
        let d0 = sub
            .upload(
                ov,
                &FrameRef {
                    width: 64,
                    height: 32,
                    rgba: &f0,
                    premultiplied: true,
                },
            )
            .unwrap();
        let f1 = pattern(64, 32, 99);
        let d1 = sub
            .upload(
                ov,
                &FrameRef {
                    width: 64,
                    height: 32,
                    rgba: &f1,
                    premultiplied: true,
                },
            )
            .unwrap();
        assert_ne!(d0.m_nImage, d1.m_nImage, "two images alternate");
        assert_eq!((d0.m_nWidth, d0.m_nHeight, d0.m_nSampleCount), (64, 32, 1));
        assert_eq!(d0.m_nFormat, vk::Format::R8G8B8A8_UNORM.as_raw() as u32);
        assert!(!d0.m_pDevice.is_null() && !d0.m_pInstance.is_null() && !d0.m_pQueue.is_null());
        assert_eq!(sub.read_back(ov, 0).unwrap(), f0);
        assert_eq!(sub.read_back(ov, 1).unwrap(), f1);

        let d2 = sub
            .upload(
                ov,
                &FrameRef {
                    width: 64,
                    height: 32,
                    rgba: &f1,
                    premultiplied: true,
                },
            )
            .unwrap();
        assert_eq!(
            d2.m_nImage, d0.m_nImage,
            "third upload reuses the first image"
        );

        // Size change: new images, old ones retired until two more submits.
        let f3 = pattern(16, 8, 5);
        let d3 = sub
            .upload(
                ov,
                &FrameRef {
                    width: 16,
                    height: 8,
                    rgba: &f3,
                    premultiplied: false,
                },
            )
            .unwrap();
        assert_eq!((d3.m_nWidth, d3.m_nHeight), (16, 8));
        assert_eq!(sub.retired.len(), 1);
        sub.age_retired(ov);
        sub.age_retired(OverlayHandle(2));
        assert_eq!(sub.retired.len(), 1);
        sub.age_retired(ov);
        assert!(sub.retired.is_empty());
        assert_eq!(sub.read_back(ov, 0).unwrap(), f3);

        // Wrong buffer size is rejected.
        assert!(sub
            .upload(
                ov,
                &FrameRef {
                    width: 16,
                    height: 8,
                    rgba: &f0,
                    premultiplied: false
                }
            )
            .is_err());

        sub.release(ov);
        assert!(sub.targets.is_empty());
    }
}
