//! `config.toml` for frame-vocal-pitch (SPEC §6.1).
//!
//! Every key is optional. Missing keys take their default, out-of-range numbers are clamped
//! ([`Config::clamp`]), unknown enum strings fall back to the default with a log line. A file that
//! cannot be parsed at all is renamed to `config.toml.bad` and replaced by defaults
//! ([`load_or_init`]). Saves go through a temporary file and `rename`, mode 0600 ([`save`]).
//! External edits are picked up by a stat-polling [`Watcher`].

#![forbid(unsafe_code)]
#![deny(unsafe_op_in_unsafe_fn)]

use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::de::{self, Deserializer, Visitor};
use serde::{Deserialize, Serialize, Serializer};

/// Directory name under `$XDG_CONFIG_HOME`.
pub const APP_DIR: &str = "frame-vocal-pitch";
/// File name of the configuration file.
pub const FILE_NAME: &str = "config.toml";

/// Target band of the `feminine` preset, Hz.
pub const FEMININE_BAND: (f32, f32) = (165.0, 255.0);
/// Target band of the `masculine` preset, Hz.
pub const MASCULINE_BAND: (f32, f32) = (85.0, 180.0);
/// Minimum gap between `custom_low_hz` and `custom_high_hz`.
pub const CUSTOM_MIN_GAP_HZ: f32 = 10.0;
/// Minimum gap between `detect_min_hz` and `detect_max_hz`.
pub const DETECT_MIN_GAP_HZ: f32 = 50.0;

const FILE_HEADER: &str =
    "# frame-vocal-pitch configuration. Edits are applied while the app runs.\n\n";

/// The whole configuration file.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub hud: Hud,
    pub target: Target,
    pub reminder: Reminder,
    pub vocal_rest: VocalRest,
    pub audio: Audio,
    pub resonance: Resonance,
}

/// `[hud]`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Hud {
    pub enabled: bool,
    pub placement: Placement,
    pub display: DisplayMode,
    #[serde(serialize_with = "ser_f32")]
    pub size_m: f32,
    #[serde(serialize_with = "ser_f32")]
    pub opacity: f32,
    #[serde(serialize_with = "ser_f32")]
    pub nudge_x_m: f32,
    #[serde(serialize_with = "ser_f32")]
    pub nudge_y_m: f32,
    #[serde(serialize_with = "ser_f32")]
    pub nudge_z_m: f32,
    #[serde(deserialize_with = "de_u32")]
    pub fade_ms: u32,
}

impl Default for Hud {
    fn default() -> Self {
        Hud {
            enabled: true,
            placement: Placement::Head,
            display: DisplayMode::Color,
            size_m: 0.05,
            opacity: 0.9,
            nudge_x_m: 0.0,
            nudge_y_m: 0.0,
            nudge_z_m: 0.0,
            fade_ms: 300,
        }
    }
}

/// `[target]`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Target {
    pub preset: Preset,
    #[serde(serialize_with = "ser_f32")]
    pub custom_low_hz: f32,
    #[serde(serialize_with = "ser_f32")]
    pub custom_high_hz: f32,
}

impl Default for Target {
    fn default() -> Self {
        Target {
            preset: Preset::Feminine,
            custom_low_hz: 165.0,
            custom_high_hz: 255.0,
        }
    }
}

/// `[reminder]`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Reminder {
    pub enabled: bool,
    #[serde(serialize_with = "ser_f32")]
    pub delay_s: f32,
    #[serde(serialize_with = "ser_f32")]
    pub pitch_hz: f32,
    #[serde(serialize_with = "ser_f32")]
    pub volume: f32,
}

impl Default for Reminder {
    fn default() -> Self {
        Reminder {
            enabled: false,
            delay_s: 2.0,
            pitch_hz: 440.0,
            volume: 0.5,
        }
    }
}

/// `[vocal_rest]`
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct VocalRest {
    /// Voiced minutes per rolling hour that trigger the toast; 0 = off, else 5–50.
    #[serde(deserialize_with = "de_u32")]
    pub minutes: u32,
}

