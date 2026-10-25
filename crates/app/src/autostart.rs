//! Autostart with SteamVR = the enabled state of the systemd user unit (SPEC §4.2, §7).
//!
//! `systemctl --user` runs with `XDG_RUNTIME_DIR=/run/user/<uid>`: in Desktop Mode the inherited
//! value may point at a nested session where `systemctl --user` fails (SPEC §2).

use std::ffi::OsString;
use std::io;
use std::process::{Command, Output, Stdio};

use nix::unistd::getuid;

/// Name of the user unit installed by `install.sh`.
pub const UNIT: &str = "frame-vocal-pitch.service";

/// Queries and changes the unit's enabled state.
#[derive(Debug, Clone)]
pub struct Autostart {
    program: OsString,
    runtime_dir: OsString,
}

impl Default for Autostart {
    fn default() -> Self {
        Autostart::new()
    }
}

impl Autostart {
    /// Uses `systemctl` from `PATH` and `/run/user/<uid>`.
    pub fn new() -> Autostart {
        Autostart {
            program: "systemctl".into(),
            runtime_dir: format!("/run/user/{}", getuid().as_raw()).into(),
        }
    }

    /// Uses another `systemctl` executable (tests, unusual installs).
    pub fn with_program(program: impl Into<OsString>) -> Autostart {
        Autostart {
            program: program.into(),
            ..Autostart::new()
        }
    }

    /// `true` if the unit is enabled (starts with `steamvr.service`).
    pub fn is_enabled(&self) -> io::Result<bool> {
        let out = self.systemctl(&["is-enabled", UNIT])?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        // `is-enabled` prints the state even when it exits non-zero (disabled, not-found, ...).
        match stdout.lines().next().map(str::trim) {
            Some("enabled" | "enabled-runtime") => Ok(true),
            Some(state) if !state.is_empty() => Ok(false),
            _ => Err(failure("is-enabled", &out)),
        }
    }

    /// Enables or disables the unit. Does not start or stop the running instance.
    pub fn set_enabled(&self, enabled: bool) -> io::Result<()> {
        let verb = if enabled { "enable" } else { "disable" };
        let out = self.systemctl(&[verb, UNIT])?;
        if out.status.success() {
            log::info!("autostart {}", if enabled { "enabled" } else { "disabled" });
            Ok(())
        } else {
            Err(failure(verb, &out))
        }
    }

    fn systemctl(&self, args: &[&str]) -> io::Result<Output> {
        Command::new(&self.program)
            .arg("--user")
            .args(args)
            .env("XDG_RUNTIME_DIR", &self.runtime_dir)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| {
                io::Error::new(
                    e.kind(),
                    format!("cannot run {}: {e}", self.program.to_string_lossy()),
                )
            })
    }
}

fn failure(verb: &str, out: &Output) -> io::Error {
    let stderr = String::from_utf8_lossy(&out.stderr);
    io::Error::other(format!(
        "systemctl --user {verb} {UNIT} failed ({}): {}",
        out.status,
        stderr.trim()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    /// A fake `systemctl` that logs its arguments and environment and answers `is-enabled` from
    /// a state file.
    struct Fake {
        dir: PathBuf,
    }

    impl Fake {
        fn new(name: &str) -> Fake {
            let dir =
                std::env::temp_dir().join(format!("fvp-autostart-{}-{name}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            let script = format!(
                "#!/bin/sh\n\
                 d='{d}'\n\
                 echo \"$* XDG_RUNTIME_DIR=$XDG_RUNTIME_DIR\" >> \"$d/log\"\n\
                 case \"$2\" in\n\
                 is-enabled) s=$(cat \"$d/state\" 2>/dev/null || echo not-found); echo \"$s\"; [ \"$s\" = enabled ];;\n\
                 enable) echo enabled > \"$d/state\";;\n\
                 disable) echo disabled > \"$d/state\";;\n\
                 esac\n",
                d = dir.display()
            );
            let prog = dir.join("systemctl");
            fs::write(&prog, script).unwrap();
            fs::set_permissions(&prog, fs::Permissions::from_mode(0o755)).unwrap();
            Fake { dir }
        }
        fn autostart(&self) -> Autostart {
            Autostart::with_program(self.dir.join("systemctl"))
        }
        fn log(&self) -> String {
            fs::read_to_string(self.dir.join("log")).unwrap_or_default()
        }
    }

    impl Drop for Fake {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    // One test function: writing an executable while another test thread forks can make exec
    // fail with ETXTBSY, so the fake programs are written and run sequentially.
    #[test]
    fn systemctl_calls() {
        enable_disable_query();
        failures_are_errors();
    }

    fn enable_disable_query() {
        let fake = Fake::new("cycle");
        let a = fake.autostart();
        assert!(!a.is_enabled().unwrap(), "not-found counts as disabled");
        a.set_enabled(true).unwrap();
        assert!(a.is_enabled().unwrap());
        a.set_enabled(false).unwrap();
        assert!(!a.is_enabled().unwrap());

        let uid = getuid().as_raw();
        let log = fake.log();
        let lines: Vec<&str> = log.lines().collect();
        assert_eq!(lines.len(), 5);
        assert_eq!(
            lines[1],
            format!("--user enable {UNIT} XDG_RUNTIME_DIR=/run/user/{uid}")
        );
        assert_eq!(
            lines[3],
            format!("--user disable {UNIT} XDG_RUNTIME_DIR=/run/user/{uid}")
        );
    }

    fn failures_are_errors() {
        let fake = Fake::new("fail");
        let prog = fake.dir.join("systemctl");
        fs::write(
            &prog,
            "#!/bin/sh\necho 'Failed to connect to bus' >&2\nexit 1\n",
        )
        .unwrap();
        let a = fake.autostart();
        let err = a.is_enabled().unwrap_err();
        assert!(
            err.to_string().contains("Failed to connect to bus"),
            "{err}"
        );
        assert!(a.set_enabled(true).is_err());

        assert!(Autostart::with_program(fake.dir.join("missing"))
            .is_enabled()
            .is_err());
    }
}
