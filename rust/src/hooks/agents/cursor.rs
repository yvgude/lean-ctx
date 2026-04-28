use std::path::PathBuf;

use super::super::{make_executable, mcp_server_quiet_mode, resolve_binary_path, write_file};
use super::shared::install_standard_hook_scripts;

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
        println!("Global mode: skipping project-local .cursor/rules/ (use without --global in a project).");
    } else {
        let rules_dir = PathBuf::from(".cursor").join("rules");
        let _ = std::fs::create_dir_all(&rules_dir);
        let rule_path = rules_dir.join("lean-ctx.mdc");
        if rule_path.exists() {
            println!("Cursor rule already exists.");
        } else {
            let rule_content = include_str!("../../templates/lean-ctx.mdc");
            write_file(&rule_path, rule_content);
            println!("Created .cursor/rules/lean-ctx.mdc in current project.");
        }
    }

    println!("Restart Cursor to activate.");
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

    let hook_config = serde_json::json!({
        "version": 1,
        "hooks": {
            "preToolUse": [
                {
                    "matcher": "Shell",
                    "command": rewrite_cmd
                },
                {
                    "matcher": "Read|Grep",
                    "command": redirect_cmd
                }
            ]
        }
    });

    let content = if hooks_json.exists() {
        std::fs::read_to_string(&hooks_json).unwrap_or_default()
    } else {
        String::new()
    };

    let has_correct_matchers = content.contains("\"Shell\"")
        && (content.contains("\"Read|Grep\"") || content.contains("\"Read\""));
    let has_correct_format = content.contains("\"version\"") && content.contains("\"preToolUse\"");
    if has_correct_format
        && has_correct_matchers
        && content.contains("hook rewrite")
        && content.contains("hook redirect")
    {
        return;
    }

    if content.is_empty() || !content.contains("\"version\"") {
        write_file(
            &hooks_json,
            &serde_json::to_string_pretty(&hook_config).unwrap_or_default(),
        );
    } else if let Ok(mut existing) = crate::core::jsonc::parse_jsonc(&content) {
        if let Some(obj) = existing.as_object_mut() {
            obj.insert("version".to_string(), serde_json::json!(1));
            obj.insert("hooks".to_string(), hook_config["hooks"].clone());
            write_file(
                &hooks_json,
                &serde_json::to_string_pretty(&existing).unwrap_or_default(),
            );
        }
    } else {
        write_file(
            &hooks_json,
            &serde_json::to_string_pretty(&hook_config).unwrap_or_default(),
        );
    }

    if !mcp_server_quiet_mode() {
        println!("Installed Cursor hooks at {}", hooks_json.display());
    }
}
