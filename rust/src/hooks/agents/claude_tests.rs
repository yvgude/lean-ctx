// SPDX-License-Identifier: Apache-2.0
use super::*;
use serde_json::json;

const BASH: &str = "Bash|bash";

fn commands_for(pre: &[serde_json::Value], action: &str) -> Vec<String> {
    pre.iter()
        .filter_map(|g| g.get("hooks").and_then(|h| h.as_array()))
        .flatten()
        .filter(|h| is_lean_ctx_command_for(h, action))
        .filter_map(|h| h.get("command").and_then(|c| c.as_str()).map(String::from))
        .collect()
}

#[test]
fn action_token_is_path_independent() {
    assert_eq!(
        lean_ctx_action_token("lean-ctx hook rewrite"),
        "hook rewrite"
    );
    assert_eq!(
        lean_ctx_action_token("/Users/x/.local/bin/lean-ctx hook redirect"),
        "hook redirect"
    );
}

#[test]
fn path_flip_refreshes_instead_of_duplicating() {
    // Installed once off PATH (absolute path), then `update` runs on PATH (bare name).
    let mut pre = vec![json!({
        "matcher": BASH,
        "hooks": [{ "type": "command", "command": "/abs/bin/lean-ctx hook rewrite" }]
    })];
    ensure_command_hook(&mut pre, BASH, "lean-ctx hook rewrite");
    assert_eq!(
        commands_for(&pre, "hook rewrite"),
        ["lean-ctx hook rewrite"]
    );
}

#[test]
fn repeated_calls_are_idempotent() {
    let mut pre = vec![];
    for _ in 0..5 {
        ensure_command_hook(&mut pre, BASH, "lean-ctx hook rewrite");
    }
    assert_eq!(
        commands_for(&pre, "hook rewrite"),
        ["lean-ctx hook rewrite"]
    );
}

#[test]
fn heals_preexisting_duplicates_across_groups() {
    // Two stale rewrite hooks left by older buggy versions, different matchers/paths.
    let mut pre = vec![
        json!({ "matcher": "Bash", "hooks": [{ "type": "command", "command": "/a/lean-ctx hook rewrite" }] }),
        json!({ "matcher": BASH, "hooks": [{ "type": "command", "command": "lean-ctx hook rewrite" }] }),
    ];
    ensure_command_hook(&mut pre, BASH, "lean-ctx hook rewrite");
    assert_eq!(
        commands_for(&pre, "hook rewrite"),
        ["lean-ctx hook rewrite"]
    );
}

#[test]
fn migrates_legacy_script_hook() {
    let mut pre = vec![json!({
        "matcher": BASH,
        "hooks": [{ "type": "command", "command": "/home/u/.claude/hooks/lean-ctx-rewrite.sh" }]
    })];
    ensure_command_hook(&mut pre, BASH, "lean-ctx hook rewrite");
    assert_eq!(
        commands_for(&pre, "hook rewrite"),
        ["lean-ctx hook rewrite"]
    );
}

#[test]
fn preserves_foreign_hooks() {
    let mut pre = vec![json!({
        "matcher": BASH,
        "hooks": [
            { "type": "command", "command": "my-own-tool --flag" },
            { "type": "command", "command": "/abs/lean-ctx hook rewrite" }
        ]
    })];
    ensure_command_hook(&mut pre, BASH, "lean-ctx hook rewrite");
    assert_eq!(
        commands_for(&pre, "hook rewrite"),
        ["lean-ctx hook rewrite"]
    );
    let all: Vec<String> = pre
        .iter()
        .filter_map(|g| g.get("hooks").and_then(|h| h.as_array()))
        .flatten()
        .filter_map(|h| h.get("command").and_then(|c| c.as_str()).map(String::from))
        .collect();
    assert!(
        all.iter().any(|c| c == "my-own-tool --flag"),
        "foreign hook dropped: {all:?}"
    );
}

#[test]
fn rewrite_and_redirect_coexist_after_reruns() {
    let mut pre = vec![];
    ensure_command_hook(&mut pre, BASH, "lean-ctx hook rewrite");
    ensure_command_hook(&mut pre, REDIRECT_MATCHER, "lean-ctx hook redirect");
    ensure_command_hook(&mut pre, BASH, "lean-ctx hook rewrite"); // re-run (update)
    assert_eq!(
        commands_for(&pre, "hook rewrite"),
        ["lean-ctx hook rewrite"]
    );
    assert_eq!(
        commands_for(&pre, "hook redirect"),
        ["lean-ctx hook redirect"]
    );
}

