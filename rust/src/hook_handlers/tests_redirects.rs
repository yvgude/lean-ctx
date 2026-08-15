use super::*;

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
    // Explicit modes are authoritative on all hosts.
    let mode = |m: &str| serde_json::json!({ "pattern": "x", "output_mode": m });
    assert!(grep_content_mode(Some(&mode("content"))));
    assert!(!grep_content_mode(Some(&mode("files_with_matches"))));
    assert!(!grep_content_mode(Some(&mode("count"))));
    // Absent output_mode defaults to host-dependent: Cursor defaults to
    // `content` (safe to redirect), Claude Code to `files_with_matches`
    // (unsafe). `hook_host_is_cursor()` gates the absent case.
    let absent = serde_json::json!({ "pattern": "x" });
    let absent_result = grep_content_mode(Some(&absent));
    let on_cursor = crate::core::config::read_redirect::hook_host_is_cursor();
    assert_eq!(absent_result, on_cursor);
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
fn redirect_read_args_smart_mode_selection() {
    // Code files now get "auto" — the auto_mode_resolver + safety chain
    // (marker guard, snapshot store, quality escalation) handle compression.
    let code_rs = redirect_read_args("/repo/src/main.rs", true);
    assert_eq!(code_rs, ["read", "/repo/src/main.rs", "-m", "auto"]);
    let code_ts = redirect_read_args("/repo/src/app.tsx", false);
    assert_eq!(code_ts, ["read", "/repo/src/app.tsx", "-m", "auto"]);

    // Terminal output and logs also get "auto".
    let terminal = redirect_read_args("/home/user/.cursor/projects/foo/terminals/1.txt", false);
    assert_eq!(
        terminal,
        [
            "read",
            "/home/user/.cursor/projects/foo/terminals/1.txt",
            "-m",
            "auto"
        ]
    );
    let log = redirect_read_args("/tmp/build.log", false);
    assert_eq!(log, ["read", "/tmp/build.log", "-m", "auto"]);

    // Unknown extensions also get "auto" (safety chain handles all cases).
    let unknown = redirect_read_args("/tmp/something.xyz", false);
    assert_eq!(unknown, ["read", "/tmp/something.xyz", "-m", "auto"]);

    // Instruction overrides stay "full" (e.g. Cargo.toml, package.json).
    let cargo = redirect_read_args("/repo/Cargo.toml", false);
    assert_eq!(cargo, ["read", "/repo/Cargo.toml", "-m", "full"]);
    let pkg = redirect_read_args("/repo/package.json", false);
    assert_eq!(pkg, ["read", "/repo/package.json", "-m", "full"]);
}

