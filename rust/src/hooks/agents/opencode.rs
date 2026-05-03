use super::super::{mcp_server_quiet_mode, resolve_binary_path};

pub(crate) fn install_opencode_hook() {
    let binary = resolve_binary_path();
    let home = crate::core::home::resolve_home_dir().unwrap_or_default();
    let config_path = home.join(".config/opencode/opencode.json");
    let display_path = "~/.config/opencode/opencode.json";

    if let Some(parent) = config_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let data_dir = crate::core::data_dir::lean_ctx_data_dir()
        .map(|d| d.to_string_lossy().to_string())
        .unwrap_or_default();
    let desired = serde_json::json!({
        "type": "local",
        "command": [&binary],
        "enabled": true,
        "environment": { "LEAN_CTX_DATA_DIR": data_dir }
    });

    if config_path.exists() {
        let content = std::fs::read_to_string(&config_path).unwrap_or_default();
        if content.contains("lean-ctx") {
            println!("OpenCode MCP already configured at {display_path}");
        } else if let Ok(mut json) = crate::core::jsonc::parse_jsonc(&content) {
            if let Some(obj) = json.as_object_mut() {
                let mcp = obj.entry("mcp").or_insert_with(|| serde_json::json!({}));
                if let Some(mcp_obj) = mcp.as_object_mut() {
                    mcp_obj.insert("lean-ctx".to_string(), desired.clone());
                }
                if let Ok(formatted) = serde_json::to_string_pretty(&json) {
                    let _ = std::fs::write(&config_path, formatted);
                    println!("  \x1b[32m✓\x1b[0m OpenCode MCP configured at {display_path}");
                }
            }
        }
    } else {
        let content = serde_json::to_string_pretty(&serde_json::json!({
            "$schema": "https://opencode.ai/config.json",
            "mcp": {
                "lean-ctx": desired
            }
        }));

        if let Ok(json_str) = content {
            let _ = std::fs::write(&config_path, json_str);
            println!("  \x1b[32m✓\x1b[0m OpenCode MCP configured at {display_path}");
        } else {
            tracing::error!("Failed to configure OpenCode");
        }
    }

    install_opencode_plugin(&home);
}

fn install_opencode_plugin(home: &std::path::Path) {
    let plugin_dir = home.join(".config/opencode/plugins");
    let _ = std::fs::create_dir_all(&plugin_dir);
    let plugin_path = plugin_dir.join("lean-ctx.ts");

    let plugin_content = include_str!("../../templates/opencode-plugin.ts");
    let _ = std::fs::write(&plugin_path, plugin_content);

    if !mcp_server_quiet_mode() {
        println!(
            "  \x1b[32m✓\x1b[0m OpenCode plugin installed at {}",
            plugin_path.display()
        );
    }
}
