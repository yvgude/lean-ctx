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

/// Pins a deterministic shell allowlist while `body` runs, so the `passes_enforced`
/// gate now consulted by [`build_rewrite_compound`] never depends on the
/// developer's `config.toml`. `git/cargo/npm/head/grep/wc/cat/rg/echo/cd/ls` are
/// allowed; `python3` and `kubectl` are deliberately absent so the tricky-sink
/// branch (left raw for the agent shell, #589) is exercised. Serialized via the
/// shared test lock; the env is removed before the caller asserts so a failed
/// assertion can never leak it into another test.
fn with_test_allowlist<T>(body: impl FnOnce() -> T) -> T {
    let _lock = crate::core::data_dir::test_env_lock();
    crate::test_env::set_var(
        "LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE",
        "git,cargo,npm,head,grep,wc,cat,rg,echo,cd,ls",
    );
    let out = body();
    crate::test_env::remove_var("LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE");
    out
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
fn rewrite_skip_reason_tracks_candidate_none_branches() {
    // Every command that `rewrite_candidate` declines must get a stable,
    // human-readable reason for the #520 debug log.
    let binary = "lean-ctx";

    let already = "lean-ctx read x";
    assert!(rewrite_candidate(already, binary).is_none());
    assert_eq!(rewrite_skip_reason(already), "already a lean-ctx command");

    let heredoc = "cat <<EOF\nhi\nEOF";
    assert!(rewrite_candidate(heredoc, binary).is_none());
    assert_eq!(
        rewrite_skip_reason(heredoc),
        "heredoc cannot be rewritten safely"
    );

    let unknown = "echo hello";
    assert!(rewrite_candidate(unknown, binary).is_none());
    assert_eq!(
        rewrite_skip_reason(unknown),
        "not a known read/search/list command"
    );

    // A compound whose sink isn't allowlisted (here `python3 -c`) is left raw for
    // the agent shell — the rewrite must not newly block it (#589). Deterministic
    // via an explicit allowlist that omits python3.
    let tricky = "git log | python3 -c 'print(1)'";
    let (declined, reason) = with_test_allowlist(|| {
        (
            rewrite_candidate(tricky, binary).is_none(),
            rewrite_skip_reason(tricky),
        )
    });
    assert!(declined, "tricky compound sink must not be rewritten");
    assert_eq!(
        reason,
        "compound pipes/chains into a non-allowlisted or interpreter sink — left raw for the agent shell"
    );
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

// --- #561: PowerShell-native cmdlet rewrites ---

#[test]
fn ps_get_content_basic_and_alias() {
    assert_eq!(
        rewrite_file_read_command("Get-Content src/main.rs", "lean-ctx"),
        Some("lean-ctx read src/main.rs".to_string())
    );
    assert_eq!(
        rewrite_file_read_command("gc src/main.rs", "lean-ctx"),
        Some("lean-ctx read src/main.rs".to_string())
    );
    assert_eq!(
        rewrite_file_read_command("Get-Content -Path src/lib.rs", "lean-ctx"),
        Some("lean-ctx read src/lib.rs".to_string())
    );
}

#[test]
fn ps_get_content_head_and_tail() {
    // -TotalCount / -Head / -First == head; -Tail / -Last == tail. Case-insensitive.
    assert_eq!(
        rewrite_file_read_command("Get-Content -TotalCount 20 src/main.rs", "lean-ctx"),
        Some("lean-ctx read src/main.rs -m lines:1-20".to_string())
    );
    assert_eq!(
        rewrite_file_read_command("Get-Content src/main.rs -head 5", "lean-ctx"),
        Some("lean-ctx read src/main.rs -m lines:1-5".to_string())
    );
    assert_eq!(
        rewrite_file_read_command("gc -Tail 10 src/main.rs", "lean-ctx"),
        Some("lean-ctx read src/main.rs -m lines:-10".to_string())
    );
}

#[test]
fn ps_get_content_passthrough() {
    // Unknown flag, both head+tail, outside-project path, and pipelines pass through.
    assert_eq!(
        rewrite_file_read_command("Get-Content -Raw src/main.rs", "lean-ctx"),
        None
    );
    assert_eq!(
        rewrite_file_read_command("Get-Content -TotalCount 5 -Tail 5 src/main.rs", "lean-ctx"),
        None
    );
    assert_eq!(
        rewrite_file_read_command("Get-Content ~/secret.txt", "lean-ctx"),
        None
    );
    assert_eq!(
        rewrite_file_read_command("Get-Content a.txt | Select-String x", "lean-ctx"),
        None
    );
}

#[test]
fn ps_select_string_forms() {
    assert_eq!(
        rewrite_search_command("Select-String TODO src/main.rs", "lean-ctx"),
        Some("lean-ctx grep TODO src/main.rs".to_string())
    );
    assert_eq!(
        rewrite_search_command("sls TODO", "lean-ctx"),
        Some("lean-ctx grep TODO".to_string())
    );
    assert_eq!(
        rewrite_search_command("Select-String -Pattern TODO -Path src/lib.rs", "lean-ctx"),
        Some("lean-ctx grep TODO src/lib.rs".to_string())
    );
    // Unknown flag passes through.
    assert_eq!(
        rewrite_search_command("Select-String -CaseSensitive TODO", "lean-ctx"),
        None
    );
}

#[test]
fn ps_get_childitem_forms() {
    assert_eq!(
        rewrite_dir_list_command("Get-ChildItem", "lean-ctx"),
        Some("lean-ctx ls".to_string())
    );
    assert_eq!(
        rewrite_dir_list_command("gci src", "lean-ctx"),
        Some("lean-ctx ls src".to_string())
    );
    assert_eq!(
        rewrite_dir_list_command("Get-ChildItem -Path src", "lean-ctx"),
        Some("lean-ctx ls src".to_string())
    );
    // -Recurse and other flags pass through.
    assert_eq!(
        rewrite_dir_list_command("Get-ChildItem -Recurse", "lean-ctx"),
        None
    );
}

#[test]
fn ps_cmdlets_route_through_rewrite_candidate() {
    // End-to-end: the dispatcher picks the right rewrite for PowerShell cmdlets.
    assert_eq!(
        rewrite_candidate("Get-Content src/main.rs", "lean-ctx"),
        Some("lean-ctx read src/main.rs".to_string())
    );
    assert_eq!(
        rewrite_candidate("Select-String TODO src/main.rs", "lean-ctx"),
        Some("lean-ctx grep TODO src/main.rs".to_string())
    );
    assert_eq!(
        rewrite_candidate("gci src", "lean-ctx"),
        Some("lean-ctx ls src".to_string())
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
fn rewrite_candidate_leaves_raw_escape_hatch_untouched() {
    // GH #625: the raw escape hatch the SessionStart hint teaches must not be
    // re-wrapped back into a compressing `lean-ctx -c "…"`, or the agent could
    // never actually reach raw bytes. Both spellings already start with
    // `lean-ctx `, so the rewrite hook leaves them as-is (reentrance-safe).
    assert_eq!(
        rewrite_candidate("lean-ctx raw \"git diff\"", "lean-ctx"),
        None
    );
    assert_eq!(
        rewrite_candidate("lean-ctx -c --raw \"git diff\"", "lean-ctx"),
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
fn codex_rewrite_output_uses_native_updated_input_contract() {
    let output = codex_rewrite_output("lean-ctx -c 'git status'");
    let parsed: serde_json::Value = serde_json::from_str(&output).expect("valid hook JSON");

    assert_eq!(parsed["hookSpecificOutput"]["hookEventName"], "PreToolUse");
    assert_eq!(parsed["hookSpecificOutput"]["permissionDecision"], "allow");
    assert_eq!(
        parsed["hookSpecificOutput"]["updatedInput"]["command"],
        "lean-ctx -c 'git status'"
    );
}

#[test]
fn dual_rewrite_output_carries_claude_cursor_and_copilot_fields() {
    // #551: one JSON object must satisfy Claude (hookSpecificOutput.updatedInput),
    // Cursor (updated_input) AND Copilot CLI (top-level permissionDecision +
    // modifiedArgs). Copilot reads modifiedArgs; without it the rewrite no-ops.
    let tool_input = serde_json::json!({ "command": "cat foo.txt", "cwd": "/repo" });
    let out = build_dual_rewrite_output(Some(&tool_input), "lean-ctx read foo.txt");
    let p: serde_json::Value = serde_json::from_str(&out).expect("valid hook JSON");

    // Copilot CLI contract (top-level).
    assert_eq!(p["permissionDecision"], "allow");
    assert_eq!(p["modifiedArgs"]["command"], "lean-ctx read foo.txt");
    assert_eq!(
        p["modifiedArgs"]["cwd"], "/repo",
        "modifiedArgs must preserve the other original args"
    );
    // Claude / CodeBuddy contract.
    assert_eq!(p["hookSpecificOutput"]["permissionDecision"], "allow");
    assert_eq!(
        p["hookSpecificOutput"]["updatedInput"]["command"],
        "lean-ctx read foo.txt"
    );
    // Cursor contract.
    assert_eq!(p["updated_input"]["command"], "lean-ctx read foo.txt");
}

#[test]
fn redirect_output_carries_copilot_modified_args() {
    // #551: the read/grep redirect must also surface modifiedArgs so Copilot CLI
    // swaps in the lean-ctx temp-file path instead of reading the original.
    let tool_input = serde_json::json!({ "path": "src/main.rs" });
    let out = build_redirect_output(Some(&tool_input), "path", "/tmp/x.lctx", None);
    let p: serde_json::Value = serde_json::from_str(&out).expect("valid hook JSON");

    assert_eq!(p["permissionDecision"], "allow");
    assert_eq!(p["modifiedArgs"]["path"], "/tmp/x.lctx");
    assert_eq!(
        p["hookSpecificOutput"]["updatedInput"]["path"],
        "/tmp/x.lctx"
    );
    assert_eq!(p["updated_input"]["path"], "/tmp/x.lctx");
}

#[test]
fn read_redirect_resolves_and_rewrites_cursor_file_path() {
    // The Cursor/Claude Read fix end-to-end: the path arrives in `file_path`, so
    // the handler must (1) resolve it via READ_PATH_FIELDS and (2) echo the SAME
    // field back in updated_input — otherwise Cursor keeps reading the original
    // file instead of the lean-ctx temp file. Before the fix the handler read
    // `path`, found nothing, and every native Read fell back to the editor.
    let tool_input = serde_json::json!({ "file_path": "/repo/src/main.rs" });

    let (field, path) = payload::resolve_path_field(Some(&tool_input), payload::READ_PATH_FIELDS)
        .expect("Cursor file_path must resolve");
    assert_eq!(field, "file_path");
    assert_eq!(path, "/repo/src/main.rs");

    let out = build_redirect_output(Some(&tool_input), field, "/tmp/x.lctx", None);
    let p: serde_json::Value = serde_json::from_str(&out).expect("valid hook JSON");
    // The redirect rewrites file_path (what Cursor reads), not the absent `path`.
    assert_eq!(p["updated_input"]["file_path"], "/tmp/x.lctx");
    assert_eq!(
        p["hookSpecificOutput"]["updatedInput"]["file_path"],
        "/tmp/x.lctx"
    );
    assert_eq!(p["modifiedArgs"]["file_path"], "/tmp/x.lctx");
    assert!(
        p["updated_input"].get("path").is_none(),
        "must not invent a `path` field Cursor never sent"
    );
}

// --- build_rewrite_compound: wrap-whole for gate-clean compounds (#589) ---
// A gate-clean compound is wrapped ENTIRELY in one `lean-ctx -c "…"`: the
// pipe/chain runs inside lean-ctx's profile-free shell (fixes the Windows
// `_lc: command not found`) and only the FINAL output is compressed (fixes the
// left-of-pipe corruption). Tricky sinks (non-allowlisted / interpreter-eval)
// are declined and left raw for the agent's shell (compat-first, no new block).

#[test]
fn compound_rewrite_and_chain() {
    let cmd = "cd src && git status && echo done";
    let result = with_test_allowlist(|| build_rewrite_compound(cmd, "lean-ctx"));
    assert_eq!(result, Some(expect_wrapped(cmd, "lean-ctx")));
}

#[test]
fn compound_rewrite_pipe() {
    let cmd = "git log --oneline | head -5";
    let result = with_test_allowlist(|| build_rewrite_compound(cmd, "lean-ctx"));
    assert_eq!(result, Some(expect_wrapped(cmd, "lean-ctx")));
}

#[test]
fn compound_rewrite_multi_pipe() {
    let cmd = "git log | grep fix | wc -l";
    let result = with_test_allowlist(|| build_rewrite_compound(cmd, "lean-ctx"));
    assert_eq!(result, Some(expect_wrapped(cmd, "lean-ctx")));
}

#[test]
fn compound_rewrite_right_only_rewritable() {
    // `cat` is a FileRead (not `-c`-wrappable alone) but `rg` makes the compound
    // rewritable; the whole thing is gate-clean, so it wraps as one unit.
    let cmd = "cat notes.txt | rg TODO";
    let result = with_test_allowlist(|| build_rewrite_compound(cmd, "lean-ctx"));
    assert_eq!(result, Some(expect_wrapped(cmd, "lean-ctx")));
}

#[test]
fn compound_rewrite_no_rewritable_segment() {
    // Neither `cd` nor `echo` is rewritable → nothing to compress → left as-is.
    let result = with_test_allowlist(|| build_rewrite_compound("cd src && echo done", "lean-ctx"));
    assert_eq!(result, None);
}

#[test]
fn compound_rewrite_multiple_rewritable() {
    let cmd = "git add . && cargo test && npm run lint";
    let result = with_test_allowlist(|| build_rewrite_compound(cmd, "lean-ctx"));
    assert_eq!(result, Some(expect_wrapped(cmd, "lean-ctx")));
}

#[test]
fn compound_rewrite_semicolons() {
    let cmd = "git add .; git commit -m 'fix'";
    let result = with_test_allowlist(|| build_rewrite_compound(cmd, "lean-ctx"));
    assert_eq!(result, Some(expect_wrapped(cmd, "lean-ctx")));
}

#[test]
fn compound_rewrite_or_chain() {
    let cmd = "git pull || echo failed";
    let result = with_test_allowlist(|| build_rewrite_compound(cmd, "lean-ctx"));
    assert_eq!(result, Some(expect_wrapped(cmd, "lean-ctx")));
}

#[test]
fn compound_skips_already_rewritten() {
    // A segment that is already a lean-ctx call must not be nested inside another
    // `lean-ctx -c "…"`; the whole compound is left untouched.
    let result = with_test_allowlist(|| {
        build_rewrite_compound("lean-ctx -c git status && git diff", "lean-ctx")
    });
    assert_eq!(result, None);
}

#[test]
fn compound_tricky_interpreter_sink_left_raw() {
    // Piping into `python3 -c` (not allowlisted) must NOT be wrapped — wrapping
    // would newly subject the interpreter to the gate and block a command the
    // agent's shell ran fine before (#589, compat-first).
    let result = with_test_allowlist(|| {
        build_rewrite_compound("git log | python3 -c 'print(1)'", "lean-ctx")
    });
    assert_eq!(result, None);
}

#[test]
fn compound_tricky_non_allowlisted_sink_left_raw() {
    // `kubectl` is rewritable but deliberately excluded from the defaults; the
    // compound therefore fails the gate and stays raw rather than being blocked.
    let result =
        with_test_allowlist(|| build_rewrite_compound("git log | kubectl apply -f -", "lean-ctx"));
    assert_eq!(result, None);
}

#[test]
fn compound_tricky_chain_sink_left_raw() {
    let result = with_test_allowlist(|| {
        build_rewrite_compound("cargo test && python3 -c 'print(1)'", "lean-ctx")
    });
    assert_eq!(result, None);
}

#[test]
fn single_command_not_compound() {
    let result = with_test_allowlist(|| build_rewrite_compound("git status", "lean-ctx"));
    assert_eq!(result, None);
}

#[test]
fn rewrite_candidate_wraps_clean_compound() {
    // End-to-end: a gate-clean pipeline routes through the compound handler and
    // is wrapped whole (never split, never falling to the single-command path).
    let cmd = "git log | head -5";
    let result = with_test_allowlist(|| rewrite_candidate(cmd, "lean-ctx"));
    assert_eq!(result, Some(expect_wrapped(cmd, "lean-ctx")));
}

#[test]
fn rewrite_candidate_leaves_tricky_compound_untouched() {
    // End-to-end: a tricky compound must not be re-wrapped by the single-command
    // `is_rewritable` fallback after the compound handler declines it (#589).
    let result =
        with_test_allowlist(|| rewrite_candidate("git log | python3 -c 'print(1)'", "lean-ctx"));
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
    // complaint). It must name the reversible compression, the concrete raw CLI
    // (`lean-ctx raw "<command>"`), and forbid the small-chunk anti-pattern; the
    // redundant "prefer `lean-ctx -c`" coaching is gone (compression is automatic).
    let hint = CODEX_SHELL_RECOVERY_HINT;
    assert!(
        hint.contains("lean-ctx raw \"<command>\""),
        "names the raw CLI: {hint}"
    );
    assert!(hint.contains("reversible"), "states reversibility: {hint}");
    assert!(
        hint.contains("small chunks"),
        "forbids chunked re-reads: {hint}"
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
fn is_shell_tool_matches_powershell_variants() {
    // #556: Copilot CLI's `powershell` shell tool was bypassing rewrite on
    // Windows because it was not recognised as a shell tool.
    assert!(is_shell_tool("powershell"));
    assert!(is_shell_tool("PowerShell"));
    assert!(is_shell_tool("pwsh"));
}

#[test]
fn is_shell_tool_matches_existing_shell_names() {
    for name in [
        "Bash",
        "bash",
        "Shell",
        "shell",
        "runInTerminal",
        "run_in_terminal",
        "terminal",
    ] {
        assert!(is_shell_tool(name), "{name} should be a shell tool");
    }
}

#[test]
fn is_shell_tool_rejects_non_shell_tools() {
    for name in ["Read", "read", "Grep", "Glob", "glob", "view", "edit", ""] {
        assert!(!is_shell_tool(name), "{name} must not be a shell tool");
    }
}

#[test]
fn classify_redirect_covers_copilot_view_and_rg() {
    // #562: Copilot CLI's documented `view` (read) and `rg` (search) tool names must
    // be redirected, not passed through uncompressed in shadow/harden mode.
    assert_eq!(classify_redirect("view"), RedirectKind::Read);
    assert_eq!(classify_redirect("rg"), RedirectKind::Grep);
}

#[test]
fn grep_content_mode_only_redirects_explicit_content() {
    // GH #398 hook follow-up: the path-swap redirect is faithful only for
    // `output_mode=content`. files_with_matches/count would surface the temp
    // file itself, and an absent mode is host-dependent (Cursor=content,
    // Claude Code=files_with_matches), so it must not be redirected blindly.
    let mode = |m: &str| serde_json::json!({ "pattern": "x", "output_mode": m });
    assert!(grep_content_mode(Some(&mode("content"))));
    assert!(!grep_content_mode(Some(&mode("files_with_matches"))));
    assert!(!grep_content_mode(Some(&mode("count"))));
    assert!(!grep_content_mode(Some(
        &serde_json::json!({ "pattern": "x" })
    )));
    assert!(!grep_content_mode(None));
}

#[test]
fn classify_redirect_covers_existing_tool_names() {
    for n in ["Read", "read", "read_file"] {
        assert_eq!(classify_redirect(n), RedirectKind::Read, "{n}");
    }
    for n in ["Grep", "grep", "search", "ripgrep"] {
        assert_eq!(classify_redirect(n), RedirectKind::Grep, "{n}");
    }
    for n in ["Glob", "glob"] {
        assert_eq!(classify_redirect(n), RedirectKind::Glob, "{n}");
    }
}

#[test]
fn classify_redirect_passes_through_shell_and_unknown() {
    // Shell tools are rewritten by handle_rewrite, not redirected; edits/writes and
    // unknown names must not be intercepted here.
    for n in [
        "Bash",
        "bash",
        "powershell",
        "pwsh",
        "edit",
        "Write",
        "Unknown",
        "",
    ] {
        assert_eq!(classify_redirect(n), RedirectKind::None, "{n}");
    }
}

#[test]
fn redirect_read_args_pin_full_mode_never_auto() {
    // #1021: a redirected native Read must fetch verbatim content. `auto`
    // degrades large files to a structure MAP (signatures), which is the wrong
    // payload for a Read and silently drops offset/limit. The host windows the
    // faithful full content itself.
    let args = redirect_read_args("/repo/src/main.rs");
    assert_eq!(args, ["read", "/repo/src/main.rs", "-m", "full"]);
    assert!(args.contains(&"full"));
    assert!(!args.contains(&"auto"));
}

#[test]
fn redirect_output_routes_shadow_note_to_additional_context() {
    // #1019: the shadow nudge must ride the model-visible additionalContext side
    // channel, never the temp file the host reads as content (a banner there
    // round-tripped into config.toml on edit). updated_input / modifiedArgs keep
    // pointing only at the faithful temp file, and no banner text leaks anywhere.
    let tool_input = serde_json::json!({ "file_path": "/repo/src/main.rs" });
    let note = "lean-ctx shadow mode: served by ctx_read.";
    let out = build_redirect_output(Some(&tool_input), "file_path", "/tmp/x.lctx", Some(note));
    let p: serde_json::Value = serde_json::from_str(&out).expect("valid hook JSON");

    assert_eq!(p["hookSpecificOutput"]["additionalContext"], note);
    assert_eq!(p["updated_input"]["file_path"], "/tmp/x.lctx");
    assert_eq!(p["modifiedArgs"]["file_path"], "/tmp/x.lctx");
    assert!(
        !out.contains("shadow-mode:"),
        "the legacy in-content banner must never reappear in redirect output"
    );
}

#[test]
fn redirect_output_omits_additional_context_without_shadow() {
    // Outside shadow mode the redirect stays silent — no side-channel note at all.
    let tool_input = serde_json::json!({ "path": "src/main.rs" });
    let out = build_redirect_output(Some(&tool_input), "path", "/tmp/x.lctx", None);
    let p: serde_json::Value = serde_json::from_str(&out).expect("valid hook JSON");
    assert!(
        p["hookSpecificOutput"].get("additionalContext").is_none(),
        "no shadow note => no additionalContext key"
    );
}

#[test]
fn gating_decision_returns_work_result_when_fast() {
    // The normal path: work finishes well within budget, so its decision is used.
    let out = decide_with_timeout(
        std::time::Duration::from_secs(5),
        "FALLBACK".to_string(),
        || "WORK".to_string(),
    );
    assert_eq!(out, "WORK");
}

#[test]
fn gating_decision_fails_open_on_timeout() {
    // #1035: a hung hook must never block the host — past the deadline the
    // pass-through (fallback) decision is returned instead of waiting on `work`.
    let start = std::time::Instant::now();
    let out = decide_with_timeout(
        std::time::Duration::from_millis(50),
        "FALLBACK".to_string(),
        || {
            std::thread::sleep(std::time::Duration::from_secs(3));
            "WORK".to_string()
        },
    );
    assert_eq!(out, "FALLBACK", "a hung hook must fail open to passthrough");
    assert!(
        start.elapsed() < std::time::Duration::from_secs(2),
        "fail-open must not wait for the hung work"
    );
}