#[test]
fn edit_risk_class_classification() {
    use super::redirect::{EditRiskClass, edit_risk_class};
    // Code files are now SafeToCompress (safety chain handles edits).
    assert_eq!(
        edit_risk_class("/src/main.rs"),
        EditRiskClass::SafeToCompress
    );
    assert_eq!(
        edit_risk_class("/src/app.tsx"),
        EditRiskClass::SafeToCompress
    );
    assert_eq!(
        edit_risk_class("/config.toml"),
        EditRiskClass::SafeToCompress
    );
    assert_eq!(edit_risk_class("/data.json"), EditRiskClass::SafeToCompress);
    assert_eq!(edit_risk_class(""), EditRiskClass::SafeToCompress);
    // Terminal output, logs remain SafeToCompress.
    assert_eq!(
        edit_risk_class("/home/.cursor/projects/x/terminals/1.txt"),
        EditRiskClass::SafeToCompress,
    );
    assert_eq!(
        edit_risk_class("/var/log/app.log"),
        EditRiskClass::SafeToCompress
    );
    // Instruction overrides are NeverCompress.
    assert_eq!(
        edit_risk_class("/repo/Cargo.toml"),
        EditRiskClass::NeverCompress
    );
    assert_eq!(
        edit_risk_class("/repo/package.json"),
        EditRiskClass::NeverCompress
    );
    assert_eq!(
        edit_risk_class("/app/Dockerfile"),
        EditRiskClass::NeverCompress
    );
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
fn redirect_read_passes_through_when_disabled_by_config() {
    // #637: read_redirect=off must make a native Read fall through untouched —
    // the exact dual-allow response, with no path-swap to a temp copy — so the
    // host's read-before-write guard tracks the real file and Write/Edit works.
    let _lock = crate::core::data_dir::test_env_lock();
    crate::test_env::remove_var("CLAUDE_PROJECT_DIR");
    crate::test_env::remove_var("CLAUDECODE");
    crate::test_env::remove_var("CODEBUDDY");
    crate::test_env::set_var("LEAN_CTX_READ_REDIRECT", "off");

    let tool_input = serde_json::json!({ "file_path": "/repo/src/main.rs" });
    let out = redirect_read(Some(&tool_input));

    crate::test_env::remove_var("LEAN_CTX_READ_REDIRECT");

    assert_eq!(
        out,
        build_dual_allow_output(),
        "disabled Read redirect must emit the plain dual-allow passthrough"
    );
    assert!(
        !out.contains(".lctx") && !out.contains("updatedInput") && !out.contains("modifiedArgs"),
        "disabled Read redirect must not rewrite the path to a temp copy: {out}"
    );
}

#[test]
fn redirect_read_auto_passes_through_under_claude_code() {
    // #637: with the default `auto`, the marker Claude Code exports to hook
    // subprocesses — CLAUDE_PROJECT_DIR — must disable the Read path-swap out of the
    // box, no config edit. This is exactly what fixes headless `claude -p`
    // (CLAUDECODE is NOT propagated to hook children, so it cannot be the signal).
    let _lock = crate::core::data_dir::test_env_lock();
    crate::test_env::set_var("LEAN_CTX_READ_REDIRECT", "auto");
    crate::test_env::remove_var("CLAUDECODE");
    crate::test_env::remove_var("CODEBUDDY");
    crate::test_env::set_var("CLAUDE_PROJECT_DIR", "/repo");

    let tool_input = serde_json::json!({ "file_path": "/repo/src/main.rs" });
    let out = redirect_read(Some(&tool_input));

    crate::test_env::remove_var("CLAUDE_PROJECT_DIR");
    crate::test_env::remove_var("LEAN_CTX_READ_REDIRECT");

    assert_eq!(
        out,
        build_dual_allow_output(),
        "auto must disable the Read redirect under Claude Code hooks (#637)"
    );
    assert!(
        !out.contains(".lctx"),
        "no temp path-swap under Claude Code: {out}"
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

// --- GH #760: non-allowlisted binaries must pass through, not block ---

#[test]
fn gh760_non_rewritable_command_not_wrapped() {
    assert_eq!(
        rewrite_candidate("mvnw clean package", "lean-ctx"),
        None,
        "mvnw is not in REWRITE_COMMANDS — hook must not wrap it"
    );
    assert_eq!(
        rewrite_candidate("md5sum file.txt", "lean-ctx"),
        None,
        "md5sum is not rewritable — must pass through raw"
    );
    assert_eq!(
        rewrite_candidate("update-alternatives --list java", "lean-ctx"),
        None,
        "update-alternatives is not rewritable — must pass through raw"
    );
}

#[test]
fn gh760_pipeline_with_path_segments_wraps_when_gate_clean() {
    let _lock = crate::core::data_dir::test_env_lock();
    crate::test_env::set_var("LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE", "find,tr");
    let cmd = "find target/quarkus-app/lib -name \"*.jar\" | tr '\\n' ':'";
    let result = rewrite_candidate(cmd, "lean-ctx");
    crate::test_env::remove_var("LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE");
    assert_eq!(
        result,
        Some(expect_wrapped(cmd, "lean-ctx")),
        "gate-clean pipeline must be wrapped whole; path segment 'lib' must not interfere"
    );
}

#[test]
fn gh760_pipeline_with_non_allowed_sink_wrapped_for_enforcement() {
    // #1408: Non-allowlisted sink is now wrapped for enforcement (security fix).
    let _lock = crate::core::data_dir::test_env_lock();
    crate::test_env::set_var("LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE", "find");
    let cmd = "find . -name '*.jar' | custom-tool";
    let result = rewrite_candidate(cmd, "lean-ctx");
    crate::test_env::remove_var("LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE");
    assert_eq!(
        result,
        Some(expect_wrapped(cmd, "lean-ctx")),
        "non-allowlisted sink must be wrapped for enforcement (#1408)"
    );
}
