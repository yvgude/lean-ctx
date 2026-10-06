// SPDX-License-Identifier: Apache-2.0
//! Hermetic environment for the merged integration-test binary.
//!
//! Unit tests get this from `#[cfg(test)]` inside the library: the data-dir
//! sandbox (GL #512) and the ambient-scope guard (#1801). This binary links the
//! library *without* `cfg(test)`, so on a developer machine every run read and
//! wrote the real `~/.lean-ctx` — stats, metering, telemetry identity — and
//! picked up the live `active_transcript.json` of the agent session running the
//! tests. Its fresh conversation id switched read-cache stubs on, so tests that
//! pass in CI failed locally.
//!
//! The setup has to run before `main`: libtest starts its worker threads
//! before the first test, and changing the environment while other threads
//! read it is undefined behavior. A constructor in the platform's init section
//! runs while the process is still single-threaded — the mechanism the `ctor`
//! crate uses, without the extra dependency.

/// Agent variables that give the process a conversation scope (see
/// `core::conversation::resolve_scope`). CI never sets them.
const AMBIENT_SCOPE_VARS: [&str; 3] = ["CLAUDECODE", "CURSOR_TASK_ID", "LEAN_CTX_SCOPE"];

/// Set when the constructor chose the sandbox dir (no explicit override).
static SANDBOXED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn sandbox_dir() -> std::path::PathBuf {
    std::env::temp_dir().join(format!("lean-ctx-itestdata-{}", std::process::id()))
}

extern "C" fn isolate_environment() {
    // An explicit data dir stays in the caller's hands, as in the unit sandbox.
    let explicit = std::env::var_os("LEAN_CTX_DATA_DIR").is_some_and(|v| !v.is_empty());
    // SAFETY: runs from the init section, before `main` and before any other
    // thread exists, so nothing can read the environment concurrently.
    unsafe {
        if !explicit {
            let dir = sandbox_dir();
            let _ = std::fs::create_dir_all(&dir);
            std::env::set_var("LEAN_CTX_DATA_DIR", dir);
            SANDBOXED.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        for var in AMBIENT_SCOPE_VARS {
            std::env::remove_var(var);
        }
    }
}

#[used]
#[cfg_attr(
    target_vendor = "apple",
    unsafe(link_section = "__DATA,__mod_init_func")
)]
#[cfg_attr(
    any(target_os = "linux", target_os = "android", target_os = "freebsd"),
    unsafe(link_section = ".init_array")
)]
#[cfg_attr(windows, unsafe(link_section = ".CRT$XCU"))]
static ISOLATE_ENVIRONMENT: extern "C" fn() = isolate_environment;

/// Stops the daemon a test started under a sandbox HOME when dropped, also
/// when an assertion panics. The daemon's pid file sits under the platform
/// data dir derived from HOME, not under LEAN_CTX_DATA_DIR.
///
/// Deliberately not `lean-ctx stop`: after stopping the daemon it kills every
/// other non-MCP `lean-ctx` process on the machine — the developer's own
/// daemon and proxy, and other sessions' CLI calls.
pub(crate) struct SandboxDaemon<'a>(pub &'a std::path::Path);

impl Drop for SandboxDaemon<'_> {
    fn drop(&mut self) {
        for pid_file in [
            ".local/share/lean-ctx/daemon.pid",
            "Library/Application Support/lean-ctx/daemon.pid",
        ] {
            let Ok(pid) = std::fs::read_to_string(self.0.join(pid_file)) else {
                continue;
            };
            if let Ok(pid) = pid.trim().parse::<u32>() {
                #[cfg(unix)]
                let _ = std::process::Command::new("kill")
                    .arg(pid.to_string())
                    .status();
                #[cfg(not(unix))]
                let _ = pid;
            }
        }
    }
}

#[test]
fn constructor_isolates_the_process_before_tests_run() {
    let dir = std::env::var_os("LEAN_CTX_DATA_DIR").expect("LEAN_CTX_DATA_DIR is set");
    if SANDBOXED.load(std::sync::atomic::Ordering::Relaxed) {
        assert_eq!(std::path::PathBuf::from(dir), sandbox_dir());
    }
    assert_eq!(
        lean_ctx::core::data_dir::lean_ctx_data_dir().ok(),
        std::env::var_os("LEAN_CTX_DATA_DIR").map(std::path::PathBuf::from),
        "the library resolves the isolated dir, not ~/.lean-ctx",
    );
    for var in AMBIENT_SCOPE_VARS {
        assert!(
            std::env::var_os(var).is_none(),
            "{var} leaked into the test binary"
        );
    }
}
