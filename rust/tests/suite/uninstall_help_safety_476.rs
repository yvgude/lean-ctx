//! #476: `lean-ctx uninstall --help` (and `-h`) must print usage and remove
//! NOTHING. Previously the `uninstall` dispatch arm ignored `--help`, so the
//! flag fell through to a real, irreversible uninstall.
//!
//! These run the real binary as a subprocess inside a fully isolated tempdir so
//! that — even if the guard ever regresses — no developer file is touched.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::{Command, Output};

/// Isolated environment: HOME + all lean-ctx dirs redirected into a tempdir,
/// with a marker file in the data dir whose survival proves "nothing removed".
struct Sandbox {
    _root: tempfile::TempDir,
    home: PathBuf,
    data: PathBuf,
    marker: PathBuf,
}

impl Sandbox {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("tempdir");
        let home = root.path().join("home");
        let data = root.path().join("data");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&data).unwrap();
        let marker = data.join("marker.txt");
        std::fs::write(&marker, "do-not-delete").unwrap();
        Self {
            _root: root,
            home,
            data,
            marker,
        }
    }

    fn run_uninstall(&self, flag: &str) -> Output {
        Command::new(env!("CARGO_BIN_EXE_lean-ctx"))
            .args(["uninstall", flag])
            .env("HOME", &self.home)
            .env("LEAN_CTX_DATA_DIR", &self.data)
            .env("LEAN_CTX_HOOK_CHILD", "1")
            .env("LEAN_CTX_QUIET", "1")
            .output()
            .expect("spawn lean-ctx uninstall")
    }
}

/// Removal log lines that must NEVER appear when only asking for help.
const REMOVAL_MARKERS: &[&str] = &[
    "Processes stopped",
    "Data directory removed",
    "Binary removed",
    "fully removed",
];

fn assert_is_help_not_uninstall(out: &Output, flag: &str, marker: &std::path::Path) {
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        out.status.success(),
        "`uninstall {flag}` should exit 0; got {}\nstdout:\n{stdout}",
        out.status
    );
    assert!(
        stdout.contains("USAGE") && stdout.contains("--dry-run"),
        "`uninstall {flag}` must print usage; stdout was:\n{stdout}"
    );
    for needle in REMOVAL_MARKERS {
        assert!(
            !stdout.contains(needle),
            "`uninstall {flag}` must not perform removal, but emitted {needle:?}:\n{stdout}"
        );
    }
    assert!(
        marker.exists(),
        "`uninstall {flag}` deleted the data dir marker — it ran a real uninstall!"
    );
}

#[test]
fn uninstall_long_help_shows_usage_and_removes_nothing() {
    let sandbox = Sandbox::new();
    let out = sandbox.run_uninstall("--help");
    assert_is_help_not_uninstall(&out, "--help", &sandbox.marker);
}

#[test]
fn uninstall_short_help_shows_usage_and_removes_nothing() {
    let sandbox = Sandbox::new();
    let out = sandbox.run_uninstall("-h");
    assert_is_help_not_uninstall(&out, "-h", &sandbox.marker);
}
