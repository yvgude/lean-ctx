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

/// #1918: the direct rewrites re-quote every path, so an unquoted glob the
/// calling shell would have expanded reached `lean-ctx read`/`grep` as a
/// literal, missing path ("file not found"). Such commands must keep the
/// agent's own text — native, or inside the `-c` wrap — never a direct rewrite.
/// Quoted metacharacters are literal to the shell too and keep the rewrite.
#[test]
fn unquoted_globs_never_take_a_direct_rewrite() {
    for cmd in [
        "tail -5 handover/*-alpha/FILE.md",
        "cat handover/*-alpha/FILE.md",
        "head -3 handover/*-alpha/FILE.md",
        "tail -3 handover/2026-09-29-?lpha/FILE.md",
        "cat src/[id].tsx",
        "cat docs/{a,b}.md",
        "grep -n NEEDLE_MARKER u*.txt",
        "rg -n foo src/*.rs",
    ] {
        let r = rewrite_candidate(cmd, "lean-ctx");
        assert!(
            r.is_none() || r == Some(expect_wrapped(cmd, "lean-ctx")),
            "{cmd} → {r:?}"
        );
    }
    assert_eq!(
        rewrite_candidate("grep -n NEEDLE_MARKER u*.txt", "lean-ctx"),
        Some(expect_wrapped("grep -n NEEDLE_MARKER u*.txt", "lean-ctx")),
        "grep keeps the glob inside the -c wrap"
    );
    // Escaped or quoted metacharacters are literal: the direct rewrite stays.
    assert_eq!(
        rewrite_candidate("grep -n 'foo.*bar' src/main.rs", "lean-ctx"),
        Some("lean-ctx grep 'foo.*bar' src/main.rs".to_owned())
    );
    assert_eq!(
        rewrite_candidate(r#"cat "weird[1].md""#, "lean-ctx"),
        Some("lean-ctx read 'weird[1].md'".to_owned())
    );
    // Chains: only the glob-free segment is rewritten.
    assert_eq!(
        rewrite_candidate("cat a.md && cat b/*.md", "lean-ctx"),
        Some("lean-ctx read a.md && cat b/*.md".to_owned())
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
    // Codex applies updatedInput only with "allow", so the Codex hook emits
    // this shape for read-only rewrites only (see `handle_codex_pretooluse`).
    let output = codex_rewrite_output("lean-ctx read src/main.rs");
    let parsed: serde_json::Value = serde_json::from_str(&output).expect("valid hook JSON");

    assert_eq!(parsed["hookSpecificOutput"]["hookEventName"], "PreToolUse");
    assert_eq!(parsed["hookSpecificOutput"]["permissionDecision"], "allow");
    assert_eq!(
        parsed["hookSpecificOutput"]["updatedInput"]["command"],
        "lean-ctx read src/main.rs"
    );
}

/// An unchanged PreToolUse call is allowed by a successful hook with no stdout.
#[test]
fn codex_allow_output_is_empty() {
    let output = codex_allow_output();
    assert!(output.is_empty(), "{output:?}");
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
    // Only a read-only rewrite may carry Cursor's/Copilot's approving fields.
    let tool_input = serde_json::json!({ "command": "cat foo.txt", "cwd": "/repo" });
    let rewrite = file_rewrite::Rewrite::read_only("lean-ctx read foo.txt".to_string());
    let out = build_dual_rewrite_output(Some(&tool_input), &rewrite);
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
    // Claude / CodeBuddy contract: the rewrite applies, the decision stays
    // with Claude's permission flow.
    assert!(p["hookSpecificOutput"].get("permissionDecision").is_none());
    assert_eq!(
        p["hookSpecificOutput"]["updatedInput"]["command"],
        "lean-ctx read foo.txt"
    );
    // Cursor contract.
    assert_eq!(p["permission"], "allow");
    assert_eq!(p["updated_input"]["command"], "lean-ctx read foo.txt");
}

/// A rewrite that a shell runs (`lean-ctx -c '…'`) can carry any command, so
/// no dialect may receive an approval for it: Claude keeps its permission
/// flow, Cursor and Copilot get no rewrite fields and run the original command
/// under their own approval.
#[test]
fn arbitrary_rewrite_output_grants_no_permission_in_any_dialect() {
    let tool_input = serde_json::json!({ "command": "rm -rf build" });
    let rewrite = file_rewrite::Rewrite::arbitrary("lean-ctx -c 'rm -rf build'".to_string());
    let out = build_dual_rewrite_output(Some(&tool_input), &rewrite);
    let p: serde_json::Value = serde_json::from_str(&out).expect("valid hook JSON");

    assert!(!out.contains("allow"), "{out}");
    assert_eq!(
        p,
        serde_json::json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "updatedInput": { "command": "lean-ctx -c 'rm -rf build'" }
            }
        })
    );
}

