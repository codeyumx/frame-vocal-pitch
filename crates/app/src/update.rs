//! Self-update (SPEC §7.1).
//!
//! Checks ask GitHub for the latest release with `curl` on a helper thread (no TLS in the
//! binary, the main loop never blocks on the network) and wake the main loop through a socket,
//! like the analysis thread does. Automatic checks (`update.check = true`) are evaluated at start
//! and every hour but hit the network at most once per 24 h, or 1 h after a failed check; the
//! last result is cached in `~/.cache/frame-vocal-pitch/update-check.json`. "Check now" ignores
//! the cache.
//!
//! A confirmed update runs the installed updater script in the transient user unit
//! `frame-vocal-pitch-update`, so it survives the service restart that ends this process. While
//! it runs the unit is polled every 2 s; if it ends and this process is still alive, the update
//! failed and the last line of `update.log` says why.

use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::os::fd::{AsFd, BorrowedFd};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use fvp_ui::{UpdateLive, UpdateStatus};
use nix::unistd::getuid;
use serde::{Deserialize, Serialize};

use crate::{analysis, autostart};

/// GitHub's "latest release" endpoint; drafts and prereleases are already excluded.
pub const LATEST_URL: &str =
    "https://api.github.com/repos/codeyumx/frame-vocal-pitch/releases/latest";
/// Transient user unit the updater runs in.
pub const UPDATE_UNIT: &str = "frame-vocal-pitch-update";
/// Automatic checks hit the network at most this often…
pub const NETWORK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
/// …or this long after a failed check.
pub const RETRY_AFTER_FAILURE: Duration = Duration::from_secs(60 * 60);
/// The automatic-check rules are evaluated at start and this often.
pub const SCHEDULE_INTERVAL: Duration = Duration::from_secs(60 * 60);
/// Updater unit poll cadence while an update runs.
pub const POLL_INTERVAL: Duration = Duration::from_secs(2);
/// Prefix of `install.sh` failure lines; stripped from "Update failed: <reason>".
const INSTALL_ERROR_PREFIX: &str = "frame-vocal-pitch install: error: ";

/// A release version `X.Y.Z` (tags are `vX.Y.Z`).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

impl Version {
    /// Parses `X.Y.Z` or `vX.Y.Z` (decimal numbers only, nothing else).
    pub fn parse(text: &str) -> Option<Version> {
        let mut parts = text.strip_prefix('v').unwrap_or(text).split('.');
        let mut number = || {
            parts
                .next()
                .filter(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))?
                .parse()
                .ok()
        };
        let version = Version {
            major: number()?,
            minor: number()?,
            patch: number()?,
        };
        parts.next().is_none().then_some(version)
    }

    /// Release tag `vX.Y.Z`.
    pub fn tag(&self) -> String {
        format!("v{self}")
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// The running version.
pub fn current_version() -> Version {
    Version::parse(env!("CARGO_PKG_VERSION")).expect("the crate version is X.Y.Z")
}

/// Extracts the release version from GitHub's JSON answer.
pub fn parse_latest(body: &str) -> Result<Version, String> {
    #[derive(Deserialize)]
    struct Release {
        tag_name: Option<String>,
    }
    let release: Release =
        serde_json::from_str(body).map_err(|_| "unexpected answer from GitHub".to_string())?;
    let tag = release
        .tag_name
        .ok_or_else(|| "GitHub's answer has no release tag".to_string())?;
    Version::parse(&tag).ok_or_else(|| format!("unexpected release tag \"{tag}\""))
}

/// Asks GitHub for the latest release with `curl` (`curl` = program and leading arguments).
/// Blocks for up to 15 s; call it off the main loop.
pub fn fetch_latest(curl: &[OsString]) -> Result<Version, String> {
    let (program, leading) = curl
        .split_first()
        .ok_or_else(|| "no curl command".to_string())?;
    let out = Command::new(program)
        .args(leading)
        .args(["-fsSL", "--max-time", "15", "-H", "Accept: application/vnd.github+json"])
        .arg(LATEST_URL)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| match e.kind() {
            io::ErrorKind::NotFound => "curl is not installed".to_string(),
            _ => format!("cannot run curl: {e}"),
        })?;
    if !out.status.success() {
        return Err(curl_reason(&out));
    }
    parse_latest(&String::from_utf8_lossy(&out.stdout))
}

/// `curl: (6) Could not resolve host: x` → `Could not resolve host: x`.
fn curl_reason(out: &Output) -> String {
    let stderr = String::from_utf8_lossy(&out.stderr);
    let Some(line) = last_line(&stderr) else {
        return format!("curl failed ({})", out.status);
    };
    let line = line.strip_prefix("curl: ").unwrap_or(line);
    match line.strip_prefix('(').and_then(|rest| rest.split_once(") ")) {
        Some((code, message)) if code.bytes().all(|b| b.is_ascii_digit()) => message.to_string(),
        _ => line.to_string(),
    }
}

