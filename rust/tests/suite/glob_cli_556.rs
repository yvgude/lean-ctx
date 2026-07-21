//! Integration test for `lean-ctx glob` (#556).
//!
//! The shadow-mode Glob redirect warms the cache by spawning `lean-ctx glob`,
//! so the CLI subcommand must exist and resolve files through the shared
//! `ctx_glob` core. Before #556 there was no `glob` subcommand at all.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Build an isolated project dir with a marker so the walk-root guard accepts
/// it, and an isolated `HOME` (the parent) so no real user state is touched.
fn project_in(home: &Path) -> PathBuf {
    let proj = home.join("proj");
    fs::create_dir_all(&proj).unwrap();
    // `Cargo.toml` is a project marker -> `is_safe_scan_root` accepts the root.
    fs::write(proj.join("Cargo.toml"), "[package]\nname = \"t\"\n").unwrap();
    proj
}

fn run_glob(args: &[&str], home: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_lean-ctx"))
        .args(args)
        .env("HOME", home)
        // Hook-child flag keeps the daemon from auto-starting; the command falls
        // back to the in-process ctx_glob path, which is what we want to assert.
        .env("LEAN_CTX_HOOK_CHILD", "1")
        .output()
        .expect("failed to spawn lean-ctx binary")
}

#[test]
fn glob_lists_matching_files_only() {
    let tmp = tempfile::tempdir().unwrap();
    let proj = project_in(tmp.path());
    fs::write(proj.join("alpha.rs"), "fn a() {}\n").unwrap();
    fs::write(proj.join("beta.rs"), "fn b() {}\n").unwrap();
    fs::write(proj.join("notes.txt"), "hello\n").unwrap();

    let out = run_glob(&["glob", "*.rs", proj.to_str().unwrap()], tmp.path());
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert_eq!(
        out.status.code(),
        Some(0),
        "glob should exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("alpha.rs"), "missing alpha.rs in: {stdout}");
    assert!(stdout.contains("beta.rs"), "missing beta.rs in: {stdout}");
    assert!(
        !stdout.contains("notes.txt"),
        "*.rs must not match notes.txt: {stdout}"
    );
}

#[test]
fn glob_without_pattern_exits_nonzero() {
    let tmp = tempfile::tempdir().unwrap();
    let out = run_glob(&["glob"], tmp.path());
    assert_eq!(
        out.status.code(),
        Some(1),
        "missing pattern must exit 1; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn glob_no_match_reports_zero() {
    let tmp = tempfile::tempdir().unwrap();
    let proj = project_in(tmp.path());
    fs::write(proj.join("only.txt"), "x\n").unwrap();

    let out = run_glob(&["glob", "*.rs", proj.to_str().unwrap()], tmp.path());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("0 files matched"),
        "expected zero-match report, got: {stdout}"
    );
}
