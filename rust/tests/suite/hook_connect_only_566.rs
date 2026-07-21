//! #566: shadow-mode hook children use *connect-only* daemon routing. They reuse
//! an already-running daemon for full detector parity (loop detection, bounce
//! tracker, adaptive thresholds all fire on the daemon's long-lived state via
//! `/v1/tools/call` → `call_tool_guarded`), but they MUST NEVER auto-start one:
//! a hook fires once per intercepted read/grep as a fresh process, so
//! auto-starting from there would spawn daemons uncontrollably (the #453 class
//! of bug). With no live daemon the read falls back to the enriched standalone
//! path.
//!
//! This guards the *safety* half of that invariant end-to-end on the shipped
//! binary — the routing-through-a-live-daemon half is covered by the daemon's own
//! `call_tool_guarded` tests. Isolation strategy (mirrors `cli_anti_inflation`):
//! the daemon socket + PID file live under `dirs::data_local_dir()` (HOME-derived
//! on every OS, see `ipc::unix`/`ipc::windows` and `daemon::data_dir`), so an
//! empty temp `HOME`/XDG guarantees no daemon is listening there. We then assert
//! the hook-child read both succeeds (standalone fallback) and leaves no
//! `daemon.pid` anywhere beneath that HOME (proving nothing was auto-started).

use std::path::{Path, PathBuf};
use std::process::Command;

/// Recursively collect every `daemon.pid` beneath `root`. A recursive walk keeps
/// the assertion OS-agnostic (macOS resolves the data dir under
/// `Library/Application Support`, Linux under `XDG_DATA_HOME`) and future-proof
/// against data-dir relocations.
fn find_daemon_pids(root: &Path) -> Vec<PathBuf> {
    let mut hits = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.file_name().is_some_and(|n| n == "daemon.pid") {
                hits.push(path);
            }
        }
    }
    hits
}

#[test]
fn hook_child_read_never_autostarts_daemon() {
    let bin = env!("CARGO_BIN_EXE_lean-ctx");
    let dir = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();

    let file = dir.path().join("sample.rs");
    std::fs::write(&file, "fn main() {\n    println!(\"hi\");\n}\n").unwrap();

    let out = Command::new(bin)
        .args(["read", file.to_str().unwrap(), "--mode", "auto"])
        .env("LEAN_CTX_HOOK_CHILD", "1")
        .env("HOME", home.path())
        .env("XDG_DATA_HOME", home.path().join("share"))
        .env("XDG_CONFIG_HOME", home.path().join("config"))
        .env("LEAN_CTX_DATA_DIR", data.path())
        .output()
        .expect("run lean-ctx read");

    assert!(
        out.status.success(),
        "hook-child read failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Availability: the standalone fallback still renders the file body even
    // though no daemon is reachable.
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("println!"),
        "standalone fallback dropped file content:\n{stdout}"
    );

    // Safety invariant: the hook child auto-started no daemon.
    let pids = find_daemon_pids(home.path());
    assert!(
        pids.is_empty(),
        "hook child auto-started a daemon (pid file(s): {pids:?})"
    );
}
