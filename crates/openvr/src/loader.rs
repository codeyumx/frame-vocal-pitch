//! Runtime loading of `libopenvr_api.so` (SPEC §3.1). The library is never bundled; it is
//! `dlopen`ed from the SteamVR installation once per process and kept loaded for the
//! lifetime of the process (OpenVR's client library is not designed to be unloaded).

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use crate::ffi::{
    FnGetGenericInterface, FnGetInitErrorAsEnglishDescription, FnInitInternal2,
    FnIsInterfaceVersionValid, FnShutdownInternal,
};
use crate::Error;

/// Location of the OpenVR client library on the Steam Frame (SPEC §2).
pub const DEVICE_LIBRARY_PATH: &str = "/opt/steamvr/bin/linuxarm64/libopenvr_api.so";

/// Environment variable that overrides the library path.
pub const LIBRARY_ENV: &str = "FVP_OPENVR_LIB";

/// Desktop SteamVR library, relative to `$HOME`; tried only on x86_64 hosts (development).
const DESKTOP_LIBRARY_REL: &str =
    ".steam/steam/steamapps/common/SteamVR/bin/linux64/libopenvr_api.so";

/// Candidate library paths in the order they are tried.
///
/// A non-empty `env_override` is the only candidate. Otherwise the Frame's path comes first;
/// on `x86_64` the desktop SteamVR library under `home` follows as a development fallback.
pub fn library_candidates(
    env_override: Option<&OsStr>,
    arch: &str,
    home: Option<&Path>,
) -> Vec<PathBuf> {
    if let Some(path) = env_override.filter(|p| !p.is_empty()) {
        return vec![PathBuf::from(path)];
    }
    let mut out = vec![PathBuf::from(DEVICE_LIBRARY_PATH)];
    if arch == "x86_64" {
        if let Some(home) = home.filter(|h| !h.as_os_str().is_empty()) {
            out.push(home.join(DESKTOP_LIBRARY_REL));
        }
    }
    out
}

/// Resolves the OpenVR library for this process (environment, architecture, `$HOME`):
/// the first existing candidate of [`library_candidates`].
pub fn find_library() -> Result<PathBuf, Error> {
    let env: Option<OsString> = std::env::var_os(LIBRARY_ENV);
    let home: Option<OsString> = std::env::var_os("HOME");
    let tried = library_candidates(
        env.as_deref(),
        std::env::consts::ARCH,
        home.as_deref().map(Path::new),
    );
    tried
        .iter()
        .find(|p| p.is_file())
        .cloned()
        .ok_or(Error::LibraryNotFound { tried })
}

/// Global entry points of the loaded library.
pub(crate) struct Api {
    pub path: PathBuf,
    pub init_internal2: FnInitInternal2,
    pub shutdown_internal: FnShutdownInternal,
    pub get_generic_interface: FnGetGenericInterface,
    pub is_interface_version_valid: FnIsInterfaceVersionValid,
    pub init_error_description: Option<FnGetInitErrorAsEnglishDescription>,
    _lib: libloading::Library,
}

static API: OnceLock<Api> = OnceLock::new();
static LOAD_LOCK: Mutex<()> = Mutex::new(());

/// Loads the library on first success and returns the cached entry points afterwards. Failures
/// are not cached, so a later call retries (e.g. after SteamVR is installed or updated).
pub(crate) fn api() -> Result<&'static Api, Error> {
    if let Some(api) = API.get() {
        return Ok(api);
    }
    let _guard = LOAD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(api) = API.get() {
        return Ok(api);
    }
    let loaded = load(&find_library()?)?;
    Ok(API.get_or_init(|| loaded))
}

