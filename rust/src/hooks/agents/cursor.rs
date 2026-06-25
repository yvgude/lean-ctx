use std::path::PathBuf;

use super::super::{
    HookMode, make_executable, mcp_server_quiet_mode, resolve_binary_path, write_file,
};
use super::shared::install_standard_hook_scripts;

fn ensure_pretooluse_hook(
    pre: &mut Vec<serde_json::Value>,
    matcher_variants: &[&str],
    desired_matcher: &str,
    desired_command: &str,
) {
    if let Some(existing) = pre.iter_mut().find(|v| {
        v.get("matcher")
            .and_then(|m| m.as_str())
            .is_some_and(|m| matcher_variants.contains(&m))
    }) {
        if let Some(obj) = existing.as_object_mut() {
            obj.insert(
                "matcher".to_string(),
                serde_json::Value::String(desired_matcher.to_string()),
            );
            obj.insert(
                "command".to_string(),
                serde_json::Value::String(desired_command.to_string()),
            );
        }
        return;
    }
    pre.push(serde_json::json!({
        "matcher": desired_matcher,
        "command": desired_command
    }));
}

fn ensure_observe_hook(
    hooks_obj: &mut serde_json::Map<String, serde_json::Value>,
    event: &str,
    observe_cmd: &str,
) {
    let arr = hooks_obj
        .entry(event.to_string())
        .or_insert_with(|| serde_json::json!([]));
    if !arr.is_array() {
        *arr = serde_json::json!([]);
    }
    let Some(entries) = arr.as_array_mut() else {
        return;
    };
    let already = entries.iter().any(|e| {
        e.get("command")
            .and_then(|c| c.as_str())
            .is_some_and(|c| c.contains("hook observe"))
    });
    if !already {
        entries.push(serde_json::json!({ "command": observe_cmd }));
    }
}

fn merge_cursor_hooks(existing: &mut serde_json::Value, rewrite_cmd: &str, redirect_cmd: &str) {
    if !existing.is_object() {
        *existing = serde_json::json!({});
    }
    let Some(root) = existing.as_object_mut() else {
        return;
    };
    root.insert("version".to_string(), serde_json::json!(1));

    let hooks = root
        .entry("hooks".to_string())
        .or_insert_with(|| serde_json::json!({}));
    if !hooks.is_object() {
        *hooks = serde_json::json!({});
    }
    let Some(hooks_obj) = hooks.as_object_mut() else {
        return;
    };

    // PreToolUse hooks (rewrite + redirect)
    let pre = hooks_obj
        .entry("preToolUse".to_string())
        .or_insert_with(|| serde_json::json!([]));
    if !pre.is_array() {
        *pre = serde_json::json!([]);
    }
    let Some(pre_arr) = pre.as_array_mut() else {
        return;
    };

    ensure_pretooluse_hook(pre_arr, &["Shell"], "Shell", rewrite_cmd);
    ensure_pretooluse_hook(
        pre_arr,
        &["Read|Grep|Glob", "Read|Grep", "Read", "Grep"],
        "Read|Grep|Glob",
        redirect_cmd,
    );

    // Observe hooks for full context awareness
    let observe_cmd = rewrite_cmd.replace("hook rewrite", "hook observe");
    ensure_observe_hook(hooks_obj, "afterMCPExecution", &observe_cmd);
    ensure_observe_hook(hooks_obj, "postToolUse", &observe_cmd);
    ensure_observe_hook(hooks_obj, "afterShellExecution", &observe_cmd);
    ensure_observe_hook(hooks_obj, "beforeReadFile", &observe_cmd);
    ensure_observe_hook(hooks_obj, "afterAgentResponse", &observe_cmd);
    ensure_observe_hook(hooks_obj, "afterAgentThought", &observe_cmd);
    ensure_observe_hook(hooks_obj, "beforeSubmitPrompt", &observe_cmd);
    ensure_observe_hook(hooks_obj, "preCompact", &observe_cmd);
    ensure_observe_hook(hooks_obj, "sessionStart", &observe_cmd);
    ensure_observe_hook(hooks_obj, "sessionEnd", &observe_cmd);
}

pub fn install_cursor_hook(global: bool) {
    let Some(home) = crate::core::home::resolve_home_dir() else {
        tracing::error!("Cannot resolve home directory");
        return;
    };

    install_cursor_hook_scripts(&home);
    install_cursor_hook_config(&home);

    let scope = crate::core::config::Config::load().rules_scope_effective();
    let skip_project = global || scope == crate::core::config::RulesScope::Global;

    if skip_project {
        if !mcp_server_quiet_mode() {
            eprintln!(
                "Global mode: skipping project-local .cursor/rules/ (use without --global in a project)."
            );
        }
    } else {
        let rules_dir = PathBuf::from(".cursor").join("rules");
        let _ = std::fs::create_dir_all(&rules_dir);
        let rule_path = rules_dir.join("lean-ctx.mdc");
        if rule_path.exists() {
            if !mcp_server_quiet_mode() {
                eprintln!("Cursor rule already exists.");
            }
        } else {
            let body = crate::core::rules_canonical::render(
                false,
                crate::core::rules_canonical::Wrapper::Dedicated,
                crate::core::config::CompressionLevel::Off,
            );
            let rule_content = format!(
                "---\n\
                 description: \"lean-ctx: context compression layer. \
                 Tools replace native Read/Grep/Shell — see rule body.\"\n\
                 globs: **/*\n\
                 alwaysApply: true\n\
                 ---\n\n\
                 {body}"
            );
            write_file(&rule_path, &rule_content);
            if !mcp_server_quiet_mode() {
                eprintln!("Created .cursor/rules/lean-ctx.mdc in current project.");
            }
        }
    }

    if !mcp_server_quiet_mode() {
        eprintln!("Restart Cursor to activate.");
    }
}

