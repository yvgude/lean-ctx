use super::super::{
    HookMode, REDIRECT_SCRIPT_CLAUDE, generate_rewrite_script, make_executable,
    mcp_server_quiet_mode, resolve_binary_path, resolve_binary_path_for_bash, write_file,
};

pub(crate) fn install_claude_hook_with_mode(global: bool, mode: HookMode) {
    let Some(home) = crate::core::home::resolve_home_dir() else {
        tracing::error!("Cannot resolve home directory");
        return;
    };

    install_claude_hook_scripts(&home);
    install_claude_hook_config(&home);

    // #281: register the MCP server only when MCP updates are enabled. The hook
    // scripts/config above and the rules/skill below still install, so an
    // MCP-disabled setup keeps the CLI integration without an MCP entry.
    if matches!(mode, HookMode::Hybrid | HookMode::Mcp) && super::super::should_register_mcp() {
        install_claude_mcp_server(&home);
    }

    let scope = crate::core::config::Config::load().rules_scope_effective();
    if scope != crate::core::config::RulesScope::Project {
        remove_claude_rules_file(&home);
        install_claude_global_claude_md_for_mode(&home, mode);
        // rules_injection=off (#361): the functional hooks above still install
        // (off opts out of *instructions*, not compression), but the on-demand
        // skill is lean-ctx-authored steering — suppress it, and remove one a
        // previous shared/dedicated install left behind.
        if crate::core::config::Config::load().rules_injection_effective()
            == crate::core::config::RulesInjection::Off
        {
            remove_claude_skill(&home);
        } else {
            install_claude_skill(&home);
        }
    }

    let _ = global;
}

fn install_claude_mcp_server(home: &std::path::Path) {
    let config_path = crate::core::editor_registry::claude_mcp_json_path(home);
    let binary = super::super::resolve_binary_path();

    let existing = std::fs::read_to_string(&config_path).unwrap_or_default();
    if existing.contains("\"lean-ctx\"") && existing.contains("mcpServers") {
        return;
    }

    let parsed: Result<serde_json::Value, _> = if existing.trim().is_empty() {
        Ok(serde_json::json!({}))
    } else {
        crate::core::jsonc::parse_jsonc(&existing)
    };

    if let Ok(mut root) = parsed
        && let Some(obj) = root.as_object_mut()
    {
        let servers = obj
            .entry("mcpServers")
            .or_insert_with(|| serde_json::json!({}));
        if let Some(servers_obj) = servers.as_object_mut()
            && !servers_obj.contains_key("lean-ctx")
        {
            servers_obj.insert(
                "lean-ctx".to_string(),
                serde_json::json!({
                    "command": binary,
                    "args": []
                }),
            );
            write_file(
                &config_path,
                &serde_json::to_string_pretty(&root).unwrap_or_default(),
            );
            if !super::super::mcp_server_quiet_mode() {
                eprintln!("Added lean-ctx MCP server to {}", config_path.display());
            }
        }
    }
}

/// Shared with `doctor` so the instructions check recognises the same block
/// this installer writes (GH #396: doctor must not demand the retired rules file).
pub(crate) const CLAUDE_MD_BLOCK_START: &str = crate::core::rules_canonical::START_MARK;
const CLAUDE_MD_BLOCK_END: &str = crate::core::rules_canonical::END_MARK;
const CLAUDE_MD_BLOCK_VERSION: &str = "lean-ctx-claude-v3";

// v3 (GL #555): self-contained, no `@rules/lean-ctx.md` import. Claude Code
// expands `@` imports inline at launch ("imports do not reduce context usage"
// — code.claude.com/docs/en/memory), so the old pointer silently tripled the
// per-session footprint. Detail docs now live in the lean-ctx skill, which
// loads on demand only.
const CLAUDE_MD_BLOCK_CONTENT_MCP: &str = "\
<!-- lean-ctx -->
<!-- lean-ctx-claude-v3 -->
## lean-ctx — Context Runtime