/// Only direct `lean-ctx read|grep|ls` mappings are read-only; every
/// `-c` wrap or rewritten compound is arbitrary.
#[test]
fn rewrite_safety_follows_the_original_command() {
    with_test_allowlist(|| {
        // In the side-effect-free set: approvable, whatever form the rewrite
        // takes (`git status` becomes a `-c` wrap of the same text).
        for cmd in [
            "cat src/main.rs",
            "head -n 5 src/main.rs",
            "ls src",
            "grep -rn foo src",
            "git status",
            "git status && git diff",
        ] {
            let r = file_rewrite::classify_rewrite(cmd, "lean-ctx").expect(cmd);
            assert_eq!(r.safety, RewriteSafety::ReadOnly, "{cmd} -> {}", r.command);
        }
        // Writers, builds and tests are never approved by the hook.
        for cmd in [
            "git push --force",
            "cargo build",
            "cargo test",
            "git status && cargo test",
        ] {
            let r = file_rewrite::classify_rewrite(cmd, "lean-ctx").expect(cmd);
            assert_eq!(r.safety, RewriteSafety::Arbitrary, "{cmd} -> {}", r.command);
        }
    });
}

/// Cursor/Copilot payloads: a rewrite is delivered (with the approving
/// fields) only for a command in the safe set; anything else is not
/// delivered, so the hook prints the neutral pass-through.
#[test]
fn approving_hosts_get_a_rewrite_only_for_safe_commands() {
    with_test_allowlist(|| {
        let cursor = serde_json::json!({ "hook_event_name": "preToolUse", "command": "x" });
        let input = serde_json::json!({ "command": "git status" });
        let safe = file_rewrite::classify_rewrite("git status", "lean-ctx").expect("rewrite");
        let out = deliver_rewrite("Shell", "git status", Some(&input), &safe, &cursor)
            .expect("delivered");
        let p: serde_json::Value = serde_json::from_str(&out).expect("valid JSON");
        assert_eq!(p["permission"], "allow");
        assert_eq!(p["permissionDecision"], "allow");
        assert_eq!(p["updated_input"]["command"], safe.command);
        assert!(p["hookSpecificOutput"].get("permissionDecision").is_none());

        let risky = file_rewrite::classify_rewrite("cargo build", "lean-ctx").expect("rewrite");
        assert!(deliver_rewrite("Shell", "cargo build", None, &risky, &cursor).is_none());
    });
}

#[test]
fn claude_rewrite_output_allows_only_when_asked() {
    let input = serde_json::json!({ "command": "lean-ctx -c 'cargo test'", "timeout": 5 });
    let p: serde_json::Value =
        serde_json::from_str(&build_claude_rewrite_output(&input, false)).expect("JSON");
    assert_eq!(
        p,
        serde_json::json!({ "hookSpecificOutput": {
            "hookEventName": "PreToolUse", "updatedInput": input } })
    );
    let p: serde_json::Value =
        serde_json::from_str(&build_claude_rewrite_output(&input, true)).expect("JSON");
    assert_eq!(p.as_object().map(serde_json::Map::len), Some(1));
    assert_eq!(p["hookSpecificOutput"]["permissionDecision"], "allow");
    assert_eq!(p["hookSpecificOutput"]["updatedInput"], input);
}

#[test]
fn passthrough_output_carries_no_permission_decision() {
    let out = build_passthrough_output();
    let parsed: serde_json::Value = serde_json::from_str(&out).expect("valid hook JSON");
    assert_eq!(parsed, serde_json::json!({}));
    assert!(!out.contains("allow"));
}

