use chrono::Utc;
use serde::Serialize;

use super::{claude_binary_exists, resolve_lean_ctx_binary, BOLD, DIM, GREEN, RST, WHITE, YELLOW};

#[derive(Debug, Clone, Copy)]
pub(super) struct IntegrationsOptions {
    pub json: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct IntegrationCheckReport {
    schema_version: u32,
    created_at: String,
    binary: String,
    integrations: Vec<IntegrationStatus>,
    ok: bool,
    repair_command: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct IntegrationStatus {
    name: String,
    detected: bool,
    checks: Vec<NamedCheck>,
    ok: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct NamedCheck {
    name: String,
    ok: bool,
    detail: String,
}

pub(super) fn run_integrations(opts: &IntegrationsOptions) -> i32 {
    let Some(home) = dirs::home_dir() else {
        eprintln!("Cannot determine home directory");
        return 2;
    };
    let binary = crate::core::portable_binary::resolve_portable_binary();
    let data_dir = crate::core::data_dir::lean_ctx_data_dir()
        .map(|d| d.to_string_lossy().to_string())
        .unwrap_or_default();

    let mut integrations = vec![
        integration_cursor(&home, &binary, &data_dir),
        integration_claude(&home, &binary, &data_dir),
    ];
    for t in crate::core::editor_registry::build_targets(&home) {
        if matches!(t.name, "Cursor" | "Claude Code") {
            continue;
        }
        integrations.push(integration_generic(&home, &binary, &data_dir, &t));
    }
    let ok = integrations.iter().all(|i| !i.detected || i.ok);

    let report = IntegrationCheckReport {
        schema_version: 1,
        created_at: Utc::now().to_rfc3339(),
        binary: binary.clone(),
        integrations,
        ok,
        repair_command: "lean-ctx setup --fix".to_string(),
    };

    if opts.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).unwrap_or_else(|_| "{}".to_string())
        );
    } else {
        println!();
        println!("  {BOLD}{WHITE}Integration health:{RST}");
        for i in &report.integrations {
            if !i.detected {
                continue;
            }
            let mark = if i.ok {
                format!("{GREEN}✓{RST}")
            } else {
                format!("{YELLOW}✗{RST}")
            };
            println!("  {mark}  {BOLD}{}{RST}", i.name);
            for c in &i.checks {
                let m = if c.ok {
                    format!("{GREEN}✓{RST}")
                } else {
                    format!("{YELLOW}✗{RST}")
                };
                println!("       {m}  {}  {DIM}{}{RST}", c.name, c.detail);
            }
        }
        if !report.ok {
            println!();
            println!(
                "  {YELLOW}Repair:{RST} run {BOLD}{}{RST}",
                report.repair_command
            );
        }
    }

    i32::from(!report.ok)
}

fn integration_generic(
    home: &std::path::Path,
    binary: &str,
    data_dir: &str,
    target: &crate::core::editor_registry::types::EditorTarget,
) -> IntegrationStatus {
    let detected = target.detect_path.exists() || target.config_path.exists();
    if !detected {
        return IntegrationStatus {
            name: target.name.to_string(),
            detected: false,
            checks: Vec::new(),
            ok: true,
        };
    }

    let mut checks = Vec::new();
    match target.config_type {
        crate::core::editor_registry::types::ConfigType::McpJson
        | crate::core::editor_registry::types::ConfigType::JetBrains
        | crate::core::editor_registry::types::ConfigType::QoderSettings => {
            checks.push(check_mcp_json(&target.config_path, binary, data_dir));
        }
        crate::core::editor_registry::types::ConfigType::Zed => {
            checks.push(check_zed_settings(&target.config_path, binary));
        }
        crate::core::editor_registry::types::ConfigType::Codex => {
            checks.push(check_codex_toml(&target.config_path, binary));
            checks.push(check_codex_hooks_enabled(home));
            checks.push(check_codex_hooks_json(home));
        }
        crate::core::editor_registry::types::ConfigType::VsCodeMcp => {
            checks.push(check_vscode_mcp(&target.config_path, binary, data_dir));
        }
        crate::core::editor_registry::types::ConfigType::CopilotCli => {
            checks.push(check_copilot_cli_mcp(&target.config_path, binary, data_dir));
        }
        crate::core::editor_registry::types::ConfigType::OpenCode => {
            checks.push(check_opencode_config(&target.config_path, binary, data_dir));
        }
        crate::core::editor_registry::types::ConfigType::Crush => {
            checks.push(check_crush_config(&target.config_path, binary, data_dir));
        }
        crate::core::editor_registry::types::ConfigType::Amp => {
            checks.push(check_amp_config(&target.config_path, binary, data_dir));
        }
        crate::core::editor_registry::types::ConfigType::HermesYaml => {
            checks.push(check_hermes_yaml(&target.config_path, binary, data_dir));
        }
        crate::core::editor_registry::types::ConfigType::GeminiSettings => {
            checks.push(check_mcp_json(&target.config_path, binary, data_dir));
            checks.push(check_gemini_trust_and_hooks(home, binary));
        }
        crate::core::editor_registry::types::ConfigType::AugmentVsCode => {
            checks.push(check_augment_vscode_mcp(
                &target.config_path,
                binary,
                data_dir,
            ));
        }
    }

    if let Some(rules_path) = rules_path_for(target.name, home) {
        checks.push(check_rules_file(&rules_path));
    }

    let ok = checks.iter().all(|c| c.ok);
    IntegrationStatus {
        name: target.name.to_string(),
        detected: true,
        checks,
        ok,
    }
}

