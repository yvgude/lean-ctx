use super::super::{
    ensure_codex_hooks_enabled as shared_ensure_codex_hooks_enabled, ensure_state_dir,
    install_codex_instruction_docs, mcp_server_quiet_mode, resolve_hook_command_binary,
    upsert_lean_ctx_codex_hook_entries, write_file,
};

pub fn install_codex_hook() {
    let Some(codex_dir) = crate::core::home::resolve_codex_dir() else {
        tracing::error!("Cannot resolve codex directory");
        return;
    };
    if !ensure_state_dir(&codex_dir) {
        return;
    }

    let hook_config_changed = install_codex_hook_config(&codex_dir);
    let solution_rules_changed = install_codex_solution_rules(&codex_dir);
    let installed_docs = install_codex_instruction_docs(&codex_dir);

    if !mcp_server_quiet_mode() {
        if hook_config_changed {
            eprintln!(
                "Installed Codex SessionStart/PreToolUse hooks (Bash/Read/Grep/Glob) at {}",
                codex_dir.display()
            );
        }
        if installed_docs || solution_rules_changed {
            eprintln!("Installed Codex instructions at {}", codex_dir.display());
        } else {
            eprintln!("Codex AGENTS.md already configured.");
        }
    }
}

fn install_codex_solution_rules(codex_dir: &std::path::Path) -> bool {
    let rules_path = codex_dir.join("instructions.md");
    // GH #1526: `rules_injection = off` must not write the instructions.md
    // rules block — and it strips the block a prior install left behind.
    if crate::core::config::Config::load().rules_injection_effective()
        == crate::core::config::RulesInjection::Off
    {
        if rules_path.exists()
            && std::fs::read_to_string(&rules_path).is_ok_and(|c| {
                crate::marked_block::contains_marker_line(
                    &c,
                    crate::core::rules_canonical::START_MARK,
                )
            })
        {
            crate::marked_block::remove_from_file(
                &rules_path,
                crate::core::rules_canonical::START_MARK,
                crate::core::rules_canonical::END_MARK,
                true,
                "Codex rules",
            );
            return true;
        }
        return false;
    }
    let block = solution_aware_rules_block(crate::rules_inject::canonical_rules_block());
    upsert_solution_rules(&rules_path, &block)
}

#[allow(clippy::needless_pass_by_value)]
fn solution_aware_rules_block(base_block: String) -> String {
    let solution_block = crate::core::rules_canonical::enabled_solution_rules_block();
    let base_block = crate::marked_block::remove_content(
        &base_block,
        crate::core::rules_canonical::SOLUTION_BLOCK_START,
        crate::core::rules_canonical::SOLUTION_BLOCK_END,
    );
    if solution_block.is_empty() {
        return base_block;
    }

    base_block.replacen(
        crate::core::rules_canonical::END_MARK,
        &format!(
            "{solution_block}\n{}",
            crate::core::rules_canonical::END_MARK
        ),
        1,
    )
}

fn upsert_solution_rules(path: &std::path::Path, block: &str) -> bool {
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    if existing.trim_end() == block.trim_end() {
        return false;
    }

    crate::marked_block::upsert(
        path,
        crate::core::rules_canonical::START_MARK,
        crate::core::rules_canonical::END_MARK,
        block,
        true,
        "Codex rules",
    );
    true
}

fn install_codex_hook_config(codex_dir: &std::path::Path) -> bool {
    let binary = resolve_hook_command_binary();
    let session_start_cmd = format!("{binary} hook codex-session-start");
    let pre_tool_use_cmd = format!("{binary} hook codex-pretooluse");
    let hooks_json_path = codex_dir.join("hooks.json");

    let mut changed = false;
    let mut root = if hooks_json_path.exists() {
        if let Some(parsed) = std::fs::read_to_string(&hooks_json_path)
            .ok()
            .and_then(|content| crate::core::jsonc::parse_jsonc(&content).ok())
        {
            parsed
        } else {
            changed = true;
            serde_json::json!({ "hooks": {} })
        }
    } else {
        changed = true;
        serde_json::json!({ "hooks": {} })
    };

    if upsert_lean_ctx_codex_hook_entries(&mut root, &session_start_cmd, &pre_tool_use_cmd) {
        changed = true;
    }

    // Observe hooks for context awareness
    let observe_cmd = format!("{binary} hook observe");
    if ensure_codex_observe_hooks(&mut root, &observe_cmd) {
        changed = true;
    }

    if changed {
        write_file(
            &hooks_json_path,
            &serde_json::to_string_pretty(&root).unwrap_or_default(),
        );
    }

    let rewrite_path = codex_dir.join("hooks").join("lean-ctx-rewrite-codex.sh");
    if rewrite_path.exists() && std::fs::remove_file(&rewrite_path).is_ok() {
        changed = true;
    }

    let config_toml_path = crate::core::home::resolve_codex_config_path()
        .unwrap_or_else(|| codex_dir.join("config.toml"));
    let config_content = std::fs::read_to_string(&config_toml_path).unwrap_or_default();

    // Hybrid mode: ensure MCP server entry exists in config.toml so Codex
    // Desktop/Cloud can reach lean-ctx even without CLI hooks.
    let mcp_updated = ensure_codex_mcp_server(
        &config_content,
        &binary,
        &super::super::mcp_server_env_pairs(),
    );
    let hooks_updated =
        ensure_codex_hooks_enabled(mcp_updated.as_deref().unwrap_or(&config_content));

    let final_content = hooks_updated
        .or(mcp_updated)
        .unwrap_or_else(|| config_content.clone());
    if final_content != config_content {
        write_file(&config_toml_path, &final_content);
        changed = true;
        if !mcp_server_quiet_mode() {
            eprintln!(
                "Updated Codex config (MCP server + hooks) in {}",
                config_toml_path.display()
            );
        }
    }

    changed
}

