//! Tests for hook handlers. Extracted from `hook_handlers/mod.rs`;
//! `super::*` resolves to the `hook_handlers` module.

use super::*;

fn expect_wrapped(cmd: &str, binary: &str) -> String {
    if cfg!(windows) {
        let escaped = cmd.replace('"', "\\\"");
        format!("{binary} -c \"{escaped}\"")
    } else {
        let shell_escaped = cmd.replace('\'', "'\\''");
        format!("{binary} -c '{shell_escaped}'")
    }
}

#[test]
fn is_rewritable_basic() {
    assert!(is_rewritable("git status"));
    assert!(is_rewritable("cargo test --lib"));
    assert!(is_rewritable("npm run build"));
    assert!(!is_rewritable("echo hello"));
    assert!(!is_rewritable("cd src"));
    assert!(!is_rewritable("cat file.rs"));
}

#[test]
fn file_read_rewrite_cat() {
    let r = rewrite_file_read_command("cat src/main.rs", "lean-ctx");
    assert_eq!(r, Some("lean-ctx read src/main.rs".to_string()));
}

#[test]
fn file_read_rewrite_head_with_n() {
    let r = rewrite_file_read_command("head -n 20 src/main.rs", "lean-ctx");
    assert_eq!(
        r,
        Some("lean-ctx read src/main.rs -m lines:1-20".to_string())
    );
}

#[test]
fn file_read_rewrite_head_short() {
    let r = rewrite_file_read_command("head -50 src/main.rs", "lean-ctx");
    assert_eq!(
        r,
        Some("lean-ctx read src/main.rs -m lines:1-50".to_string())
    );
}

#[test]
fn file_read_rewrite_tail() {
    let r = rewrite_file_read_command("tail -n 10 src/main.rs", "lean-ctx");
    assert_eq!(
        r,
        Some("lean-ctx read src/main.rs -m lines:-10".to_string())
    );
}

#[test]
fn file_read_rewrite_not_git() {
    assert_eq!(rewrite_file_read_command("git status", "lean-ctx"), None);
}

#[test]
fn file_read_skips_home_relative_paths() {
    assert_eq!(
        rewrite_file_read_command("cat ~/Library/Logs/proxy.log", "lean-ctx"),
        None
    );
    assert_eq!(
        rewrite_file_read_command("head -20 ~/.lean-ctx/logs/proxy.stderr.log", "lean-ctx"),
        None
    );
    assert_eq!(
        rewrite_file_read_command("tail -50 ~/some/file.txt", "lean-ctx"),
        None
    );
}

#[test]
fn file_read_skips_system_paths() {
    assert_eq!(
        rewrite_file_read_command("cat /tmp/test.log", "lean-ctx"),
        None
    );
    assert_eq!(
        rewrite_file_read_command("cat /var/log/syslog", "lean-ctx"),
        None
    );
    assert_eq!(
        rewrite_file_read_command("cat /proc/cpuinfo", "lean-ctx"),
        None
    );
}

#[test]
fn file_read_skips_env_var_paths() {
    assert_eq!(
        rewrite_file_read_command("cat $HOME/.bashrc", "lean-ctx"),
        None
    );
}

#[test]
fn file_read_skips_library_and_config_paths() {
    assert_eq!(
        rewrite_file_read_command(
            "cat /Users/user/Library/LaunchAgents/com.leanctx.proxy.plist",
            "lean-ctx"
        ),
        None
    );
    assert_eq!(
        rewrite_file_read_command("cat /home/user/.config/lean-ctx/config.toml", "lean-ctx"),
        None
    );
}

#[test]
fn file_read_skips_pipes_and_redirects() {
    assert_eq!(
        rewrite_file_read_command("cat file.rs | grep fn", "lean-ctx"),
        None
    );
    assert_eq!(
        rewrite_file_read_command("cat file.rs 2>&1", "lean-ctx"),
        None
    );
    assert_eq!(
        rewrite_file_read_command("cat file.rs >> output.log", "lean-ctx"),
        None
    );
    assert_eq!(
        rewrite_file_read_command("cat a.rs && cat b.rs", "lean-ctx"),
        None
    );
    assert_eq!(
        rewrite_file_read_command("cat a.rs; echo done", "lean-ctx"),
        None
    );
}

#[test]
fn file_read_still_rewrites_project_relative_paths() {
    assert_eq!(
        rewrite_file_read_command("cat src/main.rs", "lean-ctx"),
        Some("lean-ctx read src/main.rs".to_string())
    );
    assert_eq!(
        rewrite_file_read_command("cat ./Cargo.toml", "lean-ctx"),
        Some("lean-ctx read ./Cargo.toml".to_string())
    );
    assert_eq!(
        rewrite_file_read_command("head -20 src/lib.rs", "lean-ctx"),
        Some("lean-ctx read src/lib.rs -m lines:1-20".to_string())
    );
}

#[test]
fn is_outside_project_path_tests() {
    assert!(is_outside_project_path("~/foo"));
    assert!(is_outside_project_path("~/.lean-ctx/config.toml"));
    assert!(is_outside_project_path("$HOME/.bashrc"));
    assert!(is_outside_project_path("/tmp/test"));
    assert!(is_outside_project_path("/var/log/syslog"));
    assert!(is_outside_project_path("/proc/cpuinfo"));
    assert!(is_outside_project_path("/Users/x/Library/Logs/foo.log"));
    assert!(is_outside_project_path("/home/x/.config/app/conf"));
    assert!(is_outside_project_path("/root/.lean-ctx/logs/proxy.log"));

    assert!(!is_outside_project_path("src/main.rs"));
    assert!(!is_outside_project_path("./Cargo.toml"));
    assert!(!is_outside_project_path("../sibling/file.rs"));
    assert!(!is_outside_project_path("file.txt"));
}