fn integration_cursor(home: &std::path::Path, binary: &str, data_dir: &str) -> IntegrationStatus {
    let cursor_dir = home.join(".cursor");
    if !cursor_dir.exists() {
        return IntegrationStatus {
            name: "Cursor".to_string(),
            detected: false,
            checks: Vec::new(),
            ok: true,
        };
    }

    let mut checks = Vec::new();
    let mcp_path = cursor_dir.join("mcp.json");
    checks.push(check_mcp_json(&mcp_path, binary, data_dir));

    let hooks_path = cursor_dir.join("hooks.json");
    checks.push(check_cursor_hooks(&hooks_path));

    let ok = checks.iter().all(|c| c.ok);
    IntegrationStatus {
        name: "Cursor".to_string(),
        detected: true,
        checks,
        ok,
    }
}

fn integration_claude(home: &std::path::Path, binary: &str, data_dir: &str) -> IntegrationStatus {
    let target = crate::core::editor_registry::build_targets(home)
        .into_iter()
        .find(|t| t.agent_key == "claude");
    let detected = target.as_ref().is_some_and(|t| t.detect_path.exists())
        || crate::core::editor_registry::claude_state_dir(home).exists()
        || claude_binary_exists();

    if !detected {
        return IntegrationStatus {
            name: "Claude Code".to_string(),
            detected: false,
            checks: Vec::new(),
            ok: true,
        };
    }

    let mut checks = Vec::new();
    let mcp_path = crate::core::editor_registry::claude_mcp_json_path(home);
    checks.push(check_mcp_json(&mcp_path, binary, data_dir));

    let settings_path = crate::core::editor_registry::claude_state_dir(home).join("settings.json");
    checks.push(check_claude_hooks(&settings_path));

    let rules_path = crate::core::editor_registry::claude_rules_dir(home).join("lean-ctx.md");
    let has_rules = rules_path.exists();
    checks.push(NamedCheck {
        name: "Rules file".to_string(),
        ok: has_rules,
        detail: if has_rules {
            rules_path.display().to_string()
        } else {
            format!("missing ({})", rules_path.display())
        },
    });

    let ok = checks.iter().all(|c| c.ok);
    IntegrationStatus {
        name: "Claude Code".to_string(),
        detected: true,
        checks,
        ok,
    }
}

