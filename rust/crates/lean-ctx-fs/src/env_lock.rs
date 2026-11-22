// SPDX-License-Identifier: Apache-2.0
//! The process-wide test environment lock.
//!
//! `std::env::set_var` / `std::env::remove_var` became `unsafe` in Rust 2024
//! because they are not thread-safe: a concurrent environment read from another
//! thread is undefined behaviour. Every test that mutates the environment
//! serializes through [`test_env_lock`], here and in the engine (which
//! re-exports it as `core::data_dir::test_env_lock`), so there is exactly one
//! lock per process. The mutation helpers `set_var` / `remove_var` exist
//! only for tests (`cfg(test)` or the `test-support` feature).

static TEST_ENV_MUTEX: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();

thread_local! {
    /// Re-entrancy depth for the current thread. >0 means this thread already
    /// holds the lock, so a nested acquire is a no-op bump instead of a
    /// self-deadlock.
    static TEST_ENV_DEPTH: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// The real guard, held only while depth transitions 0→…→0.
    static TEST_ENV_HELD: std::cell::RefCell<Option<std::sync::MutexGuard<'static, ()>>> =
        const { std::cell::RefCell::new(None) };
}

/// Reentrant test env lock. Serializes env-mutating tests across threads (the
/// point — `std::env::set_var` is not thread-safe) while letting the SAME
/// thread re-acquire without deadlocking. A test and a `setup()` helper (or
/// `isolated_data_dir`) can both take it; only the outermost acquisition holds
/// the underlying mutex, nested ones just bump a per-thread depth. Other
/// threads still block, so cross-test serialization is unchanged.
#[track_caller]
pub fn test_env_lock() -> TestEnvGuard {
    #[cfg(any(test, feature = "test-support"))]
    refuse_in_batch();
    let mutex = TEST_ENV_MUTEX.get_or_init(|| std::sync::Mutex::new(()));
    TEST_ENV_DEPTH.with(|depth| {
        if depth.get() == 0 {
            let guard = mutex
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            TEST_ENV_HELD.with(|held| *held.borrow_mut() = Some(guard));
        }
        depth.set(depth.get() + 1);
    });
    TestEnvGuard {
        _not_send_sync: std::marker::PhantomData,
    }
}

/// Set by batched test runs (scripts/ci/test_batch.py, P5_TEST_BATCH) to a
/// file path. A batch runs many tests in one process, so a test that changes
/// the process environment would change it under every other test running
/// there. With this variable set, [`test_env_lock`] appends one line
/// `<thread name>\t<caller file:line>` to that file and panics before anything
/// is changed; the batch runner then re-runs the test in a process of its own.
/// libtest names each test's thread after the test, so on a test's own thread
/// the first field is the test name. On a thread the test spawned it is that
/// thread's name (or `<unnamed thread>`): no test name is available there, and
/// the runner re-runs every test of that process instead. The runner re-runs a
/// recorded test even when it passed, so a caught panic cannot hide a hit.
#[cfg(any(test, feature = "test-support"))]
pub const TEST_BATCH_ENV: &str = "LEAN_CTX_TEST_BATCH";

#[cfg(any(test, feature = "test-support"))]
#[track_caller]
fn refuse_in_batch() {
    let Some(log) = std::env::var_os(TEST_BATCH_ENV) else {
        return;
    };
    let test = std::thread::current()
        .name()
        .unwrap_or("<unnamed thread>")
        .to_owned();
    let caller = std::panic::Location::caller();
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log)
    {
        use std::io::Write as _;
        // One write per line: `writeln!` on a File issues a write per
        // formatted piece, and tests of the same batch interleave them into
        // torn lines that name no test (shadow !534, job 60311).
        let line = format!("{test}\t{}:{}\n", caller.file(), caller.line());
        let _ = file.write_all(line.as_bytes());
    }
    panic!(
        "{test}: takes test_env_lock, so it needs a process of its own; \
         the batched run ({TEST_BATCH_ENV}) re-runs it alone"
    );
}

