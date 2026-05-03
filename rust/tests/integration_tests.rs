use std::process::Command;

fn lean_ctx_bin() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_lean-ctx"));
    cmd.current_dir(env!("CARGO_MANIFEST_DIR"));
    cmd.env("LEAN_CTX_ACTIVE", "1");
    cmd
}

#[test]
fn binary_prints_version() {
    let output = lean_ctx_bin()
        .arg("--version")
        .output()
        .expect("failed to run lean-ctx");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("lean-ctx"),
        "version output should contain 'lean-ctx', got: {stdout}"
    );
}

#[test]
fn binary_prints_help() {
    let output = lean_ctx_bin()
        .arg("--help")
        .output()
        .expect("failed to run lean-ctx");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Context Runtime"),
        "help should contain tagline"
    );
    assert!(stdout.contains("lean-ctx"), "help should mention lean-ctx");
}

#[test]
fn binary_read_file() {
    let output = lean_ctx_bin()
        .args(["read", "Cargo.toml", "-m", "signatures"])
        .output()
        .expect("failed to run lean-ctx");
    assert!(output.status.success(), "read should succeed");
}

#[test]
fn binary_config_shows_defaults() {
    let output = lean_ctx_bin()
        .arg("config")
        .output()
        .expect("failed to run lean-ctx");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("checkpoint_interval"),
        "config should show checkpoint_interval"
    );
}

#[test]
fn shell_hook_compresses_echo() {
    let output = lean_ctx_bin()
        .args(["-c", "echo", "hello", "world"])
        .output()
        .expect("failed to run lean-ctx -c");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("hello"),
        "shell hook should pass through echo output"
    );
}

#[test]
fn disabled_env_bypasses_compression() {
    let output = Command::new(env!("CARGO_BIN_EXE_lean-ctx"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .env("LEAN_CTX_DISABLED", "1")
        .env("LEAN_CTX_COMPRESS", "1")
        .args(["-c", "echo", "passthrough test"])
        .output()
        .expect("failed to run lean-ctx with LEAN_CTX_DISABLED");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("passthrough"),
        "LEAN_CTX_DISABLED should pass output through unmodified"
    );
    assert!(
        !stdout.contains("[lean-ctx:"),
        "LEAN_CTX_DISABLED should not add compression markers"
    );
}

#[test]
fn help_shows_environment_section() {
    let output = lean_ctx_bin()
        .arg("--help")
        .output()
        .expect("failed to run lean-ctx");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("LEAN_CTX_DISABLED"),
        "help should document LEAN_CTX_DISABLED"
    );
    assert!(
        stdout.contains("LEAN_CTX_RAW"),
        "help should document LEAN_CTX_RAW"
    );
}

// ── Pipe Guard Tests ────────────────────────────────────────

#[test]
fn pipe_guard_no_compression_when_stdout_is_piped() {
    if cfg!(windows) {
        return;
    }
    let output = Command::new(env!("CARGO_BIN_EXE_lean-ctx"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args(["-c", "echo hello world"])
        .output()
        .expect("failed to run lean-ctx -c with piped stdout");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout.trim(),
        "hello world",
        "piped stdout must pass through raw output, got: {stdout}"
    );
}

#[test]
fn pipe_guard_force_compress_overrides_pipe_guard() {
    if cfg!(windows) {
        return;
    }
    let output = Command::new(env!("CARGO_BIN_EXE_lean-ctx"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .env("LEAN_CTX_COMPRESS", "1")
        .args(["-c", "echo hello world"])
        .output()
        .expect("failed to run lean-ctx -c with LEAN_CTX_COMPRESS");
    assert!(
        output.status.success(),
        "LEAN_CTX_COMPRESS should not crash even with piped stdout"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("hello"),
        "output should contain the echoed text"
    );
}

#[test]
fn pipe_guard_multiline_output_unchanged_when_piped() {
    if cfg!(windows) {
        return;
    }
    let script = "echo line1; echo line2; echo line3; echo 'result: 42'";
    let output = Command::new(env!("CARGO_BIN_EXE_lean-ctx"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args(["-c", script])
        .output()
        .expect("failed to run lean-ctx -c with multiline output");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("line1"), "must contain line1");
    assert!(stdout.contains("line2"), "must contain line2");
    assert!(stdout.contains("line3"), "must contain line3");
    assert!(
        stdout.contains("result: 42"),
        "must preserve exact output content"
    );
}

#[test]
fn pipe_guard_bash_hook_script_test() {
    if cfg!(windows) {
        return;
    }
    let binary = env!("CARGO_BIN_EXE_lean-ctx");
    let script = format!(
        r#"
_lc() {{
    if [ -n "${{LEAN_CTX_DISABLED:-}}" ] || [ ! -t 1 ]; then
        command "$@"
        return
    fi
    '{binary}' -c "$@"
}}
# Pipe test: _lc echo should bypass lean-ctx when piped
RESULT=$(_lc echo "pipe-guard-test-value")
echo "CAPTURED:$RESULT"
"#
    );
    let output = Command::new("bash")
        .args(["-c", &script])
        .output()
        .expect("failed to run bash pipe guard test");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("CAPTURED:pipe-guard-test-value"),
        "pipe guard must bypass lean-ctx in command substitution, got: {stdout}"
    );
}

#[test]
fn pipe_guard_bash_hook_pipe_to_sh() {
    if cfg!(windows) {
        return;
    }
    let binary = env!("CARGO_BIN_EXE_lean-ctx");
    let script = format!(
        r#"
_lc() {{
    if [ -n "${{LEAN_CTX_DISABLED:-}}" ] || [ ! -t 1 ]; then
        command "$@"
        return
    fi
    '{binary}' -c "$@"
}}
# Simulate curl | sh: echo a script, pipe to sh
_lc echo 'echo INSTALL_SUCCESS' | sh
"#
    );
    let output = Command::new("bash")
        .args(["-c", &script])
        .output()
        .expect("failed to run bash pipe-to-sh test");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("INSTALL_SUCCESS"),
        "piped script must execute correctly through sh, got: {stdout}"
    );
}

#[test]
fn pipe_guard_bash_hook_redirect_to_file() {
    if cfg!(windows) {
        return;
    }
    let binary = env!("CARGO_BIN_EXE_lean-ctx");
    let tmp = std::env::temp_dir().join("lean-ctx-pipe-guard-test.txt");
    let tmp_path = tmp.to_str().unwrap();
    let script = format!(
        r#"
_lc() {{
    if [ -n "${{LEAN_CTX_DISABLED:-}}" ] || [ ! -t 1 ]; then
        command "$@"
        return
    fi
    '{binary}' -c "$@"
}}
_lc echo "redirect-test-value" > {tmp_path}
cat {tmp_path}
rm -f {tmp_path}
"#
    );
    let output = Command::new("bash")
        .args(["-c", &script])
        .output()
        .expect("failed to run bash redirect test");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("redirect-test-value"),
        "redirected output must be raw, got: {stdout}"
    );
}

#[test]
fn pipe_guard_rust_side_defense_in_depth() {
    if cfg!(windows) {
        return;
    }
    let script = "for i in 1 2 3 4 5; do echo \"item_$i: $(date +%s)\"; done";
    let output = Command::new(env!("CARGO_BIN_EXE_lean-ctx"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args(["-c", script])
        .output()
        .expect("failed to run lean-ctx -c");
    let stdout = String::from_utf8_lossy(&output.stdout);
    for i in 1..=5 {
        assert!(
            stdout.contains(&format!("item_{i}:")),
            "Rust-side pipe guard must pass through all lines unchanged (missing item_{i})"
        );
    }
}