fn check_mcp_json(path: &std::path::Path, binary: &str, data_dir: &str) -> NamedCheck {
    if !path.exists() {
        return NamedCheck {
            name: "MCP config".to_string(),
            ok: false,
            detail: format!("missing ({})", path.display()),
        };
    }
    let content = std::fs::read_to_string(path).unwrap_or_default();
    let parsed = crate::core::jsonc::parse_jsonc(&content).ok();

    let Some(v) = parsed else {
        return NamedCheck {
            name: "MCP config".to_string(),
            ok: false,
            detail: format!("invalid JSON ({})", path.display()),
        };
    };

    let entry = v
        .get("mcpServers")
        .and_then(|m| m.get("lean-ctx"))
        .cloned()
        .or_else(|| {
            v.get("mcp")
                .and_then(|m| m.get("servers"))
                .and_then(|m| m.get("lean-ctx"))
                .cloned()
        });

    let Some(e) = entry else {
        return NamedCheck {
            name: "MCP config".to_string(),
            ok: false,
            detail: format!("lean-ctx missing ({})", path.display()),
        };
    };

    let cmd_ok = e
        .get("command")
        .and_then(|c| c.as_str())
        .is_some_and(|c| cmd_matches_expected(c, binary));
    let env_ok = e
        .get("env")
        .and_then(|env| env.get("LEAN_CTX_DATA_DIR"))
        .and_then(|d| d.as_str())
        .is_some_and(|d| d.trim() == data_dir.trim());

    let ok = cmd_ok && env_ok;
    let detail = if ok {
        format!("ok ({})", path.display())
    } else {
        format!("drift ({})", path.display())
    };
    NamedCheck {
        name: "MCP config".to_string(),
        ok,
        detail,
    }
}

fn cmd_matches_expected(cmd: &str, portable: &str) -> bool {
    let cmd = cmd.trim();
    if cmd == portable.trim() {
        return true;
    }
    if cmd == "lean-ctx" {
        return true;
    }
    if let Some(resolved) = resolve_lean_ctx_binary() {
        if cmd == resolved.to_string_lossy().trim() {
            return true;
        }
    }
    false
}

fn check_rules_file(path: &std::path::Path) -> NamedCheck {
    let ok = path.exists();
    NamedCheck {
        name: "Rules file".to_string(),
        ok,
        detail: if ok {
            path.display().to_string()
        } else {
            format!("missing ({})", path.display())
        },
    }
}

fn rules_path_for(name: &str, home: &std::path::Path) -> Option<std::path::PathBuf> {
    match name {
        "Windsurf" => Some(home.join(".codeium/windsurf/rules/lean-ctx.md")),
        "Cline" => Some(home.join(".cline/rules/lean-ctx.md")),
        "Roo Code" => Some(home.join(".roo/rules/lean-ctx.md")),
        "OpenCode" => Some(home.join(".config/opencode/AGENTS.md")),
        "AWS Kiro" => Some(home.join(".kiro/steering/lean-ctx.md")),
        "Verdent" => Some(home.join(".verdent/rules/lean-ctx.md")),
        "Trae" => Some(home.join(".trae/rules/lean-ctx.md")),
        "Qwen Code" => Some(home.join(".qwen/rules/lean-ctx.md")),
        "Amazon Q Developer" => Some(home.join(".aws/amazonq/rules/lean-ctx.md")),
        "JetBrains IDEs" => Some(home.join(".jb-rules/lean-ctx.md")),
        "Antigravity" => Some(home.join(".gemini/antigravity/rules/lean-ctx.md")),
        "Augment CLI" | "Augment (VS Code)" => Some(home.join(".augment/rules/lean-ctx.md")),
        "Pi Coding Agent" => Some(home.join(".pi/rules/lean-ctx.md")),
        "Crush" => Some(home.join(".config/crush/rules/lean-ctx.md")),
        _ => None,
    }
}

fn check_zed_settings(path: &std::path::Path, binary: &str) -> NamedCheck {
    if !path.exists() {
        return NamedCheck {
            name: "Zed config".to_string(),
            ok: false,
            detail: format!("missing ({})", path.display()),
        };
    }
    let content = std::fs::read_to_string(path).unwrap_or_default();
    let parsed = crate::core::jsonc::parse_jsonc(&content).ok();
    let Some(v) = parsed else {
        return NamedCheck {
            name: "Zed config".to_string(),
            ok: false,
            detail: format!("invalid JSON ({})", path.display()),
        };
    };
    let entry = v
        .get("context_servers")
        .and_then(|m| m.get("lean-ctx"))
        .cloned();
    let Some(e) = entry else {
        return NamedCheck {
            name: "Zed config".to_string(),
            ok: false,
            detail: format!("lean-ctx missing ({})", path.display()),
        };
    };

    let cmd_ok = e
        .get("command")
        .and_then(|c| c.as_str())
        .is_some_and(|c| cmd_matches_expected(c, binary));

    NamedCheck {
        name: "Zed config".to_string(),
        ok: cmd_ok,
        detail: if cmd_ok {
            format!("ok ({})", path.display())
        } else {
            format!("drift ({})", path.display())
        },
    }
}

