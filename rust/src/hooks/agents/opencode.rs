use super::super::{HookMode, mcp_server_quiet_mode, resolve_binary_path};
use crate::core::config::{Config, RulesInjection, RulesScope};

pub(crate) fn install_opencode_hook_with_mode(mode: HookMode) {
    let binary = resolve_binary_path();
    let home = crate::core::home::resolve_home_dir().unwrap_or_default();
    let config_path = home.join(".config/opencode/opencode.json");
    let display_path = "~/.config/opencode/opencode.json";

    if let Some(parent) = config_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let desired = serde_json::json!({
        "type": "local",
        "command": [&binary],
        "enabled": true,
        "environment": super::super::mcp_server_env_json()
    });

    // #313: `shadow_mode` (default off) selects the OpenCode integration, and the
    // two surfaces are mutually exclusive — running both exposes lean-ctx twice
    // (the interception plugin spawns its own lean-ctx MCP client on top of the
    // `mcp.lean-ctx` server), which wastes tokens and confuses the model.
    //   • off → MCP config only: `ctx_*` are opt-in tools the model may choose.
    //   • on  → interception plugin only: native read/grep/glob/edit/bash are
    //     transparently routed through lean-ctx.
    let cfg = Config::load();
    let shadow = cfg.shadow_mode;

    if shadow {
        remove_opencode_mcp_config(&config_path, display_path);
    } else if super::super::should_register_mcp() {
        // #281: register the lean-ctx MCP server only when MCP updates are
        // enabled. The shadow plugin and dedicated-rules handling below still run.
        match mode {
            HookMode::Mcp | HookMode::Hybrid => {
                if config_path.exists() {
                    let content = std::fs::read_to_string(&config_path).unwrap_or_default();
                    if content.contains("lean-ctx") {
                        if !mcp_server_quiet_mode() {
                            eprintln!("OpenCode MCP already configured at {display_path}");
                        }
                    } else if let Ok(mut json) = crate::core::jsonc::parse_jsonc(&content)
                        && let Some(obj) = json.as_object_mut()
                    {
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
                            eprintln!(
                                "  \x1b[32m✓\x1b[0m OpenCode MCP configured at {display_path}"
                            );
                        }
                    } else {
                        tracing::error!("Failed to configure OpenCode");
                    }
                }
            }
        }
    }

    // #313: the interception plugin is opt-in via `shadow_mode`. Toggling it off
    // removes a previously installed plugin so interception actually stops.
    if shadow {
        install_opencode_plugin(&home);
    } else {
        remove_opencode_plugin(&home);
        // #442: in MCP-only mode `ctx_*` are opt-in tools the model must choose.
        // OpenCode auto-loads ~/.config/opencode/AGENTS.md, so without the
        // "prefer ctx_*" rules block the model never calls the freshly registered
        // tools. Inject it alongside the MCP (the two complete one setup) unless
        // the user opted out of MCP management or explicitly disabled rule
        // injection. `inject_rules_for_agent` already honors rules_injection=off
        // and project-only scope, and dovetails with the dedicated-mode wiring
        // below (it writes the dedicated rules file that gets registered there).
        if super::super::should_register_mcp() && cfg.setup.auto_inject_rules != Some(false) {
            let _ = crate::rules_inject::inject_rules_for_agent(&home, "OpenCode");
        }
    }

    // Dedicated rules-injection mode (#343): register the lean-ctx-owned rules
    // file via opencode.json `instructions[]` (absolute path — OpenCode resolves
    // relative entries against the CWD, not the config dir) and strip any block a
    // prior shared install left in the global AGENTS.md. The rules file itself is
    // written by rules_inject. Shared mode (default) reverses the registration.
    // When the interception plugin is active (#313) it already forces `ctx_*`
    // routing, so re-injecting the "prefer ctx_*" rules is redundant token waste
    // — skip registration and clean up any prior entry.
    let dedicated_global = !shadow
        && cfg.rules_injection_effective() == RulesInjection::Dedicated
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
        return;
    }

    // The plugin imports `@modelcontextprotocol/sdk` and `@opencode-ai/plugin`,
    // so make sure they are declared in the plugin dir's package.json.
    ensure_plugin_package_json(&plugin_dir);

    if !mcp_server_quiet_mode() {
        eprintln!(
            "  \x1b[32m✓\x1b[0m OpenCode interception plugin installed at {}",
            plugin_path.display()
        );
    }
}

