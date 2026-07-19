use super::super::resolve_binary_path;

pub(crate) fn install_vibe_hook() {
    // Mistral Vibe is MCP-only (no shell hooks), so an
    // MCP-disabled environment writes nothing here.
    if !super::super::should_register_mcp() {
        return;
    }
    let binary = resolve_binary_path();
    let home = crate::core::home::resolve_home_dir().unwrap_or_default();
    let config_path = home.join(".vibe/config.toml");
    let display_path = "~/.vibe/config.toml";

    // Mistral Vibe expects MCP servers in config.toml as [[mcp_servers]]
    // We write the lean-ctx server entry
    let server_entry = format!(
        r#"[[mcp_servers]]
name = "lean-ctx"
transport = "stdio"
command = "{}"
args = ["serve"]
"#,
        { binary }
    );

    if config_path.exists() {
        let content = std::fs::read_to_string(&config_path).unwrap_or_default();
        if content.contains("name = \"lean-ctx\"") || content.contains("name = 'lean-ctx'") {
            eprintln!("Vibe MCP server already configured in {display_path}");
            return;
        }

        // Check if file has mcp_servers array
        if content.contains("[[mcp_servers]]") {
            // Append to existing mcp_servers
            let updated_content = if content.ends_with('\n') {
                format!("{content}{server_entry}")
            } else {
                format!("{content}\n{server_entry}")
            };
            if let Err(e) = std::fs::write(&config_path, &updated_content) {
                tracing::error!("Failed to update Vibe config: {}", e);
                return;
            }
            eprintln!("  \x1b[32m✓\x1b[0m Vibe MCP server added to {display_path}");
            return;
        }
    }

    // Create new config file with mcp_servers
    if let Err(e) = std::fs::write(&config_path, &server_entry) {
        tracing::error!("Failed to create Vibe config: {}", e);
        return;
    }
    eprintln!("  \x1b[32m✓\x1b[0m Vibe MCP server written to {display_path}");
}
