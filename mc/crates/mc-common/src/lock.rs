//! The exclusive mc lock, serialising install / upgrade / backup / restore.
//!
//! Without it the backup timer can tar `/opt/minecraft` midway through an
//! install or — worse — while a restore is emptying the directory, and the
//! retention rotation then prunes a good archive in favour of the truncated
//! one.

use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};

use crate::error::{Error, IoContext, Result};

/// Held for as long as the guard is alive. Dropping it releases the lock.
///
/// RAII rather than a cleanup registry. The shell version needed a global
/// registry and one `EXIT` trap because a `trap` set inside a function replaces
/// any other, and a `RETURN` trap is not scoped to the function that sets it —
/// it fires again in the caller, when the locals it names are gone. `Drop` runs
/// on every exit path including `?`, and composes.
#[derive(Debug)]
pub struct LockGuard {
    path: PathBuf,
    /// A re-entrant acquisition owns nothing and must not delete the file.
    owns: bool,
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        if self.owns {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

impl LockGuard {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Take the lock, or report who holds it.
///
/// RE-ENTRANT within a process: `mc upgrade` holds the lock and then runs a
/// backup, which takes it too. A second acquisition returns a guard that owns
/// nothing rather than deadlocking — which is what lets the backup path lock at
/// all.
pub fn acquire(lock_file: &Path) -> Result<LockGuard> {
    if let Some(dir) = lock_file.parent() {
        std::fs::create_dir_all(dir).at(dir)?;
    }

    for attempt in 0..2 {
        // Create-or-fail in a SINGLE syscall. A `path.exists()` test followed by
        // a separate write is a TOCTOU: two runs starting together both see no
        // lock and both proceed.
        match std::fs::File::create_new(lock_file) {
            Ok(mut file) => {
                let pid = std::process::id();
                let cmd = std::env::args()
                    .nth(1)
                    .unwrap_or_else(|| "unknown".to_string());
                file.write_all(format!("{pid}\n{cmd}\n").as_bytes())
                    .at(lock_file)?;
                return Ok(LockGuard {
                    path: lock_file.to_path_buf(),
                    owns: true,
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let (holder_pid, holder_cmd) = read_holder(lock_file);

                // Re-entrant: this process already holds it.
                if holder_pid == Some(std::process::id()) {
                    return Ok(LockGuard {
                        path: lock_file.to_path_buf(),
                        owns: false,
                    });
                }

                // NOTE: a recycled PID can make a stale lock look live, costing
                // a spurious "already running" refusal. That is the safe
                // direction to err in — probing harder (matching
                // /proc/<pid>/cmdline) risks the opposite mistake, deleting a
                // lock that is genuinely held.
                if holder_pid.is_some_and(process_alive) {
                    return Err(Error::Locked(format!(
                        "Another mc operation is already running: PID {} ({}). Try again later.",
                        holder_pid.unwrap_or(0),
                        holder_cmd.as_deref().unwrap_or("unknown")
                    )));
                }

                if attempt == 0 {
                    // Left behind by a run that was killed before its cleanup.
                    let _ = std::fs::remove_file(lock_file);
                    continue;
                }
            }
            Err(e) => return Err(Error::io(lock_file, e)),
        }
    }

    Err(Error::Locked(format!(
        "Could not acquire lock {}.",
        lock_file.display()
    )))
}

fn read_holder(path: &Path) -> (Option<u32>, Option<String>) {
    let mut text = String::new();
    if std::fs::File::open(path)
        .and_then(|mut f| f.read_to_string(&mut text))
        .is_err()
    {
        return (None, None);
    }
    let mut lines = text.lines();
    let pid = lines.next().and_then(|l| l.trim().parse().ok());
    let cmd = lines.next().map(str::to_string);
    (pid, cmd)
}

/// `kill(pid, 0)` — asks whether the process exists without signalling it.
fn process_alive(pid: u32) -> bool {
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    // ESRCH means no such process. EPERM means it exists but belongs to someone
    // else, which still counts as alive.
    match nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None) {
        Ok(()) => true,
        Err(nix::errno::Errno::EPERM) => true,
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lock_path(dir: &tempfile::TempDir) -> PathBuf {
        dir.path().join("run/minecraft/mc.lock")
    }

    #[test]
    fn releases_on_drop_including_the_error_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = lock_path(&dir);

        fn fallible(path: &Path) -> Result<()> {
            let _guard = acquire(path)?;
            Err(Error::other("something went wrong"))
        }

        assert!(fallible(&path).is_err());
        assert!(!path.exists(), "the lock must not survive an early return");
    }

    #[test]
    fn is_re_entrant_within_one_process() {
        // upgrade holds the lock and then runs a backup, which takes it too.
        let dir = tempfile::tempdir().unwrap();
        let path = lock_path(&dir);

        let outer = acquire(&path).unwrap();
        let inner = acquire(&path).unwrap();
        drop(inner);

        assert!(
            path.exists(),
            "the inner guard owns nothing and must not release"
        );
        drop(outer);
        assert!(!path.exists());
    }

    #[test]
    fn refuses_while_a_live_process_holds_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = lock_path(&dir);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();

        // PID 1 is always alive and is never us.
        std::fs::write(&path, "1\ninstall\n").unwrap();
        let err = acquire(&path).unwrap_err();
        assert!(matches!(err, Error::Locked(_)), "{err}");
        assert!(
            err.to_string().contains("install"),
            "names what is holding it: {err}"
        );
        assert!(path.exists(), "a live holder's lock must not be removed");
    }

    #[test]
    fn reclaims_a_lock_whose_holder_is_gone() {
        let dir = tempfile::tempdir().unwrap();
        let path = lock_path(&dir);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();

        // A PID that cannot exist: the kernel's pid_max is well below this.
        std::fs::write(&path, "4194304\nbackup\n").unwrap();
        let guard = acquire(&path).unwrap();
        assert!(path.exists());
        drop(guard);
        assert!(!path.exists());
    }

    #[test]
    fn reclaims_a_truncated_lock_file() {
        // Killed between create and write: the file exists with no PID in it.
        let dir = tempfile::tempdir().unwrap();
        let path = lock_path(&dir);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "").unwrap();

        let guard = acquire(&path).unwrap();
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.starts_with(&std::process::id().to_string()));
        drop(guard);
    }
}
