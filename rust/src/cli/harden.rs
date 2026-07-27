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

    match cleanup_claude_stale_bash_deny() {
        Ok(true) => applied.push("Claude Code: removed stale Bash from permissions.deny (GH #799)"),
        Ok(false) => {}
        Err(error) => eprintln!("  [ERROR] {error}"),
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

    let mut errors = remove_env_from_mcp_configs();
    if let Err(error) = remove_claude_permissions_deny() {
        errors.push(error);
    }

    if errors.is_empty() {
        println!("  [OK] Harden deactivated. Native tools allowed again.");
    } else {
        for error in &errors {
            eprintln!("  [ERROR] {error}");
        }
        eprintln!("  Harden could not be fully deactivated.");
    }
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

            match serde_json::to_string_pretty(&json)
                .map_err(|error| format!("cannot serialize {}: {error}", path.display()))
                .and_then(|out| write_json_config(&path, &out))
            {
                Ok(()) => {
                    any_set = true;
                    println!("  [OK] {}", path.display());
                }
                Err(error) => eprintln!("  [ERROR] {error}"),
            }
        }
    }
    any_set
}

fn remove_env_from_mcp_configs() -> Vec<String> {
    let mut errors = Vec::new();

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
            if let Err(error) = serde_json::to_string_pretty(&json)
                .map_err(|error| format!("cannot serialize {}: {error}", path.display()))
                .and_then(|out| write_json_config(&path, &out))
            {
                errors.push(error);
            }
        }
    }

    errors
}

/// Remove stale "Bash" from Claude Code's `permissions.deny` (GH #799).
///
/// Older versions added "Bash" here, which blocks ALL bash usage globally —
/// including plugin commands (e.g. codex-companion). The PreToolUse hook
/// (`lean-ctx hook deny`) already blocks agent-level native Bash, so the
/// permissions.deny entry is unnecessary and harmful.
fn cleanup_claude_stale_bash_deny() -> Result<bool, String> {
    let Some(home) = dirs::home_dir() else {
        return Ok(false);
    };
    let settings_path = home.join(".claude").join("settings.json");
    if !settings_path.exists() {
        return Ok(false);
    }

    let Ok(content) = std::fs::read_to_string(&settings_path) else {
        return Ok(false);
    };
    let Ok(mut json) = crate::core::jsonc::parse_jsonc(&content) else {
        return Ok(false);
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

    if removed {
        serde_json::to_string_pretty(&json)
            .map_err(|error| format!("cannot serialize {}: {error}", settings_path.display()))
            .and_then(|out| write_json_config(&settings_path, &out))?;
    }
    Ok(removed)
}

fn remove_claude_permissions_deny() -> Result<(), String> {
    let Some(home) = dirs::home_dir() else {
        return Ok(());
    };
    let settings_path = home.join(".claude").join("settings.json");
    if !settings_path.exists() {
        return Ok(());
    }

    let Ok(content) = std::fs::read_to_string(&settings_path) else {
        return Ok(());
    };
    let Ok(mut json) = crate::core::jsonc::parse_jsonc(&content) else {
        return Ok(());
    };

    if let Some(deny) = json
        .pointer_mut("/permissions/deny")
        .and_then(|d| d.as_array_mut())
    {
        deny.retain(|v| v.as_str() != Some("Bash"));
    }

    serde_json::to_string_pretty(&json)
        .map_err(|error| format!("cannot serialize {}: {error}", settings_path.display()))
        .and_then(|out| write_json_config(&settings_path, &out))
}

fn write_json_config(path: &std::path::Path, content: &str) -> Result<(), String> {
    crate::config_io::write_atomic(path, content)
        .map_err(|error| format!("cannot write {}: {error}", path.display()))
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