Always prefer lean-ctx MCP tools over native equivalents:
- `ctx_read` instead of `Read` / `cat` (cached, 10 modes, re-reads ~13 tokens)
- `ctx_shell` instead of `bash` / `Shell` (95+ compression patterns)
- `ctx_search` instead of `Grep` / `rg` (compact results)
- `ctx_tree` instead of `ls` / `find` (compact directory maps)
- Native Edit/StrReplace stay unchanged. If Edit requires Read and Read is unavailable, use `ctx_edit(path, old_string, new_string)` instead.
- Write, Delete, Glob — use normally.

Read modes: full (edit), map (overview), signatures (API), diff (post-edit), lines:N-M (range), auto.
Details live in the `lean-ctx` skill (loads on demand — keep this file lean).
<!-- /lean-ctx -->";

fn install_claude_global_claude_md_for_mode(home: &std::path::Path, mode: HookMode) {
    let claude_dir = crate::core::editor_registry::claude_state_dir(home);
    let _ = std::fs::create_dir_all(&claude_dir);
    let claude_md_path = claude_dir.join("CLAUDE.md");

    // Neither dedicated nor off keep a lean-ctx block in the user's CLAUDE.md:
    //  - dedicated (#343): the SessionStart hook injects the compact summary;
    //  - off (#361): the user opted out of lean-ctx steering entirely.
    // Strip any block a previous shared install left so switching modes is clean.
    if matches!(
        crate::core::config::Config::load().rules_injection_effective(),
        crate::core::config::RulesInjection::Dedicated | crate::core::config::RulesInjection::Off
    ) {
        strip_claude_md_block(&claude_md_path);
        return;
    }

    let existing = std::fs::read_to_string(&claude_md_path).unwrap_or_default();
    let block = match mode {
        HookMode::Mcp | HookMode::Hybrid => CLAUDE_MD_BLOCK_CONTENT_MCP,
    };
    let block_version = match mode {
        HookMode::Mcp | HookMode::Hybrid => CLAUDE_MD_BLOCK_VERSION,
    };

    if existing.contains(CLAUDE_MD_BLOCK_START) {
        if existing.contains(block_version) {
            return;
        }
        let cleaned = remove_block(&existing, CLAUDE_MD_BLOCK_START, CLAUDE_MD_BLOCK_END);
        let updated = format!("{}\n\n{}\n", cleaned.trim(), block);
        write_file(&claude_md_path, &updated);
        return;
    }

    if existing.trim().is_empty() {
        write_file(&claude_md_path, block);
    } else {
        let updated = format!("{}\n\n{}\n", existing.trim(), block);
        write_file(&claude_md_path, &updated);
    }
}

/// Remove the lean-ctx block from `CLAUDE.md` (dedicated mode). Deletes the file
/// entirely if it becomes empty (i.e. lean-ctx was its only content).
fn strip_claude_md_block(claude_md_path: &std::path::Path) {
    let Ok(existing) = std::fs::read_to_string(claude_md_path) else {
        return;
    };
    if !existing.contains(CLAUDE_MD_BLOCK_START) {
        return;
    }
    let cleaned = remove_block(&existing, CLAUDE_MD_BLOCK_START, CLAUDE_MD_BLOCK_END);
    if cleaned.trim().is_empty() {
        let _ = std::fs::remove_file(claude_md_path);
    } else {
        write_file(claude_md_path, &format!("{}\n", cleaned.trim_end()));
    }
}

fn remove_block(content: &str, start: &str, end: &str) -> String {
    let s = content.find(start);
    let e = content.find(end);
    match (s, e) {
        (Some(si), Some(ei)) if ei >= si => {
            let after_end = ei + end.len();
            let before = content[..si].trim_end_matches('\n');
            let after = &content[after_end..];
            let mut out = before.to_string();
            out.push('\n');
            if !after.trim().is_empty() {
                out.push('\n');
                out.push_str(after.trim_start_matches('\n'));
            }
            out
        }
        _ => content.to_string(),
    }
}

