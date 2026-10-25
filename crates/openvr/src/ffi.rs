//! Hand-written FFI for the subset of `openvr_capi.h` (OpenVR SDK v2.15.6) that this crate uses.
//!
//! The header is vendored in `vendor/openvr_capi.h` together with its BSD-3-Clause `LICENSE`.
//! Function tables are declared entry-for-entry in header order; entries this crate never calls
//! are typed as [`Unused`] (a pointer-sized slot). A unit test parses the vendored header and
//! checks every table's entry names and count, so a layout slip cannot go unnoticed.

#![allow(non_snake_case, dead_code)]

use std::ffi::{c_char, c_void};

/// A function-table slot this crate never calls. Pointer-sized, like every table entry.
pub type Unused = Option<unsafe extern "C" fn()>;

pub type TrackedDeviceIndex = u32;
pub type OverlayHandleRaw = u64;
pub type EVRInitError = i32;
pub type EVROverlayError = i32;

pub const IVR_SYSTEM_VERSION: &str = "IVRSystem_026";
pub const IVR_COMPOSITOR_VERSION: &str = "IVRCompositor_029";
pub const IVR_OVERLAY_VERSION: &str = "IVROverlay_028";

pub const STEAMVR_VERSION: (u32, u32, u32) = (2, 15, 6);

pub const K_UN_TRACKED_DEVICE_INDEX_HMD: u32 = 0;
pub const K_UN_MAX_TRACKED_DEVICE_COUNT: u32 = 64;
pub const K_UN_TRACKED_DEVICE_INDEX_INVALID: u32 = 0xFFFF_FFFF;

// EVRApplicationType
pub const VR_APPLICATION_OVERLAY: i32 = 2;
pub const VR_APPLICATION_BACKGROUND: i32 = 3;

// EVRInitError (subset)
pub const VR_INIT_ERROR_NONE: i32 = 0;
pub const VR_INIT_ERROR_INIT_NO_SERVER_FOR_BACKGROUND_APP: i32 = 121;
pub const VR_INIT_ERROR_INIT_INTERFACE_NOT_FOUND: i32 = 105;

// EVROverlayError (subset)
pub const VR_OVERLAY_ERROR_NONE: i32 = 0;
pub const VR_OVERLAY_ERROR_THUMBNAIL_CANT_BE_DESTROYED: i32 = 21;

// ETextureType / EColorSpace
pub const TEXTURE_TYPE_VULKAN: i32 = 2;
pub const COLOR_SPACE_GAMMA: i32 = 1;

// ETrackingUniverseOrigin
pub const TRACKING_UNIVERSE_STANDING: i32 = 1;

// ETrackedControllerRole
pub const TRACKED_CONTROLLER_ROLE_LEFT_HAND: i32 = 1;
pub const TRACKED_CONTROLLER_ROLE_RIGHT_HAND: i32 = 2;

// EDeviceActivityLevel
pub const DEVICE_ACTIVITY_LEVEL_UNKNOWN: i32 = -1;
pub const DEVICE_ACTIVITY_LEVEL_IDLE: i32 = 0;
pub const DEVICE_ACTIVITY_LEVEL_USER_INTERACTION: i32 = 1;
pub const DEVICE_ACTIVITY_LEVEL_USER_INTERACTION_TIMEOUT: i32 = 2;
pub const DEVICE_ACTIVITY_LEVEL_STANDBY: i32 = 3;
pub const DEVICE_ACTIVITY_LEVEL_IDLE_TIMEOUT: i32 = 4;

// VROverlayInputMethod
pub const VR_OVERLAY_INPUT_METHOD_NONE: i32 = 0;
pub const VR_OVERLAY_INPUT_METHOD_MOUSE: i32 = 1;

// VROverlayFlags (subset)
pub const VR_OVERLAY_FLAGS_NO_DASHBOARD_TAB: i32 = 8;
pub const VR_OVERLAY_FLAGS_SEND_VR_DISCRETE_SCROLL_EVENTS: i32 = 64;
pub const VR_OVERLAY_FLAGS_SEND_VR_SMOOTH_SCROLL_EVENTS: i32 = 131_072;
pub const VR_OVERLAY_FLAGS_IS_PREMULTIPLIED: i32 = 2_097_152;