fn ensure_codex_observe_hooks(root: &mut serde_json::Value, observe_cmd: &str) -> bool {
    let original = root.clone();
    let Some(hooks_obj) = root
        .as_object_mut()
        .and_then(|r| r.get_mut("hooks"))
        .and_then(|h| h.as_object_mut())
    else {
        return false;
    };

    // The observer is best-effort telemetry. Its process has an internal
    // fail-open deadline; this host deadline leaves scheduling headroom while
    // ensuring telemetry can never stall a tool result.
    const OBSERVE_TIMEOUT_SECS: u64 = 4;
    const SESSION_END_OBSERVE_TIMEOUT_SECS: u64 = 3;
    // Shell output can be arbitrarily large and is already observed by
    // ctx_shell/the MCP pipeline. Restrict Codex's PostToolUse observer to
    // native edits, where code-health and provenance capture add unique value.
    const POST_TOOL_EDIT_MATCHER: &str = "Write|Edit|MultiEdit|StrReplace|str_replace_editor|edit_file|str_replace|replace_in_file|create_file|write_file";
    let observe_events = ["PostToolUse", "SessionStart", "SessionEnd"];
    for event in observe_events {
        let timeout_secs = if event == "SessionEnd" {
            SESSION_END_OBSERVE_TIMEOUT_SECS
        } else {
            OBSERVE_TIMEOUT_SECS
        };
        let observe_hook = serde_json::json!({
            "type": "command", "command": observe_cmd, "timeout": timeout_secs
        });
        let arr = hooks_obj
            .entry(event.to_string())
            .or_insert_with(|| serde_json::json!([]));
        let Some(entries) = arr.as_array_mut() else {
            continue;
        };

        normalize_codex_observe_hooks(
            entries,
            event,
            observe_cmd,
            timeout_secs,
            POST_TOOL_EDIT_MATCHER,
        );

        // Migration: if observe exists as a standalone entry (separate from the
        // lean-ctx session-start entry), merge it into the lean-ctx entry and
        // remove the standalone to prevent duplicate 'hook: <Event>' log lines.
        let observe_standalone_idx = entries.iter().position(|e| {
            let hooks = e.get("hooks").and_then(|h| h.as_array());
            let Some(hooks) = hooks else { return false };
            // Standalone = only has observe, no other lean-ctx commands
            hooks.len() == 1
                && hooks[0]
                    .get("command")
                    .and_then(|c| c.as_str())
                    .is_some_and(|c| c.contains("hook observe"))
        });
        let leanctx_entry_idx = entries.iter().position(|e| {
            e.get("hooks")
                .and_then(|h| h.as_array())
                .is_some_and(|hooks| {
                    hooks.iter().any(|hook| {
                        hook.get("command")
                            .and_then(|c| c.as_str())
                            .is_some_and(|c| c.contains("lean-ctx") && !c.contains("hook observe"))
                    })
                })
        });
        if let (Some(standalone), Some(target)) = (observe_standalone_idx, leanctx_entry_idx) {
            // Remove standalone first (shifts indices if standalone < target)
            entries.remove(standalone);
            let target = if standalone < target {
                target - 1
            } else {
                target
            };
            // Merge observe into the lean-ctx entry
            if let Some(hooks_arr) = entries[target]
                .as_object_mut()
                .and_then(|e| e.get_mut("hooks"))
                .and_then(|h| h.as_array_mut())
            {
                let already = hooks_arr.iter().any(|h| {
                    h.get("command")
                        .and_then(|c| c.as_str())
                        .is_some_and(|c| c.contains("hook observe"))
                });
                if !already {
                    hooks_arr.push(observe_hook.clone());
                }
            }
            continue;
        }

        // Already properly merged — nothing to do.
        let already_has_observe = entries.iter().any(|e| {
            e.get("hooks")
                .and_then(|h| h.as_array())
                .is_some_and(|hooks| {
                    hooks.iter().any(|hook| {
                        hook.get("command")
                            .and_then(|c| c.as_str())
                            .is_some_and(|c| c.contains("hook observe"))
                    })
                })
        });
        if already_has_observe {
            continue;
        }

        // Merge into existing lean-ctx entry instead of creating a duplicate
        // that fires a separate `hook: <Event>` log line (GL #1398).
        let merged = entries.iter_mut().any(|entry| {
            let dominated = entry
                .get("hooks")
                .and_then(|h| h.as_array())
                .is_some_and(|hooks| {
                    hooks.iter().any(|hook| {
                        hook.get("command")
                            .and_then(|c| c.as_str())
                            .is_some_and(|c| c.contains("lean-ctx"))
                    })
                });
            if !dominated {
                return false;
            }
            if let Some(hooks_arr) = entry
                .as_object_mut()
                .and_then(|e| e.get_mut("hooks"))
                .and_then(|h| h.as_array_mut())
            {
                hooks_arr.push(observe_hook.clone());
                return true;
            }
            false
        });

        if !merged {
            let matcher = if event == "PostToolUse" {
                POST_TOOL_EDIT_MATCHER
            } else {
                ".*"
            };
            entries.push(serde_json::json!({
                "matcher": matcher,
                "hooks": [observe_hook.clone()]
            }));
        }
    }

    *root != original
}