fn install_claude_skill(home: &std::path::Path) {
    let skill_dir = home.join(".claude/skills/lean-ctx");
    let _ = std::fs::create_dir_all(skill_dir.join("scripts"));

    let skill_md = include_str!("../../templates/SKILL.md");
    let install_sh = include_str!("../../templates/skill_install.sh");

    let skill_path = skill_dir.join("SKILL.md");
    let script_path = skill_dir.join("scripts/install.sh");

    write_file(&skill_path, skill_md);
    write_file(&script_path, install_sh);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(mut perms) = std::fs::metadata(&script_path).map(|m| m.permissions()) {
            perms.set_mode(0o755);
            let _ = std::fs::set_permissions(&script_path, perms);
        }
    }
}

/// Remove the lean-ctx skill directory (`rules_injection=off`, GH #361). Only the
/// lean-ctx-owned `lean-ctx` skill folder is touched, so a user's other skills
/// are never affected.
fn remove_claude_skill(home: &std::path::Path) {
    let skill_dir = home.join(".claude/skills/lean-ctx");
    if skill_dir.exists()
        && std::fs::remove_dir_all(&skill_dir).is_ok()
        && !super::super::mcp_server_quiet_mode()
    {
        eprintln!(
            "Removed {} (rules_injection=off — instructions intentionally not installed)",
            skill_dir.display()
        );
    }
}

/// Remove the lean-ctx-owned `~/.claude/rules/lean-ctx.md` (GL #555).
///
/// Claude Code loads every rules file without `paths:` frontmatter
/// unconditionally at session start (code.claude.com/docs/en/memory), so this
/// file duplicated the CLAUDE.md block in *every* session — users reported
/// 12k+ tokens of memory files from stacked lean-ctx instructions. The
/// CLAUDE.md block is self-contained and detail docs live in the on-demand
/// skill; only files carrying our rules marker are touched.
fn remove_claude_rules_file(home: &std::path::Path) {
    let rules_path = crate::core::editor_registry::claude_rules_dir(home).join("lean-ctx.md");
    let Ok(existing) = std::fs::read_to_string(&rules_path) else {
        return;
    };
    if existing.contains(crate::core::rules_canonical::RULES_MARKER_PREFIX)
        && std::fs::remove_file(&rules_path).is_ok()
        && !super::super::mcp_server_quiet_mode()
    {
        eprintln!(
            "Removed {} (always-loaded duplicate; the CLAUDE.md block + on-demand skill replace it)",
            rules_path.display()
        );
    }
}

pub(crate) fn install_claude_hook_scripts(home: &std::path::Path) {
    let hooks_dir = crate::core::editor_registry::claude_state_dir(home).join("hooks");
    let _ = std::fs::create_dir_all(&hooks_dir);

    let binary = resolve_binary_path();

    let rewrite_path = hooks_dir.join("lean-ctx-rewrite.sh");
    let rewrite_script = generate_rewrite_script(&resolve_binary_path_for_bash());
    write_file(&rewrite_path, &rewrite_script);
    make_executable(&rewrite_path);

    let redirect_path = hooks_dir.join("lean-ctx-redirect.sh");
    write_file(&redirect_path, REDIRECT_SCRIPT_CLAUDE);
    make_executable(&redirect_path);

    let wrapper = |subcommand: &str| -> String {
        if cfg!(windows) {
            format!("{binary} hook {subcommand}")
        } else {
            format!("{} hook {subcommand}", resolve_binary_path_for_bash())
        }
    };

    let rewrite_native = hooks_dir.join("lean-ctx-rewrite-native");
    write_file(
        &rewrite_native,
        &format!(
            "#!/bin/sh\nexec {} hook rewrite\n",
            resolve_binary_path_for_bash()
        ),
    );
    make_executable(&rewrite_native);

    let redirect_native = hooks_dir.join("lean-ctx-redirect-native");
    write_file(
        &redirect_native,
        &format!(
            "#!/bin/sh\nexec {} hook redirect\n",
            resolve_binary_path_for_bash()
        ),
    );
    make_executable(&redirect_native);

    let _ = wrapper; // suppress unused warning on unix
}