/// Claude Code 2.1.25x+ discarded the multi-dialect deny (top-level
/// `decision: "deny"` is not `approve|block`) as a non-blocking error, losing
/// the verdict and its guidance. Claude payloads get only hookSpecificOutput;
/// other hosts keep the Cursor/Copilot keys.
#[test]
fn deny_output_is_claude_valid_for_claude_payloads_only() {
    let claude = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "transcript_path": "/t.jsonl",
        "tool_name": "Bash"
    });
    let p: serde_json::Value =
        serde_json::from_str(&build_deny_output("use ctx_shell", &claude)).expect("valid JSON");
    assert!(p.get("decision").is_none());
    assert_eq!(p["hookSpecificOutput"]["permissionDecision"], "deny");
    assert_eq!(
        p["hookSpecificOutput"]["permissionDecisionReason"],
        "use ctx_shell"
    );

    let cursor = serde_json::json!({ "tool_name": "Shell", "command": "ls" });
    let p: serde_json::Value =
        serde_json::from_str(&build_deny_output("x", &cursor)).expect("valid JSON");
    assert_eq!(p["permission"], "deny");
    assert_eq!(p["permissionDecision"], "deny");
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
    // Claude keeps its own permission flow for the redirected read.
    assert!(p["hookSpecificOutput"].get("permissionDecision").is_none());
    assert_eq!(p["modifiedArgs"]["path"], "/tmp/x.lctx");
    assert_eq!(
        p["hookSpecificOutput"]["updatedInput"]["path"],
        "/tmp/x.lctx"
    );
    assert_eq!(p["updated_input"]["path"], "/tmp/x.lctx");
}

/// Gemini CLI reads only `hookSpecificOutput.tool_input` and approves nothing
/// on a hook decision: each rewrite becomes exactly that field, with none of
/// the other hosts' (approving) fields.
#[test]
fn gemini_output_carries_tool_input_only() {
    let tool_input = serde_json::json!({ "command": "cat foo.txt", "dir_path": "/repo" });
    for rewrite in [
        file_rewrite::Rewrite::read_only("lean-ctx read foo.txt".to_string()),
        file_rewrite::Rewrite::arbitrary("lean-ctx -c 'cat foo.txt'".to_string()),
    ] {
        let out = gemini_output(&build_dual_rewrite_output(Some(&tool_input), &rewrite));
        assert!(!out.contains("allow"), "{out}");
        let p: serde_json::Value = serde_json::from_str(&out).expect("valid hook JSON");
        assert_eq!(
            p,
            serde_json::json!({
                "hookSpecificOutput": {
                    "hookEventName": "BeforeTool",
                    "tool_input": { "command": rewrite.command, "dir_path": "/repo" }
                }
            })
        );
    }
}