/// Normalize installed observer hooks without touching unrelated user hooks.
/// Existing installations used a universal PostToolUse matcher and a deadline
/// equal to Codex's own timeout, creating avoidable user-visible failures.
fn normalize_codex_observe_hooks(
    entries: &mut [serde_json::Value],
    event: &str,
    observe_cmd: &str,
    timeout_secs: u64,
    post_tool_edit_matcher: &str,
) {
    for entry in entries {
        let Some(hooks) = entry
            .get_mut("hooks")
            .and_then(|hooks| hooks.as_array_mut())
        else {
            continue;
        };
        let mut contains_observe = false;
        let mut only_observe = true;
        for hook in hooks {
            let is_observe = hook
                .get("command")
                .and_then(|command| command.as_str())
                .is_some_and(|command| command == observe_cmd || command.contains("hook observe"));
            if is_observe {
                contains_observe = true;
                if let Some(object) = hook.as_object_mut() {
                    object.insert("timeout".to_string(), serde_json::json!(timeout_secs));
                }
            } else {
                only_observe = false;
            }
        }
        if event == "PostToolUse" && contains_observe && only_observe {
            if let Some(object) = entry.as_object_mut() {
                object.insert(
                    "matcher".to_string(),
                    serde_json::json!(post_tool_edit_matcher),
                );
            }
        }
    }
}

/// Idempotent upsert of the `[mcp_servers.lean-ctx]` entry in Codex `config.toml`.
///
/// Uses a format-preserving TOML editor so existing user content/comments and an
/// orphaned `[mcp_servers.lean-ctx.env]` (issue #189) are normalized into a
/// single valid section instead of producing a duplicate table header.
///
/// `env_pairs` is injected (not read from global state) so the pure
/// config-rewriting logic is hermetically testable; the caller passes
/// [`crate::hooks::mcp_server_env_pairs`]. Existing `command`/`args` are left
/// untouched to respect user customization; env keys are upserted so a stale
/// install gains `LEAN_CTX_PROJECT_ROOT`/`LEAN_CTX_EXTRA_ROOTS` (#403). Returns
/// `None` when nothing changed, or when the file is not valid TOML (never
/// clobbers an unparseable user config).
fn ensure_codex_mcp_server(
    config_content: &str,
    binary: &str,
    env_pairs: &[(String, String)],
) -> Option<String> {
    let mut doc = config_content.parse::<toml_edit::DocumentMut>().ok()?;
    let original = doc.to_string();

    // `[mcp_servers]` stays implicit so we never emit a bare parent header.
    let servers = doc["mcp_servers"].or_insert(toml_edit::table());
    if let Some(t) = servers.as_table_mut() {
        t.set_implicit(true);
    }

    // `[mcp_servers.lean-ctx]` must be explicit so its header is rendered before
    // the `.env` child table (fixes the orphaned-env ordering from #189).
    let lean = servers["lean-ctx"].or_insert(toml_edit::table());
    let lean_tbl = lean.as_table_mut()?;
    lean_tbl.set_implicit(false);

    // Respect user customization: only fill `command`/`args` when absent.
    if !lean_tbl.contains_key("command") {
        lean_tbl["command"] = toml_edit::value(binary);
    }
    if !lean_tbl.contains_key("args") {
        let mut arr = toml_edit::Array::new();
        arr.push("mcp");
        lean_tbl["args"] = toml_edit::value(arr);
    } else if let Some(arr) = lean_tbl.get_mut("args").and_then(|a| a.as_array_mut()) {
        if arr.is_empty() {
            arr.push("mcp");
        }
    }

    // Ensure adequate timeouts: lean-ctx tools like ctx_compose can take
    // >60s on first invocation (tree-sitter parsing, index building).
    if !lean_tbl.contains_key("startup_timeout_sec") {
        lean_tbl["startup_timeout_sec"] = toml_edit::value(30);
    }
    if !lean_tbl.contains_key("tool_timeout_sec") {
        lean_tbl["tool_timeout_sec"] = toml_edit::value(120);
    }

    let env = lean_tbl["env"].or_insert(toml_edit::table());
    if let Some(env_tbl) = env.as_table_mut() {
        for (key, val) in env_pairs {
            let key = key.as_str();
            if env_tbl.get(key).and_then(toml_edit::Item::as_str) != Some(val.as_str()) {
                env_tbl[key] = toml_edit::value(val.as_str());
            }
        }
    }

    let updated = doc.to_string();
    (updated != original).then_some(updated)
}

fn ensure_codex_hooks_enabled(config_content: &str) -> Option<String> {
    shared_ensure_codex_hooks_enabled(config_content)
}

// Codex deny is handled mode-aware by the codex-pretooluse handler at runtime:
// Bash is rewritten when possible, and registered Read/Grep/Glob calls use the
// safe redirect pipeline or an MCP suggestion in Replace mode. A separate deny
// hook is not needed because PreToolUse provides the decision point for all.

