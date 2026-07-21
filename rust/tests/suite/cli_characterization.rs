//! CLI characterization tests — spawn the real binary with each subcommand and
//! assert stable exit codes + output invariants. These freeze the existing
//! behavior so that future refactors of `cli/dispatch` can't silently regress.

use std::{
    fs,
    process::{Command, Output},
};

fn lean_ctx() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_lean-ctx"));
    cmd.env("LEAN_CTX_ACTIVE", "1");
    cmd.env("HOME", "/tmp/lean-ctx-cli-test");
    cmd.env("LEAN_CTX_DISABLED", "1");
    cmd
}

fn run(args: &[&str]) -> Output {
    lean_ctx()
        .args(args)
        .output()
        .expect("failed to spawn lean-ctx binary")
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

fn exit_code(out: &Output) -> i32 {
    out.status.code().unwrap_or(-1)
}

// ═══════════════════════════════════════════════════════════════════
// Basic entry points
// ═══════════════════════════════════════════════════════════════════

#[test]
fn version_flag_exits_zero_and_prints_version() {
    let out = run(&["--version"]);
    assert_eq!(exit_code(&out), 0, "stderr: {}", stderr(&out));
    let s = stdout(&out);
    assert!(s.contains("lean-ctx"), "expected 'lean-ctx' in: {s}");
}

#[test]
fn version_short_flag() {
    let out = run(&["-V"]);
    assert_eq!(exit_code(&out), 0);
    assert!(stdout(&out).contains("lean-ctx"));
}

#[test]
fn help_flag_exits_zero_and_lists_commands() {
    let out = run(&["--help"]);
    assert_eq!(exit_code(&out), 0);
    let s = stdout(&out);
    assert!(
        s.contains("COMMANDS") || s.contains("Usage") || s.contains("lean-ctx"),
        "help must show usage info; got: {}",
        &s[..s.len().min(200)]
    );
}

#[test]
fn help_short_flag() {
    let out = run(&["-h"]);
    assert_eq!(exit_code(&out), 0);
    assert!(!stdout(&out).is_empty());
}

#[test]
fn sessions_delete_removes_saved_session_and_snapshot() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let sessions = tmp.path().join("sessions");
    fs::create_dir_all(&sessions).expect("sessions dir");
    let mut session = lean_ctx::core::session::SessionState::new();
    session.id = "cli-delete-me".to_string();
    fs::write(
        sessions.join("cli-delete-me.json"),
        serde_json::to_string_pretty(&session).unwrap(),
    )
    .unwrap();
    fs::write(sessions.join("cli-delete-me_snapshot.txt"), "snapshot").unwrap();
    fs::write(sessions.join("latest.json"), r#"{"id":"cli-delete-me"}"#).unwrap();

    let out = lean_ctx()
        .env("LEAN_CTX_DATA_DIR", tmp.path())
        .args(["sessions", "delete", "cli-delete-me"])
        .output()
        .expect("failed to spawn lean-ctx binary");

    assert_eq!(exit_code(&out), 0, "stderr: {}", stderr(&out));
    assert!(stdout(&out).contains("Deleted session cli-delete-me."));
    assert!(!sessions.join("cli-delete-me.json").exists());
    assert!(!sessions.join("cli-delete-me_snapshot.txt").exists());
    assert!(!sessions.join("latest.json").exists());
}

#[test]
fn unknown_command_exits_nonzero() {
    let out = run(&["this-command-does-not-exist-xyz"]);
    assert_ne!(exit_code(&out), 0, "unknown command should fail");
    let err = stderr(&out);
    assert!(
        err.contains("unknown command") || err.contains("this-command-does-not-exist"),
        "stderr should mention the unknown command; got: {err}"
    );
}

// ═══════════════════════════════════════════════════════════════════
// Info/read-only subcommands (no side effects)
// ═══════════════════════════════════════════════════════════════════

#[test]
fn gain_exits_zero() {
    let out = run(&["gain"]);
    assert_eq!(exit_code(&out), 0, "stderr: {}", stderr(&out));
}

#[test]
fn gain_json_exits_zero() {
    let out = run(&["gain", "--json"]);
    assert_eq!(exit_code(&out), 0, "stderr: {}", stderr(&out));
    let s = stdout(&out);
    assert!(
        s.starts_with('{') || s.starts_with('[') || s.contains('"'),
        "--json should produce JSON-like output; got: {}",
        &s[..s.len().min(100)]
    );
}

#[test]
fn gain_reset_exits_zero() {
    let out = run(&["gain", "--reset"]);
    assert_eq!(exit_code(&out), 0, "stderr: {}", stderr(&out));
}

#[test]
fn safety_levels_exits_zero() {
    let out = run(&["safety-levels"]);
    assert_eq!(exit_code(&out), 0, "stderr: {}", stderr(&out));
    assert!(
        !stdout(&out).is_empty(),
        "safety-levels should print a table"
    );
}

#[test]
fn cheatsheet_exits_zero() {
    let out = run(&["cheatsheet"]);
    assert_eq!(exit_code(&out), 0, "stderr: {}", stderr(&out));
    assert!(!stdout(&out).is_empty(), "cheatsheet must produce output");
}

#[test]
fn config_show_exits_zero() {
    let out = run(&["config", "show"]);
    assert_eq!(exit_code(&out), 0, "stderr: {}", stderr(&out));
}

#[test]
fn config_no_args_exits_zero() {
    let out = run(&["config"]);
    assert_eq!(exit_code(&out), 0, "stderr: {}", stderr(&out));
}

#[test]
fn stats_exits_zero() {
    let out = run(&["stats"]);
    assert_eq!(exit_code(&out), 0, "stderr: {}", stderr(&out));
}

#[test]
fn cep_exits_zero() {
    let out = run(&["cep"]);
    assert_eq!(exit_code(&out), 0, "stderr: {}", stderr(&out));
}

#[test]
fn wrapped_exits_zero() {
    // `wrapped` was folded into `gain --wrapped`; the standalone command now
    // prints a removal hint and exits non-zero. Characterize the replacement.
    let out = run(&["gain", "--wrapped"]);
    assert_eq!(exit_code(&out), 0, "stderr: {}", stderr(&out));
}

// ═══════════════════════════════════════════════════════════════════
// Doctor
// ═══════════════════════════════════════════════════════════════════

#[test]
fn doctor_runs_without_panic() {
    let out = run(&["doctor"]);
    let code = exit_code(&out);
    assert!(
        code == 0 || code == 1,
        "doctor should exit 0 or 1, got {code}"
    );
    let s = stdout(&out);
    assert!(
        s.contains('✓') || s.contains('✗'),
        "doctor output should contain check marks"
    );
}

#[test]
fn doctor_compact_exits_cleanly() {
    let out = run(&["doctor", "--compact"]);
    let code = exit_code(&out);
    assert!(
        code == 0 || code == 1,
        "doctor --compact should exit 0 or 1, got {code}"
    );
}

// ═══════════════════════════════════════════════════════════════════
// Subcommands that print usage on empty/wrong args
// ═══════════════════════════════════════════════════════════════════

#[test]
fn graph_no_subcommand_builds_without_crash() {
    let out = run(&["graph", "status"]);
    let code = exit_code(&out);
    assert!(
        code == 0 || code == 1,
        "graph status should not crash, got {code}"
    );
}

#[test]
fn graph_unknown_sub_prints_usage() {
    let out = run(&["graph", "nonexistent-sub"]);
    assert_ne!(exit_code(&out), 0);
    assert!(
        stderr(&out).contains("Usage") || stderr(&out).contains("lean-ctx graph"),
        "should print graph usage"
    );
}

#[test]
fn smells_exits_zero() {
    let out = run(&["smells"]);
    let code = exit_code(&out);
    assert!(
        code == 0 || code == 1,
        "smells should not crash, got {code}"
    );
}

#[test]
fn hook_no_args_prints_usage() {
    let out = run(&["hook"]);
    assert_ne!(exit_code(&out), 0);
    let err = stderr(&out);
    assert!(
        err.contains("Usage") || err.contains("lean-ctx hook"),
        "hook without subcommand should show usage"
    );
}

#[test]
fn hook_unknown_prints_usage() {
    let out = run(&["hook", "nonexistent"]);
    assert_ne!(exit_code(&out), 0);
}

// ═══════════════════════════════════════════════════════════════════
// Shell exec (-c)
// ═══════════════════════════════════════════════════════════════════

#[test]
fn exec_echo_exits_zero() {
    let out = run(&["-c", "echo hello"]);
    assert_eq!(exit_code(&out), 0, "stderr: {}", stderr(&out));
    assert!(
        stdout(&out).contains("hello"),
        "exec should pass through command output"
    );
}

#[test]
fn exec_false_exits_nonzero() {
    let out = run(&["-c", "false"]);
    assert_ne!(
        exit_code(&out),
        0,
        "exec of 'false' should propagate exit code"
    );
}

#[test]
fn exec_raw_flag_passes_through() {
    let out = run(&["-c", "--raw", "echo raw_test"]);
    assert_eq!(exit_code(&out), 0, "stderr: {}", stderr(&out));
    assert!(stdout(&out).contains("raw_test"));
}

// ═══════════════════════════════════════════════════════════════════
// Delegator commands (simple dispatch to sub-modules)
// ═══════════════════════════════════════════════════════════════════

#[test]
fn session_no_args_exits_cleanly() {
    let out = run(&["session"]);
    let code = exit_code(&out);
    assert!(
        code == 0 || code == 1,
        "session should not crash, got {code}"
    );
}

#[test]
fn session_new_alias_resets_session() {
    let out = run(&["session", "new"]);
    assert_eq!(exit_code(&out), 0, "stderr: {}", stderr(&out));
}

#[test]
fn knowledge_no_args_exits_cleanly() {
    let out = run(&["knowledge"]);
    let code = exit_code(&out);
    assert!(
        code == 0 || code == 1,
        "knowledge should not crash, got {code}"
    );
}

#[test]
fn overview_exits_cleanly() {
    let out = run(&["overview"]);
    let code = exit_code(&out);
    assert!(
        code == 0 || code == 1,
        "overview should not crash, got {code}"
    );
}

#[test]
fn compress_no_args_exits_cleanly() {
    let out = run(&["compress"]);
    let code = exit_code(&out);
    assert!(
        code == 0 || code == 1,
        "compress should not crash, got {code}"
    );
}

#[test]
fn heatmap_exits_cleanly() {
    let out = run(&["heatmap"]);
    let code = exit_code(&out);
    assert!(
        code == 0 || code == 1,
        "heatmap should not crash, got {code}"
    );
}

#[test]
fn terse_exits_cleanly() {
    let out = run(&["terse"]);
    let code = exit_code(&out);
    assert!(code == 0 || code == 1, "terse should not crash, got {code}");
}

#[test]
fn slow_log_exits_cleanly() {
    let out = run(&["slow-log"]);
    let code = exit_code(&out);
    assert!(
        code == 0 || code == 1,
        "slow-log should not crash, got {code}"
    );
}

#[test]
fn bypass_no_args_prints_usage() {
    let out = run(&["bypass"]);
    assert_ne!(exit_code(&out), 0);
    let err = stderr(&out);
    assert!(
        err.contains("Usage") || err.contains("raw"),
        "bypass alias without args should show usage"
    );
}

#[test]
fn raw_no_args_prints_usage() {
    // `raw` is the primary name for the former `bypass` subcommand (finding 5).
    let out = run(&["raw"]);
    assert_ne!(exit_code(&out), 0);
    let err = stderr(&out);
    assert!(
        err.contains("Usage") && err.contains("raw"),
        "raw without args should show usage: {err}"
    );
}

#[test]
fn audit_exits_zero() {
    let out = run(&["audit"]);
    assert_eq!(exit_code(&out), 0, "stderr: {}", stderr(&out));
    assert!(!stdout(&out).is_empty(), "audit should produce output");
}

// ═══════════════════════════════════════════════════════════════════
// Token report
// ═══════════════════════════════════════════════════════════════════

#[test]
fn token_report_no_args_exits_cleanly() {
    let out = run(&["token-report"]);
    let code = exit_code(&out);
    assert!(
        code == 0 || code == 1,
        "token-report should not crash, got {code}"
    );
}

// ═══════════════════════════════════════════════════════════════════
// Proxy (read-only status check)
// ═══════════════════════════════════════════════════════════════════

#[test]
fn proxy_status_exits_cleanly() {
    let out = run(&["proxy", "status"]);
    let code = exit_code(&out);
    assert!(
        code == 0 || code == 1,
        "proxy status should not crash, got {code}"
    );
}

// ═══════════════════════════════════════════════════════════════════
// Daemon (read-only status check)
// ═══════════════════════════════════════════════════════════════════

#[test]
fn daemon_status_exits_cleanly() {
    let out = run(&["daemon", "status"]);
    let code = exit_code(&out);
    assert!(
        code == 0 || code == 1,
        "daemon status should not crash, got {code}"
    );
}

// ═══════════════════════════════════════════════════════════════════
// Uninstall (dry-run only — safe)
// ═══════════════════════════════════════════════════════════════════

#[test]
fn uninstall_dry_run_exits_zero() {
    let out = run(&["uninstall", "--dry-run"]);
    assert_eq!(exit_code(&out), 0, "stderr: {}", stderr(&out));
    let s = stdout(&out);
    assert!(
        s.contains("dry") || s.contains("Would") || s.contains("lean-ctx") || !s.is_empty(),
        "uninstall --dry-run should describe what it would do"
    );
}
