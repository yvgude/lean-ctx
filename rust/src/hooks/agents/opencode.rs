use super::super::{mcp_server_quiet_mode, resolve_binary_path, HookMode};
use crate::core::config::{Config, RulesInjection, RulesScope};

pub(crate) fn install_opencode_hook_with_mode(mode: HookMode) {
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

    match mode {
        HookMode::Mcp | HookMode::Hybrid => {
            if config_path.exists() {
                let content = std::fs::read_to_string(&config_path).unwrap_or_default();
                if content.contains("lean-ctx") {
                    if !mcp_server_quiet_mode() {
                        eprintln!("OpenCode MCP already configured at {display_path}");
                    }
                } else if let Ok(mut json) = crate::core::jsonc::parse_jsonc(&content) {
                    if let Some(obj) = json.as_object_mut() {
                        let mcp = obj.entry("mcp").or_insert_with(|| serde_json::json!({}));
                        if let Some(mcp_obj) = mcp.as_object_mut() {
                            mcp_obj.insert("lean-ctx".to_string(), desired.clone());
                        }
                        if let Ok(formatted) = serde_json::to_string_pretty(&json) {
                            let backup = config_path.with_extension("json.bak");
                            let _ = std::fs::copy(&config_path, &backup);
                            let _ = std::fs::write(&config_path, formatted);
                            if !mcp_server_quiet_mode() {
                                eprintln!(
                                    "  \x1b[32m✓\x1b[0m OpenCode MCP configured at {display_path}"
                                );
                            }
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
                    if !mcp_server_quiet_mode() {
                        eprintln!("  \x1b[32m✓\x1b[0m OpenCode MCP configured at {display_path}");
                    }
                } else {
                    tracing::error!("Failed to configure OpenCode");
                }
            }
        }
    }

    install_opencode_plugin(&home);

    // Dedicated rules-injection mode (#343): register the lean-ctx-owned rules
    // file via opencode.json `instructions[]` (absolute path — OpenCode resolves
    // relative entries against the CWD, not the config dir) and strip any block a
    // prior shared install left in the global AGENTS.md. The rules file itself is
    // written by rules_inject. Shared mode (default) reverses the registration.
    let cfg = Config::load();
    let dedicated_global = cfg.rules_injection_effective() == RulesInjection::Dedicated
        && cfg.rules_scope_effective() != RulesScope::Project;
    if dedicated_global {
        register_opencode_instructions(&home);
        strip_opencode_agents_block(&home);
    } else {
        unregister_opencode_instructions(&home);
    }
}

fn opencode_config_path(home: &std::path::Path) -> std::path::PathBuf {
    home.join(".config/opencode/opencode.json")
}

/// Add the dedicated rules file to opencode.json `instructions[]` (idempotent).
fn register_opencode_instructions(home: &std::path::Path) {
    let config_path = opencode_config_path(home);
    let rules_str = crate::rules_inject::opencode_dedicated_rules_path(home)
        .to_string_lossy()
        .into_owned();

    let mut json = match std::fs::read_to_string(&config_path) {
        Ok(content) => crate::core::jsonc::parse_jsonc(&content).unwrap_or_else(
            |_| serde_json::json!({ "$schema": "https://opencode.ai/config.json" }),
        ),
        Err(_) => serde_json::json!({ "$schema": "https://opencode.ai/config.json" }),
    };

    let Some(obj) = json.as_object_mut() else {
        return;
    };
    let instr = obj
        .entry("instructions".to_string())
        .or_insert_with(|| serde_json::json!([]));
    if !instr.is_array() {
        *instr = serde_json::json!([]);
    }
    let arr = instr.as_array_mut().expect("instructions is an array");
    if arr.iter().any(|v| v.as_str() == Some(rules_str.as_str())) {
        return;
    }
    arr.push(serde_json::Value::String(rules_str));

    if let (Some(parent), Ok(formatted)) =
        (config_path.parent(), serde_json::to_string_pretty(&json))
    {
        let _ = std::fs::create_dir_all(parent);
        let _ = std::fs::write(&config_path, formatted);
        if !mcp_server_quiet_mode() {
            eprintln!(
                "  \x1b[32m✓\x1b[0m OpenCode rules registered in opencode.json instructions[]"
            );
        }
    }
}

/// Remove the lean-ctx instructions[] entry (shared-mode cleanup / toggle-back).
/// Remove the lean-ctx `instructions[]` entry from opencode.json. Used both for
/// shared-mode toggle-back and uninstall cleanup.
pub(crate) fn unregister_opencode_instructions(home: &std::path::Path) {
    let config_path = opencode_config_path(home);
    let Ok(content) = std::fs::read_to_string(&config_path) else {
        return;
    };
    let Ok(mut json) = crate::core::jsonc::parse_jsonc(&content) else {
        return;
    };
    let Some(obj) = json.as_object_mut() else {
        return;
    };
    let Some(arr) = obj.get_mut("instructions").and_then(|v| v.as_array_mut()) else {
        return;
    };
    let rules_str = crate::rules_inject::opencode_dedicated_rules_path(home)
        .to_string_lossy()
        .into_owned();
    let before = arr.len();
    arr.retain(|v| v.as_str() != Some(rules_str.as_str()));
    if arr.len() == before {
        return;
    }
    if arr.is_empty() {
        obj.remove("instructions");
    }
    if let Ok(formatted) = serde_json::to_string_pretty(&json) {
        let _ = std::fs::write(&config_path, formatted);
    }
}

/// Strip the lean-ctx block from the global OpenCode AGENTS.md (dedicated mode).
fn strip_opencode_agents_block(home: &std::path::Path) {
    let agents = home.join(".config/opencode/AGENTS.md");
    if agents
        .metadata()
        .is_ok_and(|m| m.is_file())
        .then(|| std::fs::read_to_string(&agents).ok())
        .flatten()
        .is_some_and(|c| c.contains(crate::rules_inject::RULES_MARKER))
    {
        crate::marked_block::remove_from_file(
            &agents,
            crate::rules_inject::RULES_MARKER,
            crate::rules_inject::RULES_END_MARKER,
            true,
            "OpenCode AGENTS.md lean-ctx block",
        );
    }
}

fn install_opencode_plugin(home: &std::path::Path) {
    let plugin_dir = home.join(".config/opencode/plugins");
    let _ = std::fs::create_dir_all(&plugin_dir);
    let plugin_path = plugin_dir.join("lean-ctx.ts");

    let plugin_content = include_str!("../../templates/opencode-plugin.ts");
    if let Err(e) = std::fs::write(&plugin_path, plugin_content) {
        eprintln!("  \x1b[33m⚠\x1b[0m Failed to write OpenCode plugin: {e}");
    }

    let package_json_path = plugin_dir.join("package.json");
    let package_json_content = include_str!("../../templates/package.json");

    if package_json_path.exists() {
        if let Ok(content) = std::fs::read_to_string(&package_json_path) {
            if let Ok(mut pkg) = serde_json::from_str::<serde_json::Value>(&content) {
                let template: serde_json::Value =
                    serde_json::from_str(package_json_content).unwrap_or_default();
                let mut changed = false;

                // Merge dependencies from template
                if let (Some(existing), Some(required)) =
                    (pkg.get_mut("dependencies"), template.get("dependencies"))
                {
                    if let (Some(existing_obj), Some(required_obj)) =
                        (existing.as_object_mut(), required.as_object())
                    {
                        for (key, value) in required_obj {
                            if !existing_obj.contains_key(key) {
                                existing_obj.insert(key.clone(), value.clone());
                                changed = true;
                            }
                        }
                    }
                }

                if changed {
                    if let Ok(formatted) = serde_json::to_string_pretty(&pkg) {
                        if let Err(e) = std::fs::write(&package_json_path, formatted) {
                            eprintln!("  \x1b[33m⚠\x1b[0m Failed to update package.json: {e}");
                        }
                    }
                }
            }
        }
    } else if let Err(e) = std::fs::write(&package_json_path, package_json_content) {
        eprintln!("  \x1b[33m⚠\x1b[0m Failed to write package.json: {e}");
    }

    if !mcp_server_quiet_mode() {
        eprintln!(
            "  \x1b[32m✓\x1b[0m OpenCode plugin installed at {}",
            plugin_path.display()
        );
    }
}

#[cfg(test)]
mod dedicated_tests {
    use super::*;

    fn temp_home(tag: &str) -> std::path::PathBuf {
        let home =
            std::env::temp_dir().join(format!("leanctx_opencode_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join(".config/opencode")).unwrap();
        home
    }

    fn read_instructions(home: &std::path::Path) -> Vec<String> {
        let content = std::fs::read_to_string(opencode_config_path(home)).unwrap();
        let json: serde_json::Value = serde_json::from_str(&content).unwrap();
        json["instructions"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn register_adds_absolute_dedicated_path() {
        let home = temp_home("add");
        register_opencode_instructions(&home);
        let expected = crate::rules_inject::opencode_dedicated_rules_path(&home)
            .to_string_lossy()
            .into_owned();
        assert_eq!(read_instructions(&home), vec![expected]);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn register_is_idempotent() {
        let home = temp_home("idem");
        register_opencode_instructions(&home);
        register_opencode_instructions(&home);
        assert_eq!(read_instructions(&home).len(), 1);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn register_preserves_user_instructions() {
        let home = temp_home("preserve");
        std::fs::write(
            opencode_config_path(&home),
            r#"{"instructions":["./CONTRIBUTING.md"]}"#,
        )
        .unwrap();
        register_opencode_instructions(&home);
        let instrs = read_instructions(&home);
        assert!(instrs.contains(&"./CONTRIBUTING.md".to_string()));
        assert_eq!(instrs.len(), 2);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn unregister_removes_only_our_entry() {
        let home = temp_home("unreg");
        std::fs::write(
            opencode_config_path(&home),
            r#"{"instructions":["./CONTRIBUTING.md"]}"#,
        )
        .unwrap();
        register_opencode_instructions(&home);
        unregister_opencode_instructions(&home);
        assert_eq!(read_instructions(&home), vec!["./CONTRIBUTING.md"]);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn unregister_drops_empty_instructions_key() {
        let home = temp_home("empty");
        register_opencode_instructions(&home);
        unregister_opencode_instructions(&home);
        let content = std::fs::read_to_string(opencode_config_path(&home)).unwrap();
        let json: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert!(json.get("instructions").is_none(), "got: {content}");
        let _ = std::fs::remove_dir_all(&home);
    }
}
