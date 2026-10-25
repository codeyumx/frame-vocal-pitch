//! OpenVR overlay binding and Vulkan texture submission for frame-vocal-pitch (SPEC §3.1, §3.2).
//!
//! - `libopenvr_api.so` is loaded at runtime ([`find_library`], env `FVP_OPENVR_LIB`).
//! - [`is_steamvr_running`] probes with `VRApplication_Background`, which never launches SteamVR.
//! - [`Vr`] is an `VRApplication_Overlay` session: overlays, transforms, input, events, poses.
//! - [`TextureSubmitter`] owns a Vulkan instance/device built from OpenVR's required extensions
//!   and uploads CPU-rendered RGBA8 frames into two alternating images per overlay.
//! - [`Vr::shutdown`] performs the §3.1 teardown order.
//!
//! All `unsafe` code of the project's OpenVR/Vulkan side lives in this crate.

#![deny(unsafe_op_in_unsafe_fn)]

mod events;
mod ffi;
mod loader;
mod matrix;
mod vk;
mod vr;

use std::fmt;
use std::path::PathBuf;

pub use events::{Event, MouseButton};
pub use loader::{find_library, library_candidates, DEVICE_LIBRARY_PATH, LIBRARY_ENV};
pub use matrix::{cross, dot, normalize, sub, Mat34, Vec3};
pub use vk::{FrameRef, TextureSubmitter};
pub use vr::{
    is_steamvr_running, ActivityLevel, ControllerRole, InputMethod, OverlayFlag, Vr, HMD_INDEX,
    SHUTDOWN_GRACE,
};

/// Handle of an overlay created through [`Vr`] (`VROverlayHandle_t`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct OverlayHandle(pub u64);

/// Errors from loading OpenVR, talking to the runtime, or Vulkan.
#[derive(Debug)]
pub enum Error {
    /// No candidate library path exists.
    LibraryNotFound { tried: Vec<PathBuf> },
    /// The library exists but could not be loaded or lacks a required symbol.
    LibraryLoad { path: PathBuf, message: String },
    /// `VR_InitInternal2` failed with an `EVRInitError` other than "no server".
    Init { code: i32, description: String },
    /// An interface table could not be obtained (version mismatch with the runtime).
    InterfaceNotFound { version: &'static str, code: i32 },
    /// An `IVROverlay` call returned an `EVROverlayError`.
    Overlay {
        call: &'static str,
        code: i32,
        name: String,
    },
    /// An OpenVR session already exists in this process (only one may exist at a time).
    AlreadyInitialized,
    /// A string argument contained an interior NUL byte.
    InvalidString(&'static str),
    /// A Vulkan call or requirement failed.
    Vulkan(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::LibraryNotFound { tried } => {
                write!(f, "OpenVR library not found (tried:")?;
                for p in tried {
                    write!(f, " {}", p.display())?;
                }
                write!(f, ")")
            }
            Error::LibraryLoad { path, message } => {
                write!(
                    f,
                    "cannot load OpenVR library {}: {message}",
                    path.display()
                )
            }
            Error::Init { code, description } => {
                write!(f, "OpenVR init failed ({code}): {description}")
            }
            Error::InterfaceNotFound { version, code } => {
                write!(
                    f,
                    "OpenVR interface {version} unavailable (EVRInitError {code})"
                )
            }
            Error::Overlay { call, code, name } => write!(f, "{call} failed: {name} ({code})"),
            Error::AlreadyInitialized => {
                write!(f, "an OpenVR session is already active in this process")
            }
            Error::InvalidString(what) => write!(f, "{what} contains a NUL byte"),
            Error::Vulkan(msg) => write!(f, "Vulkan: {msg}"),
        }
    }
}

impl std::error::Error for Error {}

/// Result alias for this crate.
pub type Result<T, E = Error> = std::result::Result<T, E>;