fn check_vscode_mcp(path: &std::path::Path, binary: &str, data_dir: &str) -> NamedCheck {
    if !path.exists() {
        return NamedCheck {
            name: "VS Code MCP".to_string(),
            ok: false,
            detail: format!("missing ({})", path.display()),
        };
    }
    let content = std::fs::read_to_string(path).unwrap_or_default();
    let parsed = crate::core::jsonc::parse_jsonc(&content).ok();
    let Some(v) = parsed else {
        return NamedCheck {
            name: "VS Code MCP".to_string(),
            ok: false,
            detail: format!("invalid JSON ({})", path.display()),
        };
    };
    let Some(e) = v.get("servers").and_then(|m| m.get("lean-ctx")) else {
        return NamedCheck {
            name: "VS Code MCP".to_string(),
            ok: false,
            detail: format!("lean-ctx missing ({})", path.display()),
        };
    };

    let ty_ok = e.get("type").and_then(|t| t.as_str()) == Some("stdio");
    let cmd_ok = e
        .get("command")
        .and_then(|c| c.as_str())
        .is_some_and(|c| cmd_matches_expected(c, binary));
    let env_ok = e
        .get("env")
        .and_then(|env| env.get("LEAN_CTX_DATA_DIR"))
        .and_then(|d| d.as_str())
        .is_some_and(|d| d.trim() == data_dir.trim());

    let ok = ty_ok && cmd_ok && env_ok;
    NamedCheck {
        name: "VS Code MCP".to_string(),
        ok,
        detail: if ok {
            format!("ok ({})", path.display())
        } else {
            format!("drift ({})", path.display())
        },
    }
}

fn check_augment_vscode_mcp(path: &std::path::Path, binary: &str, data_dir: &str) -> NamedCheck {
    if !path.exists() {
        return NamedCheck {
            name: "Augment VS Code MCP".to_string(),
            ok: false,
            detail: format!("missing ({})", path.display()),
        };
    }
    let content = std::fs::read_to_string(path).unwrap_or_default();
    let Some(v) = crate::core::jsonc::parse_jsonc(&content).ok() else {
        return NamedCheck {
            name: "Augment VS Code MCP".to_string(),
            ok: false,
            detail: format!("invalid JSON ({})", path.display()),
        };
    };
    let Some(arr) = v.as_array() else {
        return NamedCheck {
            name: "Augment VS Code MCP".to_string(),
            ok: false,
            detail: format!("expected top-level array ({})", path.display()),
        };
    };
    let Some(e) = arr
        .iter()
        .find(|e| e.get("name").and_then(|n| n.as_str()) == Some("lean-ctx"))
    else {
        return NamedCheck {
            name: "Augment VS Code MCP".to_string(),
            ok: false,
            detail: format!("lean-ctx entry missing ({})", path.display()),
        };
    };

    let ty_ok = e.get("type").and_then(|t| t.as_str()) == Some("stdio");
    let cmd_ok = e
        .get("command")
        .and_then(|c| c.as_str())
        .is_some_and(|c| cmd_matches_expected(c, binary));
    let env_ok = e
        .get("env")
        .and_then(|env| env.get("LEAN_CTX_DATA_DIR"))
        .and_then(|d| d.as_str())
        .is_some_and(|d| d.trim() == data_dir.trim());
    // The Augment VS Code panel persists user toggles via the `disabled` flag.
    // An entry with `disabled: true` is present-but-inert, so doctor must
    // surface that as drift instead of silently passing. A missing key,
    // explicit `false`, or any non-boolean value is treated as enabled — only
    // an explicit `true` counts as a user-initiated disable.
    let not_disabled = e.get("disabled").and_then(serde_json::Value::as_bool) != Some(true);

    let ok = ty_ok && cmd_ok && env_ok && not_disabled;
    NamedCheck {
        name: "Augment VS Code MCP".to_string(),
        ok,
        detail: if ok {
            format!("ok ({})", path.display())
        } else if !not_disabled {
            format!("disabled ({})", path.display())
        } else {
            format!("drift ({})", path.display())
        },
    }
}

