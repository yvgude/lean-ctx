use std::path::PathBuf;

use super::super::{
    mcp_server_quiet_mode, resolve_binary_path, to_bash_compatible_path, write_file,
};

pub(crate) fn install_copilot_hook(global: bool) {
    let binary = resolve_binary_path();
    // #281: honor `[setup] auto_update_mcp = false`. The PreToolUse hook is the
    // CLI integration and always installs; the MCP *server* registrations below
    // are the part locked-down environments opt out of, so they are gated.
    let update_mcp = crate::core::config::Config::load()
        .setup
        .should_update_mcp();

    if global {
        // Copilot CLI loads global MCP servers from `~/.copilot/mcp-config.json`,
        // independent of any project/repo config. Only register lean-ctx there
        // for global installs (the repo-scoped `.github/mcp.json` covers local).
        if update_mcp {
            write_copilot_cli_home_mcp();
        }

        let mcp_path = crate::core::editor_registry::vscode_mcp_path();
        if mcp_path.as_os_str() == "/nonexistent" {
            if !mcp_server_quiet_mode() {
                eprintln!("  \x1b[2mVS Code not found — skipping global Copilot config\x1b[0m");
            }
            return;
        }
        if update_mcp {
            write_vscode_mcp_file(&mcp_path, &binary, "global VS Code User MCP");
        }
        install_copilot_pretooluse_hook(true);
    } else {
        if update_mcp {
            let vscode_dir = PathBuf::from(".vscode");
            let _ = std::fs::create_dir_all(&vscode_dir);
            let mcp_path = vscode_dir.join("mcp.json");
            write_vscode_mcp_file(&mcp_path, &binary, ".vscode/mcp.json");

            let github_dir = PathBuf::from(".github");
            let _ = std::fs::create_dir_all(&github_dir);
            let copilot_mcp = github_dir.join("mcp.json");
            write_copilot_cli_mcp_file(&copilot_mcp, &binary, ".github/mcp.json");
        }

        install_copilot_pretooluse_hook(false);
    }
}

/// Register lean-ctx in the Copilot CLI's global MCP config at
/// `~/.copilot/mcp-config.json`. Reuses the canonical `CopilotCli` writer so the
/// entry format and merge behavior match `configure_agent_mcp`, and uses the
/// portable binary path so repeated installs stay idempotent (no churn).
fn write_copilot_cli_home_mcp() {
    let Some(home) = dirs::home_dir() else {
        return;
    };

    let binary = crate::core::portable_binary::resolve_portable_binary();
    let target = crate::core::editor_registry::EditorTarget {
        name: "Copilot CLI",
        agent_key: "copilot".to_string(),
        detect_path: PathBuf::from("/nonexistent"),
        config_path: home.join(".copilot/mcp-config.json"),
        config_type: crate::core::editor_registry::ConfigType::CopilotCli,
    };

    match crate::core::editor_registry::write_config_with_options(
        &target,
        &binary,
        crate::core::editor_registry::WriteOptions {
            overwrite_invalid: true,
        },
    ) {
        Ok(result) => {
            if !mcp_server_quiet_mode() {
                use crate::core::editor_registry::WriteAction;
                let label = "~/.copilot/mcp-config.json";
                let msg = match result.action {
                    WriteAction::Created => format!("Created {label} with lean-ctx MCP server"),
                    WriteAction::Updated => format!("Added lean-ctx to {label}"),
                    WriteAction::Already => format!("lean-ctx already configured in {label}"),
                };
                eprintln!("  \x1b[32m✓\x1b[0m {msg}");
            }
        }
        Err(e) => {
            if !mcp_server_quiet_mode() {
                eprintln!(
                    "  \x1b[33m⚠\x1b[0m  Could not configure {}: {e}",
                    target.config_path.display()
                );
            }
        }
    }
}