/// `[audio]`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Audio {
    pub mic_mode: MicMode,
    #[serde(serialize_with = "ser_f32")]
    pub detect_min_hz: f32,
    #[serde(serialize_with = "ser_f32")]
    pub detect_max_hz: f32,
    #[serde(serialize_with = "ser_f32")]
    pub noise_floor_min_dbfs: f32,
}

impl Default for Audio {
    fn default() -> Self {
        Audio {
            mic_mode: MicMode::Bypass,
            detect_min_hz: 60.0,
            detect_max_hz: 500.0,
            noise_floor_min_dbfs: -60.0,
        }
    }
}

/// `[resonance]` (SPEC §5.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Resonance {
    /// Set only after the explanation card was acknowledged.
    pub enabled: bool,
    /// Median window for the displayed value, seconds (4–20).
    #[serde(serialize_with = "ser_f32")]
    pub window_s: f32,
    /// Calibrated formant spacing of the relaxed voice, Hz; `None` = not calibrated.
    #[serde(
        serialize_with = "ser_opt_f32",
        skip_serializing_if = "Option::is_none"
    )]
    pub relaxed_df_hz: Option<f32>,
    /// Calibrated formant spacing of the target voice, Hz; `None` = not calibrated.
    #[serde(
        serialize_with = "ser_opt_f32",
        skip_serializing_if = "Option::is_none"
    )]
    pub target_df_hz: Option<f32>,
}

impl Default for Resonance {
    fn default() -> Self {
        Resonance {
            enabled: false,
            window_s: 8.0,
            relaxed_df_hz: None,
            target_df_hz: None,
        }
    }
}

/// Defines a unit enum stored as a snake_case string. Unknown strings deserialize to the default
/// variant and log a warning instead of failing the whole file.
macro_rules! string_enum {
    ($(#[$meta:meta])* $name:ident, $what:literal, default $default:ident, { $($variant:ident => $text:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum $name {
            $($variant),+
        }

        impl $name {
            /// Every variant, in declaration order.
            pub const ALL: &'static [$name] = &[$($name::$variant),+];

            /// The string used in `config.toml`.
            pub fn as_str(self) -> &'static str {
                match self {
                    $($name::$variant => $text),+
                }
            }

            /// Parses the `config.toml` string; `None` if unknown.
            pub fn from_str_opt(s: &str) -> Option<Self> {
                match s {
                    $($text => Some($name::$variant),)+
                    _ => None,
                }
            }
        }

        impl Default for $name {
            fn default() -> Self {
                $name::$default
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_str(self.as_str())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let s = String::deserialize(d)?;
                Ok($name::from_str_opt(&s).unwrap_or_else(|| {
                    let default = $name::default();
                    log::warn!(
                        "config: unknown {} {:?}, using {:?}",
                        $what,
                        s,
                        default.as_str()
                    );
                    default
                }))
            }
        }
    };
}

string_enum!(
    /// `hud.placement`
    Placement, "hud.placement", default Head, {
        Head => "head",
        LeftWrist => "left_wrist",
        RightWrist => "right_wrist",
    }
);

string_enum!(
    /// `hud.display`
    DisplayMode, "hud.display", default Color, {
        Color => "color",
        Detailed => "detailed",
    }
);

string_enum!(
    /// `target.preset`
    Preset, "target.preset", default Feminine, {
        Feminine => "feminine",
        Masculine => "masculine",
        Custom => "custom",
    }
);

string_enum!(
    /// `audio.mic_mode` (SPEC §6.3)
    MicMode, "audio.mic_mode", default Bypass, {
        Bypass => "bypass",
        Processed => "processed",
    }
);

impl Config {
    /// Target band in Hz for the selected preset.
    pub fn band(&self) -> (f32, f32) {
        match self.target.preset {
            Preset::Feminine => FEMININE_BAND,
            Preset::Masculine => MASCULINE_BAND,
            Preset::Custom => (self.target.custom_low_hz, self.target.custom_high_hz),
        }
    }