fn all_commands(pre: &[serde_json::Value]) -> Vec<String> {
    pre.iter()
        .filter_map(|g| g.get("hooks").and_then(|h| h.as_array()))
        .flatten()
        .filter_map(|h| h.get("command").and_then(|c| c.as_str()).map(String::from))
        .collect()
}

fn cmd_hook(command: &str) -> serde_json::Value {
    json!({ "type": "command", "command": command })
}

#[test]
fn own_hook_recognised_in_every_rendering() {
    for cmd in [
        "lean-ctx hook rewrite",
        "/Users/x/.local/bin/lean-ctx hook rewrite",
        "\"/Users/First Last/bin/lean-ctx\" hook rewrite",
        "C:\\Program Files\\lean-ctx\\lean-ctx.exe hook rewrite",
        "$HOME/.cargo/bin/lean-ctx hook rewrite",
        "/home/u/.claude/hooks/lean-ctx-rewrite.sh",
    ] {
        assert!(
            is_lean_ctx_command_for(&cmd_hook(cmd), "hook rewrite"),
            "not recognised as own: {cmd}"
        );
    }
}

#[test]
fn user_wrapper_mentioning_lean_ctx_is_not_own() {
    // #1879: a wrapper that ends in the same action is the user's, not ours.
    for cmd in [
        "redact-wrap lean-ctx hook rewrite",
        "sh -c 'lean-ctx hook rewrite'",
        "/opt/bin/not-lean-ctx hook rewrite",
    ] {
        assert!(
            !is_lean_ctx_command_for(&cmd_hook(cmd), "hook rewrite"),
            "user wrapper treated as own: {cmd}"
        );
    }
}

#[test]
fn user_wrapper_survives_install() {
    let mut pre = vec![json!({
        "matcher": "Bash",
        "hooks": [cmd_hook("redact-wrap lean-ctx hook rewrite")]
    })];
    ensure_command_hook(&mut pre, BASH, "lean-ctx hook rewrite");
    assert!(
        all_commands(&pre)
            .iter()
            .any(|c| c == "redact-wrap lean-ctx hook rewrite"),
        "user wrapper stripped: {pre:?}"
    );
}

#[test]
fn redirect_matcher_covers_glob() {
    // #556: shadow mode must intercept the Glob tool, so it has to be part
    // of the redirect matcher Claude installs.
    assert!(REDIRECT_MATCHER.contains("Glob"));
    assert!(REDIRECT_MATCHER.contains("glob"));
}

#[test]
fn read_dedup_hook_installs_once_alongside_observe() {
    // GL #1140: the PostToolUse array carries observe (matcher .*) AND
    // read-dedup (matcher Read); repeated installs must not duplicate either.
    let mut hooks_obj = serde_json::Map::new();
    ensure_claude_observe_hooks(&mut hooks_obj, "lean-ctx hook observe");
    ensure_read_dedup_hook(&mut hooks_obj, "lean-ctx hook read-dedup");
    ensure_read_dedup_hook(&mut hooks_obj, "lean-ctx hook read-dedup"); // re-run

    let post = hooks_obj
        .get("PostToolUse")
        .and_then(|p| p.as_array())
        .expect("PostToolUse array");
    let dedup_entries: Vec<&serde_json::Value> = post
        .iter()
        .filter(|g| {
            g.get("hooks").and_then(|h| h.as_array()).is_some_and(|h| {
                h.iter().any(|hook| {
                    hook.get("command")
                        .and_then(|c| c.as_str())
                        .is_some_and(|c| c.ends_with("hook read-dedup"))
                })
            })
        })
        .collect();
    assert_eq!(dedup_entries.len(), 1, "exactly one read-dedup group");
    assert_eq!(
        dedup_entries[0].get("matcher").and_then(|m| m.as_str()),
        Some("Read|mcp__lean-ctx__ctx_read|mcp__lean_ctx__ctx_read"),
        "read-dedup must match ONLY Read and lean-ctx ctx_read (shape safety)"
    );
    for tool in crate::hook_handlers::read_dedup_tools() {
        assert!(
            READ_DEDUP_MATCHER.split('|').any(|m| m == tool),
            "installer matcher misses {tool}, which the handler dedups"
        );
    }
    let observe_present = post.iter().any(|g| {
        g.get("hooks").and_then(|h| h.as_array()).is_some_and(|h| {
            h.iter().any(|hook| {
                hook.get("command")
                    .and_then(|c| c.as_str())
                    .is_some_and(|c| c.contains("hook observe"))
            })
        })
    });
    assert!(
        observe_present,
        "observe hook must survive read-dedup install"
    );
}

