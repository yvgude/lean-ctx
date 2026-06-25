use super::super::{mcp_server_quiet_mode, resolve_binary_path, write_file};

pub(crate) fn install_antigravity_hook() {
    let Some(home) = crate::core::home::resolve_home_dir() else {
        tracing::error!("Cannot resolve home directory");
        return;
    };

    install_antigravity_mcp_config(&home, "antigravity");
    install_antigravity_gemini_hooks(&home);
}

pub(crate) fn install_antigravity_cli_hook() {
    let Some(home) = crate::core::home::resolve_home_dir() else {
        tracing::error!("Cannot resolve home directory");
        return;
    };

    install_antigravity_mcp_config(&home, "antigravity-cli");
    install_antigravity_cli_hooks(&home);
}

fn install_antigravity_mcp_config(home: &std::path::Path, subdir: &str) {
    // #281: honor `[setup] auto_update_mcp = false` — skip the Antigravity MCP
    // server entry under lock-down; the plugin hooks still install separately.
    if !crate::core::config::Config::load()
        .setup
        .should_update_mcp()
    {
        return;
    }
    let binary = resolve_binary_path();
    let config_path = home.join(".gemini").join(subdir).join("mcp_config.json");

    if let Some(parent) = config_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let existing = if config_path.exists() {
        std::fs::read_to_string(&config_path).unwrap_or_default()
    } else {
        String::new()
    };

    let already_configured = existing.contains("lean-ctx");
    if already_configured {
        if !mcp_server_quiet_mode() {
            let label = if subdir == "antigravity-cli" {
                "Antigravity CLI"
            } else {
                "Antigravity"
            };
            eprintln!(
                "{label} MCP: lean-ctx already configured at {}",
                config_path.display()
            );
        }
        return;
    }

    let config = serde_json::json!({
        "mcpServers": {
            "lean-ctx": {
                "command": binary
            }
        }
    });

    if existing.is_empty() || existing.trim() == "{}" || existing.contains("\"mcpServers\": {}") {
        write_file(
            &config_path,
            &serde_json::to_string_pretty(&config).unwrap_or_default(),
        );
    } else if let Ok(mut existing_json) = crate::core::jsonc::parse_jsonc(&existing)
        && let Some(obj) = existing_json.as_object_mut()
    {
        let servers = obj
            .entry("mcpServers")
            .or_insert_with(|| serde_json::json!({}));
        if let Some(servers_obj) = servers.as_object_mut() {
            servers_obj.insert(
                "lean-ctx".to_string(),
                serde_json::json!({ "command": binary }),
            );
        }
        write_file(
            &config_path,
            &serde_json::to_string_pretty(&existing_json).unwrap_or_default(),
        );
    }

    if !mcp_server_quiet_mode() {
        eprintln!(
            "Installed Antigravity MCP config at {}",
            config_path.display()
        );
    }
}

fn install_antigravity_gemini_hooks(home: &std::path::Path) {
    let binary = resolve_binary_path();
    let rewrite_cmd = format!("{binary} hook rewrite");
    let redirect_cmd = format!("{binary} hook redirect");
    let observe_cmd = format!("{binary} hook observe");

    let settings_path = home.join(".gemini").join("settings.json");
    let settings_content = if settings_path.exists() {
        std::fs::read_to_string(&settings_path).unwrap_or_default()
    } else {
        String::new()
    };

    let has_hooks = settings_content.contains("hook rewrite")
        && settings_content.contains("hook redirect")
        && settings_content.contains("\"matcher\"");
    let has_observe = settings_content.contains("hook observe");

    if has_hooks && has_observe {
        return;
    }

    let hook_config = serde_json::json!({
        "hooks": {
            "BeforeTool": [
                {
                    "matcher": "shell|execute_command|run_shell_command|run_command",
                    "hooks": [{
                        "type": "command",
                        "command": rewrite_cmd
                    }]
                },
                {
                    "matcher": "read_file|view_file|read_many_files|grep|grep_search|search|list_dir",
                    "hooks": [{
                        "type": "command",
                        "command": redirect_cmd
                    }]
                }
            ],
            "AfterTool": [
                {
                    "matcher": ".*",
                    "hooks": [{
                        "type": "command",
                        "command": observe_cmd
                    }]
                }
            ]
        }
    });

    if settings_content.is_empty() {
        write_file(
            &settings_path,
            &serde_json::to_string_pretty(&hook_config).unwrap_or_default(),
        );
    } else if let Ok(mut existing) = crate::core::jsonc::parse_jsonc(&settings_content)
        && let Some(obj) = existing.as_object_mut()
    {
        if has_hooks && !has_observe {
            let hooks = obj
                .entry("hooks".to_string())
                .or_insert_with(|| serde_json::json!({}));
            if let Some(hooks_obj) = hooks.as_object_mut() {
                hooks_obj.insert(
                    "AfterTool".to_string(),
                    hook_config["hooks"]["AfterTool"].clone(),
                );
            }
        } else {
            obj.insert("hooks".to_string(), hook_config["hooks"].clone());
        }
        write_file(
            &settings_path,
            &serde_json::to_string_pretty(&existing).unwrap_or_default(),
        );
    }

    if !mcp_server_quiet_mode() {
        eprintln!(
            "Installed Gemini/Antigravity hooks at {}",
            settings_path.parent().unwrap_or(&settings_path).display()
        );
    }
}

