//! Runs the real binary: `--version`/`--help`/usage, `--probe`, `--bench`, `--analyze`,
//! `--render-png`, the normal-mode lifecycle (§6.2) and the single-instance lock.
//!
//! Every test uses its own runtime/config/home sandbox, so the process behaves as if no
//! SteamVR installation existed (`HOME` has no `.steam`) and never touches the real one.

use std::fs;
use std::io::Write;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use frame_vocal_pitch::instance::{self, Acquired};
use nix::sys::signal::{kill, Signal};
use nix::unistd::Pid;

const BIN: &str = env!("CARGO_BIN_EXE_frame-vocal-pitch");

struct Sandbox(PathBuf);

impl Sandbox {
    fn new(name: &str) -> Sandbox {
        let dir = std::env::temp_dir().join(format!("fvp-lifecycle-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("run")).unwrap();
        fs::create_dir_all(dir.join("config")).unwrap();
        Sandbox(dir)
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(BIN);
        command
            .args(args)
            .env("XDG_RUNTIME_DIR", self.0.join("run"))
            .env("XDG_CONFIG_HOME", self.0.join("config"))
            .env("XDG_DATA_HOME", self.0.join("data"))
            .env("HOME", &self.0)
            .env_remove("FVP_OPENVR_LIB");
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().unwrap()
    }

    fn path(&self, tail: &str) -> PathBuf {
        self.0.join(tail)
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Sends a signal to a child process.
fn signal(child: &Child, signal: Signal) {
    kill(Pid::from_raw(child.id() as i32), signal).unwrap();
}

/// Writes a mono 16-bit 48 kHz WAV with `tone_hz` for `seconds`.
fn write_tone_wav(path: &Path, tone_hz: f64, seconds: f64) {
    let rate = 48_000u32;
    let frames = (rate as f64 * seconds) as usize;
    let mut data = Vec::with_capacity(frames * 2);
    for i in 0..frames {
        let value = (i as f64 / rate as f64 * tone_hz * std::f64::consts::TAU).sin() * 0.5;
        data.extend_from_slice(&((value * 32767.0) as i16).to_le_bytes());
    }
    let mut wav = Vec::with_capacity(44 + data.len());
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&1u16.to_le_bytes()); // mono
    wav.extend_from_slice(&rate.to_le_bytes());
    wav.extend_from_slice(&(rate * 2).to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&(data.len() as u32).to_le_bytes());
    wav.extend_from_slice(&data);
    let mut file = fs::File::create(path).unwrap();
    file.write_all(&wav).unwrap();
}

#[test]
fn version_help_and_usage() {
    let sb = Sandbox::new("version");
    let out = sb.run(&["--version"]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        format!("frame-vocal-pitch {}\n", env!("CARGO_PKG_VERSION"))
    );

    let out = sb.run(&["--help"]);
    assert_eq!(out.status.code(), Some(0));
    let help = String::from_utf8(out.stdout).unwrap();
    for expected in ["--probe", "--analyze F.wav", "--bench", "--render-png DIR"] {
        assert!(help.contains(expected), "{help}");
    }

    for bad in [&["--nope"][..], &["--probe", "--bench"], &["--analyze"]] {
        let out = sb.run(bad);
        assert_eq!(out.status.code(), Some(2), "{bad:?}");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("Try 'frame-vocal-pitch --help'"),
            "{bad:?}"
        );
    }
}

#[test]
fn probe_reports_the_environment_without_steamvr() {
    let sb = Sandbox::new("probe");
    let out = sb.run(&["--probe"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    assert!(stdout.starts_with("frame-vocal-pitch "), "{stdout}");
    assert!(stdout.contains("OpenVR library: "), "{stdout}");
    assert!(stdout.contains("SteamVR: "), "{stdout}");
    // The audio report always names the default source and the mute state.
    assert!(stdout.contains("Default source"), "{stdout}");
    assert!(stdout.contains("muted"), "{stdout}");
}

#[test]
fn bench_prints_both_microsecond_lines() {
    let sb = Sandbox::new("bench");
    let out = sb.run(&["--bench"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    let stdout = String::from_utf8(out.stdout).unwrap();

    let micros = |prefix: &str| -> f64 {
        let line = stdout
            .lines()
            .find(|line| line.starts_with(prefix) && line.contains("µs/hop"))
            .unwrap_or_else(|| panic!("no '{prefix}' line in {stdout}"));
        let value: f64 = line
            .split(' ')
            .find_map(|token| token.parse().ok())
            .unwrap_or_else(|| panic!("no number in {line}"));
        assert!(value > 0.0 && value < 10_000.0, "{value} in {line}");
        assert!(line.contains("% of one core"), "{line}");
        value
    };
    let pitch = micros("Analysis: ");
    let both = micros("Analysis + resonance: ");
    // The combined figure includes the pitch path, so it can never be cheaper.
    assert!(both >= pitch, "resonance cost {both} < pitch {pitch}");
}

#[test]
fn analyze_prints_plausible_f0_for_a_220_hz_tone() {
    let sb = Sandbox::new("analyze");
    let wav = sb.path("tone.wav");
    write_tone_wav(&wav, 220.0, 0.6);

    let out = sb.run(&["--analyze", wav.to_str().unwrap()]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.contains("time       F0  state"), "{stdout}");

    let mut voiced = Vec::new();
    for line in stdout.lines() {
        let cells: Vec<&str> = line.split_whitespace().collect();
        if cells.len() != 3 {
            continue;
        }
        let Ok(_time) = cells[0].parse::<f64>() else {
            continue;
        };
        if let Ok(f0) = cells[1].parse::<f32>() {
            assert!(
                matches!(cells[2], "in_range" | "out_of_range"),
                "voiced hop without a voiced state: {line}"
            );
            voiced.push(f0);
        }
    }
    assert!(voiced.len() > 30, "only {} voiced hops", voiced.len());
    voiced.sort_by(f32::total_cmp);
    let median = voiced[voiced.len() / 2];
    assert!((median - 220.0).abs() < 2.2, "median F0 {median}");
    assert!(stdout.contains("in_range"), "{stdout}");
    assert!(stdout.contains("F0 median 22"), "{stdout}");
}

#[test]
fn render_png_writes_every_state() {
    let sb = Sandbox::new("render");
    let dir = sb.path("preview");
    let out = sb.run(&["--render-png", dir.to_str().unwrap()]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    for name in [
        "hud-color-in_range.png",
        "hud-color-too_low.png",
        "hud-color-too_high.png",
        "hud-color-out_of_range.png",
        "hud-color-no_voice.png",
        "hud-color-muted.png",
        "hud-color-no_mic.png",
        "hud-detailed-in_range.png",
        "hud-detailed-toast.png",
        "settings.png",
        "settings-custom.png",
    ] {
        let path = dir.join(name);
        let size = fs::metadata(&path)
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
            .len();
        assert!(size > 1_000, "{} is only {size} bytes", path.display());
    }
    assert!(String::from_utf8_lossy(&out.stdout).contains("files written to"));
}

#[test]
fn first_instance_runs_writes_config_and_stops_on_sigterm() {
    let sb = Sandbox::new("run");
    // Logs go to a file, so the test can wait for the retry state before signalling.
    let log = sb.path("stderr.log");
    let mut child = sb
        .command(&["--verbose"])
        .stdout(Stdio::null())
        .stderr(Stdio::from(fs::File::create(&log).unwrap()))
        .spawn()
        .unwrap();

    let lock = sb.path("run/frame-vocal-pitch.lock");
    let config = sb.path("config/frame-vocal-pitch/config.toml");
    let deadline = Instant::now() + Duration::from_secs(10);
    // Wait until the lock file has the PID, the config exists and one retry was logged.
    while Instant::now() < deadline {
        let owned = fs::read_to_string(&lock)
            .map(|pid| pid.trim() == child.id().to_string())
            .unwrap_or(false);
        let logged = fs::read_to_string(&log)
            .map(|text| text.contains("SteamVR unavailable") || text.contains("SteamVR is not running"))
            .unwrap_or(false);
        if owned && config.is_file() && logged {
            break;
        }
        assert!(
            child.try_wait().unwrap().is_none(),
            "the process exited early"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(config.is_file(), "no config written");
    assert!(fs::read_to_string(&config).unwrap().contains("[hud]"));

    let started = Instant::now();
    signal(&child, Signal::SIGTERM);
    let status = child.wait().unwrap();
    let stopped = started.elapsed();
    let stderr = fs::read_to_string(&log).unwrap();
    assert_eq!(status.code(), Some(0), "{stderr}");
    assert!(stopped < Duration::from_secs(2), "shutdown took {stopped:?}");
    assert!(
        stderr.contains("SteamVR unavailable") || stderr.contains("SteamVR is not running"),
        "{stderr}"
    );
    assert!(
        stderr.contains("shutting down") || stderr.contains("closing"),
        "{stderr}"
    );
    // The lock is emptied on exit.
    assert_eq!(fs::read_to_string(&lock).unwrap(), "");
}

#[test]
fn sigusr1_toggles_the_hud() {
    let sb = Sandbox::new("toggle");
    let mut child = sb
        .command(&["--verbose"])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let config = sb.path("config/frame-vocal-pitch/config.toml");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !config.is_file() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    signal(&child, Signal::SIGUSR1);
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if fs::read_to_string(&config)
            .map(|text| text.contains("enabled = false"))
            .unwrap_or(false)
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    signal(&child, Signal::SIGTERM);
    assert_eq!(child.wait().unwrap().code(), Some(0));
    assert!(
        fs::read_to_string(&config).unwrap().contains("enabled = false"),
        "SIGUSR1 did not flip hud.enabled"
    );
}

#[test]
fn second_instance_signals_owner_and_exits_zero() {
    let sb = Sandbox::new("second");
    let lock_path = sb.path("run").join(instance::LOCK_FILE);

    // Stand-in for the running instance: a child whose default SIGUSR1 action terminates it.
    let mut owner = Command::new("sleep").arg("30").spawn().unwrap();
    let held = match instance::acquire(&lock_path).unwrap() {
        Acquired::Primary(lock) => lock,
        Acquired::Secondary(pid) => panic!("lock unexpectedly held by {pid}"),
    };
    fs::write(&lock_path, format!("{}\n", owner.id())).unwrap();

    let out = sb.run(&[]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    assert!(stderr.contains("already running"), "{stderr}");

    let status = owner.wait().unwrap();
    assert_eq!(status.signal(), Some(Signal::SIGUSR1 as i32));
    drop(held);
    // No config was touched by the second instance.
    assert!(!sb.path("config/frame-vocal-pitch").exists());
}
