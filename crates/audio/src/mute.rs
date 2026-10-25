//! Mute detection (SPEC §4.1 / §6.3 step 5): `wpctl get-volume` on the default source, and on
//! the hardware node while bypass is active, polled every 2 s on a small helper thread.

use std::process::{Command, Stdio};
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use crate::capture::{Shared, NO_NODE};

const POLL_INTERVAL: Duration = Duration::from_secs(2);
pub(crate) const DEFAULT_SOURCE_TARGET: &str = "@DEFAULT_AUDIO_SOURCE@";

/// Parses `wpctl get-volume` output, e.g. `Volume: 0.40 [MUTED]`.
pub(crate) fn parse_get_volume(out: &str) -> Option<bool> {
    let line = out.lines().find(|l| l.trim_start().starts_with("Volume:"))?;
    Some(line.contains("[MUTED]"))
}

/// Runs `wpctl get-volume <target>` and reports whether the target is muted.
pub(crate) fn wpctl_muted(target: &str) -> Result<bool, String> {
    let out = Command::new("wpctl")
        .args(["get-volume", target])
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("cannot run wpctl: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!("wpctl get-volume {target} failed: {}", err.trim()));
    }
    parse_get_volume(&String::from_utf8_lossy(&out.stdout))
        .ok_or_else(|| format!("unexpected wpctl output for {target}"))
}

/// Background poller; stops and joins on drop.
pub(crate) struct MutePoller {
    stop: Option<mpsc::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl MutePoller {
    pub fn start(shared: Arc<Shared>) -> std::io::Result<MutePoller> {
        let (stop, rx) = mpsc::channel::<()>();
        let thread = std::thread::Builder::new()
            .name("fvp-mute".into())
            .spawn(move || poll_loop(&shared, &rx))?;
        Ok(MutePoller {
            stop: Some(stop),
            thread: Some(thread),
        })
    }
}

impl Drop for MutePoller {
    fn drop(&mut self) {
        drop(self.stop.take()); // disconnects the channel, waking the thread
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn poll_loop(shared: &Shared, stop: &mpsc::Receiver<()>) {
    let mut last_error: Option<String> = None;
    loop {
        let mut report = |r: Result<bool, String>| match r {
            Ok(m) => m,
            Err(e) => {
                if last_error.as_deref() != Some(e.as_str()) {
                    log::warn!("mute check: {e}");
                    last_error = Some(e);
                }
                false
            }
        };
        let default_muted = report(wpctl_muted(DEFAULT_SOURCE_TARGET));
        let hw = shared.hw_node_id.load(Ordering::Relaxed);
        let hw_muted = hw != NO_NODE && report(wpctl_muted(&hw.to_string()));
        shared.muted.store(default_muted || hw_muted, Ordering::Relaxed);
        match stop.recv_timeout(POLL_INTERVAL) {
            Err(RecvTimeoutError::Timeout) => {}
            Ok(()) | Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_wpctl_output() {
        assert_eq!(parse_get_volume("Volume: 1.00\n"), Some(false));
        assert_eq!(parse_get_volume("Volume: 0.40 [MUTED]\n"), Some(true));
        assert_eq!(parse_get_volume("Translate ID error\n"), None);
        assert_eq!(parse_get_volume(""), None);
    }
}