/// Last non-empty line, trimmed.
fn last_line(text: &str) -> Option<&str> {
    text.lines().map(str::trim).rfind(|l| !l.is_empty())
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Result of the last check, cached across runs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckRecord {
    /// Unix time of the check, seconds.
    pub checked_at: u64,
    /// Latest release `X.Y.Z` (kept from an earlier check when this one failed).
    #[serde(default)]
    pub latest: Option<String>,
    /// Why the check failed; `None` = it succeeded.
    #[serde(default)]
    pub error: Option<String>,
}

impl CheckRecord {
    /// The record for a check that ended with `result` at `now_unix`; a failure keeps the
    /// `latest` version of `previous`.
    pub fn new(result: Result<Version, String>, previous: Option<&CheckRecord>, now_unix: u64) -> Self {
        match result {
            Ok(version) => CheckRecord {
                checked_at: now_unix,
                latest: Some(version.to_string()),
                error: None,
            },
            Err(error) => CheckRecord {
                checked_at: now_unix,
                latest: previous.and_then(|r| r.latest.clone()),
                error: Some(error),
            },
        }
    }

    fn latest_version(&self) -> Option<Version> {
        self.latest.as_deref().and_then(Version::parse)
    }
}

/// Time until an automatic check may hit the network; zero = now. No record → now; a clock that
/// went backwards past the record → now.
pub fn auto_check_wait(record: Option<&CheckRecord>, now_unix: u64) -> Duration {
    let Some(record) = record else {
        return Duration::ZERO;
    };
    let Some(age) = now_unix.checked_sub(record.checked_at) else {
        return Duration::ZERO;
    };
    let interval = if record.error.is_some() { RETRY_AFTER_FAILURE } else { NETWORK_INTERVAL };
    interval.saturating_sub(Duration::from_secs(age))
}

/// Status line and offered version for a check record (no update running).
pub fn check_status(
    record: Option<&CheckRecord>,
    current: Version,
    now_unix: u64,
) -> (UpdateStatus, Option<Version>) {
    let Some(record) = record else {
        return (UpdateStatus::NotChecked, None);
    };
    let newer = record.latest_version().filter(|v| *v > current);
    let status = match (&record.error, newer) {
        (Some(error), _) => UpdateStatus::CheckFailed(error.clone()),
        (None, Some(v)) => UpdateStatus::Available(v.to_string()),
        (None, None) => UpdateStatus::UpToDate {
            checked_ago_min: now_unix.saturating_sub(record.checked_at) / 60,
        },
    };
    (status, newer)
}

/// Reads the cache; a missing or unreadable file means "never checked".
pub fn load_cache(path: &Path) -> Option<CheckRecord> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return None,
        Err(e) => {
            log::warn!("update: cannot read {}: {e}", path.display());
            return None;
        }
    };
    serde_json::from_str(&text)
        .inspect_err(|e| log::warn!("update: ignoring invalid {}: {e}", path.display()))
        .ok()
}

/// Writes the cache atomically (temp file + rename), creating its directory.
pub fn save_cache(path: &Path, record: &CheckRecord) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let json = serde_json::to_string(record).map_err(io::Error::other)?;
    let result = fs::File::create(&tmp)
        .and_then(|mut f| f.write_all(json.as_bytes()))
        .and_then(|()| fs::rename(&tmp, path));
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// Commands the updater runs; each is the program plus leading arguments, so tests can
/// substitute `sh -c '…'`.
#[derive(Clone, Debug)]
pub struct Programs {
    pub curl: Vec<OsString>,
    pub systemd_run: Vec<OsString>,
    pub systemctl: Vec<OsString>,
    /// `XDG_RUNTIME_DIR` for the systemd calls (SPEC §7: `/run/user/<uid>`).
    pub runtime_dir: OsString,
}

impl Default for Programs {
    fn default() -> Self {
        Programs {
            curl: vec!["curl".into()],
            systemd_run: vec!["systemd-run".into()],
            systemctl: vec!["systemctl".into()],
            runtime_dir: format!("/run/user/{}", getuid().as_raw()).into(),
        }
    }
}

/// Files the updater uses.
#[derive(Clone, Debug)]
pub struct Paths {
    /// `~/.cache/frame-vocal-pitch/update-check.json`
    pub cache: PathBuf,
    /// `~/.cache/frame-vocal-pitch/update.log`
    pub log: PathBuf,
    /// `~/.local/share/frame-vocal-pitch/install.sh`, installed by `install.sh`.
    pub updater: PathBuf,
}

impl Paths {
    /// The paths under the home directory `home`.
    pub fn under(home: &Path) -> Paths {
        let cache = home.join(".cache/frame-vocal-pitch");
        Paths {
            cache: cache.join("update-check.json"),
            log: cache.join("update.log"),
            updater: home.join(".local/share/frame-vocal-pitch/install.sh"),
        }
    }

    /// The paths under `$HOME` (falling back to `/`, like the config path).
    pub fn from_env() -> Paths {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/"));
        Paths::under(&home)
    }
}

/// `ActiveState` and `Result` of a unit, from `systemctl --user show`.
#[derive(Debug, Default, PartialEq, Eq)]
struct UnitState {
    active: String,
    result: String,
}

impl UnitState {
    fn parse(stdout: &str) -> UnitState {
        let mut state = UnitState::default();
        for line in stdout.lines() {
            match line.trim().split_once('=') {
                Some(("ActiveState", v)) => state.active = v.to_string(),
                Some(("Result", v)) => state.result = v.to_string(),
                _ => {}
            }
        }
        state
    }

