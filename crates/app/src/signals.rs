//! `SIGTERM`/`SIGINT` (clean shutdown) and `SIGUSR1` (toggle) as pollable file descriptors
//! (SPEC §6.2). Each delivered signal writes one byte to a self-pipe, so the main loop's `poll()`
//! wakes up and [`Signals::take`] counts what arrived. A second `SIGTERM`/`SIGINT` while a
//! shutdown is already pending terminates the process immediately (exit status 1).

use std::io::{self, Read};
use std::os::fd::{AsFd, BorrowedFd};
use std::os::unix::net::UnixStream;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use signal_hook::consts::{SIGINT, SIGTERM, SIGUSR1};
use signal_hook::{flag, low_level, SigId};

/// Signals received since the previous [`Signals::take`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Pending {
    /// `SIGTERM` or `SIGINT` arrived: shut down cleanly.
    pub terminate: bool,
    /// Number of `SIGUSR1` received: each toggles the HUD and opens the dashboard tab.
    pub toggles: usize,
}

/// Installed signal handlers. Dropping it unregisters them (signal-hook keeps its low-level
/// handler, so the signals are then ignored rather than reverting to their default action).
#[derive(Debug)]
pub struct Signals {
    term_rx: UnixStream,
    toggle_rx: UnixStream,
    ids: Vec<SigId>,
}

impl Signals {
    /// Registers the handlers.
    pub fn install() -> io::Result<Signals> {
        let (term_rx, term_tx) = UnixStream::pair()?;
        let (toggle_rx, toggle_tx) = UnixStream::pair()?;
        term_rx.set_nonblocking(true)?;
        toggle_rx.set_nonblocking(true)?;

        let shutting_down = Arc::new(AtomicBool::new(false));
        let mut ids = Vec::new();
        let mut register = || -> io::Result<()> {
            for sig in [SIGTERM, SIGINT] {
                // Order matters: the conditional exit sees the flag set by an earlier signal.
                ids.push(flag::register_conditional_shutdown(
                    sig,
                    1,
                    Arc::clone(&shutting_down),
                )?);
                ids.push(flag::register(sig, Arc::clone(&shutting_down))?);
                ids.push(low_level::pipe::register(sig, term_tx.try_clone()?)?);
            }
            ids.push(low_level::pipe::register(SIGUSR1, toggle_tx.try_clone()?)?);
            Ok(())
        };
        if let Err(e) = register() {
            for id in ids {
                low_level::unregister(id);
            }
            return Err(e);
        }
        Ok(Signals {
            term_rx,
            toggle_rx,
            ids,
        })
    }

    /// File descriptors that become readable when a signal arrives (for `poll()`).
    pub fn fds(&self) -> [BorrowedFd<'_>; 2] {
        [self.term_rx.as_fd(), self.toggle_rx.as_fd()]
    }

    /// Drains the pipes and reports what arrived. Never blocks.
    pub fn take(&self) -> Pending {
        Pending {
            terminate: drain(&self.term_rx) > 0,
            toggles: drain(&self.toggle_rx),
        }
    }
}

impl Drop for Signals {
    fn drop(&mut self) {
        for id in self.ids.drain(..) {
            low_level::unregister(id);
        }
    }
}

/// Reads everything available; returns the number of bytes (= signals) read.
fn drain(mut rx: &UnixStream) -> usize {
    let mut buf = [0u8; 64];
    let mut total = 0;
    loop {
        match rx.read(&mut buf) {
            Ok(0) => return total,
            Ok(n) => total += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return total,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    // One test: the handlers are process-wide.
    #[test]
    fn usr1_counted_and_term_flagged() {
        let s = Signals::install().unwrap();
        assert_eq!(s.take(), Pending::default());
        low_level::raise(SIGUSR1).unwrap();
        low_level::raise(SIGUSR1).unwrap();
        // raise() runs the handler synchronously; allow a little slack anyway.
        let deadline = Instant::now() + Duration::from_secs(1);
        let mut got = Pending::default();
        while got.toggles < 2 && Instant::now() < deadline {
            let p = s.take();
            got.toggles += p.toggles;
            got.terminate |= p.terminate;
        }
        assert_eq!(
            got,
            Pending {
                terminate: false,
                toggles: 2
            }
        );

        // A single SIGTERM only requests shutdown (a second one would exit the process).
        low_level::raise(SIGTERM).unwrap();
        assert_eq!(
            s.take(),
            Pending {
                terminate: true,
                toggles: 0
            }
        );
        assert_eq!(s.take(), Pending::default());
    }
}
