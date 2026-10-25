//! The OpenVR session: init/probe, overlays, device state, events and teardown (SPEC §3.1, §4).

use std::ffi::{c_char, c_void, CStr, CString};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::events::{self, Event, Source};
use crate::ffi::{
    self, CompositorTable, HmdVector2, OverlayTable, SystemTable, Texture, TrackedDevicePose,
    VREvent,
};
use crate::loader::{self, Api};
use crate::matrix::Mat34;
use crate::vk::TextureSubmitter;
use crate::{Error, OverlayHandle, Result};

/// Tracked-device index of the HMD.
pub const HMD_INDEX: u32 = ffi::K_UN_TRACKED_DEVICE_INDEX_HMD;

/// Pause between destroying overlays and `VR_ShutdownInternal` (SPEC §3.1: "wait ~400 ms").
pub const SHUTDOWN_GRACE: Duration = Duration::from_millis(400);

/// Only one OpenVR session (probe or overlay) may exist per process: `VR_InitInternal2` and
/// `VR_ShutdownInternal` act on process-global state.
static SESSION_ACTIVE: AtomicBool = AtomicBool::new(false);

struct SessionGuard;

impl SessionGuard {
    fn acquire() -> Result<Self> {
        SESSION_ACTIVE
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| SessionGuard)
            .map_err(|_| Error::AlreadyInitialized)
    }
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        SESSION_ACTIVE.store(false, Ordering::Release);
    }
}

/// Checks whether SteamVR is running **without launching it** (SPEC §3.1): initialises as
/// `VRApplication_Background` and shuts down again.
///
/// - `Ok(true)`: the runtime accepted the connection; init as overlay next ([`Vr::init_overlay`]).
/// - `Ok(false)`: `VRInitError_Init_NoServerForBackgroundApp`; retry later (the app uses 3 s).
/// - `Err(_)`: library missing/unloadable, a [`Vr`] already exists, or another init error.
pub fn is_steamvr_running() -> Result<bool> {
    let api = loader::api()?;
    let _session = SessionGuard::acquire()?;
    let mut err = ffi::VR_INIT_ERROR_NONE;
    // SAFETY: valid out-pointer, null startup info; single session guaranteed by the guard.
    unsafe { (api.init_internal2)(&mut err, ffi::VR_APPLICATION_BACKGROUND, ptr::null()) };
    // `openvr.h`'s inline VR_Init also calls VR_ShutdownInternal after a failed init.
    // SAFETY: balances the init above; no interface pointers were obtained.
    unsafe { (api.shutdown_internal)() };
    match err {
        ffi::VR_INIT_ERROR_NONE => Ok(true),
        ffi::VR_INIT_ERROR_INIT_NO_SERVER_FOR_BACKGROUND_APP => Ok(false),
        code => Err(Error::Init {
            code,
            description: api.init_error_text(code),
        }),
    }
}

/// Controller role for [`Vr::controller_index`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControllerRole {
    LeftHand,
    RightHand,
}

/// `EDeviceActivityLevel`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActivityLevel {
    Unknown,
    Idle,
    UserInteraction,
    UserInteractionTimeout,
    Standby,
    IdleTimeout,
}

impl ActivityLevel {
    fn from_raw(raw: i32) -> Self {
        match raw {
            ffi::DEVICE_ACTIVITY_LEVEL_IDLE => Self::Idle,
            ffi::DEVICE_ACTIVITY_LEVEL_USER_INTERACTION => Self::UserInteraction,
            ffi::DEVICE_ACTIVITY_LEVEL_USER_INTERACTION_TIMEOUT => Self::UserInteractionTimeout,
            ffi::DEVICE_ACTIVITY_LEVEL_STANDBY => Self::Standby,
            ffi::DEVICE_ACTIVITY_LEVEL_IDLE_TIMEOUT => Self::IdleTimeout,
            _ => Self::Unknown,
        }
    }
}