#[cfg(test)]
mod tests {
    use super::{
        ensure_codex_hooks_enabled, ensure_codex_mcp_server, ensure_codex_observe_hooks,
        upsert_lean_ctx_codex_hook_entries,
    };
    use serde_json::json;

    /// GH #1526: `rules_injection = off` must not write ANY Codex steering
    /// file (instructions.md rules block, AGENTS.md block, LEAN-CTX.md) — and
    /// it removes what a prior install left behind.
    #[test]
    fn rules_injection_off_writes_no_codex_steering_files() {
        let _lock = crate::core::data_dir::test_env_lock();
        let tmp = tempfile::tempdir().expect("temp codex dir");
        let codex_dir = tmp.path();

        // Simulate a prior shared install's artifacts.
        std::fs::write(
            codex_dir.join("LEAN-CTX.md"),
            "old lean-ctx steering content",
        )
        .expect("seed LEAN-CTX.md");
        std::fs::write(
            codex_dir.join("AGENTS.md"),
            format!(
                "# Global Agent Instructions\n\n{}\nlean-ctx block\n{}\n",
                crate::core::rules_canonical::AGENTS_BLOCK_START,
                crate::core::rules_canonical::AGENTS_BLOCK_END
            ),
        )
        .expect("seed AGENTS.md");
        std::fs::write(
            codex_dir.join("instructions.md"),
            format!(
                "{}\nold rules\n{}\n",
                crate::core::rules_canonical::START_MARK,
                crate::core::rules_canonical::END_MARK
            ),
        )
        .expect("seed instructions.md");

        crate::test_env::set_var("LEAN_CTX_RULES_INJECTION", "off");
        let docs_changed = crate::hooks::install_codex_instruction_docs(codex_dir);
        let rules_changed = super::install_codex_solution_rules(codex_dir);
        crate::test_env::remove_var("LEAN_CTX_RULES_INJECTION");

        assert!(docs_changed, "stale steering artifacts must be removed");
        assert!(rules_changed, "stale instructions block must be removed");
        assert!(
            !codex_dir.join("LEAN-CTX.md").exists(),
            "off must remove the lean-ctx-owned LEAN-CTX.md"
        );
        let agents = std::fs::read_to_string(codex_dir.join("AGENTS.md")).unwrap_or_default();
        assert!(
            !agents.contains(crate::core::rules_canonical::AGENTS_BLOCK_START),
            "off must strip the AGENTS.md lean-ctx block: {agents}"
        );
        let instructions =
            std::fs::read_to_string(codex_dir.join("instructions.md")).unwrap_or_default();
        assert!(
            !instructions.contains(crate::core::rules_canonical::START_MARK),
            "off must strip the instructions.md rules block: {instructions}"
        );

        // Idempotent: a second run on the cleaned directory writes nothing.
        crate::test_env::set_var("LEAN_CTX_RULES_INJECTION", "off");
        let docs_again = crate::hooks::install_codex_instruction_docs(codex_dir);
        let rules_again = super::install_codex_solution_rules(codex_dir);
        crate::test_env::remove_var("LEAN_CTX_RULES_INJECTION");
        assert!(!docs_again && !rules_again, "off must be idempotent");
    }

    /// Minimal env block (data dir only) for the config-rewrite tests that do
    /// not exercise project-root/extra-roots propagation.
    fn data_dir_pairs() -> Vec<(String, String)> {
        vec![(
            "LEAN_CTX_DATA_DIR".to_string(),
            "/Users/user/.lean-ctx".to_string(),
        )]
    }

