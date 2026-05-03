use super::super::{
    generate_rewrite_script, make_executable, mcp_server_quiet_mode, resolve_binary_path,
    resolve_binary_path_for_bash, write_file, REDIRECT_SCRIPT_CLAUDE,
};

pub(crate) fn install_claude_hook(global: bool) {
    let Some(home) = crate::core::home::resolve_home_dir() else {
        tracing::error!("Cannot resolve home directory");
        return;
    };

    install_claude_hook_scripts(&home);
    install_claude_hook_config(&home);

    let scope = crate::core::config::Config::load().rules_scope_effective();
    if scope != crate::core::config::RulesScope::Project {
        install_claude_rules_file(&home);
        install_claude_global_claude_md(&home);
        install_claude_skill(&home);
    }

    let _ = global;
}

const CLAUDE_MD_BLOCK_START: &str = "<!-- lean-ctx -->";
const CLAUDE_MD_BLOCK_END: &str = "<!-- /lean-ctx -->";
const CLAUDE_MD_BLOCK_VERSION: &str = "lean-ctx-claude-v2";

const CLAUDE_MD_BLOCK_CONTENT: &str = "\
<!-- lean-ctx -->
<!-- lean-ctx-claude-v2 -->
## lean-ctx — Context Runtime

Always prefer lean-ctx MCP tools over native equivalents:
- `ctx_read` instead of `Read` / `cat` (cached, 10 modes, re-reads ~13 tokens)
- `ctx_shell` instead of `bash` / `Shell` (90+ compression patterns)
- `ctx_search` instead of `Grep` / `rg` (compact results)
- `ctx_tree` instead of `ls` / `find` (compact directory maps)
- Native Edit/StrReplace stay unchanged. If Edit requires Read and Read is unavailable, use `ctx_edit(path, old_string, new_string)` instead.
- Write, Delete, Glob — use normally.

Full rules: @rules/lean-ctx.md

Verify setup: run `/mcp` to check lean-ctx is connected, `/memory` to confirm this file loaded.
<!-- /lean-ctx -->";