pub(crate) fn install_cursor_hook_with_mode(global: bool, mode: HookMode) {
    match mode {
        HookMode::Mcp => install_cursor_hook(global),
        HookMode::Hybrid => {
            install_cursor_hook(global);
            install_cursor_rules_for_mode(global, mode);
        }
    }
}

fn install_cursor_rules_for_mode(global: bool, mode: HookMode) {
    let content = cursor_mdc_for_mode(mode);
    let mode_name = match mode {
        HookMode::Hybrid => "hybrid",
        HookMode::Mcp => "mcp",
    };

    if global {
        if let Some(home) = crate::core::home::resolve_home_dir() {
            let global_rules_dir = home.join(".cursor").join("rules");
            let _ = std::fs::create_dir_all(&global_rules_dir);
            let global_path = global_rules_dir.join("lean-ctx.mdc");
            write_file(&global_path, &content);
            if !mcp_server_quiet_mode() {
                eprintln!(
                    "Installed Cursor rules in {mode_name} mode at {}",
                    global_path.display()
                );
            }
        }
    } else {
        let rules_dir = PathBuf::from(".cursor").join("rules");
        let _ = std::fs::create_dir_all(&rules_dir);
        let rule_path = rules_dir.join("lean-ctx.mdc");
        write_file(&rule_path, &content);
        if !mcp_server_quiet_mode() {
            eprintln!("Installed Cursor rules in {mode_name} mode at .cursor/rules/lean-ctx.mdc");
        }
    }
}

fn cursor_mdc_for_mode(_mode: HookMode) -> String {
    let body = crate::core::rules_canonical::render(
        false,
        crate::core::rules_canonical::Wrapper::Dedicated,
        crate::core::config::CompressionLevel::Off,
    );
    format!(
        "---\n\
         description: \"lean-ctx: context compression layer. \
         Tools replace native Read/Grep/Shell — see rule body.\"\n\
         globs: **/*\n\
         alwaysApply: true\n\
         ---\n\n\
         {body}"
    )
}

pub(crate) fn install_cursor_hook_scripts(home: &std::path::Path) {
    let hooks_dir = home.join(".cursor").join("hooks");
    install_standard_hook_scripts(&hooks_dir, "lean-ctx-rewrite.sh", "lean-ctx-redirect.sh");

    let native_binary = resolve_binary_path();
    let rewrite_native = hooks_dir.join("lean-ctx-rewrite-native");
    write_file(
        &rewrite_native,
        &format!("#!/bin/sh\nexec {native_binary} hook rewrite\n"),
    );
    make_executable(&rewrite_native);

    let redirect_native = hooks_dir.join("lean-ctx-redirect-native");
    write_file(
        &redirect_native,
        &format!("#!/bin/sh\nexec {native_binary} hook redirect\n"),
    );
    make_executable(&redirect_native);
}

pub(crate) fn install_cursor_hook_config(home: &std::path::Path) {
    let binary = resolve_binary_path();
    let rewrite_cmd = format!("{binary} hook rewrite");
    let redirect_cmd = format!("{binary} hook redirect");

    let hooks_json = home.join(".cursor").join("hooks.json");

    let content = if hooks_json.exists() {
        std::fs::read_to_string(&hooks_json).unwrap_or_default()
    } else {
        String::new()
    };

    let mut existing = if content.trim().is_empty() {
        serde_json::json!({})
    } else {
        crate::core::jsonc::parse_jsonc(&content).unwrap_or_else(|_| serde_json::json!({}))
    };

    if !existing.is_object() {
        existing = serde_json::json!({});
    }

    // Merge-based: preserve other hooks/plugins. Only upsert lean-ctx entries.
    merge_cursor_hooks(&mut existing, &rewrite_cmd, &redirect_cmd);

    let formatted = serde_json::to_string_pretty(&existing).unwrap_or_default();
    write_file(&hooks_json, &formatted);

    if !mcp_server_quiet_mode() {
        eprintln!("Installed Cursor hooks at {}", hooks_json.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_hooks_merge_preserves_other_entries() {
        let mut v = serde_json::json!({
            "version": 1,
            "hooks": {
                "preToolUse": [
                    { "matcher": "Shell", "command": "/old/bin hook rewrite" },
                    { "matcher": "Other", "command": "do-something" }
                ],
                "postToolUse": [
                    { "matcher": "Shell", "command": "post" }
                ]
            },
            "otherKey": { "x": 1 }
        });

        merge_cursor_hooks(&mut v, "/new/bin hook rewrite", "/new/bin hook redirect");

        assert!(v.get("otherKey").is_some());
        assert!(v.pointer("/hooks/postToolUse").is_some());

        let pre = v
            .pointer("/hooks/preToolUse")
            .and_then(|x| x.as_array())
            .unwrap();
        assert!(
            pre.iter()
                .any(|e| e.get("matcher").and_then(|m| m.as_str()) == Some("Other"))
        );
        assert!(pre.iter().any(|e| {
            e.get("matcher").and_then(|m| m.as_str()) == Some("Shell")
                && e.get("command").and_then(|c| c.as_str()) == Some("/new/bin hook rewrite")
        }));
        assert!(pre.iter().any(|e| {
            e.get("matcher").and_then(|m| m.as_str()) == Some("Read|Grep|Glob")
                && e.get("command").and_then(|c| c.as_str()) == Some("/new/bin hook redirect")
        }));
    }
}