fn check_copilot_cli_mcp(path: &std::path::Path, binary: &str, data_dir: &str) -> NamedCheck {
    if !path.exists() {
        return NamedCheck {
            name: "Copilot CLI MCP".to_string(),
            ok: false,
            detail: format!("missing ({})", path.display()),
        };
    }
    let content = std::fs::read_to_string(path).unwrap_or_default();
    let parsed = crate::core::jsonc::parse_jsonc(&content).ok();
    let Some(v) = parsed else {
        return NamedCheck {
            name: "Copilot CLI MCP".to_string(),
            ok: false,
            detail: format!("invalid JSON ({})", path.display()),
        };
    };
    let Some(e) = v.get("mcpServers").and_then(|m| m.get("lean-ctx")) else {
        return NamedCheck {
            name: "Copilot CLI MCP".to_string(),
            ok: false,
            detail: format!("lean-ctx missing in mcpServers ({})", path.display()),
        };
    };

    let cmd_ok = e
        .get("command")
        .and_then(|c| c.as_str())
        .is_some_and(|c| cmd_matches_expected(c, binary));
    let env_ok = e
        .get("env")
        .and_then(|env| env.get("LEAN_CTX_DATA_DIR"))
        .and_then(|d| d.as_str())
        .is_some_and(|d| d.trim() == data_dir.trim());

    let ok = cmd_ok && env_ok;
    NamedCheck {
        name: "Copilot CLI MCP".to_string(),
        ok,
        detail: if ok {
            format!("ok ({})", path.display())
        } else {
            format!("drift ({})", path.display())
        },
    }
}

fn check_opencode_config(path: &std::path::Path, binary: &str, data_dir: &str) -> NamedCheck {
    if !path.exists() {
        return NamedCheck {
            name: "OpenCode MCP".to_string(),
            ok: false,
            detail: format!("missing ({})", path.display()),
        };
    }
    let content = std::fs::read_to_string(path).unwrap_or_default();
    let parsed = crate::core::jsonc::parse_jsonc(&content).ok();
    let Some(v) = parsed else {
        return NamedCheck {
            name: "OpenCode MCP".to_string(),
            ok: false,
            detail: format!("invalid JSON ({})", path.display()),
        };
    };
    let Some(e) = v.get("mcp").and_then(|m| m.get("lean-ctx")) else {
        return NamedCheck {
            name: "OpenCode MCP".to_string(),
            ok: false,
            detail: format!("lean-ctx missing ({})", path.display()),
        };
    };

    let cmd = e
        .get("command")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .and_then(|x| x.as_str());
    let cmd_ok = cmd.is_some_and(|c| cmd_matches_expected(c, binary));
    let env_ok = e
        .get("environment")
        .and_then(|env| env.get("LEAN_CTX_DATA_DIR"))
        .and_then(|d| d.as_str())
        .is_some_and(|d| d.trim() == data_dir.trim());
    let ok = cmd_ok && env_ok;
    NamedCheck {
        name: "OpenCode MCP".to_string(),
        ok,
        detail: if ok {
            format!("ok ({})", path.display())
        } else {
            format!("drift ({})", path.display())
        },
    }
}

fn check_crush_config(path: &std::path::Path, binary: &str, data_dir: &str) -> NamedCheck {
    if !path.exists() {
        return NamedCheck {
            name: "Crush MCP".to_string(),
            ok: false,
            detail: format!("missing ({})", path.display()),
        };
    }
    let content = std::fs::read_to_string(path).unwrap_or_default();
    let parsed = crate::core::jsonc::parse_jsonc(&content).ok();
    let Some(v) = parsed else {
        return NamedCheck {
            name: "Crush MCP".to_string(),
            ok: false,
            detail: format!("invalid JSON ({})", path.display()),
        };
    };
    let Some(e) = v.get("mcp").and_then(|m| m.get("lean-ctx")) else {
        return NamedCheck {
            name: "Crush MCP".to_string(),
            ok: false,
            detail: format!("lean-ctx missing ({})", path.display()),
        };
    };

    let cmd_ok = e
        .get("command")
        .and_then(|c| c.as_str())
        .is_some_and(|c| cmd_matches_expected(c, binary));
    let env_ok = e
        .get("env")
        .and_then(|env| env.get("LEAN_CTX_DATA_DIR"))
        .and_then(|d| d.as_str())
        .is_some_and(|d| d.trim() == data_dir.trim());
    let ok = cmd_ok && env_ok;
    NamedCheck {
        name: "Crush MCP".to_string(),
        ok,
        detail: if ok {
            format!("ok ({})", path.display())
        } else {
            format!("drift ({})", path.display())
        },
    }
}