/// One Copilot hook entry. Copilot CLI runs the `bash` field on Unix and the
/// `powershell` field on Windows (#381) — an entry carrying only `bash` has no
/// runnable command on Windows, so the hook errors and the CLI rejects the tool
/// call. Both commands quote the binary: Windows install paths routinely
/// contain spaces (`C:\Users\Jane Doe\AppData\...`).
fn copilot_hook_entry(binary: &str, action: &str, timeout_sec: u64) -> serde_json::Value {
    let bash_binary = to_bash_compatible_path(binary);
    serde_json::json!({
        "type": "command",
        "bash": format!("\"{bash_binary}\" hook {action}"),
        "powershell": format!("& \"{binary}\" hook {action}"),
        "timeoutSec": timeout_sec
    })
}

/// User-level hooks directory of the Copilot CLI: `$COPILOT_HOME/hooks/` when
/// set, else `~/.copilot/hooks/`. Note `~/.github/hooks/` is *not* read by
/// Copilot — only the repo-level `.github/hooks/` is (#381).
fn copilot_user_hooks_dir() -> Option<PathBuf> {
    if let Ok(copilot_home) = std::env::var("COPILOT_HOME") {
        let trimmed = copilot_home.trim();
        if !trimmed.is_empty() {
            return Some(PathBuf::from(trimmed).join("hooks"));
        }
    }
    crate::core::home::resolve_home_dir().map(|h| h.join(".copilot").join("hooks"))
}

/// Earlier releases wrote global hooks to `~/.github/hooks/hooks.json`, which
/// Copilot never loads. Strip our entries from that stale file; delete it when
/// it was ours alone (a bare `{version, hooks}` skeleton with no foreign hooks).
fn cleanup_legacy_global_hooks() {
    let Some(home) = crate::core::home::resolve_home_dir() else {
        return;
    };
    let legacy = home.join(".github").join("hooks").join("hooks.json");
    if cleanup_legacy_global_hooks_at(&legacy) && !mcp_server_quiet_mode() {
        eprintln!(
            "  \x1b[2mMigrated stale ~/.github/hooks/hooks.json (not read by Copilot)\x1b[0m"
        );
    }
}

/// Returns `true` when the legacy file was migrated (rewritten or removed).
fn cleanup_legacy_global_hooks_at(legacy: &std::path::Path) -> bool {
    let Ok(content) = std::fs::read_to_string(legacy) else {
        return false;
    };
    if !content.contains("lean-ctx") && !content.contains("hook rewrite") {
        return false;
    }
    let Ok(mut json) = crate::core::jsonc::parse_jsonc(&content) else {
        return false;
    };
    let mut foreign_left = false;
    if let Some(hooks) = json.get_mut("hooks").and_then(|h| h.as_object_mut()) {
        for entries in hooks.values_mut() {
            if let Some(arr) = entries.as_array_mut() {
                arr.retain(|e| {
                    let s = e.to_string();
                    !(s.contains("lean-ctx") || s.contains("hook rewrite"))
                });
                foreign_left |= !arr.is_empty();
            }
        }
        hooks.retain(|_, v| v.as_array().is_none_or(|a| !a.is_empty()));
    }
    if foreign_left {
        write_file(
            legacy,
            &serde_json::to_string_pretty(&json).unwrap_or_default(),
        );
    } else {
        let _ = std::fs::remove_file(legacy);
    }
    true
}