    /// Clamps every number into its SPEC §6.1 range. NaN takes the key's default.
    ///
    /// Pair rules: `detect_min_hz ≤ detect_max_hz − 50` (raises the max, or lowers the min at the
    /// top of the range); custom band inside the detection span with `low ≤ high − 10` (raises the
    /// high, or lowers the low at the top of the span); `vocal_rest.minutes` 1–4 becomes 5.
    /// A resonance calibration value that is not a finite positive number becomes "not
    /// calibrated" (`None`).
    pub fn clamp(&mut self) {
        let d = Config::default();

        let h = &mut self.hud;
        h.size_m = clamp_f(h.size_m, 0.02, 0.20, d.hud.size_m, "hud.size_m");
        h.opacity = clamp_f(h.opacity, 0.1, 1.0, d.hud.opacity, "hud.opacity");
        h.nudge_x_m = clamp_f(h.nudge_x_m, -0.5, 0.5, 0.0, "hud.nudge_x_m");
        h.nudge_y_m = clamp_f(h.nudge_y_m, -0.5, 0.5, 0.0, "hud.nudge_y_m");
        h.nudge_z_m = clamp_f(h.nudge_z_m, -0.5, 0.5, 0.0, "hud.nudge_z_m");
        if h.fade_ms > 2000 {
            log::debug!("config: hud.fade_ms {} clamped to 2000", h.fade_ms);
            h.fade_ms = 2000;
        }

        let r = &mut self.reminder;
        r.delay_s = clamp_f(r.delay_s, 0.5, 10.0, d.reminder.delay_s, "reminder.delay_s");
        r.pitch_hz = clamp_f(
            r.pitch_hz,
            100.0,
            1000.0,
            d.reminder.pitch_hz,
            "reminder.pitch_hz",
        );
        r.volume = clamp_f(r.volume, 0.0, 1.0, d.reminder.volume, "reminder.volume");

        let m = &mut self.vocal_rest.minutes;
        let clamped = match *m {
            0 => 0,
            1..=4 => 5,
            n => n.min(50),
        };
        if clamped != *m {
            log::debug!("config: vocal_rest.minutes {} clamped to {}", *m, clamped);
            *m = clamped;
        }

        let a = &mut self.audio;
        a.noise_floor_min_dbfs = clamp_f(
            a.noise_floor_min_dbfs,
            -90.0,
            -20.0,
            d.audio.noise_floor_min_dbfs,
            "audio.noise_floor_min_dbfs",
        );
        a.detect_min_hz = clamp_f(
            a.detect_min_hz,
            40.0,
            1000.0,
            d.audio.detect_min_hz,
            "audio.detect_min_hz",
        );
        a.detect_max_hz = clamp_f(
            a.detect_max_hz,
            40.0,
            1000.0,
            d.audio.detect_max_hz,
            "audio.detect_max_hz",
        );
        (a.detect_min_hz, a.detect_max_hz) = enforce_gap(
            a.detect_min_hz,
            a.detect_max_hz,
            DETECT_MIN_GAP_HZ,
            1000.0,
            "audio.detect_min_hz/detect_max_hz",
        );

        let (lo, hi) = (a.detect_min_hz, a.detect_max_hz);
        let t = &mut self.target;
        t.custom_low_hz = clamp_f(
            t.custom_low_hz,
            lo,
            hi,
            d.target.custom_low_hz.clamp(lo, hi),
            "target.custom_low_hz",
        );
        t.custom_high_hz = clamp_f(
            t.custom_high_hz,
            lo,
            hi,
            d.target.custom_high_hz.clamp(lo, hi),
            "target.custom_high_hz",
        );
        (t.custom_low_hz, t.custom_high_hz) = enforce_gap(
            t.custom_low_hz,
            t.custom_high_hz,
            CUSTOM_MIN_GAP_HZ,
            hi,
            "target.custom_low_hz/custom_high_hz",
        );

        let res = &mut self.resonance;
        res.window_s = clamp_f(
            res.window_s,
            4.0,
            20.0,
            d.resonance.window_s,
            "resonance.window_s",
        );
        res.relaxed_df_hz = valid_calibration(res.relaxed_df_hz, "resonance.relaxed_df_hz");
        res.target_df_hz = valid_calibration(res.target_df_hz, "resonance.target_df_hz");
    }
}

fn clamp_f(v: f32, lo: f32, hi: f32, default: f32, key: &str) -> f32 {
    let out = if v.is_nan() { default } else { v.clamp(lo, hi) };
    if out != v {
        log::debug!("config: {key} {v} clamped to {out}");
    }
    out
}