fn check_amp_config(path: &std::path::Path, binary: &str, data_dir: &str) -> NamedCheck {
    if !path.exists() {
        return NamedCheck {
            name: "Amp MCP".to_string(),
            ok: false,
            detail: format!("missing ({})", path.display()),
        };
    }
    let content = std::fs::read_to_string(path).unwrap_or_default();
    let parsed = crate::core::jsonc::parse_jsonc(&content).ok();
    let Some(v) = parsed else {
        return NamedCheck {
            name: "Amp MCP".to_string(),
            ok: false,
            detail: format!("invalid JSON ({})", path.display()),
        };
    };
    let Some(e) = v.get("amp.mcpServers").and_then(|m| m.get("lean-ctx")) else {
        return NamedCheck {
            name: "Amp MCP".to_string(),
            ok: false,
            detail: format!("lean-ctx missing ({})", path.display()),
        };
    };

    let cmd_ok = e
        .get("command")
        .and_then(|c| c.as_str())
        .is_some_and(|c| cmd_matches_expected(c, binary));
    let env_ok = e
        .get("env")
        .and_then(|env| env.get("LEAN_CTX_DATA_DIR"))
        .and_then(|d| d.as_str())
        .is_some_and(|d| d.trim() == data_dir.trim());
    let ok = cmd_ok && env_ok;
    NamedCheck {
        name: "Amp MCP".to_string(),
        ok,
        detail: if ok {
            format!("ok ({})", path.display())
        } else {
            format!("drift ({})", path.display())
        },
    }
}

fn check_codex_toml(path: &std::path::Path, binary: &str) -> NamedCheck {
    if !path.exists() {
        return NamedCheck {
            name: "Codex MCP".to_string(),
            ok: false,
            detail: format!("missing ({})", path.display()),
        };
    }
    let content = std::fs::read_to_string(path).unwrap_or_default();
    let parsed: Result<toml::Value, _> = toml::from_str(&content);
    let Ok(v) = parsed else {
        return NamedCheck {
            name: "Codex MCP".to_string(),
            ok: false,
            detail: format!("invalid TOML ({})", path.display()),
        };
    };
    let cmd = v
        .get("mcp_servers")
        .and_then(|t| t.get("lean-ctx"))
        .and_then(|t| t.get("command"))
        .and_then(|c| c.as_str());
    let ok = cmd.is_some_and(|c| cmd_matches_expected(c, binary));
    NamedCheck {
        name: "Codex MCP".to_string(),
        ok,
        detail: if ok {
            format!("ok ({})", path.display())
        } else {
            format!("drift ({})", path.display())
        },
    }
}

fn check_codex_hooks_enabled(home: &std::path::Path) -> NamedCheck {
    let codex_dir = crate::core::home::resolve_codex_dir().unwrap_or_else(|| home.join(".codex"));
    let path = codex_dir.join("config.toml");
    if !path.exists() {
        return NamedCheck {
            name: "Codex hooks".to_string(),
            ok: false,
            detail: format!("missing ({})", path.display()),
        };
    }
    let content = std::fs::read_to_string(&path).unwrap_or_default();
    let parsed: Result<toml::Value, _> = toml::from_str(&content);
    let Ok(v) = parsed else {
        return NamedCheck {
            name: "Codex hooks".to_string(),
            ok: false,
            detail: format!("invalid TOML ({})", path.display()),
        };
    };
    let features = v.get("features");
    let ok = features
        .and_then(|t| t.get("hooks"))
        .and_then(toml::Value::as_bool)
        == Some(true)
        || features
            .and_then(|t| t.get("codex_hooks"))
            .and_then(toml::Value::as_bool)
            == Some(true);
    NamedCheck {
        name: "Codex hooks".to_string(),
        ok,
        detail: if ok {
            format!("enabled ({})", path.display())
        } else {
            format!("disabled ({})", path.display())
        },
    }
}