/// The `Read|…` tool matcher shared by every Claude redirect hook (global + project).
const REDIRECT_MATCHER: &str = "Read|read|ReadFile|read_file|View|view|Grep|grep|Search|search|ListFiles|list_files|ListDirectory|list_directory";

/// The trailing action token of a lean-ctx hook command, e.g. `"hook rewrite"`.
///
/// Commands are rendered as `<binary> hook <action>`, where `<binary>` is either a bare
/// `lean-ctx` (when it is on `PATH`) or an absolute path (when it is not). The action token
/// is therefore the only path-independent way to recognise a lean-ctx hook.
fn lean_ctx_action_token(command: &str) -> &str {
    match command.rfind(" hook ") {
        Some(i) => command[i + 1..].trim_end(),
        None => command.trim_end(),
    }
}

/// True if `hook` is a lean-ctx command hook for `action`, regardless of how the binary path
/// was rendered (bare `lean-ctx` vs an absolute path) and including the legacy script form
/// (`…/lean-ctx-rewrite.sh`) written by pre-3.x installs.
fn is_lean_ctx_command_for(hook: &serde_json::Value, action: &str) -> bool {
    if hook.get("type").and_then(|t| t.as_str()) != Some("command") {
        return false;
    }
    let Some(cmd) = hook.get("command").and_then(|c| c.as_str()) else {
        return false;
    };
    if !cmd.contains("lean-ctx") {
        return false;
    }
    if cmd.trim_end().ends_with(action) {
        return true;
    }
    let legacy = if action.ends_with("rewrite") {
        "lean-ctx-rewrite"
    } else if action.ends_with("redirect") {
        "lean-ctx-redirect"
    } else {
        return false;
    };
    cmd.contains(legacy)
}

/// Ensure exactly one lean-ctx hook for `command`'s action exists under `matcher`.
///
/// Earlier versions deduped on the *full* command string, but `resolve_binary_path()` renders
/// the binary as a bare `lean-ctx` (on `PATH`) or an absolute path (off `PATH`), so the
/// rendering flips between `install` and `update` and the exact-string compare missed the
/// existing hook — appending a duplicate on every run. We now strip every stale lean-ctx hook
/// for this action (any path, any matcher group, including legacy `.sh` hooks) and re-add a
/// single fresh one. This is idempotent across re-runs *and* self-heals settings files already
/// duplicated by older versions, while leaving any non-lean-ctx hooks untouched.
fn ensure_command_hook(pre_arr: &mut Vec<serde_json::Value>, matcher: &str, command: &str) {
    let action = lean_ctx_action_token(command);

    for group in pre_arr.iter_mut() {
        if let Some(hooks) = group.get_mut("hooks").and_then(|h| h.as_array_mut()) {
            hooks.retain(|h| !is_lean_ctx_command_for(h, action));
        }
    }
    pre_arr.retain(|g| {
        g.get("hooks")
            .and_then(|h| h.as_array())
            .is_none_or(|hooks| !hooks.is_empty())
    });

    let desired = serde_json::json!({ "type": "command", "command": command });
    if let Some(group) = pre_arr
        .iter_mut()
        .find(|g| g.get("matcher").and_then(|m| m.as_str()) == Some(matcher))
    {
        if let Some(obj) = group.as_object_mut() {
            match obj
                .entry("hooks".to_string())
                .or_insert_with(|| serde_json::json!([]))
                .as_array_mut()
            {
                Some(hooks) => hooks.push(desired),
                None => {
                    obj.insert("hooks".to_string(), serde_json::json!([desired]));
                }
            }
        }
        return;
    }
    pre_arr.push(serde_json::json!({ "matcher": matcher, "hooks": [desired] }));
}

