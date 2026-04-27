use super::super::resolve_binary_path;

pub(crate) fn install_jetbrains_hook() {
    let binary = resolve_binary_path();
    let home = dirs::home_dir().unwrap_or_default();
    let config_path = home.join(".jb-mcp.json");
    let display_path = "~/.jb-mcp.json";

    let entry = serde_json::json!({
        "name": "lean-ctx",
        "command": binary,
        "args": [],
        "env": {
            "LEAN_CTX_DATA_DIR": crate::core::data_dir::lean_ctx_data_dir()
                .map(|d| d.to_string_lossy().to_string())
                .unwrap_or_default()
        }
    });

    if config_path.exists() {
        let content = std::fs::read_to_string(&config_path).unwrap_or_default();
        if content.contains("lean-ctx") {
            println!("JetBrains MCP already configured at {display_path}");
            return;
        }

        if let Ok(mut json) = crate::core::jsonc::parse_jsonc(&content) {
            if let Some(obj) = json.as_object_mut() {
                let servers = obj
                    .entry("servers")
                    .or_insert_with(|| serde_json::json!([]));
                if let Some(arr) = servers.as_array_mut() {
                    arr.push(entry.clone());
                }
                if let Ok(formatted) = serde_json::to_string_pretty(&json) {
                    let _ = std::fs::write(&config_path, formatted);
                    println!("  \x1b[32m✓\x1b[0m JetBrains MCP configured at {display_path}");
                    return;
                }
            }
        }
    }

    let config = serde_json::json!({ "servers": [entry] });
    if let Ok(json_str) = serde_json::to_string_pretty(&config) {
        let _ = std::fs::write(&config_path, json_str);
        println!("  \x1b[32m✓\x1b[0m JetBrains MCP configured at {display_path}");
    } else {
        tracing::error!("Failed to configure JetBrains");
    }
}
