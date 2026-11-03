//! Process-wide per-path advisory locks.
//!
//! These in-process mutexes serialize concurrent operations on the *same* file
//! path while letting operations on *different* paths run fully in parallel.
//!
//! Why this exists: tools like `ctx_read` and `ctx_edit` would otherwise contend
//! on the single global cache write-lock for the entire duration of their disk
//! I/O. When several agents (or sub-agents) hammer files concurrently, that
//! global lock becomes a bottleneck and edits can time out waiting for it (see
//! issue #320). A per-path lock keeps the contention scoped to the one file that
//! actually needs serialization, so unrelated reads/edits never block each other.
//!
//! Lock ordering (see `LOCK_ORDERING.md`, L17): the inner registry mutex is held
//! only long enough to clone the per-path `Arc<Mutex<()>>`, then released before
//! the per-path lock itself is acquired. Never hold the registry mutex across the
//! per-path lock, and never acquire a per-path lock while holding the global
//! cache write-lock.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Upper bound on retained lock entries before we garbage-collect unused ones.
const MAX_ENTRIES: usize = 500;

/// Returns the shared advisory lock for `path`, creating it on first use.
///
/// The same path always yields the same `Arc<Mutex<()>>`, so callers across
/// threads serialize on it. Different paths yield independent mutexes.
pub fn per_file_lock(path: &str) -> Arc<Mutex<()>> {
    static FILE_LOCKS: std::sync::OnceLock<Mutex<HashMap<String, Arc<Mutex<()>>>>> =
        std::sync::OnceLock::new();
    let map = FILE_LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut map = map.lock().unwrap_or_else(|poisoned| {
        tracing::warn!("path_locks registry poisoned; recovering");
        poisoned.into_inner()
    });

    // Bounded growth: drop entries no one else is holding a reference to. The
    // `> 1` check keeps any lock that is currently in use by another caller.
    if map.len() > MAX_ENTRIES {
        map.retain(|_, v| Arc::strong_count(v) > 1);
    }

    map.entry(path.to_string())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Barrier;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn same_path_returns_same_mutex() {
        let a1 = per_file_lock("/tmp/path_locks_same.txt");
        let a2 = per_file_lock("/tmp/path_locks_same.txt");
        assert!(Arc::ptr_eq(&a1, &a2));
    }

    #[test]
    fn different_paths_return_different_mutexes() {
        let a = per_file_lock("/tmp/path_locks_a.txt");
        let b = per_file_lock("/tmp/path_locks_b.txt");
        assert!(!Arc::ptr_eq(&a, &b));
    }

    #[test]
    fn serializes_concurrent_access_to_same_path() {
        let counter = Arc::new(AtomicUsize::new(0));
        let max_concurrent = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(Barrier::new(8));
        let path = "/tmp/path_locks_serialize.txt";
        let mut handles = Vec::new();
        for _ in 0..8 {
            let counter = Arc::clone(&counter);
            let max_concurrent = Arc::clone(&max_concurrent);
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                let lock = per_file_lock(path);
                let _guard = lock
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let active = counter.fetch_add(1, Ordering::SeqCst) + 1;
                max_concurrent.fetch_max(active, Ordering::SeqCst);
                std::thread::sleep(std::time::Duration::from_millis(5));
                counter.fetch_sub(1, Ordering::SeqCst);
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(
            max_concurrent.load(Ordering::SeqCst),
            1,
            "per-file lock must serialize same-path access"
        );
    }

    #[test]
    fn allows_parallel_access_to_different_paths() {
        let counter = Arc::new(AtomicUsize::new(0));
        let max_concurrent = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(Barrier::new(8));
        let mut handles = Vec::new();
        for i in 0..8 {
            let counter = Arc::clone(&counter);
            let max_concurrent = Arc::clone(&max_concurrent);
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                let path = format!("/tmp/path_locks_parallel_{i}.txt");
                barrier.wait();
                let lock = per_file_lock(&path);
                let _guard = lock
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let active = counter.fetch_add(1, Ordering::SeqCst) + 1;
                max_concurrent.fetch_max(active, Ordering::SeqCst);
                std::thread::sleep(std::time::Duration::from_millis(5));
                counter.fetch_sub(1, Ordering::SeqCst);
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert!(
            max_concurrent.load(Ordering::SeqCst) > 1,
            "different paths must be allowed to run in parallel"
        );
    }
}