#[test]
fn parse_head_tail_args_basic() {
    let (n, path) = parse_head_tail_args(&["-n", "20", "file.rs"]);
    assert_eq!(n, Some(20));
    assert_eq!(path, Some("file.rs"));
}

#[test]
fn parse_head_tail_args_combined() {
    let (n, path) = parse_head_tail_args(&["-n20", "file.rs"]);
    assert_eq!(n, Some(20));
    assert_eq!(path, Some("file.rs"));
}

#[test]
fn parse_head_tail_args_short_flag() {
    let (n, path) = parse_head_tail_args(&["-50", "file.rs"]);
    assert_eq!(n, Some(50));
    assert_eq!(path, Some("file.rs"));
}

#[test]
fn should_passthrough_rules_files() {
    assert!(should_passthrough("/home/user/.cursorrules"));
    assert!(should_passthrough("/project/.cursor/rules/test.mdc"));
    assert!(should_passthrough("/home/.cursor/hooks/hooks.json"));
    assert!(should_passthrough("/project/SKILL.md"));
    assert!(should_passthrough("/project/AGENTS.md"));
    assert!(should_passthrough("/project/icon.png"));
    assert!(!should_passthrough("/project/src/main.rs"));
    assert!(!should_passthrough("/project/src/lib.ts"));
}

#[test]
fn wrap_single() {
    let r = wrap_single_command("git status", "lean-ctx");
    assert_eq!(r, expect_wrapped("git status", "lean-ctx"));
}

#[test]
fn wrap_with_quotes() {
    let r = wrap_single_command(r#"curl -H "Auth" https://api.com"#, "lean-ctx");
    assert_eq!(
        r,
        expect_wrapped(r#"curl -H "Auth" https://api.com"#, "lean-ctx")
    );
}

#[test]
fn rewrite_candidate_returns_none_for_existing_lean_ctx_command() {
    assert_eq!(
        rewrite_candidate("lean-ctx -c git status", "lean-ctx"),
        None
    );
}

#[test]
fn rewrite_candidate_wraps_single_command() {
    assert_eq!(
        rewrite_candidate("git status", "lean-ctx"),
        Some(expect_wrapped("git status", "lean-ctx"))
    );
}

#[test]
fn rewrite_candidate_passes_through_heredoc() {
    assert_eq!(
        rewrite_candidate(
            "git commit -m \"$(cat <<'EOF'\nfix: something\nEOF\n)\"",
            "lean-ctx"
        ),
        None
    );
}

#[test]
fn rewrite_candidate_passes_through_heredoc_compound() {
    assert_eq!(
        rewrite_candidate(
            "git add . && git commit -m \"$(cat <<EOF\nfeat: add\nEOF\n)\"",
            "lean-ctx"
        ),
        None
    );
}

#[test]
fn codex_reroute_message_includes_exact_rewritten_command() {
    let message = codex_reroute_message("lean-ctx -c 'git status'");
    assert_eq!(
            message,
            "Command should run via lean-ctx for compact output. Do not retry the original command. Re-run with: lean-ctx -c 'git status'"
        );
}

#[test]
fn compound_rewrite_and_chain() {
    let result = build_rewrite_compound("cd src && git status && echo done", "lean-ctx");
    let w = expect_wrapped("git status", "lean-ctx");
    assert_eq!(result, Some(format!("cd src && {w} && echo done")));
}

#[test]
fn compound_rewrite_pipe() {
    let result = build_rewrite_compound("git log --oneline | head -5", "lean-ctx");
    let w = expect_wrapped("git log --oneline", "lean-ctx");
    assert_eq!(result, Some(format!("{w} | head -5")));
}

#[test]
fn compound_rewrite_no_match() {
    let result = build_rewrite_compound("cd src && echo done", "lean-ctx");
    assert_eq!(result, None);
}

#[test]
fn compound_rewrite_multiple_rewritable() {
    let result = build_rewrite_compound("git add . && cargo test && npm run lint", "lean-ctx");
    let w1 = expect_wrapped("git add .", "lean-ctx");
    let w2 = expect_wrapped("cargo test", "lean-ctx");
    let w3 = expect_wrapped("npm run lint", "lean-ctx");
    assert_eq!(result, Some(format!("{w1} && {w2} && {w3}")));
}

#[test]
fn compound_rewrite_semicolons() {
    let result = build_rewrite_compound("git add .; git commit -m 'fix'", "lean-ctx");
    let w1 = expect_wrapped("git add .", "lean-ctx");
    let w2 = expect_wrapped("git commit -m 'fix'", "lean-ctx");
    assert_eq!(result, Some(format!("{w1} ; {w2}")));
}

#[test]
fn compound_rewrite_or_chain() {
    let result = build_rewrite_compound("git pull || echo failed", "lean-ctx");
    let w = expect_wrapped("git pull", "lean-ctx");
    assert_eq!(result, Some(format!("{w} || echo failed")));
}

#[test]
fn compound_skips_already_rewritten() {
    let result = build_rewrite_compound("lean-ctx -c git status && git diff", "lean-ctx");
    let w = expect_wrapped("git diff", "lean-ctx");
    assert_eq!(result, Some(format!("lean-ctx -c git status && {w}")));
}

#[test]
fn single_command_not_compound() {
    let result = build_rewrite_compound("git status", "lean-ctx");
    assert_eq!(result, None);
}

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
    let input = r#"{"tool_name":"Bash","command":"curl -H \"Authorization: Bearer token\" https://api.com"}"#;
    assert_eq!(
        extract_json_field(input, "command"),
        Some(r#"curl -H "Authorization: Bearer token" https://api.com"#.to_string())
    );
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
