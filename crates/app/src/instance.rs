//! Single instance (SPEC §6.2): an exclusive `flock` on `$XDG_RUNTIME_DIR/frame-vocal-pitch.lock`,
//! which holds the owner's PID. A second launch reads the PID, sends it `SIGUSR1` and exits 0.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use nix::errno::Errno;
use nix::fcntl::{Flock, FlockArg};
use nix::sys::signal::{kill, Signal};
use nix::unistd::{getuid, Pid};

/// File name of the lock inside the runtime directory.
pub const LOCK_FILE: &str = "frame-vocal-pitch.lock";

/// How long a second instance waits for the owner to write its PID after taking the lock.
const PID_WAIT: Duration = Duration::from_secs(1);
const PID_RETRY: Duration = Duration::from_millis(50);

/// `$XDG_RUNTIME_DIR` if set to an absolute path, else `/run/user/<uid>`.
pub fn runtime_dir() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| PathBuf::from(format!("/run/user/{}", getuid().as_raw())))
}

/// `<runtime dir>/frame-vocal-pitch.lock`.
pub fn lock_path() -> PathBuf {
    runtime_dir().join(LOCK_FILE)
}

/// Held lock of the running instance. Dropping it empties the file and releases the lock.
#[derive(Debug)]
pub struct InstanceLock {
    file: Flock<File>,
}

impl Drop for InstanceLock {
    fn drop(&mut self) {
        // Leave no stale PID behind; the file itself stays (unlinking would race with a new
        // instance that already opened it).
        let _ = self.file.set_len(0);
    }
}

/// Outcome of [`acquire`].
#[derive(Debug)]
pub enum Acquired {
    /// This process is the only instance.
    Primary(InstanceLock),
    /// Another instance holds the lock; its PID.
    Secondary(i32),
}

/// Takes the instance lock at `path` or reports the PID of the instance that holds it.
pub fn acquire(path: &Path) -> io::Result<Acquired> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path)
        .map_err(|e| io::Error::new(e.kind(), format!("cannot open {}: {e}", path.display())))?;

    match Flock::lock(file, FlockArg::LockExclusiveNonblock) {
        Ok(mut file) => {
            file.set_len(0)?;
            file.seek(SeekFrom::Start(0))?;
            file.write_all(format!("{}\n", std::process::id()).as_bytes())?;
            file.flush()?;
            Ok(Acquired::Primary(InstanceLock { file }))
        }
        Err((mut file, Errno::EWOULDBLOCK)) => {
            // The owner writes its PID right after locking; give it a moment.
            let mut waited = Duration::ZERO;
            loop {
                if let Some(pid) = read_pid(&mut file)? {
                    return Ok(Acquired::Secondary(pid));
                }
                if waited >= PID_WAIT {
                    return Err(io::Error::other(format!(
                        "{} is locked by another instance but holds no PID",
                        path.display()
                    )));
                }
                thread::sleep(PID_RETRY);
                waited += PID_RETRY;
            }
        }
        Err((_, errno)) => Err(io::Error::new(
            io::Error::from(errno).kind(),
            format!("cannot lock {}: {errno}", path.display()),
        )),
    }
}

fn read_pid(file: &mut File) -> io::Result<Option<i32>> {
    let mut text = String::new();
    file.seek(SeekFrom::Start(0))?;
    file.read_to_string(&mut text)?;
    Ok(text.trim().parse::<i32>().ok().filter(|&pid| pid > 0))
}

/// Sends `SIGUSR1` (toggle HUD + open dashboard tab) to the running instance.
pub fn signal_toggle(pid: i32) -> io::Result<()> {
    // Never 0 or negative: kill() would target a whole process group.
    if pid <= 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid PID {pid}"),
        ));
    }
    kill(Pid::from_raw(pid), Signal::SIGUSR1).map_err(|e| {
        io::Error::new(
            io::Error::from(e).kind(),
            format!("cannot signal PID {pid}: {e}"),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_lock(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("fvp-instance-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(LOCK_FILE)
    }

    #[test]
    fn second_acquire_sees_owner_pid() {
        let path = temp_lock("second");
        let first = acquire(&path).unwrap();
        assert!(matches!(first, Acquired::Primary(_)));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            format!("{}\n", std::process::id())
        );

        // flock conflicts between separate open file descriptions, even in one process.
        match acquire(&path).unwrap() {
            Acquired::Secondary(pid) => assert_eq!(pid, std::process::id() as i32),
            other => panic!("expected secondary, got {other:?}"),
        }

        drop(first);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "");
        assert!(matches!(acquire(&path).unwrap(), Acquired::Primary(_)));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn stale_pid_in_unlocked_file_is_replaced() {
        let path = temp_lock("stale");
        std::fs::write(&path, "999999999\n").unwrap();
        assert!(matches!(acquire(&path).unwrap(), Acquired::Primary(_)));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn refuses_group_pids() {
        assert!(signal_toggle(0).is_err());
        assert!(signal_toggle(-1).is_err());
    }
}