fn load(path: &Path) -> Result<Api, Error> {
    let load_err = |e: libloading::Error| Error::LibraryLoad {
        path: path.to_owned(),
        message: e.to_string(),
    };
    // SAFETY: loading the OpenVR client library runs its (C++) static initialisers, which have no
    // preconditions on our side. The symbol types below match `openvr.h`/`openvr_capi.h` v2.15.6;
    // the fn pointers stay valid because the library is leaked (never unloaded).
    unsafe {
        let lib = libloading::Library::new(path).map_err(load_err)?;
        let init_internal2 = *lib
            .get::<FnInitInternal2>(b"VR_InitInternal2\0")
            .map_err(load_err)?;
        let shutdown_internal = *lib
            .get::<FnShutdownInternal>(b"VR_ShutdownInternal\0")
            .map_err(load_err)?;
        let get_generic_interface = *lib
            .get::<FnGetGenericInterface>(b"VR_GetGenericInterface\0")
            .map_err(load_err)?;
        let is_interface_version_valid = *lib
            .get::<FnIsInterfaceVersionValid>(b"VR_IsInterfaceVersionValid\0")
            .map_err(load_err)?;
        let init_error_description = lib
            .get::<FnGetInitErrorAsEnglishDescription>(b"VR_GetVRInitErrorAsEnglishDescription\0")
            .ok()
            .map(|s| *s);
        Ok(Api {
            path: path.to_owned(),
            init_internal2,
            shutdown_internal,
            get_generic_interface,
            is_interface_version_valid,
            init_error_description,
            _lib: lib,
        })
    }
}

impl Api {
    /// English description of an `EVRInitError`.
    pub fn init_error_text(&self, code: i32) -> String {
        let Some(f) = self.init_error_description else {
            return format!("EVRInitError {code}");
        };
        // SAFETY: returns a pointer to a static C string (or null) owned by the library.
        let p = unsafe { f(code) };
        if p.is_null() {
            return format!("EVRInitError {code}");
        }
        // SAFETY: non-null, NUL-terminated, static lifetime per the API.
        unsafe { std::ffi::CStr::from_ptr(p) }
            .to_string_lossy()
            .into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_override_wins_exclusively() {
        let c = library_candidates(
            Some(OsStr::new("/tmp/x/libopenvr_api.so")),
            "aarch64",
            Some(Path::new("/home/u")),
        );
        assert_eq!(c, vec![PathBuf::from("/tmp/x/libopenvr_api.so")]);
        let c = library_candidates(
            Some(OsStr::new("/tmp/x.so")),
            "x86_64",
            Some(Path::new("/home/u")),
        );
        assert_eq!(c, vec![PathBuf::from("/tmp/x.so")]);
    }

    #[test]
    fn empty_override_is_ignored() {
        let c = library_candidates(Some(OsStr::new("")), "aarch64", None);
        assert_eq!(c, vec![PathBuf::from(DEVICE_LIBRARY_PATH)]);
    }

    #[test]
    fn aarch64_uses_device_path_only() {
        let c = library_candidates(None, "aarch64", Some(Path::new("/home/u")));
        assert_eq!(c, vec![PathBuf::from(DEVICE_LIBRARY_PATH)]);
    }

    #[test]
    fn x86_64_falls_back_to_desktop_steamvr() {
        let c = library_candidates(None, "x86_64", Some(Path::new("/home/u")));
        assert_eq!(
            c,
            vec![
                PathBuf::from(DEVICE_LIBRARY_PATH),
                PathBuf::from(
                    "/home/u/.steam/steam/steamapps/common/SteamVR/bin/linux64/libopenvr_api.so"
                ),
            ]
        );
        // Without HOME there is no desktop fallback.
        assert_eq!(
            library_candidates(None, "x86_64", None),
            vec![PathBuf::from(DEVICE_LIBRARY_PATH)]
        );
    }

    #[test]
    fn missing_library_reports_tried_paths() {
        let err = load(Path::new("/nonexistent/libopenvr_api.so"))
            .err()
            .expect("load must fail");
        assert!(matches!(err, Error::LibraryLoad { .. }), "{err}");
    }
}