fn check_codex_hooks_json(home: &std::path::Path) -> NamedCheck {
    let codex_dir = crate::core::home::resolve_codex_dir().unwrap_or_else(|| home.join(".codex"));
    let path = codex_dir.join("hooks.json");
    if !path.exists() {
        return NamedCheck {
            name: "Codex hooks.json".to_string(),
            ok: false,
            detail: format!("missing ({})", path.display()),
        };
    }
    let content = std::fs::read_to_string(&path).unwrap_or_default();
    let parsed = crate::core::jsonc::parse_jsonc(&content).ok();
    let Some(v) = parsed else {
        return NamedCheck {
            name: "Codex hooks.json".to_string(),
            ok: false,
            detail: format!("invalid JSON ({})", path.display()),
        };
    };
    let hooks = v.get("hooks");
    let mut saw_session_start = false;
    let mut saw_pretool = false;
    if let Some(h) = hooks {
        for event in ["SessionStart", "PreToolUse"] {
            if let Some(arr) = h.get(event).and_then(|x| x.as_array()) {
                for entry in arr {
                    let Some(hooks_arr) = entry.get("hooks").and_then(|x| x.as_array()) else {
                        continue;
                    };
                    for he in hooks_arr {
                        let Some(cmd) = he.get("command").and_then(|c| c.as_str()) else {
                            continue;
                        };
                        if cmd.contains("hook codex-session-start") {
                            saw_session_start = true;
                        }
                        if cmd.contains("hook codex-pretooluse") {
                            saw_pretool = true;
                        }
                    }
                }
            }
        }
    }
    let ok = saw_session_start && saw_pretool;
    NamedCheck {
        name: "Codex hooks.json".to_string(),
        ok,
        detail: if ok {
            format!("ok ({})", path.display())
        } else {
            format!("missing managed entries ({})", path.display())
        },
    }
}

fn check_hermes_yaml(path: &std::path::Path, binary: &str, data_dir: &str) -> NamedCheck {
    if !path.exists() {
        return NamedCheck {
            name: "Hermes MCP".to_string(),
            ok: false,
            detail: format!("missing ({})", path.display()),
        };
    }
    let content = std::fs::read_to_string(path).unwrap_or_default();
    let has_mcp = content.contains("mcp_servers:") && content.contains("lean-ctx:");
    let has_cmd =
        content.contains("command:") && (content.contains(binary) || content.contains("lean-ctx"));
    let has_env = content.contains("LEAN_CTX_DATA_DIR") && content.contains(data_dir);
    let ok = has_mcp && has_cmd && has_env;
    NamedCheck {
        name: "Hermes MCP".to_string(),
        ok,
        detail: if ok {
            format!("ok ({})", path.display())
        } else {
            format!("drift ({})", path.display())
        },
    }
}