    fn running(&self) -> bool {
        matches!(
            self.active.as_str(),
            "active" | "activating" | "deactivating" | "reloading" | "refreshing"
        )
    }
}

/// What starting the updater did.
enum Launch {
    /// `manual`: the app was not started by the service, so the installer will stop it.
    Started { manual: bool },
    AlreadyRunning,
}

enum Phase {
    Idle,
    /// Our updater unit runs.
    Updating {
        version: Version,
        manual: bool,
        poll_at: Instant,
    },
    /// Someone else's updater unit was running; polled until it ends.
    AlreadyRunning { poll_at: Instant },
    /// The update failed; shown until the next "Check now" or "Update".
    Failed(String),
}

/// Update checks and installs, driven by the main loop.
pub struct Updater {
    current: Version,
    current_text: &'static str,
    programs: Programs,
    paths: Paths,
    record: Option<CheckRecord>,
    phase: Phase,
    /// Result of the check in flight.
    check: Option<Receiver<Result<Version, String>>>,
    /// Next evaluation of the automatic-check rules.
    auto_at: Instant,
    /// `update.check` as of the last [`Updater::tick`].
    auto: bool,
    waker: UnixStream,
    wake_rx: UnixStream,
}

impl Updater {
    /// The updater for the running version with the real programs and paths.
    pub fn new() -> io::Result<Updater> {
        Updater::with(env!("CARGO_PKG_VERSION"), Programs::default(), Paths::from_env())
    }