#[test]
fn rules_injection_off_strips_claude_md_block() {
    // #361: with instructions opted out, the installer must not leave a
    // lean-ctx block in CLAUDE.md (and must strip one left by a prior install).
    let _lock = crate::core::data_dir::test_env_lock();
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let dir = crate::core::editor_registry::claude_state_dir(home);
    std::fs::create_dir_all(&dir).unwrap();
    let md = dir.join("CLAUDE.md");
    std::fs::write(
        &md,
        format!("# my notes\n\n{CLAUDE_MD_BLOCK_CONTENT_MCP}\n"),
    )
    .unwrap();

    crate::test_env::set_var("LEAN_CTX_RULES_INJECTION", "off");
    install_claude_global_claude_md_for_mode(home, HookMode::Mcp);
    crate::test_env::remove_var("LEAN_CTX_RULES_INJECTION");

    let after = std::fs::read_to_string(&md).unwrap_or_default();
    assert!(
        !after.contains(CLAUDE_MD_BLOCK_START),
        "rules_injection=off must strip the CLAUDE.md block, got:\n{after}"
    );
    assert!(after.contains("# my notes"), "user content must survive");
}

#[test]
fn skill_install_then_remove_roundtrips() {
    // The off path removes a previously installed skill; install+remove must
    // touch only the lean-ctx skill folder. The skill dir now honors
    // CLAUDE_CONFIG_DIR (#596); pin it off so the path is deterministic.
    let _lock = crate::core::data_dir::test_env_lock();
    crate::test_env::remove_var("CLAUDE_CONFIG_DIR");
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    install_claude_skill(home);
    assert!(home.join(".claude/skills/lean-ctx/SKILL.md").exists());
    remove_claude_skill(home);
    assert!(!home.join(".claude/skills/lean-ctx").exists());
}

#[test]
fn claude_block_content_carries_detection_markers() {
    // GH #549 regression guard: the written block MUST contain the markers the
    // doctor + installer detect with — otherwise detection silently fails and
    // every `setup`/`doctor --fix` appends a fresh duplicate.
    assert!(
        CLAUDE_MD_BLOCK_CONTENT_MCP.contains(CLAUDE_MD_BLOCK_START),
        "block content must contain its own start marker"
    );
    assert!(
        CLAUDE_MD_BLOCK_CONTENT_MCP.contains(CLAUDE_MD_BLOCK_END),
        "block content must contain its own end marker"
    );
}

#[test]
fn installer_writes_single_claude_block_and_is_idempotent() {
    // GH #549: repeated installs must converge on exactly one block.
    let _lock = crate::core::data_dir::test_env_lock();
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let md = crate::core::editor_registry::claude_state_dir(home).join("CLAUDE.md");

    crate::test_env::set_var("LEAN_CTX_RULES_INJECTION", "shared");
    install_claude_global_claude_md_for_mode(home, HookMode::Mcp);
    install_claude_global_claude_md_for_mode(home, HookMode::Mcp);
    crate::test_env::remove_var("LEAN_CTX_RULES_INJECTION");

    let after = std::fs::read_to_string(&md).unwrap();
    assert_eq!(
        after.matches(CLAUDE_MD_BLOCK_START).count(),
        1,
        "exactly one block after repeated installs, got:\n{after}"
    );
    assert!(after.contains(CLAUDE_MD_BLOCK_VERSION));
}

#[test]
fn installer_heals_duplicate_claude_blocks() {
    let _lock = crate::core::data_dir::test_env_lock();
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let dir = crate::core::editor_registry::claude_state_dir(home);
    std::fs::create_dir_all(&dir).unwrap();
    let md = dir.join("CLAUDE.md");
    let b = CLAUDE_MD_BLOCK_CONTENT_MCP;
    let dupes = format!("# my notes\n\n{b}\n\n{b}\n\n{b}\n");
    std::fs::write(&md, dupes).unwrap();

    crate::test_env::set_var("LEAN_CTX_RULES_INJECTION", "shared");
    install_claude_global_claude_md_for_mode(home, HookMode::Mcp);
    crate::test_env::remove_var("LEAN_CTX_RULES_INJECTION");

    let after = std::fs::read_to_string(&md).unwrap();
    assert_eq!(
        after.matches(CLAUDE_MD_BLOCK_START).count(),
        1,
        "duplicates must collapse to one, got:\n{after}"
    );
    assert!(after.contains("# my notes"), "user content must survive");
}

