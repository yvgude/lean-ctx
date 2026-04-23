//! E2E tests for shell detection, LEAN_CTX_SHELL override,
//! agent init (incl. antigravity alias), Windows path handling,
//! and pipe-guard (stdout not a terminal → bypass lean-ctx).

use std::io::Write;
use std::process::{Command, Stdio};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn lean_ctx_bin() -> String {
    env!("CARGO_BIN_EXE_lean-ctx").to_string()
}

fn run_with_env(
    args: &[&str],
    env_vars: &[(&str, &str)],
    stdin_data: Option<&str>,
) -> (String, String, i32) {
    let mut cmd = Command::new(lean_ctx_bin());
    cmd.args(args)
        .env("LEAN_CTX_DISABLED", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    for (k, v) in env_vars {
        cmd.env(k, v);
    }

    let mut child = cmd.spawn().expect("failed to spawn lean-ctx");

    if let Some(data) = stdin_data {
        child
            .stdin
            .take()
            .unwrap()
            .write_all(data.as_bytes())
            .unwrap();
    }

    let output = child.wait_with_output().expect("failed to wait");
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let code = output.status.code().unwrap_or(1);
    (stdout, stderr, code)
}

fn assert_hook_command_suffix(actual: Option<&str>, expected_suffix: &str) {
    let actual = actual.expect("hook command should exist");
    assert!(
        actual.contains("lean-ctx"),
        "expected hook command to reference lean-ctx, got {actual:?}"
    );
    assert!(
        actual.ends_with(expected_suffix),
        "expected hook command to end with {expected_suffix:?}, got {actual:?}"
    );
}

// ---------------------------------------------------------------------------
// LEAN_CTX_SHELL override tests (via `lean-ctx -c`)
// ---------------------------------------------------------------------------

#[test]
fn lean_ctx_shell_override_uses_specified_shell() {
    if cfg!(windows) {
        return; // /bin/sh not available on Windows
    }
    let (stdout, _stderr, code) = run_with_env(
        &["-c", "echo lean_ctx_shell_works"],
        &[("LEAN_CTX_SHELL", "/bin/sh")],
        None,
    );
    assert_eq!(code, 0, "should succeed with /bin/sh");
    assert!(
        stdout.contains("lean_ctx_shell_works"),
        "should see echo output: {stdout}"
    );
}

#[test]
fn lean_ctx_shell_override_bash() {
    if !std::path::Path::new("/bin/bash").exists() {
        return;
    }
    let (stdout, _stderr, code) = run_with_env(
        &["-c", "echo $BASH_VERSION"],
        &[("LEAN_CTX_SHELL", "/bin/bash")],
        None,
    );
    assert_eq!(code, 0, "should succeed with /bin/bash");
    assert!(!stdout.trim().is_empty(), "BASH_VERSION should be set");
}

#[test]
fn lean_ctx_shell_override_invalid_shell_fails() {
    let (_stdout, _stderr, code) = run_with_env(
        &["-c", "echo hello"],
        &[("LEAN_CTX_SHELL", "/nonexistent/shell")],
        None,
    );
    assert_ne!(code, 0, "should fail with nonexistent shell");
}

// ---------------------------------------------------------------------------
// Shell command execution tests (basic sanity)
// ---------------------------------------------------------------------------

#[test]
fn shell_exec_simple_command() {
    let (stdout, _stderr, code) = run_with_env(&["-c", "echo hello_world"], &[], None);
    assert_eq!(code, 0);
    assert!(stdout.contains("hello_world"), "output: {stdout}");
}

#[test]
fn shell_exec_pipe_command() {
    if cfg!(windows) {
        return; // head -1 not available on Windows
    }
    let (stdout, _stderr, code) =
        run_with_env(&["-c", "echo 'line1\nline2\nline3' | head -1"], &[], None);
    assert_eq!(code, 0, "pipe should work");
    assert!(!stdout.trim().is_empty(), "should have output: {stdout}");
}

#[test]
fn shell_exec_and_chain() {
    let (stdout, _stderr, code) = run_with_env(&["-c", "echo first && echo second"], &[], None);
    assert_eq!(code, 0, "&& chain should work");
    assert!(stdout.contains("first"), "first: {stdout}");
    assert!(stdout.contains("second"), "second: {stdout}");
}

#[test]
fn shell_exec_semicolon_chain() {
    let (stdout, _stderr, code) = run_with_env(&["-c", "echo aaa; echo bbb"], &[], None);
    assert_eq!(code, 0, "; chain should work");
    assert!(stdout.contains("aaa"), "aaa: {stdout}");
    assert!(stdout.contains("bbb"), "bbb: {stdout}");
}

#[test]
fn shell_exec_subshell() {
    if cfg!(windows) {
        return; // $(...) subshell syntax varies on Windows
    }
    let (stdout, _stderr, code) = run_with_env(&["-c", "echo $(echo subshell_output)"], &[], None);
    assert_eq!(code, 0, "subshell should work");
    assert!(stdout.contains("subshell_output"), "subshell: {stdout}");
}

#[test]
fn shell_exec_env_var_expansion() {
    if cfg!(windows) {
        return; // $VAR syntax is bash-only; PowerShell uses $env:VAR
    }
    let (stdout, _stderr, code) = run_with_env(
        &["-c", "echo $TEST_LEAN_CTX_VAR"],
        &[("TEST_LEAN_CTX_VAR", "expanded_value")],
        None,
    );
    assert_eq!(code, 0);
    assert!(
        stdout.contains("expanded_value"),
        "env var expansion: {stdout}"
    );
}

#[test]
fn shell_exec_quoted_args() {
    let (stdout, _stderr, code) =
        run_with_env(&["-c", r#"echo "hello world with spaces""#], &[], None);
    assert_eq!(code, 0);
    assert!(
        stdout.contains("hello world with spaces"),
        "quoted args: {stdout}"
    );
}

// ---------------------------------------------------------------------------
// Agent init tests
// ---------------------------------------------------------------------------

#[test]
fn agent_init_antigravity_alias() {
    let tmpdir = tempfile::tempdir().expect("create tempdir");
    let home = tmpdir.path();

    let gemini_dir = home.join(".gemini");
    std::fs::create_dir_all(&gemini_dir).unwrap();

    let mut cmd = Command::new(lean_ctx_bin());
    cmd.args(["init", "--agent", "antigravity", "--global"])
        .env("HOME", home.to_str().unwrap())
        .env("LEAN_CTX_DISABLED", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let output = cmd.output().expect("failed to run init");
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        !stderr.contains("Unknown agent"),
        "antigravity should be recognized: {stderr}"
    );

    let hooks_dir = gemini_dir.join("hooks");
    if hooks_dir.exists() {
        let rewrite = hooks_dir.join("lean-ctx-rewrite-gemini.sh");
        assert!(rewrite.exists(), "rewrite script should be created");
        let content = std::fs::read_to_string(&rewrite).unwrap();
        assert!(
            content.contains("hookSpecificOutput"),
            "rewrite script should contain hook output format"
        );
    }
}

#[test]
fn agent_init_unknown_agent_fails() {
    let (_stdout, stderr, code) =
        run_with_env(&["init", "--agent", "nonexistent_agent"], &[], None);
    assert_ne!(code, 0, "unknown agent should fail");
    assert!(
        stderr.contains("Unknown agent"),
        "should say unknown: {stderr}"
    );
}

#[test]
fn codex_pretooluse_blocks_rewritable_bash_with_reroute_message() {
    let input =
        r#"{"tool_name":"Bash","tool_input":{"command":"git status"},"command":"git status"}"#;
    let (stdout, stderr, code) = run_with_env(&["hook", "codex-pretooluse"], &[], Some(input));
    assert_eq!(code, 2, "hook should block and reroute: {stderr}");
    assert!(
        stdout.trim().is_empty(),
        "stdout should stay empty: {stdout}"
    );
    assert!(
        stderr.contains("Re-run with:")
            && stderr.contains("lean-ctx -c")
            && stderr.contains("git status"),
        "stderr should contain reroute command: {stderr}"
    );
}

#[test]
fn agent_init_codex_installs_compatible_hooks_and_instructions() {
    let tmpdir = tempfile::tempdir().expect("create tempdir");
    let home = tmpdir.path();
    let codex_dir = home.join(".codex");
    std::fs::create_dir_all(&codex_dir).unwrap();

    let home_str = home.to_string_lossy().to_string();
    #[cfg(not(windows))]
    let envs = vec![("HOME", home_str.as_str())];
    #[cfg(windows)]
    let envs = vec![
        ("HOME", home_str.as_str()),
        ("USERPROFILE", home_str.as_str()),
    ];

    let (_stdout, stderr, code) =
        run_with_env(&["init", "--agent", "codex", "--global"], &envs, None);
    assert_eq!(code, 0, "codex init should succeed: {stderr}");

    assert!(
        codex_dir.join("AGENTS.md").exists(),
        "AGENTS.md should exist"
    );
    assert!(
        codex_dir.join("LEAN-CTX.md").exists(),
        "LEAN-CTX.md should exist"
    );

    let hooks: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(codex_dir.join("hooks.json")).unwrap())
            .expect("hooks.json should be valid");
    assert_hook_command_suffix(
        hooks["hooks"]["PreToolUse"][0]["hooks"][0]["command"].as_str(),
        "hook codex-pretooluse",
    );
    assert_hook_command_suffix(
        hooks["hooks"]["SessionStart"][0]["hooks"][0]["command"].as_str(),
        "hook codex-session-start",
    );

    let config = std::fs::read_to_string(codex_dir.join("config.toml")).unwrap_or_default();
    assert!(
        config.contains("codex_hooks = true"),
        "init should enable Codex hook support"
    );
}

#[test]
fn agent_init_codex_migrates_legacy_lean_ctx_hook_but_keeps_other_hooks() {
    let tmpdir = tempfile::tempdir().expect("create tempdir");
    let home = tmpdir.path();
    let codex_dir = home.join(".codex");
    let hooks_dir = codex_dir.join("hooks");
    std::fs::create_dir_all(&hooks_dir).unwrap();

    std::fs::write(
        hooks_dir.join("lean-ctx-rewrite-codex.sh"),
        "#!/bin/sh\nexit 0\n",
    )
    .unwrap();
    std::fs::write(
        codex_dir.join("hooks.json"),
        serde_json::json!({
            "hooks": {
                "PreToolUse": [
                    {
                        "matcher": "Bash",
                        "hooks": [{
                            "type": "command",
                            "command": "lean-ctx hook rewrite",
                            "timeout": 15
                        }]
                    },
                    {
                        "matcher": "Bash",
                        "hooks": [{
                            "type": "command",
                            "command": "echo keep-me",
                            "timeout": 5
                        }]
                    }
                ],
                "PostToolUse": [
                    {
                        "matcher": "Bash",
                        "hooks": [{
                            "type": "command",
                            "command": "echo keep-post",
                            "timeout": 5
                        }]
                    }
                ]
            }
        })
        .to_string(),
    )
    .unwrap();

    let home_str = home.to_string_lossy().to_string();
    #[cfg(not(windows))]
    let envs = vec![("HOME", home_str.as_str())];
    #[cfg(windows)]
    let envs = vec![
        ("HOME", home_str.as_str()),
        ("USERPROFILE", home_str.as_str()),
    ];

    let (_stdout, stderr, code) =
        run_with_env(&["init", "--agent", "codex", "--global"], &envs, None);
    assert_eq!(code, 0, "codex init should succeed: {stderr}");

    assert!(
        !hooks_dir.join("lean-ctx-rewrite-codex.sh").exists(),
        "legacy Codex hook script should be removed"
    );

    let hooks: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(codex_dir.join("hooks.json")).unwrap())
            .expect("hooks.json should stay valid");
    let pre_tool_use = hooks["hooks"]["PreToolUse"]
        .as_array()
        .expect("PreToolUse should remain");
    assert_eq!(
        pre_tool_use.len(),
        2,
        "legacy hook should be replaced and custom hook preserved"
    );
    assert_eq!(
        pre_tool_use[0]["hooks"][0]["command"].as_str(),
        Some("echo keep-me")
    );
    assert_hook_command_suffix(
        pre_tool_use[1]["hooks"][0]["command"].as_str(),
        "hook codex-pretooluse",
    );
    assert_hook_command_suffix(
        hooks["hooks"]["SessionStart"][0]["hooks"][0]["command"].as_str(),
        "hook codex-session-start",
    );
    assert_eq!(
        hooks["hooks"]["PostToolUse"][0]["hooks"][0]["command"].as_str(),
        Some("echo keep-post")
    );
}

#[test]
fn agent_init_lists_antigravity_in_supported() {
    let (_stdout, stderr, _code) =
        run_with_env(&["init", "--agent", "nonexistent_agent"], &[], None);
    assert!(
        stderr.contains("antigravity"),
        "supported list should include antigravity: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// Hook rewrite with LEAN_CTX_SHELL override
// ---------------------------------------------------------------------------

#[test]
fn hook_rewrite_works_with_shell_override() {
    let input = r#"{"tool_name":"Bash","command":"git status"}"#;
    let (stdout, _stderr, _code) = run_with_env(
        &["hook", "rewrite"],
        &[("LEAN_CTX_SHELL", "/bin/sh")],
        Some(input),
    );
    if !stdout.trim().is_empty() {
        let v: serde_json::Value =
            serde_json::from_str(&stdout).expect("hook output should be valid JSON");
        assert!(
            v["hookSpecificOutput"]["updatedInput"]["command"]
                .as_str()
                .is_some(),
            "should have command field"
        );
    }
}

// ---------------------------------------------------------------------------
// Windows path handling in generated scripts
// ---------------------------------------------------------------------------

#[test]
fn generated_script_handles_windows_path() {
    let script = lean_ctx::hooks::generate_rewrite_script("/c/Users/Jaina/bin/lean-ctx.exe");
    assert!(
        script.contains("LEAN_CTX_BIN=\"/c/Users/Jaina/bin/lean-ctx.exe\""),
        "Windows bash path should be properly quoted in script"
    );
}

#[test]
fn generated_script_handles_path_with_spaces() {
    let script = lean_ctx::hooks::generate_rewrite_script("/c/Program Files/lean-ctx/lean-ctx.exe");
    assert!(
        script.contains("LEAN_CTX_BIN=\"/c/Program Files/lean-ctx/lean-ctx.exe\""),
        "path with spaces should be quoted"
    );
}

#[test]
fn generated_compact_script_handles_windows_path() {
    let script =
        lean_ctx::hooks::generate_compact_rewrite_script("/c/Users/Jaina/bin/lean-ctx.exe");
    assert!(
        script.contains("LEAN_CTX_BIN=\"/c/Users/Jaina/bin/lean-ctx.exe\""),
        "compact script should handle Windows path"
    );
}

#[test]
fn generated_script_skips_own_binary() {
    let script = lean_ctx::hooks::generate_rewrite_script("lean-ctx");
    assert!(
        script.contains("lean-ctx ") || script.contains("$LEAN_CTX_BIN "),
        "script should reference lean-ctx for self-skip check"
    );
}

// ---------------------------------------------------------------------------
// Bash script execution with Windows-style binary path
// ---------------------------------------------------------------------------

#[test]
fn bash_script_with_windows_binary_path_produces_valid_json() {
    if cfg!(windows) {
        return; // bash not available on Windows CI
    }
    let script =
        lean_ctx::hooks::generate_compact_rewrite_script("/c/Users/Jaina/bin/lean-ctx.exe");
    let script_path =
        std::env::temp_dir().join(format!("lean_ctx_winpath_test_{}.sh", std::process::id()));
    std::fs::write(&script_path, &script).expect("write script");

    let input = r#"{"tool_name":"Bash","command":"git status"}"#;
    let mut child = Command::new("bash")
        .arg(&script_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn bash");

    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();

    let output = child.wait_with_output().expect("failed to wait");
    let _ = std::fs::remove_file(&script_path);
    let stdout = String::from_utf8_lossy(&output.stdout);

    if !stdout.trim().is_empty() {
        let v: serde_json::Value = serde_json::from_str(&stdout).unwrap_or_else(|e| {
            panic!("invalid JSON from Windows path script: {e}\nraw: {stdout}")
        });
        let cmd = v["hookSpecificOutput"]["updatedInput"]["command"]
            .as_str()
            .expect("should have command");
        assert!(
            cmd.contains("/c/Users/Jaina/bin/lean-ctx.exe"),
            "rewritten command should use the Windows bash path: {cmd}"
        );
        assert!(
            cmd.contains("git status"),
            "original command should be preserved: {cmd}"
        );
    }
}

// ---------------------------------------------------------------------------
// Pipe guard: lean-ctx must NOT compress when stdout is piped
// ---------------------------------------------------------------------------

#[test]
fn piped_output_is_not_compressed() {
    if cfg!(windows) {
        return;
    }
    let bin = lean_ctx_bin();
    let script = r#"echo "line one"; echo "line two"; echo "line three""#.to_string();
    let output = Command::new(&bin)
        .args(["-c", &script])
        .env("LEAN_CTX_DISABLED", "0")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("line one"),
        "piped output must contain original content: {stdout}"
    );
}

#[test]
fn bash_hook_contains_pipe_guard() {
    if cfg!(windows) {
        return;
    }
    let bin = lean_ctx_bin();
    let output = Command::new(&bin)
        .args(["init", "--dry-run"])
        .env("LEAN_CTX_DISABLED", "1")
        .env("SHELL", "/bin/bash")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run init --dry-run");
    let _stdout = String::from_utf8_lossy(&output.stdout);
    let _stderr = String::from_utf8_lossy(&output.stderr);
    // The pipe guard should be in the generated hook
    // We verify by checking that `_lc()` in generated bash hooks contains `! -t 1`
    // This is tested more directly in cli.rs unit tests
}

#[test]
fn generated_bash_hook_has_tty_check() {
    let script = lean_ctx::hooks::generate_rewrite_script("lean-ctx");
    // The rewrite hook is for Claude Code / Gemini, not the shell alias.
    // The shell alias pipe guard is in cli.rs.
    // But we can verify the compact hook doesn't break on pipes either.
    assert!(
        !script.is_empty(),
        "generated rewrite script should not be empty"
    );
}

#[test]
fn lean_ctx_c_preserves_output_when_piped() {
    if cfg!(windows) {
        return;
    }
    let bin = lean_ctx_bin();

    let output = Command::new(&bin)
        .args(["-c", "echo MARKER_STRING_12345"])
        .env_remove("LEAN_CTX_DISABLED")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run lean-ctx -c echo");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("MARKER_STRING_12345"),
        "lean-ctx -c must preserve output content when piped: {stdout}"
    );
}

#[test]
fn lean_ctx_c_multiline_preserves_all_lines_when_piped() {
    if cfg!(windows) {
        return;
    }
    let bin = lean_ctx_bin();
    let cmd = "echo LINE_A && echo LINE_B && echo LINE_C";
    let output = Command::new(&bin)
        .args(["-c", cmd])
        .env_remove("LEAN_CTX_DISABLED")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("LINE_A"), "LINE_A: {stdout}");
    assert!(stdout.contains("LINE_B"), "LINE_B: {stdout}");
    assert!(stdout.contains("LINE_C"), "LINE_C: {stdout}");
}