    /// An updater for version `current` (`X.Y.Z`) using `programs` and `paths`; loads the cache.
    pub fn with(current: &'static str, programs: Programs, paths: Paths) -> io::Result<Updater> {
        let version = Version::parse(current).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, format!("invalid version {current}"))
        })?;
        let (waker, wake_rx) = UnixStream::pair()?;
        waker.set_nonblocking(true)?;
        wake_rx.set_nonblocking(true)?;
        Ok(Updater {
            current: version,
            current_text: current,
            record: load_cache(&paths.cache),
            programs,
            paths,
            phase: Phase::Idle,
            check: None,
            auto_at: Instant::now(),
            auto: false,
            waker,
            wake_rx,
        })
    }

    /// Readable when a check finished; add it to the main loop's `poll()`.
    pub fn wake_fd(&self) -> BorrowedFd<'_> {
        self.wake_rx.as_fd()
    }

    /// Drains the wake socket without blocking.
    pub fn drain_wake(&self) {
        analysis::drain(&self.wake_rx);
    }

    fn updating(&self) -> bool {
        matches!(self.phase, Phase::Updating { .. } | Phase::AlreadyRunning { .. })
    }

    /// Collects a finished check, runs due automatic checks (`auto` = `update.check`) and polls
    /// a running updater.
    pub fn tick(&mut self, now: Instant, auto: bool) {
        self.receive_check(now);
        if auto && !self.auto {
            // Turned on (or first tick): evaluate the rules at once.
            self.auto_at = now;
        }
        self.auto = auto;
        if auto && now >= self.auto_at {
            if self.check.is_some() || self.updating() {
                self.auto_at = now + SCHEDULE_INTERVAL;
            } else {
                let wait = auto_check_wait(self.record.as_ref(), unix_now());
                if wait.is_zero() {
                    log::debug!("update: automatic check");
                    self.spawn_check();
                    self.auto_at = now + SCHEDULE_INTERVAL;
                } else {
                    self.auto_at = now + wait.min(SCHEDULE_INTERVAL);
                }
            }
        }
        if let Phase::Updating { poll_at, .. } | Phase::AlreadyRunning { poll_at } = self.phase {
            if now >= poll_at {
                self.poll_unit(now);
            }
        }
    }

    /// Next moment [`Updater::tick`] has work to do (finished checks wake through
    /// [`Updater::wake_fd`] instead).
    pub fn next_deadline(&self) -> Option<Instant> {
        let poll = match self.phase {
            Phase::Updating { poll_at, .. } | Phase::AlreadyRunning { poll_at } => Some(poll_at),
            Phase::Idle | Phase::Failed(_) => None,
        };
        let auto = self.auto.then_some(self.auto_at);
        match (poll, auto) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// "Check now": asks GitHub regardless of the cache and of `update.check`.
    pub fn check_now(&mut self) {
        if self.updating() || self.check.is_some() {
            return;
        }
        self.phase = Phase::Idle;
        log::info!("update: checking for a new release");
        self.spawn_check();
    }

    fn spawn_check(&mut self) {
        let (tx, rx) = mpsc::channel();
        let curl = self.programs.curl.clone();
        let spawned = self.waker.try_clone().and_then(|mut waker| {
            thread::Builder::new()
                .name("update-check".into())
                .spawn(move || {
                    let _ = tx.send(fetch_latest(&curl));
                    let _ = waker.write(&[1]);
                })
        });
        match spawned {
            Ok(_) => self.check = Some(rx),
            Err(e) => self.store(Err(format!("cannot start the check: {e}")), Instant::now()),
        }
    }

    fn receive_check(&mut self, now: Instant) {
        let Some(rx) = &self.check else {
            return;
        };
        let result = match rx.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Err("the check ended unexpectedly".to_string()),
        };
        self.check = None;
        self.store(result, now);
    }

    /// Records a check result in memory and in the cache file.
    fn store(&mut self, result: Result<Version, String>, now: Instant) {
        match &result {
            Ok(latest) => log::info!("update: latest release {latest}, running {}", self.current),
            Err(e) => log::info!("update: check failed: {e}"),
        }
        let record = CheckRecord::new(result, self.record.as_ref(), unix_now());
        if let Err(e) = save_cache(&self.paths.cache, &record) {
            log::warn!("update: cannot write {}: {e}", self.paths.cache.display());
        }
        self.auto_at = now + auto_check_wait(Some(&record), unix_now()).min(SCHEDULE_INTERVAL);
        self.record = Some(record);
    }

    /// "Update to X.Y.Z" confirmed: starts the updater for `version` (`X.Y.Z`, newer than the
    /// running version).
    pub fn start_update(&mut self, version: &str, now: Instant) {
        if self.updating() {
            return;
        }
        let Some(version) = Version::parse(version).filter(|v| *v > self.current) else {
            log::warn!("update: ignoring request for {version} (running {})", self.current);
            return;
        };
        let poll_at = now + POLL_INTERVAL;
        self.phase = match self.launch(version) {
            Ok(Launch::Started { manual }) => {
                log::info!(
                    "update: installing {version} in {UPDATE_UNIT} (log {})",
                    self.paths.log.display()
                );
                Phase::Updating { version, manual, poll_at }
            }
            Ok(Launch::AlreadyRunning) => {
                log::info!("update: {UPDATE_UNIT} is already running");
                Phase::AlreadyRunning { poll_at }
            }
            Err(reason) => {
                log::warn!("update: cannot start the update to {version}: {reason}");
                Phase::Failed(reason)
            }
        };
    }

    fn launch(&self, version: Version) -> Result<Launch, String> {
        if !self.paths.updater.is_file() {
            return Err("the updater is not installed; re-run the install command".to_string());
        }
        match self.unit_state(UPDATE_UNIT) {
            Ok(state) if state.running() => return Ok(Launch::AlreadyRunning),
            Ok(_) => {}
            Err(e) => log::debug!("update: {e}"),
        }
        let manual = match self.unit_state(autostart::UNIT) {
            Ok(state) => !state.running(),
            Err(e) => {
                log::debug!("update: {e}");
                false
            }
        };
        if let Some(dir) = self.paths.log.parent() {
            fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        }
        fs::File::create(&self.paths.log)
            .map_err(|e| format!("cannot write {}: {e}", self.paths.log.display()))?;

        let log = self.paths.log.display();
        let out = command(&self.programs.systemd_run, &self.programs.runtime_dir)
            .arg("--user")
            .arg(format!("--unit={UPDATE_UNIT}"))
            .arg("--collect")
            .arg(format!("--property=StandardOutput=append:{log}"))
            .arg(format!("--property=StandardError=append:{log}"))
            .arg("/bin/bash")
            .arg(&self.paths.updater)
            .arg("--release")
            .arg(version.tag())
            .output()
            .map_err(|e| format!("cannot run systemd-run: {e}"))?;
        if out.status.success() {
            return Ok(Launch::Started { manual });
        }
        let stderr = String::from_utf8_lossy(&out.stderr);
        match last_line(&stderr) {
            Some(line) if line.contains("already exists") || line.contains("already loaded") => {
                Ok(Launch::AlreadyRunning)
            }
            Some(line) => Err(line.to_string()),
            None => Err(format!("systemd-run failed ({})", out.status)),
        }
    }

    fn unit_state(&self, unit: &str) -> Result<UnitState, String> {
        let out = command(&self.programs.systemctl, &self.programs.runtime_dir)
            .args(["--user", "show", unit, "-p", "ActiveState", "-p", "Result"])
            .output()
            .map_err(|e| format!("cannot run systemctl: {e}"))?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            return Err(format!(
                "systemctl --user show {unit} failed ({}): {}",
                out.status,
                stderr.trim()
            ));
        }
        Ok(UnitState::parse(&String::from_utf8_lossy(&out.stdout)))
    }

    fn poll_unit(&mut self, now: Instant) {
        let state = match self.unit_state(UPDATE_UNIT) {
            Ok(state) if !state.running() => state,
            other => {
                if let Err(e) = other {
                    log::debug!("update: {e}");
                }
                if let Phase::Updating { poll_at, .. } | Phase::AlreadyRunning { poll_at } =
                    &mut self.phase
                {
                    *poll_at = now + POLL_INTERVAL;
                }
                return;
            }
        };
        // The unit ended and this process is still alive: it was not replaced.
        self.phase = match self.phase {
            Phase::Updating { version, .. } => {
                let reason = self.failure_reason(&state.result);
                log::warn!("update: installing {version} failed: {reason}");
                Phase::Failed(reason)
            }
            _ => {
                log::info!("update: the other {UPDATE_UNIT} run ended");
                Phase::Idle
            }
        };
    }

    /// Last non-empty line of the update log without the installer's error prefix.
    fn failure_reason(&self, result: &str) -> String {
        let log = fs::read_to_string(&self.paths.log).unwrap_or_default();
        match last_line(&log) {
            Some(line) => line.strip_prefix(INSTALL_ERROR_PREFIX).unwrap_or(line).to_string(),
            None if !result.is_empty() && result != "success" => {
                format!("the updater stopped ({result})")
            }
            None => "the updater stopped without a message".to_string(),
        }
    }

    /// What the Updates section shows.
    pub fn view(&self) -> UpdateLive {
        let (record_status, newer) = check_status(self.record.as_ref(), self.current, unix_now());
        let status = match &self.phase {
            Phase::Updating { manual: true, .. } => UpdateStatus::UpdatingManual,
            Phase::Updating { version, .. } => UpdateStatus::Updating(version.to_string()),
            Phase::AlreadyRunning { .. } => UpdateStatus::AlreadyRunning,
            Phase::Failed(reason) => UpdateStatus::UpdateFailed(reason.clone()),
            Phase::Idle if self.check.is_some() => UpdateStatus::Checking,
            Phase::Idle => record_status,
        };
        UpdateLive {
            version: self.current_text,
            status,
            available: newer.map(|v| v.to_string()),
        }
    }
}