// EVRMouseButton
pub const VR_MOUSE_BUTTON_LEFT: u32 = 1;
pub const VR_MOUSE_BUTTON_RIGHT: u32 = 2;
pub const VR_MOUSE_BUTTON_MIDDLE: u32 = 4;

// EVREventType (subset)
pub const VR_EVENT_TRACKED_DEVICE_ACTIVATED: u32 = 100;
pub const VR_EVENT_TRACKED_DEVICE_DEACTIVATED: u32 = 101;
pub const VR_EVENT_TRACKED_DEVICE_ROLE_CHANGED: u32 = 108;
pub const VR_EVENT_MOUSE_MOVE: u32 = 300;
pub const VR_EVENT_MOUSE_BUTTON_DOWN: u32 = 301;
pub const VR_EVENT_MOUSE_BUTTON_UP: u32 = 302;
pub const VR_EVENT_SCROLL_DISCRETE: u32 = 305;
pub const VR_EVENT_SCROLL_SMOOTH: u32 = 309;
pub const VR_EVENT_OVERLAY_SHOWN: u32 = 500;
pub const VR_EVENT_OVERLAY_HIDDEN: u32 = 501;
pub const VR_EVENT_DASHBOARD_ACTIVATED: u32 = 502;
pub const VR_EVENT_DASHBOARD_DEACTIVATED: u32 = 503;
pub const VR_EVENT_QUIT: u32 = 700;

/// `HmdMatrix34_t`: row-major 3×4 affine transform.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HmdMatrix34 {
    pub m: [[f32; 4]; 3],
}

/// `HmdVector2_t`
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct HmdVector2 {
    pub v: [f32; 2],
}

/// `HmdVector3_t`
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct HmdVector3 {
    pub v: [f32; 3],
}

/// `TrackedDevicePose_t`
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct TrackedDevicePose {
    pub mDeviceToAbsoluteTracking: HmdMatrix34,
    pub vVelocity: HmdVector3,
    pub vAngularVelocity: HmdVector3,
    pub eTrackingResult: i32,
    pub bPoseIsValid: bool,
    pub bDeviceIsConnected: bool,
}

impl TrackedDevicePose {
    pub const ZERO: Self = Self {
        mDeviceToAbsoluteTracking: HmdMatrix34 { m: [[0.0; 4]; 3] },
        vVelocity: HmdVector3 { v: [0.0; 3] },
        vAngularVelocity: HmdVector3 { v: [0.0; 3] },
        eTrackingResult: 0,
        bPoseIsValid: false,
        bDeviceIsConnected: false,
    };
}

/// `Texture_t`
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Texture {
    pub handle: *mut c_void,
    pub eType: i32,
    pub eColorSpace: i32,
}

/// `VRVulkanTextureData_t`
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct VRVulkanTextureData {
    pub m_nImage: u64,
    pub m_pDevice: *mut c_void,
    pub m_pPhysicalDevice: *mut c_void,
    pub m_pInstance: *mut c_void,
    pub m_pQueue: *mut c_void,
    pub m_nQueueFamilyIndex: u32,
    pub m_nWidth: u32,
    pub m_nHeight: u32,
    pub m_nFormat: u32,
    pub m_nSampleCount: u32,
}

/// Size of `VREvent_Data_t` (the largest member is `VREvent_Reserved_t`, six `uint64_t`).
pub const VR_EVENT_DATA_SIZE: usize = 48;

/// `VREvent_t`. On Linux the header wraps it in `#pragma pack(push, 4)`, so the 8-byte-aligned
/// data union starts at offset 12 and the struct is 60 bytes. The union is kept as raw bytes and
/// decoded per event type.
#[repr(C, packed(4))]
#[derive(Clone, Copy)]
pub struct VREvent {
    pub eventType: u32,
    pub trackedDeviceIndex: TrackedDeviceIndex,
    pub eventAgeSeconds: f32,
    pub data: [u8; VR_EVENT_DATA_SIZE],
}

impl VREvent {
    pub const ZERO: Self = Self {
        eventType: 0,
        trackedDeviceIndex: 0,
        eventAgeSeconds: 0.0,
        data: [0; VR_EVENT_DATA_SIZE],
    };

    /// Reads a little-/native-endian `f32` from the data union at `offset`.
    pub fn data_f32(&self, offset: usize) -> f32 {
        f32::from_ne_bytes(self.data_4(offset))
    }