/// Whether the current thread holds [`test_env_lock`]. The lock belongs to
/// the thread that took it: a guard held by another thread does not count.
#[cfg(any(test, feature = "test-support"))]
pub fn holds_test_env_lock() -> bool {
    TEST_ENV_DEPTH.with(|depth| depth.get() > 0)
}

/// RAII guard for [`test_env_lock`]. Dropping the outermost one releases the
/// underlying mutex; nested guards just decrement the depth.
///
/// The lock and its depth live in the acquiring thread's own storage, so the
/// guard must be dropped on that thread: it is neither `Send` nor `Sync`.
/// Moved to another thread it would decrement that thread's depth and leave
/// the lock held here.
pub struct TestEnvGuard {
    _not_send_sync: std::marker::PhantomData<*const ()>,
}

impl Drop for TestEnvGuard {
    fn drop(&mut self) {
        TEST_ENV_DEPTH.with(|depth| {
            let next = depth.get().saturating_sub(1);
            depth.set(next);
            if next == 0 {
                TEST_ENV_HELD.with(|held| *held.borrow_mut() = None);
            }
        });
    }
}

/// Sets `key` to `value` in the process environment (test-only).
#[cfg(any(test, feature = "test-support"))]
pub fn set_var<K: AsRef<std::ffi::OsStr>, V: AsRef<std::ffi::OsStr>>(key: K, value: V) {
    assert_lock_held();
    // SAFETY: the assertion above holds, so this thread owns test_env_lock()
    // and no other thread reads or writes the environment concurrently.
    unsafe { std::env::set_var(key, value) };
}

/// Removes `key` from the process environment (test-only).
#[cfg(any(test, feature = "test-support"))]
pub fn remove_var<K: AsRef<std::ffi::OsStr>>(key: K) {
    assert_lock_held();
    // SAFETY: the assertion above holds, so this thread owns test_env_lock()
    // and no other thread reads or writes the environment concurrently.
    unsafe { std::env::remove_var(key) };
}

/// The precondition of the two functions above, checked in every build of the
/// tests and before anything is changed. It does not take the lock itself:
/// that would hide a caller whose lock does not span its whole mutation.
#[cfg(any(test, feature = "test-support"))]
fn assert_lock_held() {
    assert!(
        holds_test_env_lock(),
        "test environment mutation requires the current thread to hold test_env_lock()"
    );
}

#[cfg(test)]
mod tests {
    use super::{TEST_BATCH_ENV, test_env_lock};

    const NAME: &str = "env_lock::tests::batch_tripwire_records_and_refuses";

    /// Under a batched run the lock records the test and refuses before any
    /// change. The test runs itself in a child process with the variable set
    /// (the child takes the lock and must fail), so the parent never mutates
    /// its own environment.
    #[test]
    fn batch_tripwire_records_and_refuses() {
        if std::env::var_os(TEST_BATCH_ENV).is_some() {
            let _guard = test_env_lock();
            unreachable!("test_env_lock returned inside a batched run");
        }
        let dir = tempfile::tempdir().expect("tempdir");
        let log = dir.path().join("env-lock.log");
        let output = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([NAME, "--exact", "--test-threads=1"])
            .env(TEST_BATCH_ENV, &log)
            .output()
            .expect("run the child test");
        assert!(!output.status.success(), "the child must fail");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("needs a process of its own"), "{stdout}");
        let recorded = std::fs::read_to_string(&log).expect("tripwire log");
        let (thread, caller) = recorded
            .trim_end()
            .split_once('\t')
            .expect("a `<thread>\\t<caller>` line");
        assert!(
            thread.ends_with("batch_tripwire_records_and_refuses"),
            "libtest names the thread after the test: {recorded}"
        );
        assert!(caller.contains("env_lock.rs:"), "{recorded}");
    }
}