fn install_copilot_pretooluse_hook(global: bool) {
    let binary = resolve_binary_path();

    let hook_config = serde_json::json!({
        "version": 1,
        "hooks": {
            "preToolUse": [
                copilot_hook_entry(&binary, "rewrite", 15),
                copilot_hook_entry(&binary, "redirect", 5)
            ],
            "postToolUse": [
                copilot_hook_entry(&binary, "observe", 5)
            ]
        }
    });

    let hook_path = if global {
        let Some(dir) = copilot_user_hooks_dir() else {
            return;
        };
        let _ = std::fs::create_dir_all(&dir);
        cleanup_legacy_global_hooks();
        dir.join("hooks.json")
    } else {
        let dir = PathBuf::from(".github").join("hooks");
        let _ = std::fs::create_dir_all(&dir);
        dir.join("hooks.json")
    };

    let needs_write = if hook_path.exists() {
        let content = std::fs::read_to_string(&hook_path).unwrap_or_default();
        !content.contains("hook rewrite")
            || content.contains("\"PreToolUse\"")
            || !content.contains("hook observe")
            // Pre-#381 configs carry only a `bash` command — unusable on Windows.
            || !content.contains("\"powershell\"")
    } else {
        true
    };

    if !needs_write {
        return;
    }

    if hook_path.exists()
        && let Ok(mut existing) = crate::core::jsonc::parse_jsonc(
            &std::fs::read_to_string(&hook_path).unwrap_or_default(),
        )
        && let Some(obj) = existing.as_object_mut()
    {
        obj.insert("version".to_string(), serde_json::json!(1));
        let hooks = obj
            .entry("hooks".to_string())
            .or_insert_with(|| serde_json::json!({}));
        if let Some(hooks_obj) = hooks.as_object_mut()
            && let Some(desired_hooks) = hook_config["hooks"].as_object()
        {
            for (event, entries) in desired_hooks {
                hooks_obj.insert(event.clone(), entries.clone());
            }
        }
        write_file(
            &hook_path,
            &serde_json::to_string_pretty(&existing).unwrap_or_default(),
        );
        if !mcp_server_quiet_mode() {
            eprintln!("Updated Copilot hooks at {}", hook_path.display());
        }
        return;
    }

    write_file(
        &hook_path,
        &serde_json::to_string_pretty(&hook_config).unwrap_or_default(),
    );
    if !mcp_server_quiet_mode() {
        eprintln!("Installed Copilot hooks at {}", hook_path.display());
    }
}

fn server_entry(binary: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "stdio",
        "command": binary,
        "args": [],
        "env": super::super::mcp_server_env_json()
    })
}

/// VS Code uses `"servers"` as the top-level key (not `"mcpServers"`).
fn write_vscode_mcp_file(mcp_path: &PathBuf, binary: &str, label: &str) {
    write_mcp_config(mcp_path, binary, label, "servers", server_entry(binary));
}

/// Copilot CLI uses `"mcpServers"` as the top-level key in `.github/mcp.json`.
fn write_copilot_cli_mcp_file(mcp_path: &PathBuf, binary: &str, label: &str) {
    let entry = serde_json::json!({
        "command": binary,
        "args": [],
        "env": super::super::mcp_server_env_json()
    });
    write_mcp_config(mcp_path, binary, label, "mcpServers", entry);
}