/// `program` + leading arguments with the systemd runtime directory set.
fn command(argv: &[OsString], runtime_dir: &OsString) -> Command {
    let (program, leading) = argv.split_first().expect("non-empty command");
    let mut cmd = Command::new(program);
    cmd.args(leading)
        .env("XDG_RUNTIME_DIR", runtime_dir)
        .stdin(Stdio::null());
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;
    use fvp_ui::update_status_text;

    const HOUR: u64 = 3600;

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    #[test]
    fn version_comparison() {
        assert!(v("v0.10.0") > v("v0.9.9"));
        assert!(v("1.0.0") > v("0.99.99"));
        assert!(v("0.2.10") > v("0.2.9"));
        assert_eq!(v("v0.2.1"), v("0.2.1"));
        assert!(v("v0.2.1") <= v("0.2.1"));
        assert_eq!(v("v1.2.3").to_string(), "1.2.3");
        assert_eq!(v("1.2.3").tag(), "v1.2.3");
        for bad in ["", "v", "1.2", "1.2.3.4", "1.2.x", "v1.2.3-rc1", "1..3", "vv1.2.3", " 1.2.3", "1.-2.3"] {
            assert_eq!(Version::parse(bad), None, "{bad:?}");
        }
        assert_eq!(current_version().to_string(), env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn latest_tag_from_github_json() {
        let body = r#"{"url":"x","tag_name":"v0.10.0","draft":false,"assets":[{"name":"a"}]}"#;
        assert_eq!(parse_latest(body), Ok(v("0.10.0")));
        assert_eq!(parse_latest(r#"{"tag_name":"nightly"}"#), Err("unexpected release tag \"nightly\"".into()));
        assert_eq!(parse_latest(r#"{"message":"Not Found"}"#), Err("GitHub's answer has no release tag".into()));
        assert_eq!(parse_latest("<html>"), Err("unexpected answer from GitHub".into()));
    }

    #[test]
    fn malformed_tag_reads_couldnt_check() {
        let record = CheckRecord::new(parse_latest(r#"{"tag_name":"latest"}"#), None, 1000);
        let (status, newer) = check_status(Some(&record), v("0.2.1"), 1000);
        assert_eq!(newer, None);
        assert_eq!(update_status_text(&status), "Couldn't check: unexpected release tag \"latest\"");
    }

    #[test]
    fn status_from_check_records() {
        let now = 10 * HOUR;
        let current = v("0.2.1");
        assert_eq!(check_status(None, current, now), (UpdateStatus::NotChecked, None));

        let same = CheckRecord::new(Ok(v("0.2.1")), None, now - 2 * HOUR);
        let (status, newer) = check_status(Some(&same), current, now);
        assert_eq!(update_status_text(&status), "Up to date \u{00b7} checked 2 h ago");
        assert_eq!(newer, None);
        let older = CheckRecord::new(Ok(v("0.1.0")), None, now);
        assert_eq!(check_status(Some(&older), current, now).0, UpdateStatus::UpToDate { checked_ago_min: 0 });

        let newer_rec = CheckRecord::new(Ok(v("0.10.0")), None, now);
        let (status, newer) = check_status(Some(&newer_rec), current, now);
        assert_eq!(update_status_text(&status), "Version 0.10.0 is available");
        assert_eq!(newer, Some(v("0.10.0")));

        // A failed check keeps the known release, so the Update button stays.
        let failed = CheckRecord::new(Err("Could not resolve host: api.github.com".into()), Some(&newer_rec), now);
        assert_eq!(failed.latest.as_deref(), Some("0.10.0"));
        let (status, newer) = check_status(Some(&failed), current, now);
        assert_eq!(update_status_text(&status), "Couldn't check: Could not resolve host: api.github.com");
        assert_eq!(newer, Some(v("0.10.0")));
    }

    #[test]
    fn cache_rules() {
        let now = 100 * HOUR;
        assert_eq!(auto_check_wait(None, now), Duration::ZERO, "never checked");
        let ok = |age: u64| CheckRecord::new(Ok(v("0.2.1")), None, now - age);
        let failed = |age: u64| CheckRecord::new(Err("offline".into()), None, now - age);
        assert_eq!(auto_check_wait(Some(&ok(0)), now), NETWORK_INTERVAL);
        assert_eq!(auto_check_wait(Some(&ok(23 * HOUR)), now), Duration::from_secs(HOUR));
        assert_eq!(auto_check_wait(Some(&ok(24 * HOUR)), now), Duration::ZERO, "24 h");
        assert_eq!(auto_check_wait(Some(&ok(30 * HOUR)), now), Duration::ZERO);
        assert_eq!(auto_check_wait(Some(&failed(0)), now), RETRY_AFTER_FAILURE);
        assert_eq!(auto_check_wait(Some(&failed(HOUR / 2)), now), Duration::from_secs(HOUR / 2));
        assert_eq!(auto_check_wait(Some(&failed(HOUR)), now), Duration::ZERO, "1 h after failure");
        let future = CheckRecord::new(Ok(v("0.2.1")), None, now + HOUR);
        assert_eq!(auto_check_wait(Some(&future), now), Duration::ZERO, "clock went backwards");
    }

    #[test]
    fn curl_failure_reasons() {
        let out = |code: i32, stderr: &str| Output {
            status: std::os::unix::process::ExitStatusExt::from_raw(code << 8),
            stdout: Vec::new(),
            stderr: stderr.as_bytes().to_vec(),
        };
        assert_eq!(curl_reason(&out(6, "curl: (6) Could not resolve host: api.github.com\n")), "Could not resolve host: api.github.com");
        assert_eq!(curl_reason(&out(22, "curl: (22) The requested URL returned error: 403\n")), "The requested URL returned error: 403");
        assert_eq!(curl_reason(&out(1, "\nsomething odd\n\n")), "something odd");
        assert!(curl_reason(&out(7, "")).starts_with("curl failed"));
    }

    #[test]
    fn unit_state_parsing() {
        let s = UnitState::parse("ActiveState=active\nResult=success\n");
        assert!(s.running());
        let s = UnitState::parse("Result=exit-code\nActiveState=failed\n");
        assert_eq!((s.active.as_str(), s.result.as_str(), s.running()), ("failed", "exit-code", false));
        assert!(!UnitState::parse("ActiveState=inactive\nResult=success\n").running());
        assert!(UnitState::parse("ActiveState=activating\n").running());
    }

    /// A throwaway home with fake `curl`, `systemctl` and `systemd-run` run through `sh -c`
    /// (no executable files are written, so no ETXTBSY races with forking test threads).
    struct Fake {
        dir: PathBuf,
    }

    impl Fake {
        fn new(name: &str) -> Fake {
            let dir = std::env::temp_dir().join(format!("fvp-update-{}-{name}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Fake { dir }
        }

        fn sh(&self, script: &str, name: &str) -> Vec<OsString> {
            let script = format!("d='{}'\n{script}", self.dir.display());
            vec!["/bin/sh".into(), "-c".into(), script.into(), name.into()]
        }

        fn programs(&self, curl_body: &str) -> Programs {
            Programs {
                curl: self.sh(
                    &format!("printf '%s\\n' \"$@\" > \"$d/curl-args\"\nprintf '%s' '{curl_body}'"),
                    "curl",
                ),
                systemctl: self.sh(
                    "echo \"$* XDG_RUNTIME_DIR=$XDG_RUNTIME_DIR\" >> \"$d/systemctl.log\"\n\
                     case \"$3\" in\n\
                     frame-vocal-pitch-update) f=update-state;;\n\
                     *) f=service-state;;\n\
                     esac\n\
                     cat \"$d/$f\" 2>/dev/null || printf 'ActiveState=inactive\\nResult=success\\n'",
                    "systemctl",
                ),
                systemd_run: self.sh(
                    "printf '%s\\n' \"$@\" > \"$d/run-args\"\n\
                     echo \"XDG_RUNTIME_DIR=$XDG_RUNTIME_DIR\" > \"$d/run-env\"\n\
                     if [ -f \"$d/run-fail\" ]; then cat \"$d/run-fail\" >&2; exit 1; fi",
                    "systemd-run",
                ),
                runtime_dir: "/run/user/4242".into(),
            }
        }

        fn paths(&self) -> Paths {
            Paths::under(&self.dir.join("home"))
        }

        fn updater(&self, current: &'static str, curl_body: &str) -> Updater {
            Updater::with(current, self.programs(curl_body), self.paths()).unwrap()
        }

        fn install_updater(&self) {
            let p = self.paths().updater;
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, "#!/bin/bash\n").unwrap();
        }

        fn set(&self, file: &str, text: &str) {
            fs::write(self.dir.join(file), text).unwrap();
        }

        fn read(&self, file: &str) -> Option<String> {
            fs::read_to_string(self.dir.join(file)).ok()
        }
    }

    impl Drop for Fake {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    /// Ticks until the check in flight finished.
    fn finish_check(u: &mut Updater, auto: bool) {
        let end = Instant::now() + Duration::from_secs(10);
        while u.check.is_some() {
            assert!(Instant::now() < end, "check did not finish");
            thread::sleep(Duration::from_millis(5));
            u.drain_wake();
            u.tick(Instant::now(), auto);
        }
    }

    #[test]
    fn automatic_checks_follow_the_cache_and_the_setting() {
        let fake = Fake::new("auto");
        let body = r#"{"tag_name":"v0.3.0"}"#;

        // update.check = false: no automatic check, no deadline; "Check now" still works.
        let mut u = fake.updater("0.2.1", body);
        let t = Instant::now();
        u.tick(t, false);
        assert!(u.check.is_none());
        assert_eq!(u.next_deadline(), None);
        assert_eq!(u.view().status, UpdateStatus::NotChecked);
        u.check_now();
        assert_eq!(u.view().status, UpdateStatus::Checking);
        finish_check(&mut u, false);
        let view = u.view();
        assert_eq!(view.status, UpdateStatus::Available("0.3.0".into()));
        assert_eq!(view.available.as_deref(), Some("0.3.0"));
        assert_eq!(view.version, "0.2.1");
        let args = fake.read("curl-args").unwrap();
        assert_eq!(
            args.lines().collect::<Vec<_>>(),
            ["-fsSL", "--max-time", "15", "-H", "Accept: application/vnd.github+json", LATEST_URL]
        );
        let cached = load_cache(&fake.paths().cache).expect("cache written");
        assert_eq!((cached.latest.as_deref(), cached.error.as_deref()), (Some("0.3.0"), None));

        // A fresh cache: the automatic check at start does not hit the network…
        let mut u = fake.updater("0.2.1", body);
        let t = Instant::now();
        u.tick(t, true);
        assert!(u.check.is_none(), "cached result is fresh");
        assert_eq!(u.next_deadline(), Some(t + SCHEDULE_INTERVAL), "evaluated again in an hour");
        assert_eq!(u.view().status, UpdateStatus::Available("0.3.0".into()));
        // …but "Check now" does.
        u.check_now();
        assert!(u.check.is_some(), "Check now ignores the cache");
        finish_check(&mut u, true);
        u.tick(Instant::now(), false);
        assert_eq!(u.next_deadline(), None, "turning automatic checks off drops the deadline");

        // A failure 30 min ago: retried 30 min from now, not earlier.
        let failed = CheckRecord::new(Err("offline".into()), None, unix_now() - HOUR / 2);
        save_cache(&fake.paths().cache, &failed).unwrap();
        let mut u = fake.updater("0.2.1", body);
        let t = Instant::now();
        u.tick(t, true);
        assert!(u.check.is_none());
        let deadline = u.next_deadline().unwrap();
        assert!(deadline <= t + Duration::from_secs(HOUR / 2) && deadline > t + Duration::from_secs(HOUR / 2 - 60));
        assert_eq!(u.view().status, UpdateStatus::CheckFailed("offline".into()));
        // At that deadline the retry runs.
        let failed = CheckRecord::new(Err("offline".into()), None, unix_now() - HOUR);
        save_cache(&fake.paths().cache, &failed).unwrap();
        u.record = load_cache(&fake.paths().cache);
        u.tick(deadline, true);
        assert!(u.check.is_some(), "retried 1 h after the failure");
        finish_check(&mut u, true);
        assert_eq!(u.view().status, UpdateStatus::Available("0.3.0".into()));

        // Never checked + automatic: checks at start.
        fs::remove_file(fake.paths().cache).unwrap();
        let mut u = fake.updater("0.3.0", body);
        u.tick(Instant::now(), true);
        assert!(u.check.is_some());
        finish_check(&mut u, true);
        assert_eq!(u.view().status, UpdateStatus::UpToDate { checked_ago_min: 0 });
        assert_eq!(u.view().available, None);
    }

    #[test]
    fn failed_and_malformed_checks() {
        let fake = Fake::new("badcheck");
        let mut u = fake.updater("0.2.1", r#"{"tag_name":"v0.3"}"#);
        u.check_now();
        finish_check(&mut u, false);
        assert_eq!(update_status_text(&u.view().status), "Couldn't check: unexpected release tag \"v0.3\"");

        let mut programs = fake.programs("");
        programs.curl = fake.sh("echo 'curl: (6) Could not resolve host: api.github.com' >&2; exit 6", "curl");
        let mut u = Updater::with("0.2.1", programs, fake.paths()).unwrap();
        u.check_now();
        finish_check(&mut u, false);
        assert_eq!(u.view().status, UpdateStatus::CheckFailed("Could not resolve host: api.github.com".into()));
        let cached = load_cache(&fake.paths().cache).unwrap();
        assert_eq!(cached.error.as_deref(), Some("Could not resolve host: api.github.com"));
    }

    #[test]
    fn update_runs_the_contract_command_and_reports_failure_from_the_log() {
        let fake = Fake::new("install");
        fake.install_updater();
        fake.set("service-state", "ActiveState=active\nResult=success\n");
        fake.set("update-state", "ActiveState=inactive\nResult=success\n");
        let paths = fake.paths();
        fs::create_dir_all(paths.log.parent().unwrap()).unwrap();
        fs::write(&paths.log, "old run\n").unwrap();

        let mut u = fake.updater("0.2.1", "");
        let t = Instant::now();
        u.start_update("0.3.0", t);
        assert_eq!(u.view().status, UpdateStatus::Updating("0.3.0".into()));
        let log = paths.log.display().to_string();
        let updater = paths.updater.display().to_string();
        let args = fake.read("run-args").expect("systemd-run ran");
        assert_eq!(
            args.lines().collect::<Vec<_>>(),
            [
                "--user",
                "--unit=frame-vocal-pitch-update",
                "--collect",
                &format!("--property=StandardOutput=append:{log}"),
                &format!("--property=StandardError=append:{log}"),
                "/bin/bash",
                &updater,
                "--release",
                "v0.3.0",
            ]
        );
        assert!(paths.log.is_absolute());
        assert_eq!(fake.read("run-env").unwrap().trim(), "XDG_RUNTIME_DIR=/run/user/4242");
        assert_eq!(fs::read_to_string(&paths.log).unwrap(), "", "log truncated first");
        assert!(fake.read("systemctl.log").unwrap().contains("XDG_RUNTIME_DIR=/run/user/4242"));
        assert_eq!(u.next_deadline(), Some(t + POLL_INTERVAL));

        // Nothing starts twice, and Check now waits.
        u.check_now();
        assert!(u.check.is_none());

        // Polled every 2 s while the unit runs.
        fake.set("update-state", "ActiveState=active\nResult=success\n");
        u.tick(t + Duration::from_secs(1), false);
        assert_eq!(u.next_deadline(), Some(t + POLL_INTERVAL), "not due yet");
        u.tick(t + POLL_INTERVAL, false);
        assert_eq!(u.next_deadline(), Some(t + 2 * POLL_INTERVAL));
        assert_eq!(u.view().status, UpdateStatus::Updating("0.3.0".into()));

        // Ended while this process lives: failed, with the log's last non-empty line.
        fs::write(
            &paths.log,
            "downloading v0.3.0\nframe-vocal-pitch install: error: checksum mismatch for \
             frame-vocal-pitch-0.3.0-aarch64.tar.gz; nothing was changed\n\n",
        )
        .unwrap();
        fake.set("update-state", "ActiveState=inactive\nResult=exit-code\n");
        u.tick(t + 2 * POLL_INTERVAL, false);
        assert_eq!(
            update_status_text(&u.view().status),
            "Update failed: checksum mismatch for frame-vocal-pitch-0.3.0-aarch64.tar.gz; nothing was changed"
        );
        assert_eq!(u.next_deadline(), None);

        // A retry truncates the log; an empty log falls back to the unit result.
        u.start_update("0.3.0", t);
        assert_eq!(fs::read_to_string(&paths.log).unwrap(), "");
        u.tick(t + POLL_INTERVAL, false);
        assert_eq!(u.view().status, UpdateStatus::UpdateFailed("the updater stopped (exit-code)".into()));
        fake.set("update-state", "ActiveState=inactive\nResult=success\n");
        u.start_update("0.3.0", t);
        u.tick(t + POLL_INTERVAL, false);
        assert_eq!(u.view().status, UpdateStatus::UpdateFailed("the updater stopped without a message".into()));
    }

    #[test]
    fn update_start_variants() {
        let fake = Fake::new("variants");
        let t = Instant::now();

        // No updater installed (versions up to 0.2.1 had none).
        let mut u = fake.updater("0.2.1", "");
        u.start_update("0.3.0", t);
        assert_eq!(
            u.view().status,
            UpdateStatus::UpdateFailed("the updater is not installed; re-run the install command".into())
        );
        assert!(fake.read("run-args").is_none());
        fake.install_updater();

        // Not newer: ignored.
        let mut u = fake.updater("0.2.1", "");
        u.start_update("0.2.1", t);
        u.start_update("garbage", t);
        assert_eq!(u.view().status, UpdateStatus::NotChecked);
        assert!(fake.read("run-args").is_none());

        // Started by hand (service inactive): the manual notice.
        u.start_update("0.3.0", t);
        assert_eq!(u.view().status, UpdateStatus::UpdatingManual);
        fs::remove_file(fake.dir.join("run-args")).unwrap();

        // Unit already active: "An update is already running", nothing started; cleared when it ends.
        fake.set("update-state", "ActiveState=active\nResult=success\n");
        let mut u = fake.updater("0.2.1", "");
        u.start_update("0.3.0", t);
        assert_eq!(update_status_text(&u.view().status), "An update is already running");
        assert!(fake.read("run-args").is_none());
        fake.set("update-state", "ActiveState=inactive\nResult=success\n");
        u.tick(t + POLL_INTERVAL, false);
        assert_eq!(u.view().status, UpdateStatus::NotChecked);

        // systemd-run refusing a duplicate unit counts as already running; other errors fail.
        fake.set("run-fail", "Failed to start transient service unit: Unit frame-vocal-pitch-update.service already exists.\n");
        let mut u = fake.updater("0.2.1", "");
        u.start_update("0.3.0", t);
        assert_eq!(u.view().status, UpdateStatus::AlreadyRunning);
        fake.set("run-fail", "Failed to connect to bus: No medium found\n");
        let mut u = fake.updater("0.2.1", "");
        u.start_update("0.3.0", t);
        assert_eq!(u.view().status, UpdateStatus::UpdateFailed("Failed to connect to bus: No medium found".into()));
        // "Check now" clears the failure.
        u.check_now();
        assert_eq!(u.view().status, UpdateStatus::Checking);
        finish_check(&mut u, false);
    }
}