/// Overlay flags this crate sets (`VROverlayFlags` subset).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverlayFlag {
    SendVRSmoothScrollEvents,
    SendVRDiscreteScrollEvents,
    IsPremultiplied,
    NoDashboardTab,
    /// Shows a close (×) button in the dashboard control bar; pressing it sends
    /// `VREvent_OverlayClosed`.
    EnableControlBarClose,
}

impl OverlayFlag {
    fn raw(self) -> i32 {
        match self {
            Self::SendVRSmoothScrollEvents => ffi::VR_OVERLAY_FLAGS_SEND_VR_SMOOTH_SCROLL_EVENTS,
            Self::SendVRDiscreteScrollEvents => {
                ffi::VR_OVERLAY_FLAGS_SEND_VR_DISCRETE_SCROLL_EVENTS
            }
            Self::IsPremultiplied => ffi::VR_OVERLAY_FLAGS_IS_PREMULTIPLIED,
            Self::NoDashboardTab => ffi::VR_OVERLAY_FLAGS_NO_DASHBOARD_TAB,
            Self::EnableControlBarClose => ffi::VR_OVERLAY_FLAGS_ENABLE_CONTROL_BAR_CLOSE,
        }
    }
}

/// `VROverlayInputMethod`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputMethod {
    None,
    Mouse,
}

struct OverlayEntry {
    handle: OverlayHandle,
    /// Thumbnails are owned by their dashboard overlay: no events, never destroyed directly.
    thumbnail_of: Option<OverlayHandle>,
    mouse_height: f32,
    has_texture: bool,
    premultiplied: Option<bool>,
}

/// An OpenVR session initialised as `VRApplication_Overlay`.
///
/// Owns every overlay it created. Tear down with [`Vr::shutdown`], passing the
/// [`TextureSubmitter`] so Vulkan objects are destroyed after `VR_ShutdownInternal`; dropping a
/// `Vr` performs the same teardown without the Vulkan part. Not `Send`: keep it on the main loop.
pub struct Vr {
    api: &'static Api,
    system: *const SystemTable,
    overlay: *const OverlayTable,
    compositor: *const CompositorTable,
    overlays: Vec<OverlayEntry>,
    quit_acknowledged: bool,
    active: bool,
    _session: SessionGuard,
}

/// Fetches `FnTable:<version>` via `VR_GetGenericInterface`.
fn fn_table<T>(api: &Api, version: &'static str) -> Result<*const T> {
    let name = CString::new(format!("FnTable:{version}"))
        .map_err(|_| Error::InvalidString("interface version"))?;
    let mut err = ffi::VR_INIT_ERROR_NONE;
    // SAFETY: valid C string and out-pointer; called between a successful init and shutdown.
    let ptr = unsafe { (api.get_generic_interface)(name.as_ptr(), &mut err) };
    if err != ffi::VR_INIT_ERROR_NONE || ptr == 0 {
        return Err(Error::InterfaceNotFound { version, code: err });
    }
    Ok(ptr as *const T)
}

fn c_string(s: &str, what: &'static str) -> Result<CString> {
    CString::new(s).map_err(|_| Error::InvalidString(what))
}

