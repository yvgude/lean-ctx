use super::*;

#[test]
fn extract_field_works() {
    let input = r#"{"tool_name":"Bash","command":"git status"}"#;
    assert_eq!(
        extract_json_field(input, "tool_name"),
        Some("Bash".to_string())
    );
    assert_eq!(
        extract_json_field(input, "command"),
        Some("git status".to_string())
    );
}

#[test]
fn extract_field_with_spaces_after_colon() {
    let input = r#"{"tool_name": "Bash", "tool_input": {"command": "git status"}}"#;
    assert_eq!(
        extract_json_field(input, "tool_name"),
        Some("Bash".to_string())
    );
    assert_eq!(
        extract_json_field(input, "command"),
        Some("git status".to_string())
    );
}

#[test]
fn extract_field_pretty_printed() {
    let input =
        "{\n  \"tool_name\": \"Bash\",\n  \"tool_input\": {\n    \"command\": \"npm test\"\n  }\n}";
    assert_eq!(
        extract_json_field(input, "tool_name"),
        Some("Bash".to_string())
    );
    assert_eq!(
        extract_json_field(input, "command"),
        Some("npm test".to_string())
    );
}

#[test]
fn extract_field_handles_escaped_quotes() {
    let input = r#"{"tool_name":"Bash","command":"grep -r \"TODO\" src/"}"#;
    assert_eq!(
        extract_json_field(input, "command"),
        Some(r#"grep -r "TODO" src/"#.to_string())
    );
}

#[test]
fn extract_field_handles_escaped_backslash() {
    let input = r#"{"tool_name":"Bash","command":"echo \\\"hello\\\""}"#;
    assert_eq!(
        extract_json_field(input, "command"),
        Some(r#"echo \"hello\""#.to_string())
    );
}

#[test]
fn extract_field_handles_complex_curl() {
    let input = r#"{"tool_name":"Bash","command":"curl -H \"Authorization: Bearer [REDACTED:Authorization header] https://api.com"}"#;
    assert_eq!(
        extract_json_field(input, "command"),
        Some(
            r#"curl -H "Authorization: Bearer [REDACTED:Authorization header] https://api.com"#
                .to_string()
        )
    );
}

#[test]
fn extract_field_decodes_json_newlines() {
    let input = r#"{"tool_name":"Bash","command":"git add .\ngit commit -m \"done\""}"#;
    assert_eq!(
        extract_json_field(input, "command"),
        Some("git add .\ngit commit -m \"done\"".to_string())
    );
}

#[test]
fn extract_field_decodes_json_tab_and_cr() {
    let input = r#"{"command":"echo\t\"hello\"\r\n"}"#;
    assert_eq!(
        extract_json_field(input, "command"),
        Some("echo\t\"hello\"\r\n".to_string())
    );
}

#[test]
fn extract_field_preserves_escaped_backslash_before_n() {
    let input = r#"{"command":"echo \\n"}"#;
    assert_eq!(
        extract_json_field(input, "command"),
        Some("echo \\n".to_string())
    );
}

#[test]
fn unescape_json_string_roundtrips() {
    assert_eq!(super::unescape_json_string(r"a\nb"), "a\nb");
    assert_eq!(super::unescape_json_string(r"a\tb"), "a\tb");
    assert_eq!(super::unescape_json_string(r"a\\b"), "a\\b");
    assert_eq!(super::unescape_json_string(r#"a\"b"#), "a\"b");
    assert_eq!(super::unescape_json_string(r"a\/b"), "a/b");
    assert_eq!(super::unescape_json_string(r"a\r\nb"), "a\r\nb");
    assert_eq!(super::unescape_json_string(r"\\n"), "\\n");
    assert_eq!(super::unescape_json_string("plain"), "plain");
}

#[test]
fn to_bash_compatible_path_windows_drive() {
    let p = crate::hooks::to_bash_compatible_path(r"E:\packages\lean-ctx.exe");
    assert_eq!(p, "/e/packages/lean-ctx.exe");
}

#[test]
fn to_bash_compatible_path_backslashes() {
    let p = crate::hooks::to_bash_compatible_path(r"C:\Users\test\bin\lean-ctx.exe");
    assert_eq!(p, "/c/Users/test/bin/lean-ctx.exe");
}

#[test]
fn to_bash_compatible_path_unix_unchanged() {
    let p = crate::hooks::to_bash_compatible_path("/usr/local/bin/lean-ctx");
    assert_eq!(p, "/usr/local/bin/lean-ctx");
}

#[test]
fn to_bash_compatible_path_msys2_unchanged() {
    let p = crate::hooks::to_bash_compatible_path("/e/packages/lean-ctx.exe");
    assert_eq!(p, "/e/packages/lean-ctx.exe");
}

#[test]
fn resolve_binary_is_native_not_msys() {
    // #518: hook handlers spawn the binary (CreateProcess) and embed it into
    // rewritten commands; both require the native path, never the MSYS `/c/...`
    // form (unrunnable by PowerShell/cmd and invalid for CreateProcess).
    assert_eq!(
        resolve_binary(),
        crate::core::portable_binary::resolve_portable_binary()
    );
}

#[test]
fn rewrite_preserves_native_windows_binary_path() {
    // #518: a Windows native binary path must survive into the rewritten
    // command verbatim — no `/c/...` MSYS rewrite, which PowerShell/cmd
    // cannot execute.
    let win_binary = "C:/Users/Dawid/.cargo/bin/lean-ctx.exe";
    let rewritten =
        rewrite_candidate("git status", win_binary).expect("git status is a rewrite candidate");
    assert!(rewritten.contains(win_binary), "rewritten: {rewritten}");
    assert!(
        !rewritten.contains("/c/"),
        "must not emit MSYS path: {rewritten}"
    );
}

#[test]
fn wrap_command_with_bash_path() {
    let binary = crate::hooks::to_bash_compatible_path(r"E:\packages\lean-ctx.exe");
    let result = wrap_single_command("git status", &binary);
    assert!(
        !result.contains('\\'),
        "wrapped command must not contain backslashes, got: {result}"
    );
    assert!(
        result.starts_with("/e/packages/lean-ctx.exe"),
        "must use bash-compatible path, got: {result}"
    );
}

#[test]
fn wrap_single_command_em_dash() {
    let r = wrap_single_command("gh --comment \"closing — see #407\"", "lean-ctx");
    assert_eq!(
        r,
        expect_wrapped("gh --comment \"closing — see #407\"", "lean-ctx")
    );
}

#[test]
fn wrap_single_command_dollar_sign() {
    let r = wrap_single_command("echo $HOME", "lean-ctx");
    assert_eq!(r, expect_wrapped("echo $HOME", "lean-ctx"));
}

#[test]
fn wrap_single_command_backticks() {
    let r = wrap_single_command("echo `date`", "lean-ctx");
    assert_eq!(r, expect_wrapped("echo `date`", "lean-ctx"));
}

#[test]
fn wrap_single_command_nested_single_quotes() {
    let r = wrap_single_command("echo 'hello world'", "lean-ctx");
    assert_eq!(r, expect_wrapped("echo 'hello world'", "lean-ctx"));
}

#[test]
fn wrap_single_command_exclamation_mark() {
    let r = wrap_single_command("echo hello!", "lean-ctx");
    assert_eq!(r, expect_wrapped("echo hello!", "lean-ctx"));
}

#[test]
fn wrap_single_command_find_with_many_excludes() {
    let cmd = "find . -not -path ./node_modules -not -path ./.git -not -path ./dist";
    let r = wrap_single_command(cmd, "lean-ctx");
    assert_eq!(r, expect_wrapped(cmd, "lean-ctx"));
}

#[test]
fn session_start_uses_codex_additional_context_channel() {
    // #368: SessionStart guidance must travel via the documented JSON
    // `hookSpecificOutput.additionalContext` channel, not plain stdout text.
    let json = session_start_additional_context_json("prefer lean-ctx -c");
    let v: serde_json::Value = serde_json::from_str(&json).expect("valid JSON on stdout");
    assert_eq!(
        v["hookSpecificOutput"]["hookEventName"]
            .as_str()
            .unwrap_or_default(),
        "SessionStart"
    );
    assert_eq!(
        v["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap_or_default(),
        "prefer lean-ctx -c"
    );
}

#[test]
fn codex_session_start_hint_teaches_the_raw_escape_hatch() {
    // GH #625: the PreToolUse hook already auto-compresses every Bash command, so
    // the SessionStart hint's job is to teach the *raw* escape — otherwise agents
    // re-read the compressed view in small chunks (the shell-side "too compressed"
    // complaint). It must state that compressed output is not exact evidence, name
    // the concrete raw CLI (`lean-ctx raw "<exact command>"`), and forbid the
    // chunked-read anti-pattern; the redundant "prefer `lean-ctx -c`" coaching is
    // gone (compression is automatic).
    let hint = CODEX_SHELL_RECOVERY_HINT;
    assert!(
        hint.contains("lean-ctx raw \"<exact command>\""),
        "names the raw CLI: {hint}"
    );
    assert!(
        hint.contains("is not exact evidence"),
        "states compressed output is not exact evidence: {hint}"
    );
    assert!(
        hint.contains("chunked reads"),
        "forbids chunk-based reconstruction: {hint}"
    );
    assert!(
        !hint.contains("prefer `lean-ctx -c`"),
        "drops the redundant prefer-c coaching (auto-rewrite handles it): {hint}"
    );
    // The hint must survive the additionalContext JSON channel byte-for-byte.
    let json = session_start_additional_context_json(hint);
    let v: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
    assert_eq!(
        v["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap_or_default(),
        hint
    );
}

#[test]
fn warm_daemon_cache_tolerates_missing_socket() {
    // warm_daemon_cache must never panic, even when no daemon is running.
    // The function checks is_listening() first, so with no socket file on
    // disk it returns immediately.
    warm_daemon_cache("/nonexistent/file.rs");
}

#[test]
fn shell_tokenize_preserves_backslashes_in_double_quotes() {
    let result = shell_tokenize(r#"cat "C:\Users\me\file.txt""#);
    assert_eq!(result, vec!["cat", r"C:\Users\me\file.txt"]);
}

#[test]
fn shell_tokenize_escapes_posix_specials_in_double_quotes() {
    let result = shell_tokenize(r#"echo "hello\"world""#);
    assert_eq!(result, vec!["echo", r#"hello"world"#]);

    let result = shell_tokenize(r#"echo "a\\b""#);
    assert_eq!(result, vec!["echo", r"a\b"]);
}

#[test]
fn shell_tokenize_unquoted_backslash_still_escapes() {
    let result = shell_tokenize(r"echo hello\ world");
    assert_eq!(result, vec!["echo", "hello world"]);
}

#[test]
fn codex_session_briefing_contains_full_tool_guidance() {
    let briefing = super::codex::codex_session_briefing();
    assert!(
        briefing.contains("ctx_compose"),
        "briefing must mention ctx_compose: {briefing}"
    );
    assert!(
        briefing.contains("ctx_shell"),
        "briefing must mention ctx_shell: {briefing}"
    );
    assert!(
        briefing.contains("ctx_session"),
        "briefing must mention ctx_session: {briefing}"
    );
    assert!(
        briefing.contains("ctx_knowledge"),
        "briefing must mention ctx_knowledge: {briefing}"
    );
    assert!(
        briefing.contains("CHECKPOINT"),
        "briefing must include checkpoint rule: {briefing}"
    );
    assert!(
        briefing.contains("NEVER"),
        "briefing must include NEVER rule: {briefing}"
    );
    let json = session_start_additional_context_json(&briefing);
    let v: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
    let ctx = v["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap_or_default();
    assert!(ctx.contains("lean-ctx SESSION BRIEFING"));
}
