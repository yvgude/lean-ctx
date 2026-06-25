//! Pipeline lock for preventing concurrent runs, and cancellation token.
//!
//! ## PipelineLock
//!
//! File-based lock using `flock`-backed advisory locking (via `fs2`). Creates
//! `<data_dir>/index.lock` with the PID of the owning process. If the owning
//! process is dead, the lock is automatically released by the OS (flock is
//! per-fd), so [`PipelineLock::try_acquire`] can detect the stale PID and
//! override it.
//!
//! ## CancelToken
//!
//! [`CancelToken`] is a clone‑able, thread‑safe signalling flag backed by
//! `Arc<AtomicBool>`.  `cancel()` is idempotent — workers check
//! `is_cancelled()` to stop early.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use fs2::FileExt;

// ── Error type ───────────────────────────────────────────────────────────────

/// Errors that can occur when acquiring a [`PipelineLock`].
#[derive(Debug)]
pub enum PipelineLockError {
    /// Another process holds the lock.
    /// Contains the PID from the lock file, if parseable.
    AlreadyLocked(Option<u32>),

    /// I/O error interacting with the lock file.
    Io(std::io::Error),
}

impl std::fmt::Display for PipelineLockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlreadyLocked(Some(pid)) => {
                write!(f, "pipeline already locked by PID {pid}")
            }
            Self::AlreadyLocked(None) => {
                write!(f, "pipeline already locked (unknown holder)")
            }
            Self::Io(e) => write!(f, "pipeline lock I/O error: {e}"),
        }
    }
}

impl std::error::Error for PipelineLockError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::AlreadyLocked(_) => None,
        }
    }
}

impl From<std::io::Error> for PipelineLockError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

// ── PipelineLock ─────────────────────────────────────────────────────────────

/// File-based lock to prevent concurrent pipeline runs.
///
/// Creates `<data_dir>/index.lock` with the PID of the owning process.
/// Uses `flock` (via `fs2`) for advisory locking.  The lock is released
/// automatically when the `PipelineLock` is dropped (and by the OS when
/// the process exits, since `flock` is per-file-descriptor).
pub struct PipelineLock {
    _lock_file: PathBuf,
    file: Option<std::fs::File>,
}

impl std::fmt::Debug for PipelineLock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PipelineLock")
            .field("lock_file", &self._lock_file)
            .field("file", &self.file.as_ref().map(|_| "<file>"))
            .finish()
    }
}

impl PipelineLock {
    /// Try to acquire the pipeline lock for `data_dir`.
    ///
    /// Returns `Ok` if the lock was acquired, `Err(PipelineLockError::AlreadyLocked)`
    /// if another (live) process already holds it.  Stale locks (dead PID) are
    /// automatically detected and overridden.
    pub fn try_acquire(data_dir: &Path) -> Result<Self, PipelineLockError> {
        std::fs::create_dir_all(data_dir)?;
        let lock_file = data_dir.join("index.lock");

        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false) // must read stale PID before overwriting
            .read(true)
            .write(true)
            .open(&lock_file)?;

        let my_pid = std::process::id();

        // ── Same-PID guard ──────────────────────────────────────
        // Within the same process, `flock` treats each fd independently, so
        // a second `open()` + `try_lock_exclusive()` *would* succeed and let
        // us trample our own lock.  Guard against this by checking the PID
        // recorded in the lock file before trying the flock.
        if let Ok(contents) = std::fs::read_to_string(&lock_file)
            && contents.trim().parse::<u32>().ok() == Some(my_pid)
        {
            return Err(PipelineLockError::AlreadyLocked(Some(my_pid)));
        }

        // ── Acquire exclusive lock ──────────────────────────────
        if let Ok(()) = file.try_lock_exclusive() {
            Self::write_pid(&file, my_pid)?;
            Ok(Self {
                _lock_file: lock_file,
                file: Some(file),
            })
        } else {
            // Check whether the lock holder is still alive.
            let holder_pid = std::fs::read_to_string(&lock_file)
                .ok()
                .and_then(|s| s.trim().parse::<u32>().ok());

            if let Some(pid) = holder_pid
                && pid != my_pid
                && !is_pid_alive(pid)
            {
                // Stale lock: OS already released the flock.  Retry.
                if let Ok(()) = file.try_lock_exclusive() {
                    Self::write_pid(&file, my_pid)?;
                    return Ok(Self {
                        _lock_file: lock_file,
                        file: Some(file),
                    });
                }
            }

            Err(PipelineLockError::AlreadyLocked(holder_pid))
        }
    }

    /// Truncate the lock file and write the current PID.
    fn write_pid(file: &std::fs::File, pid: u32) -> Result<(), PipelineLockError> {
        file.set_len(0)?;
        writeln!(&*file, "{pid}")?;
        file.sync_all()?;
        Ok(())
    }
}