/// Path of the lean-ctx plugin directory inside the shared Gemini config that
/// the Antigravity CLI (`agy`) scans (`~/.gemini/config/plugins/lean-ctx`).
pub(crate) fn antigravity_cli_plugin_dir(home: &std::path::Path) -> std::path::PathBuf {
    antigravity_cli_config_dir(home)
        .join("plugins")
        .join("lean-ctx")
}

/// The shared Gemini config dir the Antigravity CLI reads plugins + the import
/// manifest from (`~/.gemini/config`).
pub(crate) fn antigravity_cli_config_dir(home: &std::path::Path) -> std::path::PathBuf {
    home.join(".gemini").join("config")
}

/// Install the lean-ctx **plugin** for the Antigravity CLI 2.0 (`agy`).
///
/// Ground truth, verified against the installed `agy` binary (June 2026, via
/// `agy plugin validate`/`install` and binary-string analysis):
///
/// * `agy` loads hooks **only from plugins**, never from `settings.json` (whose
///   schema is `{enableTelemetry, model, trustedWorkspaces}`). A plugin is a
///   directory `~/.gemini/config/plugins/<name>/` with a root `plugin.json`
///   (the `name` field is mandatory) and `hooks/hooks.json`, registered in
///   `~/.gemini/config/import_manifest.json`. Writing a `hooks` key into the
///   CLI's `settings.json` — as lean-ctx did before — is silently ignored; that
///   is the true root cause of GH #284.
/// * The hook I/O contract is **Gemini-style**: stdin carries
///   `tool_name`/`tool_input`/`cwd`/`session_id`; stdout honours
///   `block`/`reason`/`continue`/`systemMessage` and exit codes. It does **not**
///   honour Claude's `hookSpecificOutput.updatedInput`, so a `PreToolUse` hook
///   cannot rewrite a command/argument here. Token compression is therefore
///   delivered through the lean-ctx **MCP** `ctx_*` tools (installed separately
///   via `mcp_config.json`); the plugin carries only the `observe` hooks, which
///   work purely through lean-ctx-side telemetry/session side effects and need
///   no host-honoured stdout.
/// * Events are `PreToolUse`/`PostToolUse`/`SessionStart`/`Stop` (NOT the legacy
///   Gemini `BeforeTool`/`AfterTool`); the shell tool is `run_command`, the file
///   read tool `view_file`.
/// * **MCP is bundled inside the plugin** (`mcp_config.json` at the plugin root).
///   `agy` loads it — verified via `agy plugin validate` ("mcpServers: processed")
///   and a live session surfacing an `McpTool` confirmation. This makes the bundle
///   a self-contained, spec-"compliant" plugin (#284) and portable via
///   `agy plugin install`/export. The profile copy
///   (`~/.gemini/antigravity-cli/mcp_config.json`) is kept for back-compat; `agy`
///   keys MCP servers by name, so listing lean-ctx in both is harmless.
/// * **Hook *firing* is gated by `agy` itself, not by file placement.** `agy` only
///   executes `hooks.json` when its server-side feature flag `enable_json_hooks`
///   (proto field, applied via `applyFeatureProviderJSONHooksConfig`; experiment
///   `json-hooks-enabled`) is enabled for the account. A local
///   `~/.gemini/config/config.json` override does NOT activate it (verified). So
///   lean-ctx installs the plugin in the exact location/format that
///   `agy plugin install` itself produces, and the observe hooks light up
///   automatically once that flag rolls out — there is nothing more lean-ctx can
///   do host-side. Note: `agy -p` print mode bypasses the hook subsystem entirely
///   (hooks run in interactive sessions only).
fn install_antigravity_cli_hooks(home: &std::path::Path) {
    let binary = resolve_binary_path();
    let observe_cmd = format!("{binary} hook observe");

    let plugin_dir = antigravity_cli_plugin_dir(home);
    let hooks_dir = plugin_dir.join("hooks");
    if let Err(e) = std::fs::create_dir_all(&hooks_dir) {
        tracing::error!("Cannot create Antigravity CLI plugin dir: {e}");
        return;
    }

    let manifest = serde_json::json!({
        "name": "lean-ctx",
        "version": env!("CARGO_PKG_VERSION"),
        "description":
            "lean-ctx context engineering — session telemetry hooks; token \
             compression is provided by the lean-ctx MCP tools (ctx_*).",
    });
    write_file(
        &plugin_dir.join("plugin.json"),
        &serde_json::to_string_pretty(&manifest).unwrap_or_default(),
    );

    // Self-contained, spec-"compliant" bundle: ship the MCP definition inside the
    // plugin so `agy` exposes the `ctx_*` tools and the plugin stays portable.
    let mcp_config = serde_json::json!({
        "mcpServers": { "lean-ctx": { "command": binary } }
    });
    write_file(
        &plugin_dir.join("mcp_config.json"),
        &serde_json::to_string_pretty(&mcp_config).unwrap_or_default(),
    );

    // observe-only: agy ignores PreToolUse input rewriting, so the rewrite /
    // redirect hooks lean-ctx ships for Claude/Cursor would be inert dead weight
    // here. The observe hook records token telemetry + session continuity as a
    // pure side effect, which works regardless of how the host treats stdout.
    let mut hook_map = serde_json::Map::new();
    for event in ["PostToolUse", "SessionStart", "Stop"] {
        hook_map.insert(
            event.to_string(),
            serde_json::json!([
                { "matcher": ".*", "hooks": [{ "type": "command", "command": observe_cmd }] }
            ]),
        );
    }
    let hooks = serde_json::json!({ "hooks": serde_json::Value::Object(hook_map) });
    write_file(
        &hooks_dir.join("hooks.json"),
        &serde_json::to_string_pretty(&hooks).unwrap_or_default(),
    );

    register_antigravity_plugin(&antigravity_cli_config_dir(home));

    if !mcp_server_quiet_mode() {
        eprintln!(
            "Installed Antigravity CLI plugin at {}",
            plugin_dir.display()
        );
    }
}

