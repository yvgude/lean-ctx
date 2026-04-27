use super::super::resolve_binary_path;

pub(crate) fn install_crush_hook() {
    let binary = resolve_binary_path();
    let home = dirs::home_dir().unwrap_or_default();
    let config_path = home.join(".config/crush/crush.json");
    let display_path = "~/.config/crush/crush.json";

    if let Some(parent) = config_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    if config_path.exists() {
        let content = std::fs::read_to_string(&config_path).unwrap_or_default();
        if content.contains("lean-ctx") {
            println!("Crush MCP already configured at {display_path}");
            return;
        }

        if let Ok(mut json) = crate::core::jsonc::parse_jsonc(&content) {
            if let Some(obj) = json.as_object_mut() {
                let servers = obj.entry("mcp").or_insert_with(|| serde_json::json!({}));
                if let Some(servers_obj) = servers.as_object_mut() {
                    servers_obj.insert(
                        "lean-ctx".to_string(),
                        serde_json::json!({ "type": "stdio", "command": binary }),
                    );
                }
                if let Ok(formatted) = serde_json::to_string_pretty(&json) {
                    let _ = std::fs::write(&config_path, formatted);
                    println!("  \x1b[32m✓\x1b[0m Crush MCP configured at {display_path}");
                    return;
                }
            }
        }
    }

    let content = serde_json::to_string_pretty(&serde_json::json!({
        "mcp": {
            "lean-ctx": {
                "type": "stdio",
                "command": binary
            }
        }
    }));

    if let Ok(json_str) = content {
        let _ = std::fs::write(&config_path, json_str);
        println!("  \x1b[32m✓\x1b[0m Crush MCP configured at {display_path}");
    } else {
        tracing::error!("Failed to configure Crush");
    }
}
