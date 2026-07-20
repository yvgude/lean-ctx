use std::path::PathBuf;

pub fn run(args: &[String]) {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_help();
        return;
    }

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

fn print_help() {
    println!("lean-ctx harden — tighten IDE security by denying native Read/Grep/Glob");
    println!();
    println!("USAGE:");
    println!("    lean-ctx harden [OPTIONS]");
    println!();
    println!("OPTIONS:");
    println!("    --hard     Replace mode: deny native tools across all IDEs");
    println!("    --undo     Revert all harden changes");
    println!("    --help     Show this help message");
}

fn apply_harden(level: &str) {
    println!("lean-ctx harden (level: {level})");
    println!();

    if level == "hard" {
        println!("  Hard mode = Replace mode: denying native Read/Grep/Glob across all IDEs.");
        println!();
        let opts = crate::setup::SetupOptions {
            non_interactive: true,
            yes: true,
            fix: true,
            ..Default::default()
        };
        if let Err(e) = crate::setup::run_setup_with_options(opts) {
            eprintln!("  Setup error: {e}");
        }
        println!();
        println!("Replace mode active. All native tools denied — use ctx_* MCP tools.");
        println!("Undo with: lean-ctx harden --undo");
        return;
    }

    let mut applied = Vec::new();

    if set_env_in_mcp_configs() {
        applied.push("Set LEAN_CTX_HARDEN=1 in MCP configs");
    }

    if cleanup_claude_stale_bash_deny() {
        applied.push("Claude Code: removed stale Bash from permissions.deny (GH #799)");
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

/// Remove stale "Bash" from Claude Code's `permissions.deny` (GH #799).
///
/// Older versions added "Bash" here, which blocks ALL bash usage globally —
/// including plugin commands (e.g. codex-companion). The PreToolUse hook
/// (`lean-ctx hook deny`) already blocks agent-level native Bash, so the
/// permissions.deny entry is unnecessary and harmful.
fn cleanup_claude_stale_bash_deny() -> bool {
    let Some(home) = dirs::home_dir() else {
        return false;
    };
    let settings_path = home.join(".claude").join("settings.json");
    if !settings_path.exists() {
        return false;
    }

    let Ok(content) = std::fs::read_to_string(&settings_path) else {
        return false;
    };
    let Ok(mut json) = crate::core::jsonc::parse_jsonc(&content) else {
        return false;
    };

    let removed = if let Some(deny) = json
        .pointer_mut("/permissions/deny")
        .and_then(|d| d.as_array_mut())
    {
        let before = deny.len();
        deny.retain(|v| v.as_str() != Some("Bash"));
        deny.len() < before
    } else {
        false
    };

    if removed && let Ok(out) = serde_json::to_string_pretty(&json) {
        let _ = std::fs::write(&settings_path, out);
    }
    removed
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