/// Ensure the OpenCode plugin dir has a package.json declaring the plugin's npm
/// dependencies. Missing entries are merged into an existing file (creating the
/// `dependencies` / `devDependencies` sections when absent); a user file that
/// cannot be parsed is left untouched rather than clobbered.
fn ensure_plugin_package_json(plugin_dir: &std::path::Path) {
    let package_json_path = plugin_dir.join("package.json");
    let template_str = include_str!("../../templates/package.json");

    // Fresh install (or unreadable path) → write the template verbatim.
    let Ok(existing_str) = std::fs::read_to_string(&package_json_path) else {
        let _ = std::fs::write(&package_json_path, template_str);
        return;
    };
    let Ok(mut pkg) = serde_json::from_str::<serde_json::Value>(&existing_str) else {
        return;
    };
    let template: serde_json::Value = serde_json::from_str(template_str).unwrap_or_default();
    let Some(pkg_obj) = pkg.as_object_mut() else {
        return;
    };

    let mut changed = false;
    for section in ["dependencies", "devDependencies"] {
        let Some(required) = template.get(section).and_then(|v| v.as_object()) else {
            continue;
        };
        let existing = pkg_obj
            .entry(section.to_string())
            .or_insert_with(|| serde_json::json!({}));
        let Some(existing_obj) = existing.as_object_mut() else {
            continue;
        };
        for (key, value) in required {
            if !existing_obj.contains_key(key) {
                existing_obj.insert(key.clone(), value.clone());
                changed = true;
            }
        }
    }

    if changed && let Ok(formatted) = serde_json::to_string_pretty(&pkg) {
        let _ = std::fs::write(&package_json_path, formatted);
    }
}

/// Remove a previously installed OpenCode interception plugin so toggling
/// `shadow_mode` off actually stops interception (#313). The plugin dir's
/// package.json is intentionally left in place (it may carry user-managed deps).
pub(crate) fn remove_opencode_plugin(home: &std::path::Path) {
    let plugin_path = home.join(".config/opencode/plugins").join("lean-ctx.ts");
    if !plugin_path.exists() {
        return;
    }
    if std::fs::remove_file(&plugin_path).is_ok() && !mcp_server_quiet_mode() {
        eprintln!(
            "  OpenCode interception plugin removed (shadow_mode off; enable: lean-ctx config set shadow_mode true)"
        );
    }
}