fn ensure_claude_observe_hooks(
    hooks_obj: &mut serde_json::Map<String, serde_json::Value>,
    observe_cmd: &str,
) {
    let observe_hook = serde_json::json!([{
        "matcher": ".*",
        "hooks": [{ "type": "command", "command": observe_cmd }]
    }]);

    let observe_events = [
        "PostToolUse",
        "UserPromptSubmit",
        "Stop",
        "PreCompact",
        "SessionStart",
        "SessionEnd",
    ];

    for event in observe_events {
        let entry = hooks_obj
            .entry(event.to_string())
            .or_insert_with(|| serde_json::json!([]));

        if let Some(arr) = entry.as_array() {
            let already = arr.iter().any(|group| {
                group
                    .get("hooks")
                    .and_then(|h| h.as_array())
                    .is_some_and(|hooks| {
                        hooks.iter().any(|hook| {
                            hook.get("command")
                                .and_then(|c| c.as_str())
                                .is_some_and(|c| c.contains("hook observe"))
                        })
                    })
            });
            if already {
                continue;
            }
        }

        if let Some(arr) = entry.as_array_mut() {
            arr.push(serde_json::json!({
                "matcher": ".*",
                "hooks": [{ "type": "command", "command": observe_cmd }]
            }));
        } else {
            *entry = observe_hook.clone();
        }
    }
}

pub(crate) fn install_claude_hook_config(home: &std::path::Path) {
    let hooks_dir = crate::core::editor_registry::claude_state_dir(home).join("hooks");
    let binary = resolve_binary_path();

    let rewrite_cmd = format!("{binary} hook rewrite");
    let redirect_cmd = format!("{binary} hook redirect");
    let observe_cmd = format!("{binary} hook observe");

    let settings_path = crate::core::editor_registry::claude_state_dir(home).join("settings.json");
    let settings_content = if settings_path.exists() {
        std::fs::read_to_string(&settings_path).unwrap_or_default()
    } else {
        String::new()
    };

    let bash_matcher = if cfg!(windows) {
        "Bash|bash|PowerShell|powershell"
    } else {
        "Bash|bash"
    };

    let desired_pretooluse = serde_json::json!([
        {
            "matcher": bash_matcher,
            "hooks": [{
                "type": "command",
                "command": rewrite_cmd
            }]
        },
        {
            "matcher": REDIRECT_MATCHER,
            "hooks": [{
                "type": "command",
                "command": redirect_cmd
            }]
        }
    ]);

    if settings_content.is_empty() {
        let mut hook_map = serde_json::Map::new();
        hook_map.insert("PreToolUse".to_string(), desired_pretooluse);
        ensure_claude_observe_hooks(&mut hook_map, &observe_cmd);
        let hook_entry = serde_json::json!({ "hooks": serde_json::Value::Object(hook_map) });
        write_file(
            &settings_path,
            &serde_json::to_string_pretty(&hook_entry).unwrap_or_default(),
        );
    } else if let Ok(mut existing) = crate::core::jsonc::parse_jsonc(&settings_content) {
        let before = serde_json::to_string_pretty(&existing).unwrap_or_default();
        if let Some(root) = existing.as_object_mut() {
            let hooks = root
                .entry("hooks".to_string())
                .or_insert_with(|| serde_json::json!({}));
            if let Some(hooks_obj) = hooks.as_object_mut() {
                let pre = hooks_obj
                    .entry("PreToolUse".to_string())
                    .or_insert_with(|| serde_json::json!([]));
                if let Some(pre_arr) = pre.as_array_mut() {
                    ensure_command_hook(pre_arr, bash_matcher, &rewrite_cmd);
                    ensure_command_hook(pre_arr, REDIRECT_MATCHER, &redirect_cmd);
                }
                ensure_claude_observe_hooks(hooks_obj, &observe_cmd);
            }
        }
        // Only rewrite (with backup) when the merge actually changed something, so a
        // no-op `update` doesn't churn the user's settings.json or pile up backups.
        let after = serde_json::to_string_pretty(&existing).unwrap_or_default();
        if after != before {
            write_file(&settings_path, &after);
        }
    }
    if !mcp_server_quiet_mode() {
        eprintln!("Installed Claude Code hooks at {}", hooks_dir.display());
    }
}