/// Register the lean-ctx plugin in `~/.gemini/config/import_manifest.json`,
/// mirroring what `agy plugin install` writes. Idempotent: a pre-existing
/// `lean-ctx` entry (or a `null`/missing `imports`, which `agy plugin uninstall`
/// leaves behind) is handled without duplicating the entry or clobbering other
/// users' imports.
fn register_antigravity_plugin(config_dir: &std::path::Path) {
    let manifest_path = config_dir.join("import_manifest.json");

    let mut json = std::fs::read_to_string(&manifest_path)
        .ok()
        .and_then(|s| crate::core::jsonc::parse_jsonc(&s).ok())
        .filter(serde_json::Value::is_object)
        .unwrap_or_else(|| serde_json::json!({ "imports": [] }));

    let Some(obj) = json.as_object_mut() else {
        return;
    };
    let entry = obj
        .entry("imports".to_string())
        .or_insert_with(|| serde_json::json!([]));
    // `agy plugin uninstall` collapses `imports` to `null`; normalise it back.
    if !entry.is_array() {
        *entry = serde_json::json!([]);
    }
    let Some(arr) = entry.as_array_mut() else {
        return;
    };
    let already = arr
        .iter()
        .any(|e| e.get("name").and_then(|n| n.as_str()) == Some("lean-ctx"));
    if already {
        return;
    }
    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    arr.push(serde_json::json!({
        "name": "lean-ctx",
        "source": "local-install",
        "importedAt": now,
        "components": ["installed"],
    }));
    write_file(
        &manifest_path,
        &serde_json::to_string_pretty(&json).unwrap_or_default(),
    );
}