    /// Reads a native-endian `u32` from the data union at `offset`.
    pub fn data_u32(&self, offset: usize) -> u32 {
        u32::from_ne_bytes(self.data_4(offset))
    }

    fn data_4(&self, offset: usize) -> [u8; 4] {
        let d = self.data;
        [d[offset], d[offset + 1], d[offset + 2], d[offset + 3]]
    }
}

// Offsets inside the data union (from `VREvent_Mouse_t` / `VREvent_Scroll_t`).
pub const MOUSE_X: usize = 0;
pub const MOUSE_Y: usize = 4;
pub const MOUSE_BUTTON: usize = 8;
pub const SCROLL_XDELTA: usize = 0;
pub const SCROLL_YDELTA: usize = 4;

/// Global entry points exported by `libopenvr_api.so`.
pub type FnInitInternal2 = unsafe extern "C" fn(
    pe_error: *mut EVRInitError,
    app_type: i32,
    startup_info: *const c_char,
) -> u32;
pub type FnShutdownInternal = unsafe extern "C" fn();
pub type FnGetGenericInterface =
    unsafe extern "C" fn(version: *const c_char, pe_error: *mut EVRInitError) -> isize;
pub type FnIsInterfaceVersionValid = unsafe extern "C" fn(version: *const c_char) -> bool;
pub type FnGetInitErrorAsEnglishDescription =
    unsafe extern "C" fn(error: EVRInitError) -> *const c_char;

