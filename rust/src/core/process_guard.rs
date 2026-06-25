//! Global concurrency limiter for lean-ctx processes.
//!
//! Prevents runaway CPU usage by limiting the number of concurrent lean-ctx
//! processes to `MAX_CONCURRENT`. Each process acquires a numbered lock slot
//! under `~/.lean-ctx/locks/`. If all slots are taken, the caller gets `None`.

use std::fs::File;
use std::path::PathBuf;

const MAX_CONCURRENT: usize = 4;

pub struct ProcessGuard {
    _file: File,
    path: PathBuf,
}

impl Drop for ProcessGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn lock_dir() -> Option<PathBuf> {
    let dir = crate::core::data_dir::lean_ctx_data_dir()
        .ok()?
        .join("locks");
    let _ = std::fs::create_dir_all(&dir);
    Some(dir)
}

/// Try to acquire one of N concurrent process slots.
/// Returns `None` if all slots are occupied (= too many lean-ctx already running).
#[must_use]
pub fn acquire() -> Option<ProcessGuard> {
    let dir = lock_dir()?;

    for slot in 0..MAX_CONCURRENT {
        let path = dir.join(format!("slot-{slot}.lock"));

        let Ok(file) = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
        else {
            continue;
        };

        if try_flock(&file) {
            use std::io::Write;
            let mut f = file;
            let _ = f.write_all(format!("{}", std::process::id()).as_bytes());
            return Some(ProcessGuard { _file: f, path });
        }
    }

    None
}

/// Checks how many slots are currently held (best-effort).
#[must_use]
pub fn active_count() -> usize {
    let Some(dir) = lock_dir() else { return 0 };
    let mut count = 0;
    for slot in 0..MAX_CONCURRENT {
        let path = dir.join(format!("slot-{slot}.lock"));
        if let Ok(f) = std::fs::OpenOptions::new().read(true).open(&path)
            && !try_flock(&f)
        {
            count += 1;
        }
    }
    count
}

#[cfg(unix)]
fn try_flock(file: &File) -> bool {
    use std::os::unix::io::AsRawFd;
    let fd = file.as_raw_fd();
    // SAFETY: `fd` is a valid, open descriptor owned by `file`, which outlives
    // this call; `flock` performs no pointer dereference and reports errors via
    // its return value.
    let rc = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
    rc == 0
}

#[cfg(not(unix))]
fn try_flock(_file: &File) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Restores `LEAN_CTX_DATA_DIR` to its previous value on drop (panic-safe).
    struct EnvRestore(Option<String>);
    impl Drop for EnvRestore {
        fn drop(&mut self) {
            match &self.0 {
                Some(v) => crate::test_env::set_var("LEAN_CTX_DATA_DIR", v),
                None => crate::test_env::remove_var("LEAN_CTX_DATA_DIR"),
            }
        }
    }

    /// Runs `body` against a private, empty lock directory.
    ///
    /// `acquire()` and `active_count()` both resolve the lock dir from
    /// `LEAN_CTX_DATA_DIR`. Serializing on `test_env_lock` stops a concurrent
    /// test from repointing that variable between the two calls (which made
    /// `active_count` inspect a different, empty dir and miss the held slot), and
    /// the private temp dir keeps slots independent of any real lean-ctx process
    /// (daemon/proxy) that might otherwise occupy them.
    fn with_isolated_lock_dir(body: impl FnOnce()) {
        let _env = crate::core::data_dir::test_env_lock();
        let tmp = tempfile::tempdir().expect("tempdir");
        // Restore runs before `tmp` is removed and while the lock is still held.
        let _restore = EnvRestore(std::env::var("LEAN_CTX_DATA_DIR").ok());
        crate::test_env::set_var("LEAN_CTX_DATA_DIR", tmp.path());
        body();
    }

    #[test]
    fn acquire_and_release() {
        with_isolated_lock_dir(|| {
            let guard = acquire();
            assert!(guard.is_some(), "should acquire first slot");
            drop(guard);
        });
    }

    #[cfg(unix)]
    #[test]
    fn active_count_reflects_held_slots() {
        with_isolated_lock_dir(|| {
            let g1 = acquire();
            assert!(g1.is_some());
            let count = active_count();
            assert!(count >= 1, "at least one slot held, got {count}");
            drop(g1);
        });
    }
}