impl Drop for PipelineLock {
    fn drop(&mut self) {
        if let Some(file) = self.file.take() {
            // Truncate while we still hold the exclusive lock so the next
            // same‑process caller does not see a stale PID in the guard check.
            let _ = file.set_len(0);
            let _ = file.unlock();
        }
    }
}

// ── PID liveness check ───────────────────────────────────────────────────────

/// Returns `true` if `pid` refers to a live process.
#[cfg(unix)]
fn is_pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    // POSIX: kill(pid, 0) returns 0 if process exists, -1 + ESRCH if not.
    // SAFETY: `libc::kill` with signal 0 is safe when `pid` is a valid i32.
    // PID 0 was already filtered above, and real PIDs fit in i32 on all Unix
    // platforms.
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

/// Non-Unix fallback: conservatively treat all PIDs as alive, preventing
/// stale-lock override on platforms where we cannot reliably probe.
#[cfg(not(unix))]
fn is_pid_alive(_pid: u32) -> bool {
    true
}

// ── CancelToken ──────────────────────────────────────────────────────────────

/// Clone-able cancellation token for signalling workers to stop.
#[derive(Clone)]
pub struct CancelToken {
    cancelled: Arc<AtomicBool>,
}

impl CancelToken {
    /// Create a new, uncancelled token.
    pub fn new() -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Signal cancellation.  Idempotent — subsequent calls are no-ops.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    /// Returns `true` if [`cancel`](Self::cancel) has been called.
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    /// Convenience: `true` when **not** cancelled.
    pub fn is_uncancelled(&self) -> bool {
        !self.cancelled.load(Ordering::SeqCst)
    }
}

impl Default for CancelToken {
    fn default() -> Self {
        Self::new()
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    // ── PipelineLock ──

    #[test]
    fn try_acquire_success() {
        let dir = tempfile::tempdir().unwrap();
        let lock = PipelineLock::try_acquire(dir.path());
        assert!(lock.is_ok());
    }

    #[test]
    fn try_acquire_second_fails() {
        let dir = tempfile::tempdir().unwrap();
        let _lock1 = PipelineLock::try_acquire(dir.path()).unwrap();
        let lock2 = PipelineLock::try_acquire(dir.path());
        match lock2 {
            Err(PipelineLockError::AlreadyLocked(Some(pid))) => {
                assert_eq!(pid, std::process::id());
            }
            other => panic!("expected AlreadyLocked(Some(PID)), got {other:?}"),
        }
    }

    #[test]
    fn drop_releases_lock() {
        let dir = tempfile::tempdir().unwrap();
        {
            let _lock1 = PipelineLock::try_acquire(dir.path()).unwrap();
        } // lock1 dropped → lock released
        let lock2 = PipelineLock::try_acquire(dir.path());
        assert!(
            lock2.is_ok(),
            "lock should be acquirable after drop: {lock2:?}"
        );
    }

    #[test]
    fn stale_pid_is_overridden() {
        // Simulate a stale lock by writing a fake dead PID into the lock file.
        let dir = tempfile::tempdir().unwrap();
        let lock_file = dir.path().join("index.lock");
        // Use PID 0xDEAD (which cannot exist — PID 0 is the idle process on
        // Unix and won't pass our liveness check; should also be dead on
        // Windows by virtue of being near the reserved range).
        std::fs::write(&lock_file, "57005\n").unwrap(); // 0xDEAD

        // Now acquire — should succeed because PID 57005 is unlikely to exist.
        let lock = PipelineLock::try_acquire(dir.path());
        assert!(lock.is_ok(), "stale PID should be overridden: {lock:?}");
    }

    // ── CancelToken ──

    #[test]
    fn cancel_token_basic() {
        let t = CancelToken::new();
        assert!(!t.is_cancelled());
        assert!(t.is_uncancelled());
        t.cancel();
        assert!(t.is_cancelled());
        assert!(!t.is_uncancelled());
    }

    #[test]
    fn cancel_idempotent() {
        let t = CancelToken::new();
        t.cancel();
        t.cancel(); // must not panic or change state
        assert!(t.is_cancelled());
    }

    #[test]
    fn cancel_token_clone_shared_state() {
        let t1 = CancelToken::new();
        let t2 = t1.clone();
        assert!(!t2.is_cancelled());
        t1.cancel();
        assert!(t2.is_cancelled());
        assert!(t1.is_cancelled());
    }

    #[test]
    fn cancel_token_send_sync() {
        fn assert_send<T: Send>(_v: &T) {}
        fn assert_sync<T: Sync>(_v: &T) {}
        let t = CancelToken::new();
        assert_send(&t);
        assert_sync(&t);
    }

    #[test]
    fn cancel_token_cross_thread() {
        let t1 = CancelToken::new();
        let t2 = t1.clone();
        let h = thread::spawn(move || {
            while t2.is_uncancelled() {
                thread::yield_now();
            }
        });
        t1.cancel();
        h.join().expect("worker should stop after cancel");
    }

    #[test]
    fn cancel_default_is_uncancelled() {
        let t = CancelToken::default();
        assert!(t.is_uncancelled());
    }
}
