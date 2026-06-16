use std::path::PathBuf;

pub fn run(args: &[String]) {
    let undo = args.iter().any(|a| a == "--undo");
    let level = if args.iter().any(|a| a == "--hard") {
        "hard"
    } else {
        "soft"
    };

    if undo {
        undo_harden();
    } else {
        apply_harden(level);
    }
}

fn apply_harden(level: &str) {
    println!("lean-ctx harden (level: {level})");
    println!();

    let mut applied = Vec::new();

    if set_env_in_mcp_configs() {
        applied.push("Set LEAN_CTX_HARDEN=1 in MCP configs");
    }

    if level == "hard"
        && let Some(msg) = apply_claude_permissions_deny()
    {
        applied.push("Claude Code: added Bash to permissions.deny");
        println!("  {msg}");
    }

    if applied.is_empty() {
        println!("  Nothing to harden (no supported editors detected).");
    } else {
        println!();
        for item in &applied {
            println!("  [OK] {item}");
        }
        println!();
        println!("Harden active. Native Read/Grep will be denied (except after Edit).");
        println!("Undo with: lean-ctx harden --undo");
    }
}

fn undo_harden() {
    println!("lean-ctx harden --undo");
    println!();

    remove_env_from_mcp_configs();
    remove_claude_permissions_deny();

    println!("  [OK] Harden deactivated. Native tools allowed again.");
}

fn set_env_in_mcp_configs() -> bool {
    let targets = discover_mcp_configs();
    let mut any_set = false;

    for path in targets {
        if let Ok(content) = std::fs::read_to_string(&path)
            && let Ok(mut json) = crate::core::jsonc::parse_jsonc(&content)
            && let Some(servers) = find_lean_ctx_server_mut(&mut json)
        {
            let env = servers
                .as_object_mut()
                .and_then(|s| s.get_mut("env"))
                .and_then(|e| e.as_object_mut());

            if let Some(env_map) = env {
                env_map.insert(
                    "LEAN_CTX_HARDEN".to_string(),
                    serde_json::Value::String("1".to_string()),
                );
            } else if let Some(server_obj) = servers.as_object_mut() {
                let mut env_map = serde_json::Map::new();
                env_map.insert(
                    "LEAN_CTX_HARDEN".to_string(),
                    serde_json::Value::String("1".to_string()),
                );
                server_obj.insert("env".to_string(), serde_json::Value::Object(env_map));
            }

            if let Ok(out) = serde_json::to_string_pretty(&json) {
                let _ = std::fs::write(&path, out);
                any_set = true;
                println!("  [OK] {}", path.display());
            }
        }
    }
    any_set
}

fn remove_env_from_mcp_configs() {
    for path in discover_mcp_configs() {
        if let Ok(content) = std::fs::read_to_string(&path)
            && let Ok(mut json) = crate::core::jsonc::parse_jsonc(&content)
            && let Some(servers) = find_lean_ctx_server_mut(&mut json)
            && let Some(env) = servers
                .as_object_mut()
                .and_then(|s| s.get_mut("env"))
                .and_then(|e| e.as_object_mut())
        {
            env.remove("LEAN_CTX_HARDEN");
            if let Ok(out) = serde_json::to_string_pretty(&json) {
                let _ = std::fs::write(&path, out);
            }
        }
    }
}

fn apply_claude_permissions_deny() -> Option<&'static str> {
    let home = dirs::home_dir()?;
    let settings_path = home.join(".claude").join("settings.json");

    let mut json = if settings_path.exists() {
        let content = std::fs::read_to_string(&settings_path).ok()?;
        crate::core::jsonc::parse_jsonc(&content).ok()?
    } else {
        serde_json::json!({})
    };

    let obj = json.as_object_mut()?;

    let permissions = obj
        .entry("permissions")
        .or_insert_with(|| serde_json::json!({}));
    let deny = permissions
        .as_object_mut()?
        .entry("deny")
        .or_insert_with(|| serde_json::json!([]));

    if let Some(arr) = deny.as_array_mut() {
        let bash_str = serde_json::Value::String("Bash".to_string());
        if !arr.contains(&bash_str) {
            arr.push(bash_str);
        }
    }

    let out = serde_json::to_string_pretty(&json).ok()?;
    std::fs::write(&settings_path, out).ok()?;
    Some("Added 'Bash' to ~/.claude/settings.json permissions.deny")
}

fn remove_claude_permissions_deny() {
    let Some(home) = dirs::home_dir() else {
        return;
    };
    let settings_path = home.join(".claude").join("settings.json");
    if !settings_path.exists() {
        return;
    }

    let Ok(content) = std::fs::read_to_string(&settings_path) else {
        return;
    };
    let Ok(mut json) = crate::core::jsonc::parse_jsonc(&content) else {
        return;
    };

    if let Some(deny) = json
        .pointer_mut("/permissions/deny")
        .and_then(|d| d.as_array_mut())
    {
        deny.retain(|v| v.as_str() != Some("Bash"));
    }

    if let Ok(out) = serde_json::to_string_pretty(&json) {
        let _ = std::fs::write(&settings_path, out);
    }
}

fn discover_mcp_configs() -> Vec<PathBuf> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };

    let candidates = [
        home.join(".cursor").join("mcp.json"),
        home.join(".claude.json"),
        home.join(".codebuddy.json"),
        home.join(".codeium")
            .join("windsurf")
            .join("mcp_config.json"),
    ];

    candidates.into_iter().filter(|p| p.exists()).collect()
}

fn find_lean_ctx_server_mut(json: &mut serde_json::Value) -> Option<&mut serde_json::Value> {
    if let Some(servers) = json.get_mut("mcpServers")
        && let Some(lctx) = servers.get_mut("lean-ctx")
    {
        return Some(lctx);
    }
    None
}
