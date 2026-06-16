use super::super::{HYBRID_RULES, HookMode, resolve_binary_path, write_file};

pub(crate) fn install_crush_hook() {
    // #281: only the MCP-server entry is gated; `install_crush_hook_with_mode`
    // still installs the hybrid rules for MCP-disabled setups.
    if !super::super::should_register_mcp() {
        return;
    }
    let binary = resolve_binary_path();
    let home = crate::core::home::resolve_home_dir().unwrap_or_default();
    let config_path = home.join(".config/crush/crush.json");
    let display_path = "~/.config/crush/crush.json";

    if let Some(parent) = config_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let desired = serde_json::json!({
        "type": "stdio",
        "command": binary,
        "env": super::super::mcp_server_env_json()
    });

    if config_path.exists() {
        let content = std::fs::read_to_string(&config_path).unwrap_or_default();
        if let Ok(mut json) = crate::core::jsonc::parse_jsonc(&content)
            && let Some(obj) = json.as_object_mut()
        {
            let servers = obj.entry("mcp").or_insert_with(|| serde_json::json!({}));
            if let Some(servers_obj) = servers.as_object_mut() {
                if servers_obj.get("lean-ctx") == Some(&desired) {
                    eprintln!("Crush MCP already configured at {display_path}");
                    return;
                }
                servers_obj.insert("lean-ctx".to_string(), desired.clone());
            }
            if let Ok(formatted) = serde_json::to_string_pretty(&json) {
                let _ = std::fs::write(&config_path, formatted);
                eprintln!("  \x1b[32m✓\x1b[0m Crush MCP configured at {display_path}");
                return;
            }
        }
    }

    let content = serde_json::to_string_pretty(&serde_json::json!({
        "mcp": {
            "lean-ctx": desired
        }
    }));

    if let Ok(json_str) = content {
        let _ = std::fs::write(&config_path, json_str);
        eprintln!("  \x1b[32m✓\x1b[0m Crush MCP configured at {display_path}");
    } else {
        tracing::error!("Failed to configure Crush");
    }
}

pub(crate) fn install_crush_hook_with_mode(mode: HookMode) {
    match mode {
        HookMode::Hybrid => {
            install_crush_hook();
            install_crush_hybrid_rules(mode);
        }
        HookMode::Mcp => {
            install_crush_hook();
        }
    }
}

fn install_crush_hybrid_rules(mode: HookMode) {
    let home = crate::core::home::resolve_home_dir().unwrap_or_default();
    let rules_dir = home.join(".config/crush/rules");
    let _ = std::fs::create_dir_all(&rules_dir);
    let rules_path = rules_dir.join("lean-ctx.md");

    let content = match mode {
        HookMode::Hybrid => HYBRID_RULES,
        HookMode::Mcp => return,
    };

    write_file(&rules_path, content);

    let mode_name = match mode {
        HookMode::Hybrid => "hybrid",
        HookMode::Mcp => "mcp",
    };
    eprintln!(
        "  \x1b[32m✓\x1b[0m Crush rules installed in {mode_name} mode at {}",
        rules_path.display()
    );
}