#[test]
fn prose_marker_mention_does_not_stack_solution_blocks() {
    // #1901: a prose mention of `<!-- lean-ctx -->` made the substring
    // count 2, forcing a rewrite that removed only the lean-ctx block and
    // left the solution block behind — every run stacked another copy.
    let _lock = crate::core::data_dir::test_env_lock();
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let dir = crate::core::editor_registry::claude_state_dir(home);
    std::fs::create_dir_all(&dir).unwrap();
    let md = dir.join("CLAUDE.md");
    let prose = "# Test\n\n- The `<!-- lean-ctx -->` block below is auto-generated.\n";
    let solution = crate::core::rules_canonical::SOLUTION_BLOCK_START;
    let solution_end = crate::core::rules_canonical::SOLUTION_BLOCK_END;
    let stacked =
        format!("{prose}\n{solution}\nold\n{solution_end}\n\n{solution}\nold\n{solution_end}\n");
    std::fs::write(&md, stacked).unwrap();

    crate::test_env::set_var("LEAN_CTX_RULES_INJECTION", "shared");
    for _ in 0..3 {
        install_claude_global_claude_md_for_mode(home, HookMode::Replace);
    }
    let after = std::fs::read_to_string(&md).unwrap();
    install_claude_global_claude_md_for_mode(home, HookMode::Replace);
    let again = std::fs::read_to_string(&md).unwrap();
    crate::test_env::remove_var("LEAN_CTX_RULES_INJECTION");

    let expected_solutions =
        usize::from(!crate::core::rules_canonical::enabled_solution_rules_block().is_empty());
    assert_eq!(
        super::super::shared::count_marker_lines(&after, solution),
        expected_solutions,
        "stale solution blocks must collapse, got:\n{after}"
    );
    assert_eq!(
        super::super::shared::count_marker_lines(&after, CLAUDE_MD_BLOCK_START),
        1,
        "{after}"
    );
    assert!(
        after.starts_with(prose),
        "user prose must survive:\n{after}"
    );
    assert_eq!(after, again, "a current file must not be rewritten");
}

#[test]
fn strip_removes_orphaned_solution_block() {
    let tmp = tempfile::tempdir().unwrap();
    let md = tmp.path().join("CLAUDE.md");
    let start = crate::core::rules_canonical::SOLUTION_BLOCK_START;
    let end = crate::core::rules_canonical::SOLUTION_BLOCK_END;
    std::fs::write(&md, format!("# notes\n\n{start}\nrules\n{end}\n")).unwrap();
    strip_claude_md_block(&md);
    assert_eq!(std::fs::read_to_string(&md).unwrap(), "# notes\n");
}

#[test]
fn replace_mode_adds_permissions_deny() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    std::fs::create_dir_all(home.join(".claude")).unwrap();

    // Start with existing settings
    let settings = home.join(".claude").join("settings.json");
    std::fs::write(&settings, r#"{"hooks": {}}"#).unwrap();

    install_claude_permissions_deny_replace(home);

    let content = std::fs::read_to_string(&settings).unwrap();
    let json: serde_json::Value = serde_json::from_str(&content).unwrap();
    let deny = json
        .pointer("/permissions/deny")
        .and_then(|d| d.as_array())
        .unwrap();

    // GH #1228: bare Read must not be denied (auto memory + edit gate).
    assert!(!deny.contains(&serde_json::json!("Read")));
    assert!(deny.contains(&serde_json::json!("Grep")));
    assert!(deny.contains(&serde_json::json!("Glob")));
    // Bash must NOT be in permissions.deny — it blocks plugins (GH #799)
    assert!(!deny.contains(&serde_json::json!("Bash")));
    // Original content preserved
    assert!(json.get("hooks").is_some());
}

