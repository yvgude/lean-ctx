use super::super::{
    HookMode, REDIRECT_SCRIPT_CLAUDE, generate_rewrite_script, make_executable,
    mcp_server_quiet_mode, resolve_binary_path_for_bash, resolve_hook_command_binary,
    shell_quoted_binary, write_file, write_wrapper_file,
};
use super::shared::{contains_managed_md_block, managed_md_is_current, remove_managed_md_blocks};

/// CodeBuddy shares Claude Code's `permissions.deny` contract: a listed tool
/// leaves the model's tool list, so hiding costs no refusal. Grep/Glob are
/// hidden whenever [`crate::hooks::claude_hides_native_search`] says so; Bash
/// only in Replace — hiding it elsewhere would take the native shell away, so
/// a bare `Bash` entry an earlier Replace default wrote is removed.
pub(crate) fn install_codebuddy_permissions_deny_replace(home: &std::path::Path, mode: HookMode) {
    let settings_path =
        crate::core::editor_registry::codebuddy_state_dir(home).join("settings.json");

    let mut json = if settings_path.exists() {
        let content = std::fs::read_to_string(&settings_path).unwrap_or_default();
        if content.trim().is_empty() {
            serde_json::json!({})
        } else {
            crate::core::jsonc::parse_jsonc(&content).unwrap_or_else(|_| serde_json::json!({}))
        }
    } else {
        let _ = std::fs::create_dir_all(settings_path.parent().unwrap_or(home.as_ref()));
        serde_json::json!({})
    };

    let Some(obj) = json.as_object_mut() else {
        return;
    };

    let permissions = obj
        .entry("permissions")
        .or_insert_with(|| serde_json::json!({}));
    let Some(perm_obj) = permissions.as_object_mut() else {
        return;
    };
    let deny = perm_obj
        .entry("deny")
        .or_insert_with(|| serde_json::json!([]));
    let Some(arr) = deny.as_array_mut() else {
        return;
    };

    // Read kept available for edit gate + auto memory (shared Claude hook
    // contract, GH #1228).
    let deny_tools: &[&str] = if mode == HookMode::Replace {
        &["Grep", "Glob", "Bash"]
    } else {
        &["Grep", "Glob"]
    };
    let mut changed = false;
    for tool in deny_tools {
        let val = serde_json::Value::String((*tool).to_string());
        if !arr.contains(&val) {
            arr.push(val);
            changed = true;
        }
    }

    let released: &[&str] = if mode == HookMode::Replace {
        &["Read"]
    } else {
        &["Read", "Bash"]
    };
    for tool in released {
        let val = serde_json::Value::String((*tool).to_string());
        if let Some(pos) = arr.iter().position(|v| v == &val) {
            arr.remove(pos);
            changed = true;
        }
    }

    if changed {
        if let Ok(out) = serde_json::to_string_pretty(&json) {
            let _ = std::fs::write(&settings_path, out);
        }
        if !mcp_server_quiet_mode() {
            eprintln!(
                "  \x1b[32m✓\x1b[0m CodeBuddy permissions.deny updated (native Grep/Glob hidden; Read kept for auto memory)"
            );
        }
    }
}

pub(crate) fn install_codebuddy_hook_with_mode(global: bool, mode: HookMode) {
    let Some(home) = crate::core::home::resolve_home_dir() else {
        tracing::error!("Cannot resolve home directory");
        return;
    };

    install_codebuddy_hook_scripts(&home);
    install_codebuddy_hook_config(&home);

    if matches!(mode, HookMode::Hybrid | HookMode::Mcp | HookMode::Replace) {
        install_codebuddy_mcp_server(&home);
    }

    if super::super::claude_hides_native_search(mode) {
        install_codebuddy_permissions_deny_replace(&home, mode);
    }

    let scope = crate::core::config::Config::load().rules_scope_effective();
    if scope != crate::core::config::RulesScope::Project {
        remove_codebuddy_rules_file(&home);
        install_codebuddy_global_codebuddy_md_for_mode(&home, mode);
        // rules_injection=off (#361): the functional hooks above still install
        // (off opts out of *instructions*, not compression), but the on-demand
        // skill is lean-ctx-authored steering — suppress it, and remove one a
        // previous shared/dedicated install left behind.
        if crate::core::config::Config::load().rules_injection_effective()
            == crate::core::config::RulesInjection::Off
        {
            remove_codebuddy_skill(&home);
        } else {
            install_codebuddy_skill(&home);
        }
    }

    let _ = global;
}