fn valid_calibration(v: Option<f32>, key: &str) -> Option<f32> {
    match v {
        Some(x) if !(x.is_finite() && x > 0.0) => {
            log::debug!("config: {key} {x} is not a valid calibration; treated as absent");
            None
        }
        v => v,
    }
}

/// Makes `low ≤ high − gap`, preferring to raise `high`; at the `top` limit lowers `low` instead.
fn enforce_gap(low: f32, high: f32, gap: f32, top: f32, key: &str) -> (f32, f32) {
    if low <= high - gap {
        return (low, high);
    }
    let (new_low, new_high) = if low + gap <= top {
        (low, low + gap)
    } else {
        (top - gap, top)
    };
    log::debug!("config: {key} {low}/{high} adjusted to {new_low}/{new_high}");
    (new_low, new_high)
}

/// Serializes an `f32` through its shortest decimal form so `0.05` is written as `0.05`, not
/// `0.05000000074505806`.
fn ser_f32<S: Serializer>(v: &f32, s: S) -> Result<S::Ok, S::Error> {
    let shortest: f64 = v.to_string().parse().unwrap_or(f64::from(*v));
    s.serialize_f64(shortest)
}

/// [`ser_f32`] for optional values; `None` is skipped by `skip_serializing_if`.
fn ser_opt_f32<S: Serializer>(v: &Option<f32>, s: S) -> Result<S::Ok, S::Error> {
    match v {
        Some(x) => ser_f32(x, s),
        None => s.serialize_none(),
    }
}

/// Deserializes a non-negative integer, saturating negative values to 0 and huge ones to
/// `u32::MAX` so clamping can handle them instead of rejecting the file.
fn de_u32<'de, D: Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
    struct V;
    impl Visitor<'_> for V {
        type Value = u32;
        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("an integer")
        }
        fn visit_i64<E: de::Error>(self, v: i64) -> Result<u32, E> {
            Ok(v.clamp(0, i64::from(u32::MAX)) as u32)
        }
        fn visit_u64<E: de::Error>(self, v: u64) -> Result<u32, E> {
            Ok(v.min(u64::from(u32::MAX)) as u32)
        }
    }
    d.deserialize_i64(V)
}

/// Parses `config.toml` text and clamps the result.
pub fn parse(text: &str) -> Result<Config, toml::de::Error> {
    let mut cfg: Config = toml::from_str(text)?;
    cfg.clamp();
    Ok(cfg)
}

/// Renders the configuration as `config.toml` text.
pub fn to_toml(cfg: &Config) -> String {
    let body = toml::to_string_pretty(cfg).expect("Config always serializes to TOML");
    format!("{FILE_HEADER}{body}")
}

/// `$XDG_CONFIG_HOME/frame-vocal-pitch/config.toml`, falling back to `~/.config/...`.
pub fn default_path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| {
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/"));
            home.join(".config")
        });
    base.join(APP_DIR).join(FILE_NAME)
}

/// Path the unparseable file is moved to: `<path>.bad`.
pub fn bad_path(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".bad");
    PathBuf::from(s)
}

/// Loads the config at `path`, creating it with defaults if missing. An unparseable file is
/// renamed to `<path>.bad` and replaced by defaults. I/O failures are logged and yield defaults;
/// this never fails.
pub fn load_or_init(path: &Path) -> Config {
    match fs::read_to_string(path) {
        Ok(text) => match parse(&text) {
            Ok(cfg) => cfg,
            Err(e) => {
                let bad = bad_path(path);
                log::warn!(
                    "config: {} is not valid ({}); moved to {} and wrote defaults",
                    path.display(),
                    e.message(),
                    bad.display()
                );
                if let Err(e) = fs::rename(path, &bad) {
                    log::error!(
                        "config: cannot rename {} to {}: {e}",
                        path.display(),
                        bad.display()
                    );
                }
                write_defaults(path)
            }
        },
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            log::info!("config: {} not found; writing defaults", path.display());
            write_defaults(path)
        }
        Err(e) => {
            log::error!(
                "config: cannot read {}: {e}; using defaults",
                path.display()
            );
            Config::default()
        }
    }
}

fn write_defaults(path: &Path) -> Config {
    let cfg = Config::default();
    if let Err(e) = save(&cfg, path) {
        log::error!("config: cannot write {}: {e}", path.display());
    }
    cfg
}

