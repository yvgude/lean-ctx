// SPDX-License-Identifier: Apache-2.0
//! Test-only helpers for process environment isolation and mutation.
//!
//! `std::env::set_var` / `std::env::remove_var` became `unsafe` in Rust 2024
//! because they are not thread-safe: a concurrent environment read from another
//! thread is undefined behaviour. lean-ctx's tests serialize every environment
//! mutation through [`crate::core::data_dir::test_env_lock`], so the precondition
//! holds and the call is sound. Centralising the `unsafe` here documents that
//! invariant exactly once instead of at hundreds of call sites, while keeping
//! `#![warn(clippy::undocumented_unsafe_blocks)]` strict everywhere else.

use std::ffi::OsStr;

/// Runs one exact test in a fresh process so its cached scope policy cannot be
/// inherited from the parent suite. Returns false only inside that child.
pub(crate) fn run_with_conversation_scope(test_name: &str, enabled: bool) -> bool {
    run_in_own_process(
        test_name,
        &[(
            "LEAN_CTX_CONVERSATION_SCOPE",
            if enabled { "1" } else { "0" },
        )],
    )
}

/// Runs one exact test alone in a fresh process, for tests that change
/// process-wide state other tests read without a lock (the policy override,
/// for one). Returns false only inside that child.
pub(crate) fn run_in_own_process(test_name: &str, env: &[(&str, &str)]) -> bool {
    run_in_own_process_at(test_name, env, None)
}

/// Runs an exact test with an optional child working directory, without changing
/// the parent suite's process-wide cwd or requiring a writable source checkout.
pub(crate) fn run_in_own_process_at(
    test_name: &str,
    env: &[(&str, &str)],
    directory: Option<&std::path::Path>,
) -> bool {
    const CHILD: &str = "LEAN_CTX_TEST_ISOLATED_CASE";
    if std::env::var(CHILD).as_deref() == Ok(test_name) {
        return false;
    }
    let _environment = crate::core::data_dir::test_env_lock();
    let mut child = std::process::Command::new(std::env::current_exe().expect("test executable"));
    child
        .args(["--exact", test_name, "--nocapture"])
        .env(CHILD, test_name)
        .envs(env.iter().copied());
    if let Some(directory) = directory {
        child.current_dir(directory);
    }
    let output = child.output().expect("isolated test process");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("test result: ok. 1 passed"),
        "isolated test {test_name} failed or did not run: {}\n{stdout}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );
    true
}

/// Sets `key` to `value` in the process environment (test-only).
pub(crate) fn set_var<K: AsRef<OsStr>, V: AsRef<OsStr>>(key: K, value: V) {
    assert_env_lock_held(key.as_ref());
    // SAFETY: tests serialize all environment access through test_env_lock(),
    // so no other thread reads or writes the environment concurrently.
    unsafe { std::env::set_var(key, value) };
}

/// Removes `key` from the process environment (test-only).
pub(crate) fn remove_var<K: AsRef<OsStr>>(key: K) {
    assert_env_lock_held(key.as_ref());
    // SAFETY: tests serialize all environment access through test_env_lock(),
    // so no other thread reads or writes the environment concurrently.
    unsafe { std::env::remove_var(key) };
}

/// The SAFETY argument above is only true if the caller holds the lock. An
/// unlocked mutation does not crash; it silently repoints a parallel test's
/// data dir or role mid-run, and on a many-core runner every run tripped a
/// different pair. Enforcing the invariant turns that into a deterministic
/// failure at the offending call site.
fn assert_env_lock_held(key: &OsStr) {
    #[cfg(test)]
    assert!(
        crate::core::data_dir::holds_test_env_lock(),
        "test env mutation of {key:?} without crate::core::data_dir::test_env_lock()"
    );
    #[cfg(not(test))]
    let _ = key;
}
