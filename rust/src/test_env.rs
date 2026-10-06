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
    const CHILD: &str = "LEAN_CTX_TEST_CONVERSATION_SCOPE_CASE";
    if std::env::var(CHILD).as_deref() == Ok(test_name) {
        return false;
    }
    let _environment = crate::core::data_dir::test_env_lock();
    let output = std::process::Command::new(std::env::current_exe().expect("test executable"))
        .args(["--exact", test_name, "--nocapture"])
        .env(CHILD, test_name)
        .env(
            "LEAN_CTX_CONVERSATION_SCOPE",
            if enabled { "1" } else { "0" },
        )
        .output()
        .expect("isolated conversation-scope test");
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
    // SAFETY: tests serialize all environment access through test_env_lock(),
    // so no other thread reads or writes the environment concurrently.
    unsafe { std::env::set_var(key, value) };
}

/// Removes `key` from the process environment (test-only).
pub(crate) fn remove_var<K: AsRef<OsStr>>(key: K) {
    // SAFETY: tests serialize all environment access through test_env_lock(),
    // so no other thread reads or writes the environment concurrently.
    unsafe { std::env::remove_var(key) };
}
