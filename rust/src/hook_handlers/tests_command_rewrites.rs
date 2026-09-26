use super::*;

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

/// #1862: the wrap is single-quoted for the Bash tool on every platform, so the
/// calling shell expands nothing — not the detected shell's quoting, which on
/// Windows can be cmd.exe's double quotes.
#[test]
fn wrap_is_posix_single_quoted_whatever_shell_is_detected() {
    assert_eq!(
        wrap_single_command(r#"git log --format="$HOME $(id) `id`" -1"#, "lean-ctx"),
        r#"lean-ctx -c 'git log --format="$HOME $(id) `id`" -1'"#
    );
    assert_eq!(
        wrap_single_command("git log --grep 'it''s'", "lean-ctx"),
        r"lean-ctx -c 'git log --grep '\''it'\'''\''s'\'''"
    );
}

/// #1862: the tokenizer behind the direct rewrites drops the quotes, so it
/// cannot tell `'$HOME'` (literal) from `"$HOME"` (expand). A command with `$`
/// or `` ` `` keeps its own quoting — inside the `-c` wrap, or untouched where
/// the command is never wrapped (`cat`). Every other word is re-quoted in
/// single quotes so the calling shell leaves it alone.
#[test]
fn expansion_chars_keep_the_agents_own_quoting() {
    for cmd in [
        "grep -n '$HOME' README.md",
        r#"grep -n "$HOME" README.md"#,
        "rg 'foo`id`$(id)' src",
    ] {
        assert_eq!(
            rewrite_candidate(cmd, "lean-ctx"),
            Some(expect_wrapped(cmd, "lean-ctx")),
            "{cmd}"
        );
    }
    assert_eq!(rewrite_candidate("cat 'a$b.txt'", "lean-ctx"), None);
    assert_eq!(
        rewrite_candidate("cat a.md && cat 'b$c.md'", "lean-ctx"),
        Some("lean-ctx read a.md && cat 'b$c.md'".to_owned())
    );
    assert_eq!(
        rewrite_candidate(r#"cat "it's here.txt""#, "lean-ctx"),
        Some(r"lean-ctx read 'it'\''s here.txt'".to_owned())
    );
}

/// #1865: a lean-ctx binary under a path with a space (Windows: `C:\Program
/// Files\...`) must be quoted in the direct rewrites too, or the calling shell
/// splits it and fails with exit 127.
#[test]
fn direct_rewrite_quotes_binary_path_with_space() {
    let bin = "/opt/Program Files/lean-ctx";
    assert_eq!(
        rewrite_candidate("cat notes.md", bin),
        Some("'/opt/Program Files/lean-ctx' read notes.md".to_owned())
    );
    assert_eq!(
        rewrite_candidate("cat a.md && cat b.md", bin),
        Some(
            "'/opt/Program Files/lean-ctx' read a.md && '/opt/Program Files/lean-ctx' read b.md"
                .to_owned()
        )
    );
    // The quoted form is recognised as an existing lean-ctx call (no re-wrap).
    assert!(super::file_rewrite::is_leanctx_call(
        "'/opt/Program Files/lean-ctx' read notes.md",
        bin
    ));
    assert_eq!(
        rewrite_candidate("'/opt/Program Files/lean-ctx' read notes.md", bin),
        None
    );
}

#[test]
fn shell_quote_round_trips_through_shell_tokenize() {
    for word in [
        "$HOME",
        "`id`",
        "$(id)",
        "it's",
        r#"say "hi""#,
        r"C:\Users\x",
        "a b",
    ] {
        assert_eq!(
            shell_tokenize(&shell_quote(word)),
            vec![word.to_owned()],
            "{word}"
        );
    }
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

/// An unchanged PreToolUse call is allowed by a successful hook with no stdout.
#[test]
fn codex_allow_output_is_empty() {
    let output = codex_allow_output();
    assert!(output.is_empty());
}

/// #809: codex_deny_output with heavily escaped content is valid JSON.
#[test]
fn codex_deny_output_with_nested_quotes_is_valid_json() {
    let complex_cmd = r#"lean-ctx raw 'cat file; printf "
---
"; sed -n "55,145p" file2'"#;
    let output = codex_deny_output(complex_cmd);
    let parsed: serde_json::Value =
        serde_json::from_str(&output).expect("codex_deny_output must be valid JSON");
    assert_eq!(parsed["hookSpecificOutput"]["permissionDecision"], "deny");
    assert!(
        parsed["hookSpecificOutput"].get("reason").is_none(),
        "Codex rejects unknown fields in hookSpecificOutput"
    );
    let reason = parsed["hookSpecificOutput"]["permissionDecisionReason"]
        .as_str()
        .unwrap_or("");
    assert!(
        reason.contains("lean-ctx raw"),
        "deny reason must contain the command: {reason}"
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

    // #1264: Claude validates the deprecated top-level `decision` field when
    // present, and `"allow"` is not a valid value there. PreToolUse decisions
    // belong under `hookSpecificOutput`.
    assert!(
        p.get("decision").is_none(),
        "PreToolUse output must not emit the deprecated top-level decision"
    );
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
fn claude_allow_output_omits_deprecated_top_level_decision() {
    let parsed: serde_json::Value =
        serde_json::from_str(&build_dual_allow_output()).expect("valid hook JSON");

    assert!(parsed.get("decision").is_none());
    assert_eq!(parsed["hookSpecificOutput"]["permissionDecision"], "allow");
}

#[test]
fn redirect_output_carries_copilot_modified_args() {
    // #551: the read/grep redirect must also surface modifiedArgs so Copilot CLI
    // swaps in the lean-ctx temp-file path instead of reading the original.
    let tool_input = serde_json::json!({ "path": "src/main.rs" });
    let out = build_redirect_output(Some(&tool_input), "path", "/tmp/x.lctx", None);
    let p: serde_json::Value = serde_json::from_str(&out).expect("valid hook JSON");

    assert!(p.get("decision").is_none());
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
fn compound_tricky_interpreter_sink_wrapped_for_enforcement() {
    // #1408: Non-allowlisted sinks (python3 -c) in compounds are now wrapped
    // for enforcement when shell_security != Off. This closes the bypass where
    // `true && docker --version` would skip the allowlist.
    let result = with_test_allowlist(|| {
        build_rewrite_compound("git log | python3 -c 'print(1)'", "lean-ctx")
    });
    assert_eq!(
        result,
        Some(expect_wrapped(
            "git log | python3 -c 'print(1)'",
            "lean-ctx"
        ))
    );
}

#[test]
fn compound_non_allowlisted_sink_wrapped_for_enforcement() {
    // #1408: Non-allowlisted commands in compounds are now wrapped for enforcement.
    let result =
        with_test_allowlist(|| build_rewrite_compound("git log | kubectl apply -f -", "lean-ctx"));
    assert_eq!(
        result,
        Some(expect_wrapped("git log | kubectl apply -f -", "lean-ctx"))
    );
}

#[test]
fn compound_chain_non_allowlisted_wrapped_for_enforcement() {
    // #1408: Chain with non-allowlisted command is wrapped for enforcement.
    let result = with_test_allowlist(|| {
        build_rewrite_compound("cargo test && python3 -c 'print(1)'", "lean-ctx")
    });
    assert_eq!(
        result,
        Some(expect_wrapped(
            "cargo test && python3 -c 'print(1)'",
            "lean-ctx"
        ))
    );
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
fn rewrite_candidate_wraps_non_allowlisted_compound_for_enforcement() {
    // #1408: End-to-end: a compound with non-allowlisted sink is wrapped for
    // enforcement (closes the bypass where compounds skipped the allowlist).
    let cmd = "git log | python3 -c 'print(1)'";
    let result = with_test_allowlist(|| rewrite_candidate(cmd, "lean-ctx"));
    assert_eq!(result, Some(expect_wrapped(cmd, "lean-ctx")));
}