/// Remove the `mcp.lean-ctx` entry from opencode.json. Used when `shadow_mode`
/// is on so the interception plugin is the single lean-ctx surface (#313),
/// avoiding a redundant second lean-ctx MCP server.
fn remove_opencode_mcp_config(config_path: &std::path::Path, display_path: &str) {
    let Ok(content) = std::fs::read_to_string(config_path) else {
        return;
    };
    let Ok(mut json) = crate::core::jsonc::parse_jsonc(&content) else {
        return;
    };
    let Some(obj) = json.as_object_mut() else {
        return;
    };
    let Some(mcp) = obj.get_mut("mcp").and_then(|m| m.as_object_mut()) else {
        return;
    };
    if mcp.remove("lean-ctx").is_none() {
        return;
    }
    if mcp.is_empty() {
        obj.remove("mcp");
    }
    if let Ok(formatted) = serde_json::to_string_pretty(&json)
        && std::fs::write(config_path, formatted).is_ok()
        && !mcp_server_quiet_mode()
    {
        eprintln!(
            "  OpenCode mcp.lean-ctx removed (shadow_mode on → interception plugin is the active surface) at {display_path}"
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

#[cfg(test)]
mod shadow_gating_tests {
    use super::*;

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("leanctx_oc_shadow_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn plugin_ts(home: &std::path::Path) -> std::path::PathBuf {
        home.join(".config/opencode/plugins/lean-ctx.ts")
    }

    #[test]
    fn install_then_remove_plugin_toggles_the_file() {
        let home = temp_dir("toggle");
        install_opencode_plugin(&home);
        assert!(plugin_ts(&home).exists(), "plugin must be installed");
        let pkg = home.join(".config/opencode/plugins/package.json");
        assert!(pkg.exists(), "package.json must be written");

        remove_opencode_plugin(&home);
        assert!(
            !plugin_ts(&home).exists(),
            "plugin must be removed on toggle-off"
        );
        assert!(pkg.exists(), "package.json is intentionally left in place");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn remove_plugin_is_noop_when_absent() {
        let home = temp_dir("noop");
        remove_opencode_plugin(&home); // must not panic when nothing to remove
        assert!(!plugin_ts(&home).exists());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn package_json_fresh_write_declares_plugin_deps() {
        let dir = temp_dir("pkg_fresh");
        ensure_plugin_package_json(&dir);
        let json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("package.json")).unwrap())
                .unwrap();
        assert!(json["dependencies"]["@modelcontextprotocol/sdk"].is_string());
        assert!(json["dependencies"]["@opencode-ai/plugin"].is_string());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn package_json_merge_preserves_user_deps_and_adds_ours() {
        let dir = temp_dir("pkg_merge");
        std::fs::write(
            dir.join("package.json"),
            r#"{"dependencies":{"left-pad":"^1.0.0","@opencode-ai/plugin":"^9.9.9"}}"#,
        )
        .unwrap();
        ensure_plugin_package_json(&dir);
        let json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("package.json")).unwrap())
                .unwrap();
        // User dep preserved; a user pin for a shared dep is NOT overwritten; the
        // missing dep is added.
        assert_eq!(json["dependencies"]["left-pad"], "^1.0.0");
        assert_eq!(json["dependencies"]["@opencode-ai/plugin"], "^9.9.9");
        assert!(json["dependencies"]["@modelcontextprotocol/sdk"].is_string());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn package_json_creates_missing_dependencies_section() {
        let dir = temp_dir("pkg_nodeps");
        std::fs::write(dir.join("package.json"), r#"{"name":"user-plugins"}"#).unwrap();
        ensure_plugin_package_json(&dir);
        let json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("package.json")).unwrap())
                .unwrap();
        assert_eq!(json["name"], "user-plugins", "user fields preserved");
        assert!(
            json["dependencies"]["@modelcontextprotocol/sdk"].is_string(),
            "dependencies section must be created"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn remove_mcp_config_drops_only_lean_ctx() {
        let dir = temp_dir("mcp_multi");
        let cfg = dir.join("opencode.json");
        std::fs::write(
            &cfg,
            r#"{"mcp":{"lean-ctx":{"type":"local"},"other":{"type":"local"}}}"#,
        )
        .unwrap();
        remove_opencode_mcp_config(&cfg, "test");
        let json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&cfg).unwrap()).unwrap();
        assert!(json["mcp"]["lean-ctx"].is_null(), "lean-ctx removed");
        assert!(json["mcp"]["other"].is_object(), "other server preserved");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn remove_mcp_config_drops_empty_mcp_key() {
        let dir = temp_dir("mcp_solo");
        let cfg = dir.join("opencode.json");
        std::fs::write(&cfg, r#"{"mcp":{"lean-ctx":{"type":"local"}}}"#).unwrap();
        remove_opencode_mcp_config(&cfg, "test");
        let json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&cfg).unwrap()).unwrap();
        assert!(
            json.get("mcp").is_none(),
            "empty mcp object dropped: {json}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