// The permission writers replaced settings.json with a plain `fs::write`:
// no backup, no atomic rename, no check for a concurrent writer.
#[test]
fn permission_writes_keep_a_backup_of_the_previous_settings() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    let settings = home.join(".claude").join("settings.json");
    let original = r#"{"hooks": {}, "model": "kept"}"#;
    std::fs::write(&settings, original).unwrap();

    install_claude_permissions_deny_replace(home);

    let backup = home.join(".claude").join("settings.json.bak");
    assert_eq!(std::fs::read_to_string(&backup).unwrap(), original);
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
    assert_eq!(json["model"], "kept");
    assert!(json.pointer("/permissions/deny").is_some());

    let before_allow = std::fs::read_to_string(&settings).unwrap();
    install_claude_permissions_allow_mcp(home);
    assert_eq!(std::fs::read_to_string(&backup).unwrap(), before_allow);
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
    assert!(json.pointer("/permissions/allow").is_some());
    assert!(json.pointer("/permissions/deny").is_some());
}

// A settings.json that did not parse was replaced with `{}` plus the
// lean-ctx entries: every other setting of the user was gone (a `.bak` kept
// it). It is now refused and left byte-identical, without a backup.
#[test]
fn unparseable_settings_are_refused_and_left_untouched() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    let settings = home.join(".claude").join("settings.json");
    let broken = "{\"model\": \"kept\", \"permissions\": {\"allow\": [\n";
    std::fs::write(&settings, broken).unwrap();

    install_claude_permissions_deny_replace(home);
    install_claude_permissions_allow_mcp(home);
    install_claude_hook_config(home);

    assert_eq!(std::fs::read_to_string(&settings).unwrap(), broken);
    assert!(!home.join(".claude").join("settings.json.bak").exists());
    let error = read_claude_settings(&settings).unwrap_err();
    assert!(error.contains("settings.json is not valid JSON"), "{error}");
    assert!(error.contains("left unchanged"), "{error}");
    assert!(error.contains("lean-ctx init --agent claude"), "{error}");

    // Missing and blank files still start from an empty object.
    assert_eq!(
        read_claude_settings(&home.join("new").join("settings.json")),
        Ok(json!({}))
    );
    assert!(home.join("new").is_dir());
    std::fs::write(&settings, " \n").unwrap();
    assert_eq!(read_claude_settings(&settings), Ok(json!({})));
}

#[test]
fn unparseable_project_settings_are_left_untouched() {
    let tmp = tempfile::tempdir().unwrap();
    let local = tmp.path().join(".claude").join("settings.local.json");
    std::fs::create_dir_all(local.parent().unwrap()).unwrap();
    std::fs::write(&local, "{ not json").unwrap();

    install_claude_project_hooks(tmp.path());

    assert_eq!(std::fs::read_to_string(&local).unwrap(), "{ not json");
}

#[test]
fn replace_mode_permissions_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    std::fs::create_dir_all(home.join(".claude")).unwrap();

    let settings = home.join(".claude").join("settings.json");
    std::fs::write(&settings, r"{}").unwrap();

    install_claude_permissions_deny_replace(home);
    install_claude_permissions_deny_replace(home);

    let content = std::fs::read_to_string(&settings).unwrap();
    let json: serde_json::Value = serde_json::from_str(&content).unwrap();
    let deny = json
        .pointer("/permissions/deny")
        .and_then(|d| d.as_array())
        .unwrap();

    // Each tool should appear exactly once
    let grep_count = deny.iter().filter(|v| v.as_str() == Some("Grep")).count();
    assert_eq!(grep_count, 1, "idempotent: Grep must appear exactly once");
    assert_eq!(
        deny.iter().filter(|v| v.as_str() == Some("Read")).count(),
        0,
        "Read must stay off the deny list"
    );
}

#[test]
fn replace_mode_removes_stale_read_and_bash_deny() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    std::fs::create_dir_all(home.join(".claude")).unwrap();

    // Simulate a settings.json left by an older lean-ctx version
    let settings = home.join(".claude").join("settings.json");
    std::fs::write(
        &settings,
        r#"{"permissions": {"deny": ["Read", "Grep", "Glob", "Bash"]}}"#,
    )
    .unwrap();

    install_claude_permissions_deny_replace(home);

    let content = std::fs::read_to_string(&settings).unwrap();
    let json: serde_json::Value = serde_json::from_str(&content).unwrap();
    let deny = json
        .pointer("/permissions/deny")
        .and_then(|d| d.as_array())
        .unwrap();

    assert!(
        !deny.contains(&serde_json::json!("Read")),
        "stale Read must be removed (GH #1228)"
    );
    assert!(deny.contains(&serde_json::json!("Grep")));
    assert!(deny.contains(&serde_json::json!("Glob")));
    assert!(
        !deny.contains(&serde_json::json!("Bash")),
        "stale Bash must be removed (GH #799)"
    );
}