    #[test]
    fn upsert_replaces_legacy_codex_rewrite_but_keeps_custom_hooks() {
        let mut input = json!({
            "hooks": {
                "PreToolUse": [
                    {
                        "matcher": "Bash",
                        "hooks": [{
                            "type": "command",
                            "command": "/opt/homebrew/bin/lean-ctx hook rewrite",
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
                "SessionStart": [
                    {
                        "matcher": "startup|resume|clear",
                        "hooks": [{
                            "type": "command",
                            "command": "lean-ctx hook codex-session-start",
                            "timeout": 15
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
        });

        let changed = upsert_lean_ctx_codex_hook_entries(
            &mut input,
            "lean-ctx hook codex-session-start",
            "lean-ctx hook codex-pretooluse",
        );
        assert!(changed, "legacy hooks should be migrated");

        let pre_tool_use = input["hooks"]["PreToolUse"]
            .as_array()
            .expect("PreToolUse array should remain");
        assert_eq!(pre_tool_use.len(), 2, "custom hook should be preserved");
        assert_eq!(
            pre_tool_use[0]["hooks"][0]["command"].as_str(),
            Some("echo keep-me")
        );
        assert_eq!(
            pre_tool_use[1]["hooks"][0]["command"].as_str(),
            Some("lean-ctx hook codex-pretooluse")
        );
        assert_eq!(
            pre_tool_use[1]["matcher"].as_str(),
            Some("Bash|Read|Grep|Glob"),
            "the Codex installer must register native file tool coverage"
        );
        assert_eq!(
            input["hooks"]["SessionStart"][0]["hooks"][0]["command"].as_str(),
            Some("lean-ctx hook codex-session-start")
        );
        assert_eq!(
            input["hooks"]["PostToolUse"][0]["hooks"][0]["command"].as_str(),
            Some("echo keep-post")
        );
    }

    #[test]
    fn ignores_non_lean_ctx_codex_entries() {
        let custom = json!({
            "matcher": "Bash",
            "hooks": [{
                "type": "command",
                "command": "echo keep-me",
                "timeout": 5
            }]
        });
        assert!(
            !crate::hooks::support::is_lean_ctx_codex_managed_entry("PreToolUse", &custom),
            "custom Codex hooks must be preserved"
        );
    }

    #[test]
    fn detects_managed_codex_session_start_entry() {
        let managed = json!({
            "matcher": "startup|resume|clear",
            "hooks": [{
                "type": "command",
                "command": "/opt/homebrew/bin/lean-ctx hook codex-session-start",
                "timeout": 15
            }]
        });
        assert!(crate::hooks::support::is_lean_ctx_codex_managed_entry(
            "SessionStart",
            &managed
        ));
    }

    #[test]
    fn ensure_codex_hooks_enabled_updates_existing_features_flag() {
        let input = "\
[features]
other = true
codex_hooks = false

[mcp_servers.other]
command = \"other\"
";

        let output =
            ensure_codex_hooks_enabled(input).expect("codex_hooks=false should be migrated");

        assert!(output.contains("[features]\nother = true\nhooks = true\n"));
        assert!(!output.contains("codex_hooks = false"));
    }

    #[test]
    fn ensure_codex_hooks_enabled_moves_stray_assignment_into_features_section() {
        let input = "\
[features]
other = true

[mcp_servers.lean-ctx]
command = \"lean-ctx\"
codex_hooks = true
";

        let output = ensure_codex_hooks_enabled(input)
            .expect("stray codex_hooks assignment should be normalized");

        assert!(output.contains("[features]\nother = true\nhooks = true\n"));
        assert_eq!(output.matches("hooks = true").count(), 1);
        assert!(!output.contains("[mcp_servers.lean-ctx]\ncommand = \"lean-ctx\"\nhooks = true"));
    }

    #[test]
    fn ensure_codex_hooks_enabled_adds_features_section_when_missing() {
        let input = "\
[mcp_servers.lean-ctx]
command = \"lean-ctx\"
";

        let output =
            ensure_codex_hooks_enabled(input).expect("missing features section should be added");

        assert!(output.ends_with("\n[features]\nhooks = true\n"));
    }

    #[test]
    fn codex_docs_steer_to_reliable_mcp_path_without_false_hook_claim() {
        // GH #1526 follow-up: install_codex_instruction_docs branches on
        // rules_injection now — pin Shared under the env lock so the
        // off-mode regression test cannot poison this one in parallel.
        let _lock = crate::core::data_dir::test_env_lock();
        crate::test_env::set_var("LEAN_CTX_RULES_INJECTION", "shared");
        let tmp = std::env::temp_dir().join("lean-ctx-test-codex-desktop-note");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        crate::hooks::support::install_codex_instruction_docs(&tmp);

        let lean_ctx_md = std::fs::read_to_string(tmp.join("LEAN-CTX.md")).unwrap();
        assert!(
            lean_ctx_md.contains("ctx_shell") && lean_ctx_md.contains("ctx_read"),
            "LEAN-CTX.md must steer the agent to the MCP tools"
        );
        // Regression guard for #350: never assert as fact that Desktop/Cloud hooks
        // do not run — they can (gated by trust via /hooks, varies by version).
        let normalized = lean_ctx_md.replace('\n', " ");
        assert!(
            !normalized.contains("hooks do not run")
                && !normalized.contains("no automatic compression"),
            "LEAN-CTX.md must not make the false blanket claim that Codex Desktop hooks never run (#350)"
        );

        let agents_md = std::fs::read_to_string(tmp.join("AGENTS.md")).unwrap();
        assert!(
            agents_md.contains("ctx_shell") && agents_md.contains("ctx_search"),
            "AGENTS.md block must steer to the reliable MCP tools"
        );
        let agents_norm = agents_md.replace('\n', " ");
        assert!(
            !agents_norm.contains("hooks do not run"),
            "AGENTS.md must not claim Codex hooks never run (#350)"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn install_codex_docs_preserves_existing_user_instructions() {
        // GH #1526 follow-up: install_codex_instruction_docs branches on
        // rules_injection now — pin Shared under the env lock so the
        // off-mode regression test cannot poison this one in parallel.
        let _lock = crate::core::data_dir::test_env_lock();
        crate::test_env::set_var("LEAN_CTX_RULES_INJECTION", "shared");
        let tmp = std::env::temp_dir().join("lean-ctx-test-codex-preserve");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let agents_md = tmp.join("AGENTS.md");
        let user_content = "# My Custom Instructions\n\nDo not change my codebase style.\n\n## Rules\n- Always use tabs\n- No semicolons\n";
        std::fs::write(&agents_md, user_content).unwrap();

        crate::hooks::support::install_codex_instruction_docs(&tmp);

        let result = std::fs::read_to_string(&agents_md).unwrap();
        assert!(
            result.contains("My Custom Instructions"),
            "user content must be preserved"
        );
        assert!(
            result.contains("Always use tabs"),
            "user rules must be preserved"
        );
        assert!(
            result.contains(crate::core::rules_canonical::AGENTS_BLOCK_START),
            "lean-ctx block must be appended"
        );
        let expected_ref = tmp.join("LEAN-CTX.md").display().to_string();
        assert!(
            result.contains(&expected_ref),
            "lean-ctx reference must use codex_dir path"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn install_codex_docs_updates_only_marked_block() {
        // GH #1526 follow-up: install_codex_instruction_docs branches on
        // rules_injection now — pin Shared under the env lock so the
        // off-mode regression test cannot poison this one in parallel.
        let _lock = crate::core::data_dir::test_env_lock();
        crate::test_env::set_var("LEAN_CTX_RULES_INJECTION", "shared");
        let tmp = std::env::temp_dir().join("lean-ctx-test-codex-marked");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let agents_md = tmp.join("AGENTS.md");
        let content_with_block = format!(
            "# My Instructions\n\nCustom rule here.\n\n{}\n## lean-ctx\n\n@OLD-LEAN-CTX.md\n{}\n\n## Other Section\nKeep this.\n",
            crate::core::rules_canonical::AGENTS_BLOCK_START,
            crate::core::rules_canonical::AGENTS_BLOCK_END,
        );
        std::fs::write(&agents_md, content_with_block).unwrap();

        crate::hooks::support::install_codex_instruction_docs(&tmp);

        let result = std::fs::read_to_string(&agents_md).unwrap();
        assert!(
            result.contains("Custom rule here."),
            "user content before block preserved"
        );
        assert!(
            result.contains("Other Section"),
            "user content after block preserved"
        );
        let expected_ref = tmp.join("LEAN-CTX.md").display().to_string();
        assert!(
            result.contains(&expected_ref),
            "block updated to current reference"
        );
        assert!(
            !result.contains("OLD-LEAN-CTX"),
            "old block content replaced"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn ensure_mcp_server_adds_section_when_missing() {
        let input = "[features]\ncodex_hooks = true\n";
        let result = ensure_codex_mcp_server(input, "lean-ctx", &data_dir_pairs())
            .expect("should add MCP section");
        assert!(result.contains("[mcp_servers.lean-ctx]"));
        assert!(result.contains("command = \"lean-ctx\""));
        assert!(result.contains("args = [\"mcp\"]"));
        assert!(result.contains("[features]\ncodex_hooks = true\n"));
    }

    #[test]
    fn ensure_mcp_server_noop_when_already_complete() {
        // Parent + args + timeouts + an env block already carrying every desired
        // key: the upsert must be a true no-op (no churn on every session start).
        let input = "[mcp_servers.lean-ctx]\ncommand = \"lean-ctx\"\nargs = [\"mcp\"]\n\
                     startup_timeout_sec = 30\ntool_timeout_sec = 120\n\n\
                     [mcp_servers.lean-ctx.env]\nLEAN_CTX_DATA_DIR = \"/Users/user/.lean-ctx\"\n";
        assert!(
            ensure_codex_mcp_server(input, "lean-ctx", &data_dir_pairs()).is_none(),
            "should not modify config when MCP section already has all keys"
        );
    }

    #[test]
    fn ensure_mcp_server_upgrades_empty_args_to_mcp() {
        let input = "[mcp_servers.lean-ctx]\ncommand = \"lean-ctx\"\nargs = []\n\
                     startup_timeout_sec = 30\ntool_timeout_sec = 120\n\n\
                     [mcp_servers.lean-ctx.env]\nLEAN_CTX_DATA_DIR = \"/Users/user/.lean-ctx\"\n";
        let result = ensure_codex_mcp_server(input, "lean-ctx", &data_dir_pairs())
            .expect("should upgrade empty args to ['mcp']");
        assert!(result.contains("args = [\"mcp\"]"));
    }

    #[test]
    fn ensure_mcp_server_adds_timeouts() {
        let input = "[mcp_servers.lean-ctx]\ncommand = \"lean-ctx\"\nargs = [\"mcp\"]\n";
        let result = ensure_codex_mcp_server(input, "lean-ctx", &data_dir_pairs())
            .expect("should add timeouts");
        assert!(
            result.contains("startup_timeout_sec = 30"),
            "must set startup_timeout_sec"
        );
        assert!(
            result.contains("tool_timeout_sec = 120"),
            "must set tool_timeout_sec"
        );
    }

    #[test]
    fn ensure_mcp_server_preserves_existing_sections() {
        let input = "[mcp_servers.other]\ncommand = \"other\"\n";
        let result = ensure_codex_mcp_server(input, "/usr/bin/lean-ctx", &data_dir_pairs())
            .expect("should add lean-ctx section");
        assert!(result.contains("[mcp_servers.other]"));
        assert!(result.contains("[mcp_servers.lean-ctx]"));
        assert!(result.contains("command = \"/usr/bin/lean-ctx\""));
    }

    #[test]
    fn ensure_mcp_server_inserts_before_orphaned_env_subtable() {
        let input = "\
[mcp_servers.lean-ctx.env]
LEAN_CTX_DATA_DIR = \"/Users/user/.lean-ctx\"
";
        let result = ensure_codex_mcp_server(input, "/usr/local/bin/lean-ctx", &data_dir_pairs())
            .expect("should insert parent section before orphaned env");
        let parent_pos = result
            .find("[mcp_servers.lean-ctx]")
            .expect("parent section must exist");
        let env_pos = result
            .find("[mcp_servers.lean-ctx.env]")
            .expect("env sub-table must be preserved");
        assert!(
            parent_pos < env_pos,
            "parent section must come before env sub-table"
        );
        assert!(result.contains("command = \"/usr/local/bin/lean-ctx\""));
        assert!(result.contains("LEAN_CTX_DATA_DIR"));
        assert_eq!(
            result.matches("[mcp_servers.lean-ctx.env]").count(),
            1,
            "must not duplicate the env table (would be invalid TOML)"
        );
    }

    #[test]
    fn ensure_mcp_server_handles_issue_189_scenario() {
        let input = "\
source = \"/Users/user/.cache/codex-runtimes/codex-primary-runtime/plugins/openai-primary-runtime\"
source_type = \"local\"

[mcp_servers.lean-ctx.env]
LEAN_CTX_DATA_DIR = \"/Users/user/.lean-ctx\"
";
        let result = ensure_codex_mcp_server(input, "/usr/local/bin/lean-ctx", &data_dir_pairs())
            .expect("should fix orphaned config from issue #189");
        assert!(result.contains("[mcp_servers.lean-ctx]\n"));
        assert!(result.contains("command = \"/usr/local/bin/lean-ctx\""));
        assert!(result.contains("[mcp_servers.lean-ctx.env]"));
        assert!(result.contains("LEAN_CTX_DATA_DIR"));

        let parent_pos = result.find("[mcp_servers.lean-ctx]\n").unwrap();
        let env_pos = result.find("[mcp_servers.lean-ctx.env]").unwrap();
        assert!(parent_pos < env_pos);
        assert_eq!(
            result.matches("[mcp_servers.lean-ctx.env]").count(),
            1,
            "issue #189 fix must merge into one env table, not duplicate it"
        );
        // Original sibling content must survive the normalization.
        assert!(result.contains("source_type = \"local\""));
    }

    #[test]
    fn ensure_mcp_server_quotes_windows_backslash_paths() {
        let input = "[features]\ncodex_hooks = true\n";
        let win_path = r"C:\Users\Foo\AppData\Roaming\npm\lean-ctx.cmd";
        let result = ensure_codex_mcp_server(input, win_path, &data_dir_pairs())
            .expect("should add MCP section");
        // Quote style is the TOML editor's concern; what matters is that the
        // backslash path round-trips to exactly the same string and stays valid.
        let doc = result
            .parse::<toml_edit::DocumentMut>()
            .expect("output must be valid TOML");
        assert_eq!(
            doc["mcp_servers"]["lean-ctx"]["command"].as_str(),
            Some(win_path),
            "Windows backslash path must round-trip exactly: {result}"
        );
    }

    #[test]
    fn ensure_mcp_server_does_not_match_similarly_named_section() {
        let input = "\
[mcp_servers.lean-ctx-other]
command = \"other\"
";
        let result = ensure_codex_mcp_server(input, "lean-ctx", &data_dir_pairs())
            .expect("should add lean-ctx section despite similarly-named section");
        assert!(result.contains("[mcp_servers.lean-ctx]\n"));
        assert!(result.contains("[mcp_servers.lean-ctx-other]"));
    }

    #[test]
    fn ensure_mcp_server_writes_project_and_extra_roots() {
        // #403: when init captured a project root + sibling worktrees, those
        // must be propagated into the env block so the long-lived MCP server
        // resolves explicit paths under every root.
        let pairs = vec![
            (
                "LEAN_CTX_DATA_DIR".to_string(),
                "/home/u/.lean-ctx".to_string(),
            ),
            (
                "LEAN_CTX_PROJECT_ROOT".to_string(),
                "/work/main".to_string(),
            ),
            (
                "LEAN_CTX_EXTRA_ROOTS".to_string(),
                "/work/wt-a:/work/wt-b".to_string(),
            ),
        ];
        let result =
            ensure_codex_mcp_server("", "lean-ctx", &pairs).expect("fresh config must be created");

        let doc = result
            .parse::<toml_edit::DocumentMut>()
            .expect("output must be valid TOML");
        let env = &doc["mcp_servers"]["lean-ctx"]["env"];
        assert_eq!(env["LEAN_CTX_PROJECT_ROOT"].as_str(), Some("/work/main"));
        assert_eq!(
            env["LEAN_CTX_EXTRA_ROOTS"].as_str(),
            Some("/work/wt-a:/work/wt-b")
        );
        assert_eq!(env["LEAN_CTX_DATA_DIR"].as_str(), Some("/home/u/.lean-ctx"));
    }

    #[test]
    fn ensure_mcp_server_upserts_missing_keys_into_existing_env() {
        // Pre-existing install (only DATA_DIR) must gain the new roots without
        // duplicating the section, and the operation must be idempotent.
        let input = "[mcp_servers.lean-ctx]\ncommand = \"lean-ctx\"\nargs = []\n\n\
                     [mcp_servers.lean-ctx.env]\nLEAN_CTX_DATA_DIR = \"/home/u/.lean-ctx\"\n";
        let pairs = vec![
            (
                "LEAN_CTX_DATA_DIR".to_string(),
                "/home/u/.lean-ctx".to_string(),
            ),
            (
                "LEAN_CTX_PROJECT_ROOT".to_string(),
                "/work/main".to_string(),
            ),
        ];

        let result = ensure_codex_mcp_server(input, "lean-ctx", &pairs)
            .expect("should upsert the missing project root");
        assert_eq!(
            result.matches("[mcp_servers.lean-ctx]").count(),
            1,
            "must not duplicate the parent section"
        );
        let doc = result
            .parse::<toml_edit::DocumentMut>()
            .expect("output must be valid TOML");
        assert_eq!(
            doc["mcp_servers"]["lean-ctx"]["env"]["LEAN_CTX_PROJECT_ROOT"].as_str(),
            Some("/work/main")
        );

        // Second pass over the upserted config is a no-op.
        assert!(
            ensure_codex_mcp_server(&result, "lean-ctx", &pairs).is_none(),
            "upsert must be idempotent"
        );
    }

    #[test]
    fn observe_merges_into_existing_session_start_entry() {
        let mut root = serde_json::json!({
            "hooks": {
                "SessionStart": [{
                    "matcher": "startup|resume|clear",
                    "hooks": [{
                        "type": "command",
                        "command": "/bin/lean-ctx hook codex-session-start",
                        "timeout": 15
                    }]
                }]
            }
        });
        let changed = ensure_codex_observe_hooks(&mut root, "/bin/lean-ctx hook observe");
        assert!(changed);
        let entries = root["hooks"]["SessionStart"].as_array().unwrap();
        assert_eq!(entries.len(), 1, "must be single entry, not two");
        let hooks = entries[0]["hooks"].as_array().unwrap();
        assert_eq!(hooks.len(), 2, "session-start + observe in one entry");
        assert!(hooks[1]["command"].as_str().unwrap().contains("observe"));
        assert_eq!(hooks[1]["timeout"].as_u64(), Some(4));
    }

    #[test]
    fn observe_creates_new_entry_when_no_lean_ctx_exists() {
        let mut root = serde_json::json!({ "hooks": {} });
        let changed = ensure_codex_observe_hooks(&mut root, "/bin/lean-ctx hook observe");
        assert!(changed);
        let entries = root["hooks"]["PostToolUse"].as_array().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0]["matcher"].as_str().unwrap(),
            "Write|Edit|MultiEdit|StrReplace|str_replace_editor|edit_file|str_replace|replace_in_file|create_file|write_file"
        );
        assert_eq!(entries[0]["hooks"][0]["timeout"].as_u64(), Some(4));
    }

    #[test]
    fn observe_tightens_legacy_universal_post_tool_hook() {
        let mut root = serde_json::json!({
            "hooks": {
                "PostToolUse": [{
                    "matcher": ".*",
                    "hooks": [{
                        "type": "command",
                        "command": "/bin/lean-ctx hook observe",
                        "timeout": 5
                    }]
                }]
            }
        });

        assert!(ensure_codex_observe_hooks(
            &mut root,
            "/bin/lean-ctx hook observe"
        ));
        let entry = &root["hooks"]["PostToolUse"][0];
        assert_eq!(
            entry["matcher"].as_str(),
            Some(
                "Write|Edit|MultiEdit|StrReplace|str_replace_editor|edit_file|str_replace|replace_in_file|create_file|write_file"
            )
        );
        assert_eq!(entry["hooks"][0]["timeout"].as_u64(), Some(4));
    }

    #[test]
    fn observe_writes_codex_compatible_session_end_timeout() {
        let mut root = serde_json::json!({ "hooks": {} });
        assert!(ensure_codex_observe_hooks(
            &mut root,
            "/bin/lean-ctx hook observe"
        ));
        assert_eq!(
            root["hooks"]["SessionEnd"][0]["hooks"][0]["timeout"].as_u64(),
            Some(3)
        );
    }

    #[test]
    fn observe_standalone_migrated_into_lean_ctx_entry() {
        // Reproduces the bug: observe exists as a separate entry from
        // codex-session-start, causing duplicate "hook: SessionStart" logs.
        let mut root = serde_json::json!({
            "hooks": {
                "SessionStart": [
                    {
                        "matcher": ".*",
                        "hooks": [{
                            "type": "command",
                            "command": "/bin/lean-ctx hook observe",
                            "timeout": 5
                        }]
                    },
                    {
                        "matcher": "startup|resume|clear",
                        "hooks": [{
                            "type": "command",
                            "command": "/bin/lean-ctx hook codex-session-start",
                            "timeout": 15
                        }]
                    }
                ]
            }
        });
        let changed = ensure_codex_observe_hooks(&mut root, "/bin/lean-ctx hook observe");
        assert!(changed, "migration must report a change");
        let entries = root["hooks"]["SessionStart"].as_array().unwrap();
        assert_eq!(
            entries.len(),
            1,
            "standalone must be removed, leaving 1 entry"
        );
        let hooks = entries[0]["hooks"].as_array().unwrap();
        assert_eq!(
            hooks.len(),
            2,
            "session-start + observe merged in one entry"
        );
        assert!(
            hooks.iter().any(|h| h["command"]
                .as_str()
                .unwrap()
                .contains("codex-session-start")),
            "must keep codex-session-start"
        );
        assert!(
            hooks
                .iter()
                .any(|h| h["command"].as_str().unwrap().contains("hook observe")),
            "must keep observe"
        );
    }
}