macro_rules! fn_table {
    ($(#[$meta:meta])* pub struct $name:ident = $cname:literal { $($field:ident : $ty:ty),* $(,)? }) => {
        $(#[$meta])*
        #[repr(C)]
        pub struct $name {
            $(pub $field: $ty),*
        }

        impl $name {
            /// Name of the struct in `openvr_capi.h`.
            #[allow(unused)]
            pub const C_NAME: &'static str = $cname;
            /// Entry names in declaration order (checked against the vendored header in tests).
            #[allow(unused)]
            pub const FIELD_NAMES: &'static [&'static str] = &[$(stringify!($field)),*];
        }
    };
}

fn_table! {
    /// `struct VR_IVRSystem_FnTable` (IVRSystem_026).
    pub struct SystemTable = "VR_IVRSystem_FnTable" {
        GetRecommendedRenderTargetSize: Unused,
        GetProjectionMatrix: Unused,
        GetProjectionRaw: Unused,
        ComputeDistortion: Unused,
        ComputeDistortionSet: Unused,
        GetEyeToHeadTransform: Unused,
        GetTimeSinceLastVsync: Unused,
        GetD3D9AdapterIndex: Unused,
        GetDXGIOutputInfo: Unused,
        GetOutputDevice: unsafe extern "C" fn(pn_device: *mut u64, texture_type: i32, instance: *mut c_void),
        IsDisplayOnDesktop: Unused,
        SetDisplayVisibility: Unused,
        GetDeviceToAbsoluteTrackingPose: unsafe extern "C" fn(
            origin: i32,
            predicted_seconds: f32,
            poses: *mut TrackedDevicePose,
            count: u32,
        ),
        GetSeatedZeroPoseToStandingAbsoluteTrackingPose: Unused,
        GetRawZeroPoseToStandingAbsoluteTrackingPose: Unused,
        GetSortedTrackedDeviceIndicesOfClass: Unused,
        GetTrackedDeviceActivityLevel: unsafe extern "C" fn(device: TrackedDeviceIndex) -> i32,
        ApplyTransform: Unused,
        GetTrackedDeviceIndexForControllerRole: unsafe extern "C" fn(role: i32) -> TrackedDeviceIndex,
        GetControllerRoleForTrackedDeviceIndex: Unused,
        GetTrackedDeviceClass: Unused,
        IsTrackedDeviceConnected: unsafe extern "C" fn(device: TrackedDeviceIndex) -> bool,
        GetBoolTrackedDeviceProperty: Unused,
        GetFloatTrackedDeviceProperty: Unused,
        GetInt32TrackedDeviceProperty: Unused,
        GetUint64TrackedDeviceProperty: Unused,
        GetMatrix34TrackedDeviceProperty: Unused,
        GetArrayTrackedDeviceProperty: Unused,
        GetStringTrackedDeviceProperty: Unused,
        GetPropErrorNameFromEnum: Unused,
        PollNextEvent: unsafe extern "C" fn(event: *mut VREvent, size: u32) -> bool,
        PollNextEventWithPose: Unused,
        PollNextEventWithPoseAndOverlays: Unused,
        GetEventTypeNameFromEnum: Unused,
        GetHiddenAreaMesh: Unused,
        GetEyeTrackedFoveationCenter: Unused,
        GetEyeTrackedFoveationCenterForProjection: Unused,
        GetControllerState: Unused,
        GetControllerStateWithPose: Unused,
        TriggerHapticPulse: Unused,
        GetButtonIdNameFromEnum: Unused,
        GetControllerAxisTypeNameFromEnum: Unused,
        IsInputAvailable: Unused,
        IsSteamVRDrawingControllers: Unused,
        ShouldApplicationPause: Unused,
        ShouldApplicationReduceRenderingWork: Unused,
        PerformFirmwareUpdate: Unused,
        AcknowledgeQuit_Exiting: unsafe extern "C" fn(),
        GetAppContainerFilePaths: Unused,
        GetRuntimeVersion: unsafe extern "C" fn() -> *const c_char,
        SetSDKVersion: unsafe extern "C" fn(major: u32, minor: u32, build: u32) -> EVRInitError,
    }
}

fn_table! {
    /// `struct VR_IVRCompositor_FnTable` (IVRCompositor_029).
    pub struct CompositorTable = "VR_IVRCompositor_FnTable" {
        SetTrackingSpace: Unused,
        GetTrackingSpace: Unused,
        WaitGetPoses: Unused,
        GetLastPoses: Unused,
        GetLastPoseForTrackedDeviceIndex: Unused,
        GetSubmitTexture: Unused,
        Submit: Unused,
        SubmitWithArrayIndex: Unused,
        ClearLastSubmittedFrame: Unused,
        PostPresentHandoff: Unused,
        GetFrameTiming: Unused,
        GetFrameTimings: Unused,
        GetFrameTimeRemaining: Unused,
        GetCumulativeStats: Unused,
        FadeToColor: Unused,
        GetCurrentFadeColor: Unused,
        FadeGrid: Unused,
        GetCurrentGridAlpha: Unused,
        SetSkyboxOverride: Unused,
        ClearSkyboxOverride: Unused,
        CompositorBringToFront: Unused,
        CompositorGoToBack: Unused,
        CompositorQuit: Unused,
        IsFullscreen: Unused,
        GetCurrentSceneFocusProcess: Unused,
        GetLastFrameRenderer: Unused,
        CanRenderScene: Unused,
        ShowMirrorWindow: Unused,
        HideMirrorWindow: Unused,
        IsMirrorWindowVisible: Unused,
        CompositorDumpImages: Unused,
        ShouldAppRenderWithLowResources: Unused,
        ForceInterleavedReprojectionOn: Unused,
        ForceReconnectProcess: Unused,
        SuspendRendering: Unused,
        GetMirrorTextureD3D11: Unused,
        ReleaseMirrorTextureD3D11: Unused,
        GetMirrorTextureGL: Unused,
        ReleaseSharedGLTexture: Unused,
        LockGLSharedTextureForAccess: Unused,
        UnlockGLSharedTextureForAccess: Unused,
        GetVulkanInstanceExtensionsRequired: unsafe extern "C" fn(value: *mut c_char, size: u32) -> u32,
        GetVulkanDeviceExtensionsRequired: unsafe extern "C" fn(
            physical_device: *mut c_void,
            value: *mut c_char,
            size: u32,
        ) -> u32,
        SetExplicitTimingMode: Unused,
        SubmitExplicitTimingData: Unused,
        IsMotionSmoothingEnabled: Unused,
        IsMotionSmoothingSupported: Unused,
        IsCurrentSceneFocusAppLoading: Unused,
        SetStageOverride_Async: Unused,
        ClearStageOverride: Unused,
        GetCompositorBenchmarkResults: Unused,
        GetLastPosePredictionIDs: Unused,
        GetPosesForFrame: Unused,
    }
}

fn_table! {
    /// `struct VR_IVROverlay_FnTable` (IVROverlay_028). `SetOverlayRaw` is deliberately `Unused`
    /// (SPEC §3.2: never use it).
    pub struct OverlayTable = "VR_IVROverlay_FnTable" {
        FindOverlay: Unused,
        CreateOverlay: unsafe extern "C" fn(
            key: *const c_char,
            name: *const c_char,
            handle: *mut OverlayHandleRaw,
        ) -> EVROverlayError,
        CreateSubviewOverlay: Unused,
        DestroyOverlay: unsafe extern "C" fn(handle: OverlayHandleRaw) -> EVROverlayError,
        GetOverlayKey: Unused,
        GetOverlayName: Unused,
        SetOverlayName: Unused,
        GetOverlayImageData: Unused,
        GetOverlayErrorNameFromEnum: unsafe extern "C" fn(error: EVROverlayError) -> *const c_char,
        SetOverlayRenderingPid: Unused,
        GetOverlayRenderingPid: Unused,
        SetOverlayFlag: unsafe extern "C" fn(handle: OverlayHandleRaw, flag: i32, enabled: bool) -> EVROverlayError,
        GetOverlayFlag: Unused,
        GetOverlayFlags: Unused,
        SetOverlayColor: Unused,
        GetOverlayColor: Unused,
        SetOverlayAlpha: unsafe extern "C" fn(handle: OverlayHandleRaw, alpha: f32) -> EVROverlayError,
        GetOverlayAlpha: Unused,
        SetOverlayTexelAspect: Unused,
        GetOverlayTexelAspect: Unused,
        SetOverlaySortOrder: unsafe extern "C" fn(handle: OverlayHandleRaw, order: u32) -> EVROverlayError,
        GetOverlaySortOrder: Unused,
        SetOverlayWidthInMeters: unsafe extern "C" fn(handle: OverlayHandleRaw, meters: f32) -> EVROverlayError,
        GetOverlayWidthInMeters: Unused,
        SetOverlayCurvature: Unused,
        GetOverlayCurvature: Unused,
        SetOverlayPreCurvePitch: Unused,
        GetOverlayPreCurvePitch: Unused,
        SetOverlayTextureColorSpace: Unused,
        GetOverlayTextureColorSpace: Unused,
        SetOverlayTextureBounds: Unused,
        GetOverlayTextureBounds: Unused,
        GetOverlayTransformType: Unused,
        SetOverlayTransformAbsolute: Unused,
        GetOverlayTransformAbsolute: Unused,
        SetOverlayTransformTrackedDeviceRelative: unsafe extern "C" fn(
            handle: OverlayHandleRaw,
            device: TrackedDeviceIndex,
            transform: *const HmdMatrix34,
        ) -> EVROverlayError,
        GetOverlayTransformTrackedDeviceRelative: Unused,
        SetOverlayTransformTrackedDeviceComponent: Unused,
        GetOverlayTransformTrackedDeviceComponent: Unused,
        SetOverlayTransformCursor: Unused,
        GetOverlayTransformCursor: Unused,
        SetOverlayTransformProjection: Unused,
        SetSubviewPosition: Unused,
        ShowOverlay: unsafe extern "C" fn(handle: OverlayHandleRaw) -> EVROverlayError,
        HideOverlay: unsafe extern "C" fn(handle: OverlayHandleRaw) -> EVROverlayError,
        IsOverlayVisible: unsafe extern "C" fn(handle: OverlayHandleRaw) -> bool,
        GetTransformForOverlayCoordinates: Unused,
        WaitFrameSync: Unused,
        PollNextOverlayEvent: unsafe extern "C" fn(handle: OverlayHandleRaw, event: *mut VREvent, size: u32) -> bool,
        GetOverlayInputMethod: Unused,
        SetOverlayInputMethod: unsafe extern "C" fn(handle: OverlayHandleRaw, method: i32) -> EVROverlayError,
        GetOverlayMouseScale: Unused,
        SetOverlayMouseScale: unsafe extern "C" fn(handle: OverlayHandleRaw, scale: *const HmdVector2) -> EVROverlayError,
        ComputeOverlayIntersection: Unused,
        IsHoverTargetOverlay: Unused,
        SetOverlayIntersectionMask: Unused,
        TriggerLaserMouseHapticVibration: Unused,
        SetOverlayCursor: Unused,
        SetOverlayCursorPositionOverride: Unused,
        ClearOverlayCursorPositionOverride: Unused,
        SetOverlayTexture: unsafe extern "C" fn(handle: OverlayHandleRaw, texture: *const Texture) -> EVROverlayError,
        ClearOverlayTexture: unsafe extern "C" fn(handle: OverlayHandleRaw) -> EVROverlayError,
        SetOverlayRaw: Unused,
        SetOverlayFromFile: unsafe extern "C" fn(handle: OverlayHandleRaw, path: *const c_char) -> EVROverlayError,
        GetOverlayTexture: Unused,
        ReleaseNativeOverlayHandle: Unused,
        GetOverlayTextureSize: Unused,
        CreateDashboardOverlay: unsafe extern "C" fn(
            key: *const c_char,
            name: *const c_char,
            main: *mut OverlayHandleRaw,
            thumbnail: *mut OverlayHandleRaw,
        ) -> EVROverlayError,
        IsDashboardVisible: unsafe extern "C" fn() -> bool,
        IsActiveDashboardOverlay: unsafe extern "C" fn(handle: OverlayHandleRaw) -> bool,
        SetDashboardOverlaySceneProcess: Unused,
        GetDashboardOverlaySceneProcess: Unused,
        ShowDashboard: unsafe extern "C" fn(overlay_to_show: *const c_char),
        GetPrimaryDashboardDevice: Unused,
        ShowKeyboard: Unused,
        ShowKeyboardForOverlay: Unused,
        GetKeyboardText: Unused,
        HideKeyboard: Unused,
        SetKeyboardTransformAbsolute: Unused,
        SetKeyboardPositionForOverlay: Unused,
        ShowMessageOverlay: Unused,
        CloseMessageOverlay: Unused,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::{offset_of, size_of};

    const HEADER: &str = include_str!("../vendor/openvr_capi.h");

    /// Entry names of `struct <c_name>` in the vendored header, in order.
    fn header_entries(c_name: &str) -> Vec<String> {
        let start = HEADER
            .find(&format!("struct {c_name}\n"))
            .unwrap_or_else(|| panic!("{c_name} not in header"));
        let body = &HEADER[start..];
        let body = &body[..body.find("};").expect("table end")];
        body.lines()
            .filter_map(|l| {
                let s = l.find("OPENVR_FNTABLE_CALLTYPE *")? + "OPENVR_FNTABLE_CALLTYPE *".len();
                let rest = &l[s..];
                Some(rest[..rest.find(')')?].to_string())
            })
            .collect()
    }

    fn check<T>(c_name: &str, names: &[&str]) {
        let expected = header_entries(c_name);
        assert!(!expected.is_empty());
        assert_eq!(names, expected.as_slice(), "{c_name} entry order");
        assert_eq!(
            size_of::<T>(),
            expected.len() * size_of::<usize>(),
            "{c_name} size"
        );
    }

    #[test]
    fn tables_match_vendored_header() {
        check::<SystemTable>(SystemTable::C_NAME, SystemTable::FIELD_NAMES);
        check::<CompositorTable>(CompositorTable::C_NAME, CompositorTable::FIELD_NAMES);
        check::<OverlayTable>(OverlayTable::C_NAME, OverlayTable::FIELD_NAMES);
    }

    #[test]
    fn interface_versions_match_header() {
        for v in [
            IVR_SYSTEM_VERSION,
            IVR_COMPOSITOR_VERSION,
            IVR_OVERLAY_VERSION,
        ] {
            let prefix = v.split('_').next().unwrap();
            assert!(
                HEADER.contains(&format!("static const char * {prefix}_Version = \"{v}\";")),
                "{v}"
            );
        }
    }

    #[test]
    fn struct_layouts() {
        assert_eq!(size_of::<VREvent>(), 60);
        assert_eq!(offset_of!(VREvent, data), 12);
        assert_eq!(size_of::<HmdMatrix34>(), 48);
        assert_eq!(size_of::<TrackedDevicePose>(), 80);
        assert_eq!(offset_of!(TrackedDevicePose, bPoseIsValid), 76);
        assert_eq!(size_of::<Texture>(), 16);
        assert_eq!(size_of::<VRVulkanTextureData>(), 60 + 4);
        assert_eq!(offset_of!(VRVulkanTextureData, m_nQueueFamilyIndex), 40);
    }

    #[test]
    fn enum_values_match_header() {
        let pairs: &[(&str, i64)] = &[
            (
                "EVRApplicationType_VRApplication_Overlay",
                VR_APPLICATION_OVERLAY as i64,
            ),
            (
                "EVRApplicationType_VRApplication_Background",
                VR_APPLICATION_BACKGROUND as i64,
            ),
            (
                "EVRInitError_VRInitError_Init_NoServerForBackgroundApp",
                VR_INIT_ERROR_INIT_NO_SERVER_FOR_BACKGROUND_APP as i64,
            ),
            (
                "ETextureType_TextureType_Vulkan",
                TEXTURE_TYPE_VULKAN as i64,
            ),
            ("EColorSpace_ColorSpace_Gamma", COLOR_SPACE_GAMMA as i64),
            (
                "ETrackingUniverseOrigin_TrackingUniverseStanding",
                TRACKING_UNIVERSE_STANDING as i64,
            ),
            (
                "ETrackedControllerRole_TrackedControllerRole_LeftHand",
                TRACKED_CONTROLLER_ROLE_LEFT_HAND as i64,
            ),
            (
                "ETrackedControllerRole_TrackedControllerRole_RightHand",
                TRACKED_CONTROLLER_ROLE_RIGHT_HAND as i64,
            ),
            (
                "EDeviceActivityLevel_k_EDeviceActivityLevel_UserInteraction",
                DEVICE_ACTIVITY_LEVEL_USER_INTERACTION as i64,
            ),
            (
                "VROverlayInputMethod_Mouse",
                VR_OVERLAY_INPUT_METHOD_MOUSE as i64,
            ),
            (
                "VROverlayFlags_SendVRSmoothScrollEvents",
                VR_OVERLAY_FLAGS_SEND_VR_SMOOTH_SCROLL_EVENTS as i64,
            ),
            (
                "VROverlayFlags_IsPremultiplied",
                VR_OVERLAY_FLAGS_IS_PREMULTIPLIED as i64,
            ),
            (
                "EVRMouseButton_VRMouseButton_Left",
                VR_MOUSE_BUTTON_LEFT as i64,
            ),
            (
                "EVREventType_VREvent_TrackedDeviceActivated",
                VR_EVENT_TRACKED_DEVICE_ACTIVATED as i64,
            ),
            (
                "EVREventType_VREvent_TrackedDeviceDeactivated",
                VR_EVENT_TRACKED_DEVICE_DEACTIVATED as i64,
            ),
            (
                "EVREventType_VREvent_TrackedDeviceRoleChanged",
                VR_EVENT_TRACKED_DEVICE_ROLE_CHANGED as i64,
            ),
            ("EVREventType_VREvent_MouseMove", VR_EVENT_MOUSE_MOVE as i64),
            (
                "EVREventType_VREvent_MouseButtonDown",
                VR_EVENT_MOUSE_BUTTON_DOWN as i64,
            ),
            (
                "EVREventType_VREvent_MouseButtonUp",
                VR_EVENT_MOUSE_BUTTON_UP as i64,
            ),
            (
                "EVREventType_VREvent_ScrollDiscrete",
                VR_EVENT_SCROLL_DISCRETE as i64,
            ),
            (
                "EVREventType_VREvent_ScrollSmooth",
                VR_EVENT_SCROLL_SMOOTH as i64,
            ),
            (
                "EVREventType_VREvent_OverlayShown",
                VR_EVENT_OVERLAY_SHOWN as i64,
            ),
            (
                "EVREventType_VREvent_OverlayHidden",
                VR_EVENT_OVERLAY_HIDDEN as i64,
            ),
            (
                "EVREventType_VREvent_DashboardActivated",
                VR_EVENT_DASHBOARD_ACTIVATED as i64,
            ),
            (
                "EVREventType_VREvent_DashboardDeactivated",
                VR_EVENT_DASHBOARD_DEACTIVATED as i64,
            ),
            ("EVREventType_VREvent_Quit", VR_EVENT_QUIT as i64),
        ];
        for (name, value) in pairs {
            assert!(
                HEADER.contains(&format!("\t{name} = {value},")),
                "{name} = {value}"
            );
        }
    }
}