/// Reads an OpenVR "query size, then fill" string (`GetVulkan*ExtensionsRequired`).
fn read_sized_string(mut call: impl FnMut(*mut c_char, u32) -> u32) -> String {
    let needed = call(ptr::null_mut(), 0);
    if needed == 0 {
        return String::new();
    }
    let mut buf = vec![0u8; needed as usize];
    let written = call(buf.as_mut_ptr().cast(), needed);
    if written == 0 || written > needed {
        return String::new();
    }
    CStr::from_bytes_until_nul(&buf)
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Splits OpenVR's space-separated extension list.
pub(crate) fn split_extensions(list: &str) -> Vec<String> {
    list.split_ascii_whitespace().map(str::to_owned).collect()
}

impl Vr {
    /// Initialises OpenVR as `VRApplication_Overlay` and fetches the IVRSystem, IVROverlay and
    /// IVRCompositor function tables. Call after [`is_steamvr_running`] returned `Ok(true)`.
    pub fn init_overlay() -> Result<Vr> {
        let api = loader::api()?;
        let session = SessionGuard::acquire()?;
        let mut err = ffi::VR_INIT_ERROR_NONE;
        // SAFETY: valid out-pointer, null startup info; single session guaranteed by the guard.
        unsafe { (api.init_internal2)(&mut err, ffi::VR_APPLICATION_OVERLAY, ptr::null()) };
        if err != ffi::VR_INIT_ERROR_NONE {
            // SAFETY: mirrors openvr.h's VR_Init, which shuts down after a failed init.
            unsafe { (api.shutdown_internal)() };
            return Err(Error::Init {
                code: err,
                description: api.init_error_text(err),
            });
        }
        match Self::fetch_tables(api) {
            Ok((system, overlay, compositor)) => Ok(Vr {
                api,
                system,
                overlay,
                compositor,
                overlays: Vec::new(),
                quit_acknowledged: false,
                active: true,
                _session: session,
            }),
            Err(e) => {
                // SAFETY: balances the successful init; no tables are retained.
                unsafe { (api.shutdown_internal)() };
                Err(e)
            }
        }
    }

    #[allow(clippy::type_complexity)]
    fn fetch_tables(
        api: &Api,
    ) -> Result<(
        *const SystemTable,
        *const OverlayTable,
        *const CompositorTable,
    )> {
        let version = c_string(ffi::IVR_SYSTEM_VERSION, "interface version")?;
        // SAFETY: valid C string.
        if !unsafe { (api.is_interface_version_valid)(version.as_ptr()) } {
            return Err(Error::InterfaceNotFound {
                version: ffi::IVR_SYSTEM_VERSION,
                code: ffi::VR_INIT_ERROR_INIT_INTERFACE_NOT_FOUND,
            });
        }
        let system = fn_table::<SystemTable>(api, ffi::IVR_SYSTEM_VERSION)?;
        let overlay = fn_table::<OverlayTable>(api, ffi::IVR_OVERLAY_VERSION)?;
        let compositor = fn_table::<CompositorTable>(api, ffi::IVR_COMPOSITOR_VERSION)?;
        let (major, minor, build) = ffi::STEAMVR_VERSION;
        // SAFETY: `system` is a valid IVRSystem_026 table for the live session.
        let err = unsafe { ((*system).SetSDKVersion)(major, minor, build) };
        if err != ffi::VR_INIT_ERROR_NONE {
            return Err(Error::Init {
                code: err,
                description: api.init_error_text(err),
            });
        }
        Ok((system, overlay, compositor))
    }

    fn sys(&self) -> &SystemTable {
        // SAFETY: valid from init until `teardown`, which only runs when the `Vr` is consumed or
        // dropped; no method can observe the table afterwards.
        unsafe { &*self.system }
    }

    fn ovl(&self) -> &OverlayTable {
        // SAFETY: as for `sys`.
        unsafe { &*self.overlay }
    }

    fn comp(&self) -> &CompositorTable {
        // SAFETY: as for `sys`.
        unsafe { &*self.compositor }
    }

    /// Path of the loaded `libopenvr_api.so`.
    pub fn library_path(&self) -> &Path {
        &self.api.path
    }

    /// `IVRSystem::GetRuntimeVersion`.
    pub fn runtime_version(&self) -> String {
        // SAFETY: returns a static C string owned by the runtime (or null).
        let p = unsafe { (self.sys().GetRuntimeVersion)() };
        if p.is_null() {
            return String::new();
        }
        // SAFETY: non-null NUL-terminated string.
        unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
    }

    fn check(&self, call: &'static str, code: ffi::EVROverlayError) -> Result<()> {
        if code == ffi::VR_OVERLAY_ERROR_NONE {
            return Ok(());
        }
        // SAFETY: returns a static C string (or null) for any error value.
        let p = unsafe { (self.ovl().GetOverlayErrorNameFromEnum)(code) };
        let name = if p.is_null() {
            format!("EVROverlayError {code}")
        } else {
            // SAFETY: non-null NUL-terminated string.
            unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
        };
        Err(Error::Overlay { call, code, name })
    }

    fn entry_mut(&mut self, handle: OverlayHandle) -> Option<&mut OverlayEntry> {
        self.overlays.iter_mut().find(|e| e.handle == handle)
    }

    fn track(&mut self, handle: OverlayHandle, thumbnail_of: Option<OverlayHandle>) {
        self.overlays.push(OverlayEntry {
            handle,
            thumbnail_of,
            mouse_height: 1.0,
            has_texture: false,
            premultiplied: None,
        });
    }

    // ---- Overlay lifetime ----

    /// `IVROverlay::CreateOverlay`.
    pub fn create_overlay(&mut self, key: &str, name: &str) -> Result<OverlayHandle> {
        let key = c_string(key, "overlay key")?;
        let name = c_string(name, "overlay name")?;
        let mut handle = 0u64;
        // SAFETY: valid C strings and out-pointer.
        let code = unsafe { (self.ovl().CreateOverlay)(key.as_ptr(), name.as_ptr(), &mut handle) };
        self.check("CreateOverlay", code)?;
        let handle = OverlayHandle(handle);
        self.track(handle, None);
        Ok(handle)
    }

    /// `IVROverlay::CreateDashboardOverlay`; returns `(main, thumbnail)`.
    pub fn create_dashboard_overlay(
        &mut self,
        key: &str,
        name: &str,
    ) -> Result<(OverlayHandle, OverlayHandle)> {
        let key = c_string(key, "overlay key")?;
        let name = c_string(name, "overlay name")?;
        let (mut main, mut thumb) = (0u64, 0u64);
        // SAFETY: valid C strings and out-pointers.
        let code = unsafe {
            (self.ovl().CreateDashboardOverlay)(key.as_ptr(), name.as_ptr(), &mut main, &mut thumb)
        };
        self.check("CreateDashboardOverlay", code)?;
        let (main, thumb) = (OverlayHandle(main), OverlayHandle(thumb));
        self.track(main, None);
        self.track(thumb, Some(main));
        Ok((main, thumb))
    }

    /// Clears the texture (if one was submitted) and destroys the overlay; a dashboard overlay's
    /// thumbnail goes with it. Release the overlay's images afterwards with
    /// [`TextureSubmitter::release`].
    pub fn destroy_overlay(&mut self, handle: OverlayHandle) -> Result<()> {
        if let Some(entry) = self.overlays.iter().find(|e| e.handle == handle) {
            if entry.thumbnail_of.is_some() {
                // OpenVR refuses this; report it the way the runtime would.
                return self.check(
                    "DestroyOverlay",
                    ffi::VR_OVERLAY_ERROR_THUMBNAIL_CANT_BE_DESTROYED,
                );
            }
            if entry.has_texture {
                // SAFETY: valid handle.
                let code = unsafe { (self.ovl().ClearOverlayTexture)(handle.0) };
                self.check("ClearOverlayTexture", code)?;
            }
        }
        // SAFETY: plain handle argument.
        let code = unsafe { (self.ovl().DestroyOverlay)(handle.0) };
        self.check("DestroyOverlay", code)?;
        self.overlays
            .retain(|e| e.handle != handle && e.thumbnail_of != Some(handle));
        Ok(())
    }

    // ---- Overlay properties ----

    /// `IVROverlay::SetOverlayWidthInMeters`.
    pub fn set_width(&self, handle: OverlayHandle, meters: f32) -> Result<()> {
        // SAFETY: plain value arguments.
        let code = unsafe { (self.ovl().SetOverlayWidthInMeters)(handle.0, meters) };
        self.check("SetOverlayWidthInMeters", code)
    }

    /// `IVROverlay::SetOverlayAlpha` (0..=1).
    pub fn set_alpha(&self, handle: OverlayHandle, alpha: f32) -> Result<()> {
        // SAFETY: plain value arguments.
        let code = unsafe { (self.ovl().SetOverlayAlpha)(handle.0, alpha.clamp(0.0, 1.0)) };
        self.check("SetOverlayAlpha", code)
    }

    /// `IVROverlay::SetOverlaySortOrder` (higher draws on top).
    pub fn set_sort_order(&self, handle: OverlayHandle, order: u32) -> Result<()> {
        // SAFETY: plain value arguments.
        let code = unsafe { (self.ovl().SetOverlaySortOrder)(handle.0, order) };
        self.check("SetOverlaySortOrder", code)
    }

    /// `IVROverlay::SetOverlayFlag`.
    pub fn set_flag(
        &mut self,
        handle: OverlayHandle,
        flag: OverlayFlag,
        enabled: bool,
    ) -> Result<()> {
        // SAFETY: plain value arguments.
        let code = unsafe { (self.ovl().SetOverlayFlag)(handle.0, flag.raw(), enabled) };
        self.check("SetOverlayFlag", code)?;
        if flag == OverlayFlag::IsPremultiplied {
            if let Some(e) = self.entry_mut(handle) {
                e.premultiplied = Some(enabled);
            }
        }
        Ok(())
    }

    /// `IVROverlay::SetOverlayInputMethod`.
    pub fn set_input_method(&self, handle: OverlayHandle, method: InputMethod) -> Result<()> {
        let raw = match method {
            InputMethod::None => ffi::VR_OVERLAY_INPUT_METHOD_NONE,
            InputMethod::Mouse => ffi::VR_OVERLAY_INPUT_METHOD_MOUSE,
        };
        // SAFETY: plain value arguments.
        let code = unsafe { (self.ovl().SetOverlayInputMethod)(handle.0, raw) };
        self.check("SetOverlayInputMethod", code)
    }

    /// `IVROverlay::SetOverlayMouseScale`. Use the panel's pixel size so mouse events arrive in
    /// pixels; the height is also used to flip event `y` to a top-left origin.
    pub fn set_mouse_scale(
        &mut self,
        handle: OverlayHandle,
        width: f32,
        height: f32,
    ) -> Result<()> {
        let scale = HmdVector2 { v: [width, height] };
        // SAFETY: pointer to a live HmdVector2_t.
        let code = unsafe { (self.ovl().SetOverlayMouseScale)(handle.0, &scale) };
        self.check("SetOverlayMouseScale", code)?;
        if let Some(e) = self.entry_mut(handle) {
            e.mouse_height = height;
        }
        Ok(())
    }

    /// `IVROverlay::SetOverlayTransformTrackedDeviceRelative`.
    pub fn set_transform_tracked_device_relative(
        &self,
        handle: OverlayHandle,
        device: u32,
        transform: &Mat34,
    ) -> Result<()> {
        // SAFETY: pointer to a live HmdMatrix34_t.
        let code = unsafe {
            (self.ovl().SetOverlayTransformTrackedDeviceRelative)(handle.0, device, transform)
        };
        self.check("SetOverlayTransformTrackedDeviceRelative", code)
    }

    /// Sets a dashboard thumbnail (or any overlay) from an image file via
    /// `IVROverlay::SetOverlayFromFile`. `path` should be absolute.
    pub fn set_thumbnail_from_file(&self, handle: OverlayHandle, path: &Path) -> Result<()> {
        let path = CString::new(path.as_os_str().as_bytes())
            .map_err(|_| Error::InvalidString("thumbnail path"))?;
        // SAFETY: valid C string.
        let code = unsafe { (self.ovl().SetOverlayFromFile)(handle.0, path.as_ptr()) };
        self.check("SetOverlayFromFile", code)
    }

    /// `IVROverlay::ShowOverlay`.
    pub fn show(&self, handle: OverlayHandle) -> Result<()> {
        // SAFETY: plain value argument.
        let code = unsafe { (self.ovl().ShowOverlay)(handle.0) };
        self.check("ShowOverlay", code)
    }

    /// `IVROverlay::HideOverlay`.
    pub fn hide(&self, handle: OverlayHandle) -> Result<()> {
        // SAFETY: plain value argument.
        let code = unsafe { (self.ovl().HideOverlay)(handle.0) };
        self.check("HideOverlay", code)
    }

    /// `IVROverlay::IsOverlayVisible`.
    pub fn is_visible(&self, handle: OverlayHandle) -> bool {
        // SAFETY: plain value argument.
        unsafe { (self.ovl().IsOverlayVisible)(handle.0) }
    }

    /// `IVROverlay::ClearOverlayTexture`.
    pub fn clear_texture(&mut self, handle: OverlayHandle) -> Result<()> {
        // SAFETY: plain value argument.
        let code = unsafe { (self.ovl().ClearOverlayTexture)(handle.0) };
        self.check("ClearOverlayTexture", code)?;
        if let Some(e) = self.entry_mut(handle) {
            e.has_texture = false;
        }
        Ok(())
    }

    // ---- Dashboard ----

    /// `IVROverlay::IsDashboardVisible`.
    pub fn is_dashboard_visible(&self) -> bool {
        // SAFETY: no arguments.
        unsafe { (self.ovl().IsDashboardVisible)() }
    }

    /// `IVROverlay::IsActiveDashboardOverlay`.
    pub fn is_active_dashboard_overlay(&self, handle: OverlayHandle) -> bool {
        // SAFETY: plain value argument.
        unsafe { (self.ovl().IsActiveDashboardOverlay)(handle.0) }
    }

    /// `IVROverlay::ShowDashboard`, opening the tab of the overlay with `key`.
    pub fn show_dashboard(&self, key: &str) -> Result<()> {
        let key = c_string(key, "overlay key")?;
        // SAFETY: valid C string.
        unsafe { (self.ovl().ShowDashboard)(key.as_ptr()) };
        Ok(())
    }

    // ---- Devices ----

    /// `IVRSystem::GetTrackedDeviceActivityLevel`.
    pub fn activity_level(&self, device: u32) -> ActivityLevel {
        // SAFETY: plain value argument.
        ActivityLevel::from_raw(unsafe { (self.sys().GetTrackedDeviceActivityLevel)(device) })
    }

    /// The HMD is being worn: activity level `k_EDeviceActivityLevel_UserInteraction` (SPEC §4.1).
    pub fn hmd_worn(&self) -> bool {
        self.activity_level(HMD_INDEX) == ActivityLevel::UserInteraction
    }

    /// `IVRSystem::GetTrackedDeviceIndexForControllerRole`; `None` if no device has the role.
    pub fn controller_index(&self, role: ControllerRole) -> Option<u32> {
        let raw = match role {
            ControllerRole::LeftHand => ffi::TRACKED_CONTROLLER_ROLE_LEFT_HAND,
            ControllerRole::RightHand => ffi::TRACKED_CONTROLLER_ROLE_RIGHT_HAND,
        };
        // SAFETY: plain value argument.
        let index = unsafe { (self.sys().GetTrackedDeviceIndexForControllerRole)(raw) };
        (index < ffi::K_UN_MAX_TRACKED_DEVICE_COUNT).then_some(index)
    }

    /// `IVRSystem::IsTrackedDeviceConnected`.
    pub fn is_device_connected(&self, device: u32) -> bool {
        // SAFETY: plain value argument.
        unsafe { (self.sys().IsTrackedDeviceConnected)(device) }
    }

    /// Current device-to-standing-space transform of `device`, or `None` if the index is out of
    /// range, the device is disconnected or its pose is invalid.
    pub fn device_pose(&self, device: u32) -> Option<Mat34> {
        if device >= ffi::K_UN_MAX_TRACKED_DEVICE_COUNT {
            return None;
        }
        let mut poses = [TrackedDevicePose::ZERO; ffi::K_UN_MAX_TRACKED_DEVICE_COUNT as usize];
        // SAFETY: the array holds at least `device + 1` poses.
        unsafe {
            (self.sys().GetDeviceToAbsoluteTrackingPose)(
                ffi::TRACKING_UNIVERSE_STANDING,
                0.0,
                poses.as_mut_ptr(),
                device + 1,
            )
        };
        let pose = &poses[device as usize];
        (pose.bPoseIsValid && pose.bDeviceIsConnected).then_some(pose.mDeviceToAbsoluteTracking)
    }

    // ---- Events ----

    /// Drains the system queue and every owned overlay's queue. See [`Vr::poll_events_into`].
    pub fn poll_events(&mut self) -> Vec<Event> {
        let mut out = Vec::new();
        self.poll_events_into(&mut out);
        out
    }

    /// Appends pending events to `out`: system events first, then per-overlay events
    /// (mouse/scroll/shown/hidden) in overlay creation order. On `VREvent_Quit` this calls
    /// `IVRSystem::AcknowledgeQuit_Exiting` once before returning [`Event::Quit`].
    pub fn poll_events_into(&mut self, out: &mut Vec<Event>) {
        let size = std::mem::size_of::<VREvent>() as u32;
        let mut raw = VREvent::ZERO;
        let start = out.len();
        // SAFETY: `raw` is a writable VREvent_t of `size` bytes.
        while unsafe { (self.sys().PollNextEvent)(&mut raw, size) } {
            out.extend(events::decode(&raw, Source::System));
        }
        for entry in self.overlays.iter().filter(|e| e.thumbnail_of.is_none()) {
            let source = Source::Overlay {
                overlay: entry.handle,
                mouse_height: entry.mouse_height,
            };
            // SAFETY: as above, for a handle this session created.
            while unsafe { (self.ovl().PollNextOverlayEvent)(entry.handle.0, &mut raw, size) } {
                out.extend(events::decode(&raw, source));
            }
        }
        if !self.quit_acknowledged && out[start..].contains(&Event::Quit) {
            // SAFETY: no arguments; tells the runtime this process is exiting its session.
            unsafe { (self.sys().AcknowledgeQuit_Exiting)() };
            self.quit_acknowledged = true;
        }
    }

    // ---- Vulkan / texture plumbing for `TextureSubmitter` ----

    /// `IVRCompositor::GetVulkanInstanceExtensionsRequired`, split into names.
    pub fn vulkan_instance_extensions(&self) -> Vec<String> {
        let f = self.comp().GetVulkanInstanceExtensionsRequired;
        // SAFETY: buffer pointer/size pairs come from `read_sized_string` and are consistent.
        split_extensions(&read_sized_string(|buf, len| unsafe { f(buf, len) }))
    }

    /// `IVRCompositor::GetVulkanDeviceExtensionsRequired` for a `VkPhysicalDevice` handle.
    pub fn vulkan_device_extensions(&self, physical_device: u64) -> Vec<String> {
        let f = self.comp().GetVulkanDeviceExtensionsRequired;
        let pd = physical_device as usize as *mut c_void;
        // SAFETY: `pd` is a VkPhysicalDevice handle of the caller's instance; buffer pairs consistent.
        split_extensions(&read_sized_string(|buf, len| unsafe { f(pd, buf, len) }))
    }

    /// `IVRSystem::GetOutputDevice(TextureType_Vulkan, instance)`: the `VkPhysicalDevice` handle
    /// to use (0 if the runtime does not name one).
    pub fn vulkan_output_device(&self, instance: u64) -> u64 {
        let mut device = 0u64;
        // SAFETY: valid out-pointer; `instance` is a live VkInstance created with the required
        // instance extensions.
        unsafe {
            (self.sys().GetOutputDevice)(
                &mut device,
                ffi::TEXTURE_TYPE_VULKAN,
                instance as usize as *mut c_void,
            )
        };
        device
    }

    /// Premultiplied flag as last set through this session (`None` = never set).
    pub(crate) fn premultiplied(&self, handle: OverlayHandle) -> Option<bool> {
        self.overlays
            .iter()
            .find(|e| e.handle == handle)
            .and_then(|e| e.premultiplied)
    }

    /// `IVROverlay::SetOverlayTexture` with a `Texture_t` whose handle points at a
    /// `VRVulkanTextureData_t`.
    pub(crate) fn set_overlay_texture(
        &mut self,
        handle: OverlayHandle,
        data: &mut ffi::VRVulkanTextureData,
    ) -> Result<()> {
        let texture = Texture {
            handle: (data as *mut ffi::VRVulkanTextureData).cast(),
            eType: ffi::TEXTURE_TYPE_VULKAN,
            eColorSpace: ffi::COLOR_SPACE_GAMMA,
        };
        // SAFETY: `texture` and the data it points to are live for the call; the runtime copies
        // what it needs (the image itself stays alive in the submitter).
        let code = unsafe { (self.ovl().SetOverlayTexture)(handle.0, &texture) };
        self.check("SetOverlayTexture", code)?;
        if let Some(e) = self.entry_mut(handle) {
            e.has_texture = true;
        }
        Ok(())
    }

    // ---- Teardown ----

    /// SPEC §3.1 teardown: clear overlay textures → destroy overlays → wait [`SHUTDOWN_GRACE`] →
    /// `VR_ShutdownInternal` → destroy Vulkan objects (`submitter`). Errors from the runtime are
    /// ignored here (it may already be gone after `VREvent_Quit`).
    pub fn shutdown(mut self, submitter: Option<TextureSubmitter>) {
        self.teardown();
        drop(submitter);
    }

    fn teardown(&mut self) {
        if !self.active {
            return;
        }
        self.active = false;
        for e in self.overlays.iter().filter(|e| e.has_texture) {
            // SAFETY: handle created by this session; result intentionally ignored.
            unsafe { (self.ovl().ClearOverlayTexture)(e.handle.0) };
        }
        for e in self.overlays.iter().filter(|e| e.thumbnail_of.is_none()) {
            // SAFETY: as above.
            unsafe { (self.ovl().DestroyOverlay)(e.handle.0) };
        }
        if !self.overlays.is_empty() {
            std::thread::sleep(SHUTDOWN_GRACE);
        }
        self.overlays.clear();
        // SAFETY: balances the successful init; the tables are not used afterwards.
        unsafe { (self.api.shutdown_internal)() };
    }
}

impl Drop for Vr {
    fn drop(&mut self) {
        self.teardown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_list_split() {
        assert_eq!(
            split_extensions(
                "VK_KHR_external_memory_capabilities  VK_KHR_get_physical_device_properties2 "
            ),
            vec![
                "VK_KHR_external_memory_capabilities",
                "VK_KHR_get_physical_device_properties2"
            ]
        );
        assert!(split_extensions("").is_empty());
    }

    #[test]
    fn sized_string_reader() {
        let src = b"VK_A VK_B\0";
        let s = read_sized_string(|buf, len| {
            if buf.is_null() || (len as usize) < src.len() {
                return src.len() as u32;
            }
            // SAFETY: test buffer of at least `src.len()` bytes.
            unsafe { ptr::copy_nonoverlapping(src.as_ptr(), buf.cast(), src.len()) };
            src.len() as u32
        });
        assert_eq!(s, "VK_A VK_B");
        assert_eq!(read_sized_string(|_, _| 0), "");
    }

    #[test]
    fn activity_levels() {
        assert_eq!(ActivityLevel::from_raw(1), ActivityLevel::UserInteraction);
        assert_eq!(ActivityLevel::from_raw(3), ActivityLevel::Standby);
        assert_eq!(ActivityLevel::from_raw(-1), ActivityLevel::Unknown);
        assert_eq!(ActivityLevel::from_raw(99), ActivityLevel::Unknown);
    }

    #[test]
    fn session_guard_is_exclusive() {
        let g = SessionGuard::acquire().expect("first");
        assert!(matches!(
            SessionGuard::acquire(),
            Err(Error::AlreadyInitialized)
        ));
        drop(g);
        drop(SessionGuard::acquire().expect("after release"));
    }
}