pub(crate) fn install_claude_project_hooks(cwd: &std::path::Path) {
    let binary = resolve_binary_path();
    let rewrite_cmd = format!("{binary} hook rewrite");
    let redirect_cmd = format!("{binary} hook redirect");
    let observe_cmd = format!("{binary} hook observe");

    let settings_path = cwd.join(".claude").join("settings.local.json");
    let _ = std::fs::create_dir_all(cwd.join(".claude"));

    let existing = std::fs::read_to_string(&settings_path).unwrap_or_default();
    let bash_matcher = if cfg!(windows) {
        "Bash|bash|PowerShell|powershell"
    } else {
        "Bash|bash"
    };

    let desired_pretooluse = serde_json::json!([
        {
            "matcher": bash_matcher,
            "hooks": [{
                "type": "command",
                "command": rewrite_cmd
            }]
        },
        {
            "matcher": REDIRECT_MATCHER,
            "hooks": [{
                "type": "command",
                "command": redirect_cmd
            }]
        }
    ]);

    if existing.is_empty() {
        let mut hook_map = serde_json::Map::new();
        hook_map.insert("PreToolUse".to_string(), desired_pretooluse);
        ensure_claude_project_observe_hooks(&mut hook_map, &observe_cmd);
        let hook_entry = serde_json::json!({ "hooks": serde_json::Value::Object(hook_map) });
        write_file(
            &settings_path,
            &serde_json::to_string_pretty(&hook_entry).unwrap_or_default(),
        );
    } else if let Ok(mut json) = crate::core::jsonc::parse_jsonc(&existing) {
        let before = serde_json::to_string_pretty(&json).unwrap_or_default();
        if let Some(root) = json.as_object_mut() {
            let hooks = root
                .entry("hooks".to_string())
                .or_insert_with(|| serde_json::json!({}));
            if let Some(hooks_obj) = hooks.as_object_mut() {
                let pre = hooks_obj
                    .entry("PreToolUse".to_string())
                    .or_insert_with(|| serde_json::json!([]));
                if let Some(pre_arr) = pre.as_array_mut() {
                    ensure_command_hook(pre_arr, bash_matcher, &rewrite_cmd);
                    ensure_command_hook(pre_arr, REDIRECT_MATCHER, &redirect_cmd);
                }
                ensure_claude_project_observe_hooks(hooks_obj, &observe_cmd);
            }
        }
        let after = serde_json::to_string_pretty(&json).unwrap_or_default();
        if after != before {
            write_file(&settings_path, &after);
        }
    }
    if !mcp_server_quiet_mode() {
        eprintln!("Created .claude/settings.local.json (project-local hooks with observe).");
    }
}

/// Project-level observe hooks — excludes SessionStart/SessionEnd (global-only).
fn ensure_claude_project_observe_hooks(
    hooks_obj: &mut serde_json::Map<String, serde_json::Value>,
    observe_cmd: &str,
) {
    let project_events = ["PostToolUse", "UserPromptSubmit", "Stop", "PreCompact"];
    for event in project_events {
        let entry = hooks_obj
            .entry(event.to_string())
            .or_insert_with(|| serde_json::json!([]));

        if let Some(arr) = entry.as_array() {
            let already = arr.iter().any(|group| {
                group
                    .get("hooks")
                    .and_then(|h| h.as_array())
                    .is_some_and(|hooks| {
                        hooks.iter().any(|hook| {
                            hook.get("command")
                                .and_then(|c| c.as_str())
                                .is_some_and(|c| c.contains("hook observe"))
                        })
                    })
            });
            if already {
                continue;
            }
        }

        if let Some(arr) = entry.as_array_mut() {
            arr.push(serde_json::json!({
                "matcher": ".*",
                "hooks": [{ "type": "command", "command": observe_cmd }]
            }));
        } else {
            *entry = serde_json::json!([{
                "matcher": ".*",
                "hooks": [{ "type": "command", "command": observe_cmd }]
            }]);
        }
    }
}

#[cfg(test)]
mod tests {
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
        // touch only the lean-ctx skill folder.
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        install_claude_skill(home);
        assert!(home.join(".claude/skills/lean-ctx/SKILL.md").exists());
        remove_claude_skill(home);
        assert!(!home.join(".claude/skills/lean-ctx").exists());
    }
}