fn check_gemini_trust_and_hooks(home: &std::path::Path, binary: &str) -> NamedCheck {
    let settings = home.join(".gemini").join("settings.json");
    if !settings.exists() {
        return NamedCheck {
            name: "Gemini hooks".to_string(),
            ok: false,
            detail: format!("missing ({})", settings.display()),
        };
    }
    let content = std::fs::read_to_string(&settings).unwrap_or_default();
    let parsed = crate::core::jsonc::parse_jsonc(&content).ok();
    let Some(v) = parsed else {
        return NamedCheck {
            name: "Gemini hooks".to_string(),
            ok: false,
            detail: format!("invalid JSON ({})", settings.display()),
        };
    };

    let trust_ok = v
        .get("mcpServers")
        .and_then(|m| m.get("lean-ctx"))
        .and_then(|e| e.get("trust"))
        .and_then(serde_json::Value::as_bool)
        == Some(true);

    let hooks_ok = v
        .get("hooks")
        .and_then(|h| h.get("BeforeTool"))
        .and_then(|x| x.as_array())
        .is_some_and(|arr| {
            let mut saw_rewrite = false;
            let mut saw_redirect = false;
            for entry in arr {
                let hooks = entry
                    .get("hooks")
                    .and_then(|x| x.as_array())
                    .cloned()
                    .unwrap_or_default();
                for h in hooks {
                    let cmd = h
                        .get("command")
                        .and_then(|c| c.as_str())
                        .unwrap_or_default();
                    let first = cmd.split_whitespace().next().unwrap_or_default();
                    if cmd.contains("hook rewrite") && cmd_matches_expected(first, binary) {
                        saw_rewrite = true;
                    }
                    if cmd.contains("hook redirect") && cmd_matches_expected(first, binary) {
                        saw_redirect = true;
                    }
                }
            }
            saw_rewrite && saw_redirect
        });

    let scripts_ok = home
        .join(".gemini")
        .join("hooks")
        .join("lean-ctx-rewrite-gemini.sh")
        .exists()
        && home
            .join(".gemini")
            .join("hooks")
            .join("lean-ctx-redirect-gemini.sh")
            .exists();

    let ok = trust_ok && hooks_ok && scripts_ok;
    NamedCheck {
        name: "Gemini hooks".to_string(),
        ok,
        detail: if ok {
            format!("ok ({})", settings.display())
        } else {
            "drift (hooks/trust/scripts)".to_string()
        },
    }
}

fn check_cursor_hooks(path: &std::path::Path) -> NamedCheck {
    if !path.exists() {
        return NamedCheck {
            name: "Hooks".to_string(),
            ok: false,
            detail: format!("missing ({})", path.display()),
        };
    }
    let content = std::fs::read_to_string(path).unwrap_or_default();
    let parsed = crate::core::jsonc::parse_jsonc(&content).ok();
    let Some(v) = parsed else {
        return NamedCheck {
            name: "Hooks".to_string(),
            ok: false,
            detail: format!("invalid JSON ({})", path.display()),
        };
    };
    let pre = v
        .get("hooks")
        .and_then(|h| h.get("preToolUse"))
        .and_then(|x| x.as_array())
        .cloned()
        .unwrap_or_default();
    let has_rewrite = pre.iter().any(|e| {
        e.get("matcher").and_then(|m| m.as_str()) == Some("Shell")
            && e.get("command")
                .and_then(|c| c.as_str())
                .is_some_and(|c| c.contains(" hook rewrite"))
    });
    let has_redirect = pre.iter().any(|e| {
        matches!(
            e.get("matcher").and_then(|m| m.as_str()),
            Some("Read|Grep" | "Read" | "Grep")
        ) && e
            .get("command")
            .and_then(|c| c.as_str())
            .is_some_and(|c| c.contains(" hook redirect"))
    });
    NamedCheck {
        name: "Hooks".to_string(),
        ok: has_rewrite && has_redirect,
        detail: if has_rewrite && has_redirect {
            format!("ok ({})", path.display())
        } else {
            format!("drift ({})", path.display())
        },
    }
}

fn check_claude_hooks(path: &std::path::Path) -> NamedCheck {
    if !path.exists() {
        return NamedCheck {
            name: "Hooks".to_string(),
            ok: false,
            detail: format!("missing ({})", path.display()),
        };
    }
    let content = std::fs::read_to_string(path).unwrap_or_default();
    let parsed = crate::core::jsonc::parse_jsonc(&content).ok();
    let Some(v) = parsed else {
        return NamedCheck {
            name: "Hooks".to_string(),
            ok: false,
            detail: format!("invalid JSON ({})", path.display()),
        };
    };
    let pre = v
        .get("hooks")
        .and_then(|h| h.get("PreToolUse"))
        .and_then(|x| x.as_array())
        .cloned()
        .unwrap_or_default();
    let joined = serde_json::to_string(&pre).unwrap_or_default();
    let ok = joined.contains(" hook rewrite") && joined.contains(" hook redirect");
    NamedCheck {
        name: "Hooks".to_string(),
        ok,
        detail: if ok {
            format!("ok ({})", path.display())
        } else {
            format!("drift ({})", path.display())
        },
    }
}
