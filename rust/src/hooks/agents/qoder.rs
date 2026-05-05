use std::path::Path;

use super::super::{mcp_server_quiet_mode, resolve_binary_path, write_file};

pub fn install_qoder_hook() {
    let Some(home) = crate::core::home::resolve_home_dir() else {
        tracing::error!("Cannot resolve home directory");
        return;
    };
    let settings_path = home.join(".qoder").join("settings.json");
    install_qoder_hook_config_at("Qoder", &settings_path);
}

fn install_qoder_hook_config_at(name: &str, settings_path: &Path) -> bool {
    let command = format!("{} hook rewrite", resolve_binary_path());
    let mut changed = false;
    let mut root = if settings_path.exists() {
        if let Some(parsed) = std::fs::read_to_string(settings_path)
            .ok()
            .and_then(|content| crate::core::jsonc::parse_jsonc(&content).ok())
        {
            parsed
        } else {
            changed = true;
            serde_json::json!({})
        }
    } else {
        changed = true;
        serde_json::json!({})
    };

    if upsert_qoder_hook_config(&mut root, &command) {
        changed = true;
    }

    if changed {
        write_file(
            settings_path,
            &serde_json::to_string_pretty(&root).unwrap_or_default(),
        );
        if !mcp_server_quiet_mode() {
            println!("Installed {name} hooks at {}", settings_path.display());
        }
    }

    changed
}

fn upsert_qoder_hook_config(root: &mut serde_json::Value, rewrite_cmd: &str) -> bool {
    let original = root.clone();
    if !root.is_object() {
        *root = serde_json::json!({});
    }
    let root_obj = root.as_object_mut().expect("root should be object");
    let hooks_value = root_obj
        .entry("hooks".to_string())
        .or_insert_with(|| serde_json::json!({}));
    if !hooks_value.is_object() {
        *hooks_value = serde_json::json!({});
    }
    let hooks_obj = hooks_value
        .as_object_mut()
        .expect("hooks should be object after normalization");

    let pre_tool_use = hooks_obj
        .entry("PreToolUse".to_string())
        .or_insert_with(|| serde_json::json!([]));
    if !pre_tool_use.is_array() {
        *pre_tool_use = serde_json::json!([]);
    }
    let entries = pre_tool_use
        .as_array_mut()
        .expect("PreToolUse should be array after normalization");

    entries.retain(|entry| !is_lean_ctx_qoder_managed_entry(entry));
    entries.push(serde_json::json!({
        "matcher": "Bash",
        "hooks": [{
            "type": "command",
            "command": rewrite_cmd,
            "timeout": 60
        }]
    }));

    *root != original
}

fn is_lean_ctx_qoder_managed_entry(entry: &serde_json::Value) -> bool {
    let Some(entry_obj) = entry.as_object() else {
        return false;
    };
    let matcher = entry_obj
        .get("matcher")
        .and_then(|value| value.as_str())
        .unwrap_or("*");
    let is_shell_matcher = matcher
        .split('|')
        .map(str::trim)
        .any(|part| matches!(part, "Bash" | "run_in_terminal"));
    if !is_shell_matcher {
        return false;
    }
    entry_obj
        .get("hooks")
        .and_then(|value| value.as_array())
        .is_some_and(|hooks| {
            hooks.iter().any(|hook| {
                hook.get("command")
                    .and_then(|value| value.as_str())
                    .is_some_and(|command| {
                        command.contains("lean-ctx") && command.contains("hook rewrite")
                    })
            })
        })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    #[test]
    fn qoder_hook_config_preserves_custom_hooks_and_upserts_rewrite() {
        let mut root = json!({
            "hooks": {
                "PreToolUse": [
                    {
                        "matcher": "Bash",
                        "hooks": [{ "type": "command", "command": "echo keep-me", "timeout": 5 }]
                    },
                    {
                        "matcher": "Bash",
                        "hooks": [{ "type": "command", "command": "lean-ctx hook rewrite", "timeout": 60 }]
                    }
                ],
                "Stop": [
                    {
                        "hooks": [{ "type": "command", "command": "echo stop", "timeout": 5 }]
                    }
                ]
            }
        });

        let changed = super::upsert_qoder_hook_config(&mut root, "/c/bin/lean-ctx hook rewrite");
        assert!(changed);

        let pre_tool_use = root["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(pre_tool_use.len(), 2);
        assert_eq!(pre_tool_use[0]["hooks"][0]["command"], "echo keep-me");
        assert_eq!(
            pre_tool_use[1]["hooks"][0]["command"],
            "/c/bin/lean-ctx hook rewrite"
        );
        assert_eq!(root["hooks"]["Stop"][0]["hooks"][0]["command"], "echo stop");
    }

    #[test]
    fn qoder_hook_config_creates_fresh_pretooluse_group() {
        let mut root = json!({});
        let changed = super::upsert_qoder_hook_config(&mut root, "lean-ctx hook rewrite");
        assert!(changed);

        assert_eq!(root["hooks"]["PreToolUse"][0]["matcher"], "Bash");
        assert_eq!(
            root["hooks"]["PreToolUse"][0]["hooks"][0],
            json!({ "type": "command", "command": "lean-ctx hook rewrite", "timeout": 60 })
        );
    }
}