fn write_mcp_config(
    mcp_path: &PathBuf,
    binary: &str,
    label: &str,
    root_key: &str,
    desired: serde_json::Value,
) {
    if mcp_path.exists() {
        let content = std::fs::read_to_string(mcp_path).unwrap_or_default();
        match crate::core::jsonc::parse_jsonc(&content) {
            Ok(mut json) => {
                if let Some(obj) = json.as_object_mut() {
                    let servers = obj.entry(root_key).or_insert_with(|| serde_json::json!({}));
                    if let Some(servers_obj) = servers.as_object_mut() {
                        if servers_obj.get("lean-ctx") == Some(&desired) {
                            if !mcp_server_quiet_mode() {
                                eprintln!(
                                    "  \x1b[32m✓\x1b[0m lean-ctx already configured in {label}"
                                );
                            }
                            return;
                        }
                        servers_obj.insert("lean-ctx".to_string(), desired);
                    }
                    write_file(
                        mcp_path,
                        &serde_json::to_string_pretty(&json).unwrap_or_default(),
                    );
                    if !mcp_server_quiet_mode() {
                        eprintln!("  \x1b[32m✓\x1b[0m Added lean-ctx to {label}");
                    }
                    return;
                }
            }
            Err(e) => {
                tracing::warn!(
                    "Could not parse MCP config at {}: {e}\nAdd to \"{root_key}\": \"lean-ctx\": {{ \"command\": \"{}\", \"args\": [] }}",
                    mcp_path.display(),
                    binary
                );
                return;
            }
        }
    }

    if let Some(parent) = mcp_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let config = serde_json::json!({
        root_key: {
            "lean-ctx": desired
        }
    });

    write_file(
        mcp_path,
        &serde_json::to_string_pretty(&config).unwrap_or_default(),
    );
    if !mcp_server_quiet_mode() {
        eprintln!("  \x1b[32m✓\x1b[0m Created {label} with lean-ctx MCP server");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hook_entry_carries_bash_and_powershell_with_quoting() {
        // Windows-style install path with a space — the #381 failure shape.
        let entry = copilot_hook_entry(
            r"C:\Users\Jane Doe\AppData\Local\lean-ctx.exe",
            "rewrite",
            15,
        );
        assert_eq!(
            entry["bash"], "\"/c/Users/Jane Doe/AppData/Local/lean-ctx.exe\" hook rewrite",
            "bash field must use a quoted MSYS-style path"
        );
        assert_eq!(
            entry["powershell"],
            "& \"C:\\Users\\Jane Doe\\AppData\\Local\\lean-ctx.exe\" hook rewrite",
            "powershell field must invoke the quoted native path via the call operator"
        );
        assert_eq!(entry["timeoutSec"], 15);
        assert_eq!(entry["type"], "command");

        // Unix paths stay untouched (modulo quoting).
        let unix = copilot_hook_entry("/usr/local/bin/lean-ctx", "observe", 5);
        assert_eq!(unix["bash"], "\"/usr/local/bin/lean-ctx\" hook observe");
        assert_eq!(
            unix["powershell"],
            "& \"/usr/local/bin/lean-ctx\" hook observe"
        );
    }

    #[test]
    fn legacy_global_hooks_file_is_deleted_when_lean_ctx_only() {
        let tmp = tempfile::tempdir().unwrap();
        let legacy = tmp.path().join("hooks.json");
        let ours = serde_json::json!({
            "version": 1,
            "hooks": {
                "preToolUse": [
                    { "type": "command", "bash": "/usr/local/bin/lean-ctx hook rewrite", "timeoutSec": 15 }
                ],
                "postToolUse": [
                    { "type": "command", "bash": "/usr/local/bin/lean-ctx hook observe", "timeoutSec": 5 }
                ]
            }
        });
        std::fs::write(&legacy, serde_json::to_string_pretty(&ours).unwrap()).unwrap();

        assert!(cleanup_legacy_global_hooks_at(&legacy));
        assert!(
            !legacy.exists(),
            "lean-ctx-only legacy file must be removed"
        );
    }

    #[test]
    fn legacy_global_hooks_migration_preserves_foreign_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let legacy = tmp.path().join("hooks.json");
        let mixed = serde_json::json!({
            "version": 1,
            "hooks": {
                "preToolUse": [
                    { "type": "command", "bash": "/usr/local/bin/lean-ctx hook rewrite", "timeoutSec": 15 },
                    { "type": "command", "bash": "./scripts/security-check.sh", "timeoutSec": 10 }
                ]
            }
        });
        std::fs::write(&legacy, serde_json::to_string_pretty(&mixed).unwrap()).unwrap();

        assert!(cleanup_legacy_global_hooks_at(&legacy));
        let after: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&legacy).unwrap()).unwrap();
        let pre = after["hooks"]["preToolUse"].as_array().unwrap();
        assert_eq!(pre.len(), 1, "only the foreign hook may remain");
        assert_eq!(pre[0]["bash"], "./scripts/security-check.sh");
    }

    #[test]
    fn legacy_cleanup_ignores_files_without_lean_ctx() {
        let tmp = tempfile::tempdir().unwrap();
        let legacy = tmp.path().join("hooks.json");
        std::fs::write(
            &legacy,
            r#"{"version":1,"hooks":{"preToolUse":[{"type":"command","bash":"./mine.sh"}]}}"#,
        )
        .unwrap();

        assert!(!cleanup_legacy_global_hooks_at(&legacy));
        assert!(legacy.exists(), "foreign-only file must stay untouched");
    }
}