fn install_claude_global_claude_md(home: &std::path::Path) {
    let claude_dir = crate::core::editor_registry::claude_state_dir(home);
    let _ = std::fs::create_dir_all(&claude_dir);
    let claude_md_path = claude_dir.join("CLAUDE.md");

    let existing = std::fs::read_to_string(&claude_md_path).unwrap_or_default();

    if existing.contains(CLAUDE_MD_BLOCK_START) {
        if existing.contains(CLAUDE_MD_BLOCK_VERSION) {
            return;
        }
        let cleaned = remove_block(&existing, CLAUDE_MD_BLOCK_START, CLAUDE_MD_BLOCK_END);
        let updated = format!("{}\n\n{}\n", cleaned.trim(), CLAUDE_MD_BLOCK_CONTENT);
        write_file(&claude_md_path, &updated);
        return;
    }

    if existing.trim().is_empty() {
        write_file(&claude_md_path, CLAUDE_MD_BLOCK_CONTENT);
    } else {
        let updated = format!("{}\n\n{}\n", existing.trim(), CLAUDE_MD_BLOCK_CONTENT);
        write_file(&claude_md_path, &updated);
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

fn install_claude_rules_file(home: &std::path::Path) {
    let rules_dir = crate::core::editor_registry::claude_rules_dir(home);
    let _ = std::fs::create_dir_all(&rules_dir);
    let rules_path = rules_dir.join("lean-ctx.md");

    let desired = crate::rules_inject::rules_dedicated_markdown();
    let existing = std::fs::read_to_string(&rules_path).unwrap_or_default();

    if existing.is_empty() {
        write_file(&rules_path, desired);
        return;
    }
    if existing.contains(crate::rules_inject::RULES_VERSION_STR) {
        return;
    }
    if existing.contains("<!-- lean-ctx-rules-") {
        write_file(&rules_path, desired);
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

fn ensure_command_hook(pre_arr: &mut Vec<serde_json::Value>, matcher: &str, command: &str) {
    let desired = serde_json::json!({ "type": "command", "command": command });

    if let Some(group) = pre_arr.iter_mut().find(|g| {
        g.get("matcher")
            .and_then(|m| m.as_str())
            .is_some_and(|m| m == matcher)
    }) {
        if let Some(obj) = group.as_object_mut() {
            let hooks_val = obj
                .entry("hooks".to_string())
                .or_insert_with(|| serde_json::json!([]));

            if let Some(hooks) = hooks_val.as_array_mut() {
                let already = hooks.iter().any(|h| {
                    h.get("type").and_then(|t| t.as_str()) == Some("command")
                        && h.get("command").and_then(|c| c.as_str()) == Some(command)
                });
                if !already {
                    hooks.push(desired);
                }
            } else {
                obj.insert("hooks".to_string(), serde_json::json!([desired]));
            }
        }
        return;
    }

    pre_arr.push(serde_json::json!({ "matcher": matcher, "hooks": [desired] }));
}

pub(crate) fn install_claude_hook_config(home: &std::path::Path) {
    let hooks_dir = crate::core::editor_registry::claude_state_dir(home).join("hooks");
    let binary = resolve_binary_path();

    let rewrite_cmd = format!("{binary} hook rewrite");
    let redirect_cmd = format!("{binary} hook redirect");

    let settings_path = crate::core::editor_registry::claude_state_dir(home).join("settings.json");
    let settings_content = if settings_path.exists() {
        std::fs::read_to_string(&settings_path).unwrap_or_default()
    } else {
        String::new()
    };

    let has_old_hooks = settings_content.contains("lean-ctx-rewrite.sh")
        || settings_content.contains("lean-ctx-redirect.sh");

    let desired_pretooluse = serde_json::json!([
        {
            "matcher": "Bash|bash",
            "hooks": [{
                "type": "command",
                "command": rewrite_cmd
            }]
        },
        {
            "matcher": "Read|read|ReadFile|read_file|View|view|Grep|grep|Search|search|ListFiles|list_files|ListDirectory|list_directory",
            "hooks": [{
                "type": "command",
                "command": redirect_cmd
            }]
        }
    ]);

    fn contains_lean_ctx_commands(v: &serde_json::Value) -> bool {
        let Some(arr) = v.as_array() else {
            return false;
        };
        arr.iter().any(|group| {
            group
                .get("hooks")
                .and_then(|h| h.as_array())
                .is_some_and(|hooks| {
                    hooks.iter().any(|hook| {
                        hook.get("command")
                            .and_then(|c| c.as_str())
                            .is_some_and(|c| {
                                c.contains(" hook rewrite") || c.contains(" hook redirect")
                            })
                    })
                })
        })
    }

    if !has_old_hooks {
        if let Ok(existing_json) = crate::core::jsonc::parse_jsonc(&settings_content) {
            if let Some(pre) = existing_json.get("hooks").and_then(|h| h.get("PreToolUse")) {
                if contains_lean_ctx_commands(pre) {
                    return;
                }
            }
        }
    }

    if settings_content.is_empty() {
        let hook_entry = serde_json::json!({ "hooks": { "PreToolUse": desired_pretooluse } });
        write_file(
            &settings_path,
            &serde_json::to_string_pretty(&hook_entry).unwrap_or_default(),
        );
    } else if let Ok(mut existing) = crate::core::jsonc::parse_jsonc(&settings_content) {
        if let Some(root) = existing.as_object_mut() {
            let hooks = root
                .entry("hooks".to_string())
                .or_insert_with(|| serde_json::json!({}));
            if let Some(hooks_obj) = hooks.as_object_mut() {
                let pre = hooks_obj
                    .entry("PreToolUse".to_string())
                    .or_insert_with(|| serde_json::json!([]));
                if let Some(pre_arr) = pre.as_array_mut() {
                    // Merge: preserve other hooks/plugins. Only ensure our command hooks exist.
                    ensure_command_hook(pre_arr, "Bash|bash", &rewrite_cmd);
                    ensure_command_hook(
                        pre_arr,
                        "Read|read|ReadFile|read_file|View|view|Grep|grep|Search|search|ListFiles|list_files|ListDirectory|list_directory",
                        &redirect_cmd,
                    );
                }
            }
            write_file(
                &settings_path,
                &serde_json::to_string_pretty(&existing).unwrap_or_default(),
            );
        }
    }
    if !mcp_server_quiet_mode() {
        println!("Installed Claude Code hooks at {}", hooks_dir.display());
    }
}

pub(crate) fn install_claude_project_hooks(cwd: &std::path::Path) {
    let binary = resolve_binary_path();
    let rewrite_cmd = format!("{binary} hook rewrite");
    let redirect_cmd = format!("{binary} hook redirect");

    let settings_path = cwd.join(".claude").join("settings.local.json");
    let _ = std::fs::create_dir_all(cwd.join(".claude"));

    let existing = std::fs::read_to_string(&settings_path).unwrap_or_default();
    let desired_pretooluse = serde_json::json!([
        {
            "matcher": "Bash|bash",
            "hooks": [{
                "type": "command",
                "command": rewrite_cmd
            }]
        },
        {
            "matcher": "Read|read|ReadFile|read_file|View|view|Grep|grep|Search|search|ListFiles|list_files|ListDirectory|list_directory",
            "hooks": [{
                "type": "command",
                "command": redirect_cmd
            }]
        }
    ]);

    if existing.is_empty() {
        let hook_entry = serde_json::json!({ "hooks": { "PreToolUse": desired_pretooluse } });
        write_file(
            &settings_path,
            &serde_json::to_string_pretty(&hook_entry).unwrap_or_default(),
        );
    } else if let Ok(mut json) = crate::core::jsonc::parse_jsonc(&existing) {
        if let Some(root) = json.as_object_mut() {
            let hooks = root
                .entry("hooks".to_string())
                .or_insert_with(|| serde_json::json!({}));
            if let Some(hooks_obj) = hooks.as_object_mut() {
                let pre = hooks_obj
                    .entry("PreToolUse".to_string())
                    .or_insert_with(|| serde_json::json!([]));
                if let Some(pre_arr) = pre.as_array_mut() {
                    // Merge: preserve other hooks/plugins. Only ensure our command hooks exist.
                    ensure_command_hook(pre_arr, "Bash|bash", &rewrite_cmd);
                    ensure_command_hook(
                        pre_arr,
                        "Read|read|ReadFile|read_file|View|view|Grep|grep|Search|search|ListFiles|list_files|ListDirectory|list_directory",
                        &redirect_cmd,
                    );
                }
            }
            write_file(
                &settings_path,
                &serde_json::to_string_pretty(&json).unwrap_or_default(),
            );
        }
    }
    println!("Created .claude/settings.local.json (project-local PreToolUse hooks).");
}
