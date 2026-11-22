// SPDX-License-Identifier: Apache-2.0
//! A test that needs a process of its own, inside a merged test binary.
//!
//! Some integration tests need a process nobody else touched: they set the
//! process environment, or they rely on a process-wide latch, `OnceLock` or
//! registry that starts empty. Each used to be a test binary of its own, and
//! every one of those linked the whole Engine (about 130 MB written per build
//! and binary). They are modules of their crate's merged test binary now, and
//! each of their tests starts with
//!
//! ```ignore
//! if !crate::fresh_process::enter() {
//!     return;
//! }
//! ```
//!
//! Under nextest (CI, one process per test) `enter` returns true: the test
//! already runs alone in the process nextest started for it. Under libtest
//! (`cargo test`, a direct run of the binary), where a binary's tests share
//! one process, `enter` runs this binary again for exactly this test with a
//! marker variable, waits for it, fails unless it passed and ran exactly this
//! one test, and returns false; in that child `enter` sees the marker and
//! returns true. Either way the test body runs once, in a fresh process.
//!
//! The test's name is the name libtest and nextest give the test's thread
//! (`<module>::<test>`), so a call cannot name the wrong test.

use std::process::Command;

/// Set in the child to the name of the one test it runs.
const CHILD_MARKER: &str = "LEAN_CTX_TEST_FRESH_PROCESS";

/// True when the calling test should run its body here; false when the body
/// already ran (and passed) in a child process. Call it first in the test,
/// before anything touches process state.
#[must_use]
pub(crate) fn enter() -> bool {
    let current = std::thread::current();
    let test = current
        .name()
        .filter(|name| name.contains("::"))
        .expect("fresh_process::enter runs on the test's own thread, named <module>::<test>");
    if std::env::var_os("NEXTEST_EXECUTION_MODE").is_some_and(|mode| mode == "process-per-test") {
        return true;
    }
    if std::env::var_os(CHILD_MARKER).is_some_and(|marker| marker == test) {
        return true;
    }
    let exe = std::env::current_exe().expect("path of this test binary");
    let output = Command::new(exe)
        .args([test, "--exact", "--test-threads=1", "--nocapture"])
        .env(CHILD_MARKER, test)
        // The child is the process of its own a batched run (LEAN_CTX_TEST_BATCH)
        // asks for: its env-lock tripwire must not fire there.
        .env_remove("LEAN_CTX_TEST_BATCH")
        .output()
        .unwrap_or_else(|e| panic!("{test}: cannot run it in a process of its own: {e}"));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    // Captured by libtest like any test output: shown when the test fails.
    print!("{stdout}");
    eprint!("{stderr}");
    assert!(
        output.status.success(),
        "{test} failed in its own process ({})",
        output.status
    );
    assert!(
        stdout.contains("test result: ok. 1 passed;"),
        "{test}: the run in its own process did not run exactly this test"
    );
    false
}
