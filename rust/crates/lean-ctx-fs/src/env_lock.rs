// SPDX-License-Identifier: Apache-2.0
//! The process-wide test environment lock.
//!
//! `std::env::set_var` / `std::env::remove_var` became `unsafe` in Rust 2024
//! because they are not thread-safe: a concurrent environment read from another
//! thread is undefined behaviour. Every test that mutates the environment
//! serializes through [`test_env_lock`], here and in the engine (which
//! re-exports it as `core::data_dir::test_env_lock`), so there is exactly one
//! lock per process. The mutation helpers [`set_var`] / [`remove_var`] exist
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
pub fn test_env_lock() -> TestEnvGuard {
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