fn install_codebuddy_mcp_server(home: &std::path::Path) {
    let config_path = crate::core::editor_registry::codebuddy_mcp_json_path(home);
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
/// this installer writes. The CODEBUDDY.md block is a lightweight *pointer*
/// block, so it uses `AGENTS_BLOCK_START`/`END` (`<!-- lean-ctx -->`), not the
/// full-rules markers (`<!-- lean-ctx-rules -->`) — pointing them at the rules
/// markers broke detection and caused duplicate blocks (GH #549).
pub(crate) const CODEBUDDY_MD_BLOCK_START: &str = crate::core::rules_canonical::AGENTS_BLOCK_START;
const CODEBUDDY_MD_BLOCK_END: &str = crate::core::rules_canonical::AGENTS_BLOCK_END;
const CODEBUDDY_MD_BLOCK_VERSION: &str = "lean-ctx-codebuddy-v4";

// v2 (GH #637 / GL #1138): MCP-aware guidance, mirroring the Claude v4 block.
// CodeBuddy is a Claude Code fork with the same path-keyed read-before-write
// gate, so native Read → Edit is a guard-safe editing path and `ctx_edit` is
// only advertised when the ctx_* tools actually exist in the session.
//
// v3 (#1008 / GL #1144): anchored editing routes to `ctx_patch` (now advertised
// in the lazy core for CodeBuddy); `ctx_edit` demoted to a legacy power-profile
// mention. Native Read → Edit stays fully supported (v2 guard semantics).
//
// v4 (#1228): stop denying native Read — auto memory + edit gate need it.
const CODEBUDDY_MD_BLOCK_CONTENT_REPLACE: &str = "\
<!-- lean-ctx -->
<!-- lean-ctx-codebuddy-v4 -->
## lean-ctx — Replace Mode (native Grep/Glob/Bash denied by policy)

Native Grep/Glob/Bash are denied by policy. Prefer `ctx_*` MCP tools for project work:
- `ctx_read` for exploration reads (cached, 10 modes, unchanged full/auto re-reads ~13 tokens)
- `ctx_shell` for shell commands (95+ compression patterns)
- `ctx_search` instead of Grep/rg (compact results)
- `ctx_tree` instead of ls/find (compact directory maps)
- `ctx_glob` instead of Glob (file pattern matching)
- Project edits: `ctx_read(mode=\"anchored\")` → `ctx_patch` (line+hash anchors; `op=create` for new files).

Native `Read` stays available for the edit gate and for auto memory under
`~/.claude/projects/<slug>/memory/`. Use native Read/Edit there; do NOT call
MCP `resources/read` with file:// URIs. Write and Delete — use native tools normally.

Read modes: anchored (edit), full (verbatim), map (overview), signatures (API), diff (post-edit), lines:N-M (range), auto.
Details live in the `lean-ctx` skill (loads on demand — keep this file lean).
<!-- /lean-ctx -->";

const CODEBUDDY_MD_BLOCK_CONTENT_MCP: &str = "\
<!-- lean-ctx -->
<!-- lean-ctx-codebuddy-v4 -->
## lean-ctx — Context Runtime

When the `ctx_*` MCP tools are listed in this session, prefer them over native equivalents:
- `ctx_read` instead of `Read` / `cat` for exploration (cached, 10 modes, unchanged full/auto re-reads ~13 tokens)
- `ctx_shell` instead of `bash` / `Shell` (95+ compression patterns)
- `ctx_search` instead of `Grep` / `rg` (compact results)
- `ctx_tree` instead of `ls` / `find` (compact directory maps)
- Edits: `ctx_read(mode=\"anchored\")` → `ctx_patch` (line+hash anchors, never echo old text; `op=create` for new files). `ctx_edit` (str_replace) is the legacy power-profile fallback.

Native `Read` → `Edit`/`StrReplace` stays fully supported — the edit gate requires a
prior native Read of the same file path. Auto memory under
`~/.claude/projects/<slug>/memory/` also uses native Read/Edit (not MCP resources).
Write, Delete, Glob — use normally.
If no `ctx_*` tools are listed in this session, use the native tools throughout.

Read modes: anchored (edit), full (verbatim), map (overview), signatures (API), diff (post-edit), lines:N-M (range), auto.
Details live in the `lean-ctx` skill (loads on demand — keep this file lean).
<!-- /lean-ctx -->";

fn install_codebuddy_global_codebuddy_md_for_mode(home: &std::path::Path, mode: HookMode) {
    let codebuddy_dir = crate::core::editor_registry::codebuddy_state_dir(home);
    let _ = std::fs::create_dir_all(&codebuddy_dir);
    let codebuddy_md_path = codebuddy_dir.join("CODEBUDDY.md");

    // Neither dedicated nor off keep a lean-ctx block in CODEBUDDY.md:
    //  - dedicated (#343): the SessionStart hook injects the compact summary;
    //  - off (#361): the user opted out of lean-ctx steering entirely.
    // Strip any block a previous shared install left so switching modes is clean.
    if matches!(
        crate::core::config::Config::load().rules_injection_effective(),
        crate::core::config::RulesInjection::Dedicated | crate::core::config::RulesInjection::Off
    ) {
        strip_codebuddy_md_block(&codebuddy_md_path);
        return;
    }

    let existing = std::fs::read_to_string(&codebuddy_md_path).unwrap_or_default();
    let base_block = match mode {
        HookMode::Replace => CODEBUDDY_MD_BLOCK_CONTENT_REPLACE,
        HookMode::Mcp | HookMode::Hybrid => CODEBUDDY_MD_BLOCK_CONTENT_MCP,
    };

    // A single up-to-date block needs no rewrite. Check both version tag AND
    // mode-specific content — the Replace and MCP blocks share the version tag
    // but have different instructions.
    let solution_block = crate::core::rules_canonical::enabled_solution_rules_block();
    let block = if solution_block.is_empty() {
        base_block.to_owned()
    } else {
        format!("{base_block}\n\n{solution_block}")
    };
    let is_replace_block = existing.contains("denied by policy");
    let mode_matches = matches!(mode, HookMode::Replace) == is_replace_block;
    if managed_md_is_current(&existing, CODEBUDDY_MD_BLOCK_START, &block)
        && existing.contains(CODEBUDDY_MD_BLOCK_VERSION)
        && mode_matches
    {
        return;
    }
    let cleaned =
        remove_managed_md_blocks(&existing, CODEBUDDY_MD_BLOCK_START, CODEBUDDY_MD_BLOCK_END);
    let cleaned = cleaned.trim();
    let updated = if cleaned.is_empty() {
        format!("{block}\n")
    } else {
        format!("{cleaned}\n\n{block}\n")
    };
    write_file(&codebuddy_md_path, &updated);
}

fn strip_codebuddy_md_block(codebuddy_md_path: &std::path::Path) {
    let Ok(existing) = std::fs::read_to_string(codebuddy_md_path) else {
        return;
    };
    if !contains_managed_md_block(&existing, CODEBUDDY_MD_BLOCK_START) {
        return;
    }
    let cleaned =
        remove_managed_md_blocks(&existing, CODEBUDDY_MD_BLOCK_START, CODEBUDDY_MD_BLOCK_END);
    if cleaned.trim().is_empty() {
        let _ = std::fs::remove_file(codebuddy_md_path);
    } else {
        write_file(codebuddy_md_path, &format!("{}\n", cleaned.trim_end()));
    }
}

/// Remove the lean-ctx-owned `~/.codebuddy/rules/lean-ctx.md` (GL #555/#558).
///
/// CodeBuddy auto-loads every `~/.codebuddy/rules/*.md` file unconditionally at
/// session start, so this file duplicated the CODEBUDDY.md block in every session.
/// The CODEBUDDY.md block is self-contained and detail docs live in the on-demand
/// skill; only files carrying our rules marker are touched.
fn remove_codebuddy_rules_file(home: &std::path::Path) {
    let rules_path = crate::core::editor_registry::codebuddy_rules_dir(home).join("lean-ctx.md");
    let Ok(existing) = std::fs::read_to_string(&rules_path) else {
        return;
    };
    if existing.contains(crate::core::rules_canonical::RULES_MARKER_PREFIX)
        && std::fs::remove_file(&rules_path).is_ok()
        && !super::super::mcp_server_quiet_mode()
    {
        eprintln!(
            "Removed {} (always-loaded duplicate; CODEBUDDY.md block + skill replace it)",
            rules_path.display()
        );
    }
}

fn install_codebuddy_skill(home: &std::path::Path) {
    let skill_dir = home.join(".codebuddy/skills/lean-ctx");
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
/// lean-ctx-owned `lean-ctx` skill folder is touched.
fn remove_codebuddy_skill(home: &std::path::Path) {
    let skill_dir = home.join(".codebuddy/skills/lean-ctx");
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

pub(crate) fn install_codebuddy_hook_scripts(home: &std::path::Path) {
    let hooks_dir = crate::core::editor_registry::codebuddy_state_dir(home).join("hooks");
    let _ = std::fs::create_dir_all(&hooks_dir);

    // #719: wrapper writes go through write_wrapper_file — a synced portable
    // wrapper is never re-stamped with a machine-absolute path, and the
    // binary token is shell-quoted.
    let rewrite_path = hooks_dir.join("lean-ctx-rewrite.sh");
    let rewrite_script = generate_rewrite_script(&resolve_binary_path_for_bash());
    write_wrapper_file(&rewrite_path, &rewrite_script, home);
    make_executable(&rewrite_path);

    let redirect_path = hooks_dir.join("lean-ctx-redirect.sh");
    write_file(&redirect_path, REDIRECT_SCRIPT_CLAUDE);
    make_executable(&redirect_path);

    let rewrite_native = hooks_dir.join("lean-ctx-rewrite-native");
    write_wrapper_file(
        &rewrite_native,
        &format!(
            "#!/bin/sh\nexec {} hook rewrite\n",
            shell_quoted_binary(&resolve_binary_path_for_bash())
        ),
        home,
    );
    make_executable(&rewrite_native);

    let redirect_native = hooks_dir.join("lean-ctx-redirect-native");
    write_wrapper_file(
        &redirect_native,
        &format!(
            "#!/bin/sh\nexec {} hook redirect\n",
            shell_quoted_binary(&resolve_binary_path_for_bash())
        ),
        home,
    );
    make_executable(&redirect_native);
}

const REDIRECT_MATCHER: &str = "Read|read|ReadFile|read_file|View|view|Grep|grep|Search|search|ListFiles|list_files|ListDirectory|list_directory|Glob|glob";

/// PostToolUse matcher for the guard-safe re-read dedup (GL #1140): the same
/// set as the Claude installer (native Read and lean-ctx `ctx_read`).
const READ_DEDUP_MATCHER: &str = super::claude::READ_DEDUP_MATCHER;

/// Ensure the PostToolUse `hook read-dedup` entry exists exactly once (GL #1140).
/// CodeBuddy shares Claude Code's hook contract and read-before-write guard.
fn ensure_read_dedup_hook(
    hooks_obj: &mut serde_json::Map<String, serde_json::Value>,
    read_dedup_cmd: &str,
) {
    let post = hooks_obj
        .entry("PostToolUse".to_string())
        .or_insert_with(|| serde_json::json!([]));
    if let Some(post_arr) = post.as_array_mut() {
        ensure_command_hook(post_arr, READ_DEDUP_MATCHER, read_dedup_cmd);
    }
}

fn lean_ctx_action_token(command: &str) -> &str {
    match command.rfind(" hook ") {
        Some(i) => command[i + 1..].trim_end(),
        None => command.trim_end(),
    }
}

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

fn ensure_codebuddy_observe_hooks(
    hooks_obj: &mut serde_json::Map<String, serde_json::Value>,
    observe_cmd: &str,
) {
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
            *entry = serde_json::json!([{
                "matcher": ".*",
                "hooks": [{ "type": "command", "command": observe_cmd }]
            }]);
        }
    }
}

pub(crate) fn install_codebuddy_hook_config(home: &std::path::Path) {
    let hooks_dir = crate::core::editor_registry::codebuddy_state_dir(home).join("hooks");
    let binary = resolve_hook_command_binary();

    let rewrite_cmd = format!("{binary} hook rewrite");
    let redirect_cmd = format!("{binary} hook redirect");
    let observe_cmd = format!("{binary} hook observe");
    let read_dedup_cmd = format!("{binary} hook read-dedup");

    let settings_path =
        crate::core::editor_registry::codebuddy_state_dir(home).join("settings.json");
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
        ensure_codebuddy_observe_hooks(&mut hook_map, &observe_cmd);
        ensure_read_dedup_hook(&mut hook_map, &read_dedup_cmd);
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
                ensure_codebuddy_observe_hooks(hooks_obj, &observe_cmd);
                ensure_read_dedup_hook(hooks_obj, &read_dedup_cmd);
            }
        }
        let after = serde_json::to_string_pretty(&existing).unwrap_or_default();
        if after != before {
            write_file(&settings_path, &after);
        }
    }
    if !mcp_server_quiet_mode() {
        eprintln!("Installed CodeBuddy hooks at {}", hooks_dir.display());
    }
}