/// Writes `cfg` to `path` atomically: temp file in the same directory (mode 0600), fsync, rename.
/// Creates the parent directory (mode 0700) if needed.
pub fn save(cfg: &Config, path: &Path) -> io::Result<()> {
    let dir = path
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;

    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);

    let result = (|| {
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        // A stale temp file may have been created with another mode.
        f.set_permissions(fs::Permissions::from_mode(0o600))?;
        f.write_all(to_toml(cfg).as_bytes())?;
        f.sync_all()?;
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// Identity of one version of the file, as seen by `stat`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stamp {
    dev: u64,
    ino: u64,
    size: u64,
    mtime_s: i64,
    mtime_ns: i64,
}

impl Stamp {
    fn of(path: &Path) -> Option<Stamp> {
        let m = fs::metadata(path).ok()?;
        Some(Stamp {
            dev: m.dev(),
            ino: m.ino(),
            size: m.size(),
            mtime_s: m.mtime(),
            mtime_ns: m.mtime_nsec(),
        })
    }
}

/// Detects external edits of the config file by polling `stat` (call [`Watcher::poll`] every
/// [`Watcher::INTERVAL`], i.e. 2 Hz).
///
/// A changed file that does not parse is logged once per change and ignored (the running
/// configuration stays); it is not renamed, because the user may be in the middle of editing it.
/// A deleted file is ignored until it reappears.
#[derive(Debug)]
pub struct Watcher {
    path: PathBuf,
    last: Option<Stamp>,
}

impl Watcher {
    /// Poll period required by SPEC §6.1 (2 Hz).
    pub const INTERVAL: Duration = Duration::from_millis(500);

    /// Starts watching `path`; the file's current state counts as already seen.
    pub fn new(path: impl Into<PathBuf>) -> Watcher {
        let path = path.into();
        let last = Stamp::of(&path);
        Watcher { path, last }
    }

    /// The watched path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns the new configuration if the file changed since the last poll and parses.
    pub fn poll(&mut self) -> Option<Config> {
        let now = Stamp::of(&self.path);
        if now == self.last {
            return None;
        }
        self.last = now;
        now?;
        match fs::read_to_string(&self.path) {
            Ok(text) => match parse(&text) {
                Ok(cfg) => {
                    log::info!("config: reloaded {}", self.path.display());
                    Some(cfg)
                }
                Err(e) => {
                    log::warn!(
                        "config: ignoring edit of {}: {}",
                        self.path.display(),
                        e.message()
                    );
                    None
                }
            },
            Err(e) => {
                log::warn!("config: cannot read {}: {e}", self.path.display());
                None
            }
        }
    }

    /// Records the file's current state as seen, so the app's own [`save`] is not reported back
    /// by the next [`Watcher::poll`].
    pub fn note_saved(&mut self) {
        self.last = Stamp::of(&self.path);
    }

    /// [`save`] to the watched path followed by [`Watcher::note_saved`].
    pub fn save(&mut self, cfg: &Config) -> io::Result<()> {
        save(cfg, &self.path)?;
        self.note_saved();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// Unique empty directory under the system temp dir, removed on drop.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> TempDir {
            static N: AtomicU32 = AtomicU32::new(0);
            let p = std::env::temp_dir().join(format!(
                "fvp-config-test-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = fs::remove_dir_all(&p);
            fs::create_dir_all(&p).unwrap();
            TempDir(p)
        }
        fn file(&self) -> PathBuf {
            self.0.join("sub").join(FILE_NAME)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn defaults_match_spec() {
        let c = Config::default();
        assert!(c.hud.enabled);
        assert_eq!(c.hud.placement, Placement::Head);
        assert_eq!(c.hud.display, DisplayMode::Color);
        assert_eq!(c.hud.size_m, 0.05);
        assert_eq!(c.hud.opacity, 0.9);
        assert_eq!(
            (c.hud.nudge_x_m, c.hud.nudge_y_m, c.hud.nudge_z_m),
            (0.0, 0.0, 0.0)
        );
        assert_eq!(c.hud.fade_ms, 300);
        assert_eq!(c.target.preset, Preset::Feminine);
        assert_eq!(
            (c.target.custom_low_hz, c.target.custom_high_hz),
            (165.0, 255.0)
        );
        assert!(!c.reminder.enabled);
        assert_eq!(c.reminder.delay_s, 2.0);
        assert_eq!(c.reminder.pitch_hz, 440.0);
        assert_eq!(c.reminder.volume, 0.5);
        assert_eq!(c.vocal_rest.minutes, 0);
        assert_eq!(c.audio.mic_mode, MicMode::Bypass);
        assert_eq!(
            (c.audio.detect_min_hz, c.audio.detect_max_hz),
            (60.0, 500.0)
        );
        assert_eq!(c.audio.noise_floor_min_dbfs, -60.0);
        assert_eq!(c.resonance, Resonance::default());
        assert!(!c.resonance.enabled);
        assert_eq!(c.resonance.window_s, 8.0);
        assert_eq!(c.resonance.relaxed_df_hz, None);
        assert_eq!(c.resonance.target_df_hz, None);
        assert!(
            !to_toml(&c).contains("_df_hz"),
            "absent calibration is omitted"
        );
        assert_eq!(c.band(), (165.0, 255.0));

        // Defaults are already within range.
        let mut clamped = c.clone();
        clamped.clamp();
        assert_eq!(clamped, c);
        // Empty file = defaults.
        assert_eq!(parse("").unwrap(), c);
    }

    #[test]
    fn bands_per_preset() {
        let mut c = Config::default();
        c.target.preset = Preset::Masculine;
        assert_eq!(c.band(), (85.0, 180.0));
        c.target.preset = Preset::Custom;
        c.target.custom_low_hz = 120.0;
        c.target.custom_high_hz = 200.0;
        assert_eq!(c.band(), (120.0, 200.0));
    }

    #[test]
    fn partial_file_and_enums() {
        let c = parse(
            "[hud]\nplacement = \"left_wrist\"\ndisplay = \"detailed\"\nsize_m = 0.1\n\
             [target]\npreset = \"custom\"\n[audio]\nmic_mode = \"processed\"\n",
        )
        .unwrap();
        assert_eq!(c.hud.placement, Placement::LeftWrist);
        assert_eq!(c.hud.display, DisplayMode::Detailed);
        assert_eq!(c.hud.size_m, 0.1);
        assert!(c.hud.enabled);
        assert_eq!(c.target.preset, Preset::Custom);
        assert_eq!(c.audio.mic_mode, MicMode::Processed);
        assert_eq!(c.reminder, Reminder::default());
    }

    #[test]
    fn unknown_enum_strings_fall_back_to_default() {
        let c = parse(
            "[hud]\nplacement = \"forehead\"\ndisplay = \"x\"\n[target]\npreset = \"alto\"\n\
             [audio]\nmic_mode = \"raw\"\n",
        )
        .unwrap();
        assert_eq!(c.hud.placement, Placement::Head);
        assert_eq!(c.hud.display, DisplayMode::Color);
        assert_eq!(c.target.preset, Preset::Feminine);
        assert_eq!(c.audio.mic_mode, MicMode::Bypass);
    }

    #[test]
    fn integers_accepted_for_floats() {
        let c = parse("[reminder]\npitch_hz = 500\ndelay_s = 3\n").unwrap();
        assert_eq!(c.reminder.pitch_hz, 500.0);
        assert_eq!(c.reminder.delay_s, 3.0);
    }

    #[test]
    fn clamping_single_values() {
        let c = parse(
            "[hud]\nsize_m = 5.0\nopacity = 0.0\nnudge_x_m = -3\nnudge_y_m = 0.7\nnudge_z_m = nan\n\
             fade_ms = 99999\n[reminder]\ndelay_s = 0.1\npitch_hz = 5000\nvolume = -1\n\
             [audio]\nnoise_floor_min_dbfs = -200\n",
        )
        .unwrap();
        assert_eq!(c.hud.size_m, 0.20);
        assert_eq!(c.hud.opacity, 0.1);
        assert_eq!(c.hud.nudge_x_m, -0.5);
        assert_eq!(c.hud.nudge_y_m, 0.5);
        assert_eq!(c.hud.nudge_z_m, 0.0);
        assert_eq!(c.hud.fade_ms, 2000);
        assert_eq!(c.reminder.delay_s, 0.5);
        assert_eq!(c.reminder.pitch_hz, 1000.0);
        assert_eq!(c.reminder.volume, 0.0);
        assert_eq!(c.audio.noise_floor_min_dbfs, -90.0);

        assert_eq!(parse("[hud]\nfade_ms = -5\n").unwrap().hud.fade_ms, 0);
        assert_eq!(parse("[hud]\nsize_m = inf\n").unwrap().hud.size_m, 0.20);
    }

    #[test]
    fn clamping_vocal_rest() {
        for (input, expected) in [
            (0, 0),
            (1, 5),
            (4, 5),
            (5, 5),
            (30, 30),
            (50, 50),
            (51, 50),
            (-3, 0),
        ] {
            let c = parse(&format!("[vocal_rest]\nminutes = {input}\n")).unwrap();
            assert_eq!(c.vocal_rest.minutes, expected, "minutes = {input}");
        }
    }

    #[test]
    fn clamping_detection_span() {
        let c = parse("[audio]\ndetect_min_hz = 10\ndetect_max_hz = 2000\n").unwrap();
        assert_eq!(
            (c.audio.detect_min_hz, c.audio.detect_max_hz),
            (40.0, 1000.0)
        );

        let c = parse("[audio]\ndetect_min_hz = 300\ndetect_max_hz = 320\n").unwrap();
        assert_eq!(
            (c.audio.detect_min_hz, c.audio.detect_max_hz),
            (300.0, 350.0)
        );

        let c = parse("[audio]\ndetect_min_hz = 990\ndetect_max_hz = 100\n").unwrap();
        assert_eq!(
            (c.audio.detect_min_hz, c.audio.detect_max_hz),
            (950.0, 1000.0)
        );
    }

    #[test]
    fn clamping_resonance() {
        let c = parse(
            "[resonance]\nenabled = true\nwindow_s = 1\nrelaxed_df_hz = -3\ntarget_df_hz = nan\n",
        )
        .unwrap();
        assert!(c.resonance.enabled);
        assert_eq!(c.resonance.window_s, 4.0);
        assert_eq!(c.resonance.relaxed_df_hz, None);
        assert_eq!(c.resonance.target_df_hz, None);

        let c = parse("[resonance]\nwindow_s = 60\nrelaxed_df_hz = 1000\n").unwrap();
        assert_eq!(c.resonance.window_s, 20.0);
        assert_eq!(c.resonance.relaxed_df_hz, Some(1000.0));
        assert_eq!(c.resonance.target_df_hz, None);
        assert_eq!(
            parse("[resonance]\nwindow_s = nan\n")
                .unwrap()
                .resonance
                .window_s,
            8.0
        );
    }

    #[test]
    fn clamping_custom_band() {
        // Inside the detection span.
        let c = parse("[target]\ncustom_low_hz = 20\ncustom_high_hz = 900\n").unwrap();
        assert_eq!(
            (c.target.custom_low_hz, c.target.custom_high_hz),
            (60.0, 500.0)
        );

        // Gap of at least 10 Hz, raising the high end.
        let c = parse("[target]\ncustom_low_hz = 200\ncustom_high_hz = 195\n").unwrap();
        assert_eq!(
            (c.target.custom_low_hz, c.target.custom_high_hz),
            (200.0, 210.0)
        );

        // At the top of the span the low end moves down instead.
        let c = parse("[target]\ncustom_low_hz = 498\ncustom_high_hz = 499\n").unwrap();
        assert_eq!(
            (c.target.custom_low_hz, c.target.custom_high_hz),
            (490.0, 500.0)
        );

        // Follows a narrowed detection span.
        let c = parse("[audio]\ndetect_min_hz = 200\ndetect_max_hz = 400\n").unwrap();
        assert_eq!(
            (c.target.custom_low_hz, c.target.custom_high_hz),
            (200.0, 255.0)
        );
    }

    #[test]
    fn round_trip() {
        let mut c = Config::default();
        c.hud.enabled = false;
        c.hud.placement = Placement::RightWrist;
        c.hud.display = DisplayMode::Detailed;
        c.hud.size_m = 0.07;
        c.hud.opacity = 0.35;
        c.hud.nudge_x_m = -0.03;
        c.hud.nudge_y_m = 0.01;
        c.hud.nudge_z_m = 0.12;
        c.hud.fade_ms = 0;
        c.target.preset = Preset::Custom;
        c.target.custom_low_hz = 101.0;
        c.target.custom_high_hz = 143.5;
        c.reminder.enabled = true;
        c.reminder.delay_s = 4.5;
        c.reminder.pitch_hz = 660.0;
        c.reminder.volume = 0.25;
        c.vocal_rest.minutes = 20;
        c.audio.mic_mode = MicMode::Processed;
        c.audio.detect_min_hz = 70.0;
        c.audio.detect_max_hz = 450.0;
        c.audio.noise_floor_min_dbfs = -55.0;
        c.resonance.enabled = true;
        c.resonance.window_s = 12.5;
        c.resonance.relaxed_df_hz = Some(1031.25);
        c.resonance.target_df_hz = Some(1187.4);

        let text = to_toml(&c);
        assert!(
            text.contains("size_m = 0.07\n"),
            "shortest float form:\n{text}"
        );
        assert!(text.contains("placement = \"right_wrist\""));
        assert_eq!(parse(&text).unwrap(), c);

        let dir = TempDir::new();
        let path = dir.file();
        save(&c, &path).unwrap();
        assert_eq!(load_or_init(&path), c);
    }

    #[test]
    fn save_mode_and_no_temp_left() {
        let dir = TempDir::new();
        let path = dir.file();
        save(&Config::default(), &path).unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let names: Vec<_> = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from(FILE_NAME)]);
    }

    #[test]
    fn missing_file_writes_defaults() {
        let dir = TempDir::new();
        let path = dir.file();
        assert_eq!(load_or_init(&path), Config::default());
        assert_eq!(
            parse(&fs::read_to_string(&path).unwrap()).unwrap(),
            Config::default()
        );
    }

    #[test]
    fn bad_file_is_renamed() {
        let dir = TempDir::new();
        let path = dir.file();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let garbage = "[hud\nthis is not toml = = =\n";
        fs::write(&path, garbage).unwrap();

        assert_eq!(load_or_init(&path), Config::default());
        assert_eq!(fs::read_to_string(bad_path(&path)).unwrap(), garbage);
        assert_eq!(
            parse(&fs::read_to_string(&path).unwrap()).unwrap(),
            Config::default()
        );

        // Wrong value types also count as unparseable.
        fs::write(&path, "[hud]\nenabled = \"yes\"\n").unwrap();
        assert_eq!(load_or_init(&path), Config::default());
        assert_eq!(
            fs::read_to_string(bad_path(&path)).unwrap(),
            "[hud]\nenabled = \"yes\"\n"
        );
    }

    #[test]
    fn default_path_uses_xdg_config_home() {
        // Only check the shape; the environment is shared by parallel tests.
        let p = default_path();
        assert!(p.ends_with("frame-vocal-pitch/config.toml"));
    }

    #[test]
    fn watcher_reports_external_edits_only() {
        let dir = TempDir::new();
        let path = dir.file();
        let mut w = Watcher::new(&path);
        assert_eq!(w.poll(), None, "missing file");

        let mut c = Config::default();
        w.save(&c).unwrap();
        assert_eq!(w.poll(), None, "own save is not reported");

        // External edit with different size.
        fs::write(&path, "[hud]\nenabled = false\n").unwrap();
        c.hud.enabled = false;
        assert_eq!(w.poll(), Some(c.clone()));
        assert_eq!(w.poll(), None, "reported once");

        // Invalid edit is ignored and the file left in place.
        fs::write(&path, "[[[").unwrap();
        assert_eq!(w.poll(), None);
        assert!(path.exists());
        assert!(!bad_path(&path).exists());

        // External atomic replace (new inode).
        let other = path.with_file_name("other.toml");
        fs::write(&other, "[hud]\nopacity = 0.5\n").unwrap();
        fs::rename(&other, &path).unwrap();
        let got = w.poll().unwrap();
        assert_eq!(got.hud.opacity, 0.5);

        // Deletion is ignored.
        fs::remove_file(&path).unwrap();
        assert_eq!(w.poll(), None);
    }
}