/// Remove the lean-ctx plugin directory and its `import_manifest.json` entry from
/// the Antigravity CLI config. Used by `lean-ctx uninstall`. Other users' plugin
/// imports are preserved; an emptied `imports` array is left in place.
pub(crate) fn uninstall_antigravity_cli_plugin(home: &std::path::Path) -> bool {
    let mut changed = false;
    let plugin_dir = antigravity_cli_plugin_dir(home);
    if plugin_dir.exists() {
        let _ = std::fs::remove_dir_all(&plugin_dir);
        changed = true;
    }

    let manifest_path = antigravity_cli_config_dir(home).join("import_manifest.json");
    if let Ok(content) = std::fs::read_to_string(&manifest_path)
        && let Ok(mut json) = crate::core::jsonc::parse_jsonc(&content)
        && let Some(arr) = json.get_mut("imports").and_then(|i| i.as_array_mut())
    {
        let before = arr.len();
        arr.retain(|e| e.get("name").and_then(|n| n.as_str()) != Some("lean-ctx"));
        if arr.len() != before {
            changed = true;
            let _ = std::fs::write(
                &manifest_path,
                serde_json::to_string_pretty(&json).unwrap_or_default(),
            );
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_json(path: &std::path::Path) -> serde_json::Value {
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    // GH #284: `agy` loads hooks ONLY from a plugin under
    // `~/.gemini/config/plugins/lean-ctx/` (root plugin.json + hooks/hooks.json),
    // registered in `~/.gemini/config/import_manifest.json`. It must NOT touch
    // the CLI's settings.json (ignored) nor the legacy global ~/.gemini/settings.json.
    #[test]
    fn cli_hooks_install_as_plugin_not_settings_json() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();

        install_antigravity_cli_hooks(home);

        let plugin_json = home.join(".gemini/config/plugins/lean-ctx/plugin.json");
        let hooks_json = home.join(".gemini/config/plugins/lean-ctx/hooks/hooks.json");
        assert!(
            plugin_json.exists(),
            "plugin.json must exist at {plugin_json:?}"
        );
        assert!(
            hooks_json.exists(),
            "hooks/hooks.json must exist at {hooks_json:?}"
        );

        // The plugin is self-contained: MCP lives in the plugin root so `agy`
        // exposes the ctx_* tools (verified against `agy plugin validate`).
        let mcp_json = home.join(".gemini/config/plugins/lean-ctx/mcp_config.json");
        assert!(
            mcp_json.exists(),
            "plugin-local mcp_config.json must exist at {mcp_json:?}"
        );
        let mcp = read_json(&mcp_json);
        assert!(
            mcp["mcpServers"]["lean-ctx"]["command"].is_string(),
            "plugin mcp_config.json must define the lean-ctx MCP server"
        );

        // plugin.json carries the mandatory `name` field.
        let manifest = read_json(&plugin_json);
        assert_eq!(manifest["name"], "lean-ctx", "plugin manifest needs name");

        // hooks.json uses the agy-honoured PreToolUse/PostToolUse-family events
        // and the observe handler (no rewrite/redirect — agy ignores updatedInput).
        let hooks = read_json(&hooks_json);
        let map = hooks["hooks"].as_object().unwrap();
        for event in ["PostToolUse", "SessionStart", "Stop"] {
            assert!(map.contains_key(event), "missing observe event {event}");
        }
        let content = std::fs::read_to_string(&hooks_json).unwrap();
        assert!(
            content.contains("hook observe"),
            "must wire the observe hook"
        );
        assert!(
            !content.contains("hook rewrite") && !content.contains("hook redirect"),
            "rewrite/redirect cannot work on agy (no updatedInput) — must not be installed"
        );
        assert!(
            !content.contains("BeforeTool") && !content.contains("AfterTool"),
            "must not use the legacy Gemini event names"
        );

        // The import manifest registers the plugin so `agy plugin list` sees it.
        let manifest_path = home.join(".gemini/config/import_manifest.json");
        let imports = read_json(&manifest_path);
        let arr = imports["imports"].as_array().unwrap();
        assert_eq!(
            arr.iter().filter(|e| e["name"] == "lean-ctx").count(),
            1,
            "exactly one lean-ctx import entry"
        );

        // Never write to the wrong (ignored) locations.
        assert!(
            !home.join(".gemini/antigravity-cli/settings.json").exists(),
            "must not write hooks into the CLI settings.json (ignored by agy)"
        );
        assert!(
            !home.join(".gemini/settings.json").exists(),
            "CLI install must not touch the legacy ~/.gemini/settings.json"
        );
    }

    #[test]
    fn cli_plugin_install_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let manifest_path = home.join(".gemini/config/import_manifest.json");

        install_antigravity_cli_hooks(home);
        let first = std::fs::read_to_string(&manifest_path).unwrap();
        install_antigravity_cli_hooks(home);
        let second = std::fs::read_to_string(&manifest_path).unwrap();

        let arr = read_json(&manifest_path);
        assert_eq!(
            arr["imports"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|e| e["name"] == "lean-ctx")
                .count(),
            1,
            "re-running install must not duplicate the import entry"
        );
        // The importedAt timestamp differs across runs only when a new entry is
        // added; idempotent runs leave the manifest untouched.
        assert_eq!(
            first, second,
            "re-running install must not churn the manifest"
        );
    }

    #[test]
    fn register_normalizes_null_imports_from_uninstall() {
        // `agy plugin uninstall` leaves `{ "imports": null }`. Re-installing must
        // recover gracefully instead of failing or double-wrapping.
        let tmp = tempfile::tempdir().unwrap();
        let config_dir = antigravity_cli_config_dir(tmp.path());
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(
            config_dir.join("import_manifest.json"),
            r#"{"imports": null}"#,
        )
        .unwrap();

        register_antigravity_plugin(&config_dir);

        let v = read_json(&config_dir.join("import_manifest.json"));
        let arr = v["imports"]
            .as_array()
            .expect("imports normalised to array");
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["name"], "lean-ctx");
    }

    #[test]
    fn register_preserves_foreign_imports() {
        let tmp = tempfile::tempdir().unwrap();
        let config_dir = antigravity_cli_config_dir(tmp.path());
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(
            config_dir.join("import_manifest.json"),
            r#"{"imports":[{"name":"other-plugin","source":"local-install"}]}"#,
        )
        .unwrap();

        register_antigravity_plugin(&config_dir);

        let v = read_json(&config_dir.join("import_manifest.json"));
        let arr = v["imports"].as_array().unwrap();
        assert!(
            arr.iter().any(|e| e["name"] == "other-plugin"),
            "must preserve the user's other plugins"
        );
        assert!(arr.iter().any(|e| e["name"] == "lean-ctx"));
    }

    #[test]
    fn uninstall_removes_plugin_and_manifest_entry_but_keeps_others() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let config_dir = antigravity_cli_config_dir(home);
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(
            config_dir.join("import_manifest.json"),
            r#"{"imports":[{"name":"other-plugin"}]}"#,
        )
        .unwrap();

        install_antigravity_cli_hooks(home);
        assert!(antigravity_cli_plugin_dir(home).exists());

        let changed = uninstall_antigravity_cli_plugin(home);
        assert!(changed, "uninstall must report a change");
        assert!(
            !antigravity_cli_plugin_dir(home).exists(),
            "plugin dir must be removed"
        );

        let v = read_json(&config_dir.join("import_manifest.json"));
        let arr = v["imports"].as_array().unwrap();
        assert!(
            !arr.iter().any(|e| e["name"] == "lean-ctx"),
            "lean-ctx entry must be gone"
        );
        assert!(
            arr.iter().any(|e| e["name"] == "other-plugin"),
            "foreign entries must survive uninstall"
        );
    }
}