pub(crate) fn install_codebuddy_project_hooks(cwd: &std::path::Path) {
    let binary = resolve_hook_command_binary();
    let rewrite_cmd = format!("{binary} hook rewrite");
    let redirect_cmd = format!("{binary} hook redirect");
    let observe_cmd = format!("{binary} hook observe");
    let read_dedup_cmd = format!("{binary} hook read-dedup");

    let settings_path = cwd.join(".codebuddy").join("settings.local.json");
    let _ = std::fs::create_dir_all(cwd.join(".codebuddy"));

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
        ensure_codebuddy_project_observe_hooks(&mut hook_map, &observe_cmd);
        ensure_read_dedup_hook(&mut hook_map, &read_dedup_cmd);
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
                ensure_codebuddy_project_observe_hooks(hooks_obj, &observe_cmd);
                ensure_read_dedup_hook(hooks_obj, &read_dedup_cmd);
            }
        }
        let after = serde_json::to_string_pretty(&json).unwrap_or_default();
        if after != before {
            write_file(&settings_path, &after);
        }
    }
    if !mcp_server_quiet_mode() {
        eprintln!("Created .codebuddy/settings.local.json (project-local hooks with observe).");
    }
}

fn ensure_codebuddy_project_observe_hooks(
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

    #[test]
    fn rules_injection_off_strips_codebuddy_md_block() {
        // #361: with instructions opted out, no lean-ctx block may remain in
        // CODEBUDDY.md (and one left by a prior install must be stripped).
        let _lock = crate::core::data_dir::test_env_lock();
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let dir = crate::core::editor_registry::codebuddy_state_dir(home);
        std::fs::create_dir_all(&dir).unwrap();
        let md = dir.join("CODEBUDDY.md");
        std::fs::write(
            &md,
            format!("# my notes\n\n{CODEBUDDY_MD_BLOCK_CONTENT_MCP}\n"),
        )
        .unwrap();

        crate::test_env::set_var("LEAN_CTX_RULES_INJECTION", "off");
        install_codebuddy_global_codebuddy_md_for_mode(home, HookMode::Mcp);
        crate::test_env::remove_var("LEAN_CTX_RULES_INJECTION");

        let after = std::fs::read_to_string(&md).unwrap_or_default();
        assert!(
            !after.contains(CODEBUDDY_MD_BLOCK_START),
            "rules_injection=off must strip the CODEBUDDY.md block, got:\n{after}"
        );
        assert!(after.contains("# my notes"), "user content must survive");
    }

    /// Grep/Glob stay hidden by default (no refusal, compressed searches);
    /// Bash only under Replace — a bare entry the old Replace default wrote is
    /// released so the native shell comes back.
    #[test]
    fn default_mode_hides_search_but_gives_bash_back() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let dir = crate::core::editor_registry::codebuddy_state_dir(home);
        std::fs::create_dir_all(&dir).unwrap();
        let settings = dir.join("settings.json");
        let deny = |path: &std::path::Path| -> Vec<String> {
            let json: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
            json["permissions"]["deny"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        };

        install_codebuddy_permissions_deny_replace(home, HookMode::Replace);
        assert_eq!(deny(&settings), ["Grep", "Glob", "Bash"]);

        std::fs::write(
            &settings,
            r#"{"permissions":{"deny":["Grep","Glob","Bash","Bash(rm:*)"]}}"#,
        )
        .unwrap();
        install_codebuddy_permissions_deny_replace(home, HookMode::Hybrid);
        assert_eq!(deny(&settings), ["Grep", "Glob", "Bash(rm:*)"]);
    }

    #[test]
    fn skill_install_then_remove_roundtrips() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        install_codebuddy_skill(home);
        assert!(home.join(".codebuddy/skills/lean-ctx/SKILL.md").exists());
        remove_codebuddy_skill(home);
        assert!(!home.join(".codebuddy/skills/lean-ctx").exists());
    }

    #[test]
    fn codebuddy_block_content_carries_detection_markers() {
        // GH #549 regression guard (see claude.rs for the full rationale).
        assert!(
            CODEBUDDY_MD_BLOCK_CONTENT_MCP.contains(CODEBUDDY_MD_BLOCK_START),
            "block content must contain its own start marker"
        );
        assert!(
            CODEBUDDY_MD_BLOCK_CONTENT_MCP.contains(CODEBUDDY_MD_BLOCK_END),
            "block content must contain its own end marker"
        );
    }

    #[test]
    fn installer_writes_single_codebuddy_block_and_is_idempotent() {
        let _lock = crate::core::data_dir::test_env_lock();
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let md = crate::core::editor_registry::codebuddy_state_dir(home).join("CODEBUDDY.md");

        crate::test_env::set_var("LEAN_CTX_RULES_INJECTION", "shared");
        install_codebuddy_global_codebuddy_md_for_mode(home, HookMode::Mcp);
        install_codebuddy_global_codebuddy_md_for_mode(home, HookMode::Mcp);
        crate::test_env::remove_var("LEAN_CTX_RULES_INJECTION");

        let after = std::fs::read_to_string(&md).unwrap();
        assert_eq!(
            after.matches(CODEBUDDY_MD_BLOCK_START).count(),
            1,
            "exactly one block after repeated installs, got:\n{after}"
        );
        assert!(after.contains(CODEBUDDY_MD_BLOCK_VERSION));
    }

    #[test]
    fn installer_heals_duplicate_codebuddy_blocks() {
        let _lock = crate::core::data_dir::test_env_lock();
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let dir = crate::core::editor_registry::codebuddy_state_dir(home);
        std::fs::create_dir_all(&dir).unwrap();
        let md = dir.join("CODEBUDDY.md");
        let b = CODEBUDDY_MD_BLOCK_CONTENT_MCP;
        let dupes = format!("# my notes\n\n{b}\n\n{b}\n\n{b}\n");
        std::fs::write(&md, dupes).unwrap();

        crate::test_env::set_var("LEAN_CTX_RULES_INJECTION", "shared");
        install_codebuddy_global_codebuddy_md_for_mode(home, HookMode::Mcp);
        crate::test_env::remove_var("LEAN_CTX_RULES_INJECTION");

        let after = std::fs::read_to_string(&md).unwrap();
        assert_eq!(
            after.matches(CODEBUDDY_MD_BLOCK_START).count(),
            1,
            "duplicates must collapse to one, got:\n{after}"
        );
        assert!(after.contains("# my notes"), "user content must survive");
    }
}