/// Deny and pass-through outputs reach Gemini unchanged: it blocks on the
/// shared top-level `decision: "deny"` + `reason`, and `{}` decides nothing.
#[test]
fn gemini_output_keeps_deny_and_passthrough() {
    let gemini =
        serde_json::json!({ "hook_event_name": "BeforeTool", "tool_name": "run_shell_command" });
    assert!(is_gemini_beforetool_payload(&gemini));
    assert!(!is_claude_pretooluse_payload(&gemini));
    let deny = build_deny_output("blocked", &gemini);
    assert_eq!(gemini_output(&deny), deny);
    let p: serde_json::Value = serde_json::from_str(&deny).expect("valid JSON");
    assert_eq!(p["decision"], "deny");
    assert_eq!(p["reason"], "blocked");
    assert_eq!(gemini_output(&build_passthrough_output()), "{}");

    for other in [
        serde_json::json!({ "hook_event_name": "PreToolUse", "transcript_path": "/t" }),
        serde_json::json!({ "hook_event_name": "preToolUse" }),
        serde_json::json!({ "toolName": "bash" }),
    ] {
        assert!(!is_gemini_beforetool_payload(&other), "{other}");
    }
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

/// Bypass found in benchmark p9 (tmb/change-filter-reason r03): a heredoc is
/// never wrapped for enforcement, so `python -I - <<'EOF'` ran natively
/// unchecked right after `python -c` had been refused. The hook now checks
/// what it cannot wrap; a heredoc of allowlisted programs still passes.
#[test]
fn heredoc_that_fails_the_allowlist_is_denied_not_passed_through() {
    let (interpreter, commit) = with_test_allowlist(|| {
        crate::test_env::set_var("LEAN_CTX_SHELL_SECURITY", "enforce");
        let interpreter = file_rewrite::unwrappable_allowlist_denial(
            "python3 -I - <<'EOF'\nimport os\nprint(os.getcwd())\nEOF",
            "lean-ctx",
        );
        let commit = file_rewrite::unwrappable_allowlist_denial(
            "git commit -m \"$(cat <<'EOF'\nfix: keep the verdict line\nEOF\n)\"",
            "lean-ctx",
        );
        crate::test_env::remove_var("LEAN_CTX_SHELL_SECURITY");
        (interpreter, commit)
    });
    assert!(interpreter.is_some(), "the heredoc must not run unchecked");
    assert_eq!(commit, None);
}

/// A command whose program is lean-ctx is never wrapped (#2088), and the hooks
/// used to skip it altogether on a bare `starts_with("lean-ctx ")`: a compound
/// behind the prefix, or `lean-ctx allow …` loosening the agent's own policy,
/// ran unchecked. Every spelling of the program is recognised and checked; a
/// plain lean-ctx call and an allowlisted compound still pass.
#[test]
fn leanctx_prefixed_command_is_checked_not_skipped() {
    let verdicts = with_test_allowlist(|| {
        crate::test_env::set_var("LEAN_CTX_SHELL_SECURITY", "enforce");
        crate::test_env::set_var(
            "LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE",
            "git,cargo,cat,echo,lean-ctx,env,emspace",
        );
        let verdicts: Vec<_> = [
            ("lean-ctx gain; python3 - <<'E'\nx\nE", true),
            ("lean-ctx gain && bash -c id", true),
            ("FOO=1 lean-ctx gain && bash -c id", true),
            ("env X=1 lean-ctx gain && bash -c id", true),
            ("'lean-ctx' gain && bash -c id", true),
            ("\"lean-ctx\" gain; bash -c id", true),
            ("/usr/local/bin/lean-ctx gain && bash -c id", true),
            ("~/.local/bin/lean-ctx gain && bash -c id", true),
            ("/opt/lc/bin/lean-ctx gain && bash -c id", true),
            ("lean-ctx read $(bash -c id)", true),
            ("lean-ctx allow curl", true),
            ("env lean-ctx allow curl", true),
            ("lean-ctx config set shell_security off", true),
            // The shell expands braces and globs after the check.
            ("lean-ctx {yolo,} --yes", true),
            ("lean-ctx {allow,} curl", true),
            ("lean-ctx config set {shell_security,} off", true),
            ("lean-ctx {__session-starter,} prog", true),
            ("lean-ctx yol?", true),
            ("lean-ctx __session-starter prog", true),
            // Only the first argument selects a hidden mode.
            ("lean-ctx grep __version__ .", false),
            ("lean-ctx read __init__.py", false),
            ("lean-ctx read src/main.rs", false),
            ("/opt/lc/bin/lean-ctx read src/main.rs", false),
            ("lean-ctx read x && cargo test", false),
            ("lean-ctx -c 'cargo test && git status'", false),
            // `emspace` is on the default allowlist (owner 31.10.).
            ("emspace page show --body hooks", false),
            ("emspace status && git status", false),
            ("lean-ctx gain && emspace task list --json", false),
        ]
        .into_iter()
        .map(|(cmd, denied)| {
            let verdict = file_rewrite::unwrappable_allowlist_denial(cmd, "/opt/lc/bin/lean-ctx");
            (cmd, denied, verdict)
        })
        .collect();
        crate::test_env::remove_var("LEAN_CTX_SHELL_SECURITY");
        verdicts
    });
    for (cmd, denied, verdict) in verdicts {
        assert_eq!(verdict.is_some(), denied, "{cmd}: {verdict:?}");
    }
    // A program that merely starts with `lean-ctx` is not lean-ctx: it keeps
    // the ordinary enforcement wrap.
    assert!(!file_rewrite::is_leanctx_call(
        "lean-ctx-evil gain && bash -c id",
        "lean-ctx"
    ));
}

/// The gate refuses what it cannot validate — nesting deeper than the lexer
/// follows, or more than 64 KiB — and the hook denies it instead of passing it
/// on. Unbounded, this nesting overflowed the hook's stack, and a crashed hook
/// fails open.
#[test]
fn command_beyond_the_validation_limits_is_denied() {
    let deep = format!("echo {}{}", "$(".repeat(10_000), ")".repeat(10_000));
    let long = format!("echo {}", "a".repeat(70 * 1024));
    let (checked, hooked) = with_test_allowlist(|| {
        crate::test_env::set_var("LEAN_CTX_SHELL_SECURITY", "enforce");
        let checked =
            [&deep, &long].map(|c| crate::core::shell_allowlist::check_shell_allowlist(c));
        let hooked =
            [&deep, &long].map(|c| file_rewrite::unwrappable_allowlist_denial(c, "lean-ctx"));
        crate::test_env::remove_var("LEAN_CTX_SHELL_SECURITY");
        (checked, hooked)
    });
    // Each refusal names its own limit and the way forward, and never echoes
    // the (deep or long) command back.
    for (result, limit) in checked.iter().zip(["64 levels", "65536 bytes"]) {
        let msg = result.as_ref().expect_err("refused").to_string();
        assert!(msg.contains(limit) && msg.contains("Split it"), "{msg}");
        assert!(!msg.contains("$($(") && !msg.contains("aaaa"), "{msg}");
    }
    assert!(hooked.iter().all(Option::is_some));
}

/// End to end through the Claude Code PreToolUse payload: a line that sets
/// the shell-security variable through a `${…:=}` default and a bare
/// `export` before calling `lean-ctx -c` is denied, and so is a state write
/// through a `<>` redirect. A plain read passes.
#[test]
fn pretooluse_payload_cannot_loosen_the_gate_or_write_lean_ctx_state() {
    let outputs = with_test_allowlist(|| {
        crate::test_env::set_var("LEAN_CTX_SHELL_SECURITY", "enforce");
        crate::test_env::set_var(
            "LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE",
            "git,cargo,cat,echo,lean-ctx,id",
        );
        let outputs: Vec<_> = [
            ": ${LEAN_CTX_SHELL_SECURITY:=off}; export LEAN_CTX_SHELL_SECURITY; lean-ctx -c id",
            "echo x 1<>~/.config/lean-ctx/config.toml",
            "k=yolo; lean-ctx $k --yes",
            "git status --short",
        ]
        .into_iter()
        .map(|cmd| {
            let payload = serde_json::json!({
                "hook_event_name": "PreToolUse",
                "tool_name": "Bash",
                "tool_input": { "command": cmd },
            });
            let out = file_rewrite::compute_rewrite_for(&payload.to_string(), "lean-ctx");
            (cmd, out)
        })
        .collect();
        crate::test_env::remove_var("LEAN_CTX_SHELL_SECURITY");
        outputs
    });
    for (cmd, out) in &outputs[..3] {
        assert!(out.contains("\"deny\""), "{cmd} must be denied: {out}");
    }
    assert!(!outputs[3].1.contains("\"deny\""), "{}", outputs[3].1);
}

/// Every command a rewrite emits still passes the gate it will meet: the
/// lean-ctx subcommand allowlist must cover the hooks' own targets, with the
/// inner command of a `lean-ctx -c` wrap checked like any other line.
#[test]
fn every_rewrite_output_passes_the_gate() {
    let failures = with_test_allowlist(|| {
        crate::test_env::set_var("LEAN_CTX_SHELL_SECURITY", "enforce");
        crate::test_env::set_var(
            "LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE",
            "git,cargo,npm,head,tail,grep,wc,cat,rg,echo,cd,ls,find,tree,lean-ctx,sed",
        );
        let mut failures = Vec::new();
        for cmd in [
            "cat src/main.rs",
            "head -50 src/lib.rs",
            "tail -n 20 Cargo.toml",
            "sed -n 1,40p src/main.rs",
            "grep -rn TODO src",
            "rg -n 'fn main' src",
            "ls -la src",
            "find src -name '*.rs'",
            "tree src",
            "git status",
            "git diff HEAD~1",
            "git log --oneline -5",
            "cargo test --lib",
            "cargo test && git status",
            "npm run build",
            "git log | head -5",
        ] {
            for binary in ["lean-ctx", "/opt/lc/bin/lean-ctx"] {
                // An enforcement wrap carries a command the gate refuses on
                // purpose; only the compression rewrites must pass.
                let emitted = file_rewrite::classify_rewrite(cmd, binary).map(|r| r.command);
                if let Some(emitted) = emitted
                    && let Err(err) = crate::core::shell_allowlist::check_shell_allowlist(&emitted)
                {
                    failures.push(format!("{cmd} → {emitted}: {err}"));
                }
            }
        }
        crate::test_env::remove_var("LEAN_CTX_SHELL_SECURITY");
        failures
    });
    assert!(failures.is_empty(), "{failures:#?}");
}
