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

// The environment mutation helpers sit next to the lock they assert, in
// `lean-ctx-fs` (enabled here by its `test-support` feature).
pub(crate) use lean_ctx_fs::env_lock::{remove_var, set_var};

/// Sets `key` until dropped, then restores its previous value — also when
/// the test panics. Declare it after the `test_env_lock()` guard so it is
/// dropped while the lock is still held.
pub(crate) struct VarGuard {
    key: std::ffi::OsString,
    previous: Option<std::ffi::OsString>,
}

impl VarGuard {
    pub(crate) fn set<K: AsRef<OsStr>, V: AsRef<OsStr>>(key: K, value: V) -> Self {
        let previous = std::env::var_os(&key);
        set_var(&key, value);
        Self {
            key: key.as_ref().to_owned(),
            previous,
        }
    }
}

impl Drop for VarGuard {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(previous) => set_var(&self.key, previous),
            None => remove_var(&self.key),
        }
    }
}

/// Sets `key` until dropped, then restores its previous value, holding
/// `test_env_lock()` itself for its whole lifetime (unlike [`VarGuard`],
/// which relies on a lock the caller already holds).
pub(crate) struct EnvVarGuard {
    key: &'static str,
    previous: Option<std::ffi::OsString>,
    // Held from before the snapshot until after the restore below: the
    // struct's own `Drop` runs before this field is released.
    _lock: crate::core::data_dir::TestEnvGuard,
}

impl EnvVarGuard {
    pub(crate) fn set(key: &'static str, value: &str) -> Self {
        let lock = crate::core::data_dir::test_env_lock();
        let previous = std::env::var_os(key);
        set_var(key, value);
        Self {
            key,
            previous,
            _lock: lock,
        }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        if let Some(previous) = &self.previous {
            set_var(self.key, previous);
        } else {
            remove_var(self.key);
        }
    }
}

/// The process working directory set to a test's directory until dropped,
/// then restored — also when the test panics. It holds `test_env_lock()` for
/// its whole lifetime: the working directory is process-wide state like the
/// environment, and code a locked test runs reads it (a policy is verified
/// for the working directory outside a request). A private mutex serialized
/// only the tests that shared it: a `policy.unverifiable` admission read the
/// invalid pack another test had entered (25.10.).
pub(crate) struct CurrentDirGuard {
    previous: std::path::PathBuf,
    _lock: crate::core::data_dir::TestEnvGuard,
}

impl CurrentDirGuard {
    pub(crate) fn enter(directory: &std::path::Path) -> Self {
        let lock = crate::core::data_dir::test_env_lock();
        let previous = std::env::current_dir().expect("read the current directory");
        std::env::set_current_dir(directory).expect("enter the test directory");
        Self {
            previous,
            _lock: lock,
        }
    }
}

impl Drop for CurrentDirGuard {
    fn drop(&mut self) {
        // Restored before the lock field is released.
        let _ = std::env::set_current_dir(&self.previous);
    }
}
