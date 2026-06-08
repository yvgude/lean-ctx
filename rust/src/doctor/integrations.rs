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
        | crate::core::editor_registry::types::ConfigType::QoderSettings => {
            checks.push(check_mcp_json(&target.config_path, binary, data_dir));
            // The Antigravity CLI also installs observe hooks as a plugin under
            // ~/.gemini/config/plugins/lean-ctx (registered in import_manifest.json,
            // NOT in any settings.json — see GH #284); verify that plugin too.
            if target.agent_key == "antigravity-cli" {
                checks.push(check_antigravity_cli_hooks(home, binary));
                checks.push(antigravity_cli_hooks_note());
            }
        }
        crate::core::editor_registry::types::ConfigType::JetBrains => {
            checks.push(check_jetbrains_snippet(
                &target.config_path,
                binary,
                data_dir,
            ));
        }
        crate::core::editor_registry::types::ConfigType::Zed => {
            checks.push(check_zed_settings(&target.config_path, binary));
        }
        crate::core::editor_registry::types::ConfigType::Codex => {
            checks.push(check_codex_toml(&target.config_path, binary));
            checks.push(check_codex_hooks_enabled(home));
            checks.push(check_codex_hooks_json(home, binary));
            checks.push(codex_desktop_note());
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
    checks.push(check_cursor_hooks(&hooks_path, binary));

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
    checks.push(check_claude_hooks(&settings_path, binary));

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

/// JetBrains AI Assistant has no auto-wiring: lean-ctx writes a ready-to-paste
/// snippet to `~/.jb-mcp.json`, which the user imports once via the IDE. The
/// `doctor` verdict therefore verifies the snippet exists and is current, while
/// making the required manual step explicit instead of implying auto-wiring.
fn check_jetbrains_snippet(path: &std::path::Path, binary: &str, data_dir: &str) -> NamedCheck {
    let mut c = check_mcp_json(path, binary, data_dir);
    c.name = "MCP snippet".to_string();
    if c.ok {
        c.detail = format!(
            "ready — paste into Settings → Tools → AI Assistant → MCP ({})",
            path.display()
        );
    }
    c
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

/// Collect the `lean-ctx` binary tokens that appear immediately before a
/// ` hook ` invocation inside a hook config file. Managed hook commands look
/// like `"<binary> hook rewrite"` / `"<binary> hook redirect"` /
/// `"<binary> hook codex-pretooluse"`, so the token directly preceding a
/// ` hook ` delimiter is the binary the hook will execute.
fn hook_binary_refs(content: &str) -> Vec<String> {
    let pieces: Vec<&str> = content.split(" hook ").collect();
    if pieces.len() < 2 {
        return Vec::new();
    }
    pieces[..pieces.len() - 1]
        .iter()
        .filter_map(|piece| {
            // The binary token is the trailing run before " hook ", bounded by
            // whitespace or JSON string delimiters. Splitting on whitespace
            // alone breaks on minified JSON (e.g. `serde_json::to_string`
            // output), where there is no space between the opening quote and
            // the command — we would otherwise capture the whole JSON prefix.
            piece
                .rsplit(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == '`')
                .find(|tok| !tok.is_empty())
                .map(|tok| tok.trim_end_matches(',').to_string())
        })
        .filter(|tok| tok.contains("lean-ctx"))
        .collect()
}

/// If a hook file references a `lean-ctx` binary path that does not match the
/// currently installed binary (and none of its references do), return that
/// stale path. Returns `None` when there are no hook references or at least one
/// reference points at the current binary (or the bare `lean-ctx` PATH command).
fn stale_hook_binary(content: &str, binary: &str) -> Option<String> {
    let refs = hook_binary_refs(content);
    if refs.is_empty() || refs.iter().any(|r| cmd_matches_expected(r, binary)) {
        return None;
    }
    refs.into_iter().next()
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

/// Informational note (always `ok`): lean-ctx's transparent shell/file
/// compression is hook-driven, and whether Codex lifecycle hooks fire depends on
/// the surface (CLI / Desktop / Cloud), the Codex version, and whether the hooks
/// are trusted (`/hooks`). Rather than asserting any one surface "can't" run hooks
/// (it varies and changes across Codex releases), this note points at the reliable
/// path: the lean-ctx MCP tools (`ctx_shell`/`ctx_read`/`ctx_search`) compress on
/// every surface. Guidance only — it never fails.
fn codex_desktop_note() -> NamedCheck {
    NamedCheck {
        name: "Codex compression".to_string(),
        ok: true,
        detail: "hooks auto-compress when trusted (/hooks); the ctx_shell/ctx_read/ctx_search MCP tools compress reliably on every surface (CLI/Desktop/Cloud)".to_string(),
    }
}

fn check_codex_hooks_json(home: &std::path::Path, binary: &str) -> NamedCheck {
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
    let entries_ok = saw_session_start && saw_pretool;
    let stale = stale_hook_binary(&content, binary);
    let ok = entries_ok && stale.is_none();
    let detail = if !entries_ok {
        format!("missing managed entries ({})", path.display())
    } else if let Some(old) = stale {
        format!("stale binary {old} — run lean-ctx setup --fix")
    } else {
        format!("ok ({})", path.display())
    };
    NamedCheck {
        name: "Codex hooks.json".to_string(),
        ok,
        detail,
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

/// Verify that the lean-ctx **plugin** for the Antigravity CLI (`agy`) is
/// installed and registered, pointing at the *current* binary.
///
/// `agy` (verified against the real binary, v1.0.6) loads plugins only from
/// `~/.gemini/config/plugins/<name>/` — exactly where `agy plugin install` itself
/// stages them — with a root `plugin.json`, hooks in the `hooks/hooks.json`
/// **subdir** (a root `hooks.json` is *not* processed) and an optional
/// plugin-local `mcp_config.json`; the plugin is registered in
/// `~/.gemini/config/import_manifest.json`. This guards the GH #284 regression
/// where hooks were written to a `settings.json` that `agy` ignores. We verify
/// the full self-contained bundle (`plugin.json` + `hooks/hooks.json` +
/// `mcp_config.json`) so the check stays in lockstep with the installer.
///
/// Note: hook *firing* is additionally gated by `agy`'s server-side
/// `enable_json_hooks` experiment, which no local config can force — so a green
/// check here means "installed exactly as `agy` expects", not "hooks are live".
fn check_antigravity_cli_hooks(home: &std::path::Path, binary: &str) -> NamedCheck {
    let name = "Antigravity CLI plugin".to_string();
    let plugin_dir = crate::hooks::agents::antigravity_cli_plugin_dir(home);
    let hooks_json = plugin_dir.join("hooks").join("hooks.json");
    if !hooks_json.exists() {
        return NamedCheck {
            name,
            ok: false,
            detail: format!("missing ({})", hooks_json.display()),
        };
    }

    let Some(v) = std::fs::read_to_string(&hooks_json)
        .ok()
        .and_then(|c| crate::core::jsonc::parse_jsonc(&c).ok())
    else {
        return NamedCheck {
            name,
            ok: false,
            detail: format!("invalid JSON ({})", hooks_json.display()),
        };
    };

    // observe hook on PostToolUse, pointing at the current binary.
    let observe_ok = v
        .get("hooks")
        .and_then(|h| h.get("PostToolUse"))
        .and_then(|x| x.as_array())
        .is_some_and(|arr| {
            arr.iter().any(|entry| {
                entry
                    .get("hooks")
                    .and_then(|x| x.as_array())
                    .is_some_and(|hooks| {
                        hooks.iter().any(|h| {
                            let cmd = h
                                .get("command")
                                .and_then(|c| c.as_str())
                                .unwrap_or_default();
                            let first = cmd.split_whitespace().next().unwrap_or_default();
                            cmd.contains("hook observe") && cmd_matches_expected(first, binary)
                        })
                    })
            })
        });

    // The plugin must be registered in the shared import manifest so `agy`
    // discovers it (`agy plugin list`).
    let manifest =
        crate::hooks::agents::antigravity_cli_config_dir(home).join("import_manifest.json");
    let registered = std::fs::read_to_string(&manifest)
        .ok()
        .and_then(|c| crate::core::jsonc::parse_jsonc(&c).ok())
        .and_then(|v| {
            v.get("imports").and_then(|i| i.as_array()).map(|a| {
                a.iter()
                    .any(|e| e.get("name").and_then(|n| n.as_str()) == Some("lean-ctx"))
            })
        })
        .unwrap_or(false);

    // Self-contained bundle (#284): the plugin ships its own `mcp_config.json`
    // next to `plugin.json`/`hooks/`, so `agy plugin validate` reports
    // `mcpServers` and the `ctx_*` tools travel with the plugin. Verify it exists
    // and defines the lean-ctx server pointing at the current binary.
    let mcp_config = plugin_dir.join("mcp_config.json");
    let mcp_ok = std::fs::read_to_string(&mcp_config)
        .ok()
        .and_then(|c| crate::core::jsonc::parse_jsonc(&c).ok())
        .and_then(|v| {
            v.get("mcpServers")
                .and_then(|s| s.get("lean-ctx"))
                .and_then(|s| s.get("command"))
                .and_then(|c| c.as_str())
                .map(|cmd| cmd_matches_expected(cmd, binary))
        })
        .unwrap_or(false);

    let ok = observe_ok && registered && mcp_ok;
    NamedCheck {
        name,
        ok,
        detail: if ok {
            format!("ok ({})", plugin_dir.display())
        } else if !registered {
            format!(
                "not registered in import_manifest.json ({})",
                plugin_dir.display()
            )
        } else if !mcp_ok {
            format!(
                "missing/stale plugin mcp_config.json ({})",
                mcp_config.display()
            )
        } else {
            format!("drift (observe hook) ({})", hooks_json.display())
        },
    }
}

/// Informational note (always `ok`): even when the lean-ctx plugin is installed
/// exactly as `agy` expects, hook *execution* is gated server-side by the
/// Antigravity CLI's `enable_json_hooks` experiment (`json-hooks-enabled`),
/// which no local config can force. Until that flag reaches the account, `/hooks`
/// shows the observe hook as dormant — yet the plugin is correctly installed and
/// the MCP `ctx_*` tools compress regardless. Surfacing this stops users from
/// chasing a local misconfiguration that isn't there (GH #284).
fn antigravity_cli_hooks_note() -> NamedCheck {
    NamedCheck {
        name: "Antigravity CLI hook gating".to_string(),
        ok: true,
        detail: "hook execution is gated server-side by agy's enable_json_hooks experiment (no local config can force it) — if /hooks shows lean-ctx dormant, the plugin is still installed correctly; verify with `agy plugin validate ~/.gemini/config/plugins/lean-ctx`. The ctx_* MCP tools compress on every surface regardless.".to_string(),
    }
}

fn check_cursor_hooks(path: &std::path::Path, binary: &str) -> NamedCheck {
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
    let entries_ok = has_rewrite && has_redirect;
    let stale = stale_hook_binary(&content, binary);
    finalize_hook_check("Hooks", path, entries_ok, stale)
}

/// Shared verdict for hook checks: distinguishes missing/incomplete managed
/// entries from a stale binary reference, so `doctor` can show the precise
/// repair reason (the #249 observability pattern, extended to hook staleness).
fn finalize_hook_check(
    name: &str,
    path: &std::path::Path,
    entries_ok: bool,
    stale: Option<String>,
) -> NamedCheck {
    let ok = entries_ok && stale.is_none();
    let detail = if !entries_ok {
        format!("drift ({})", path.display())
    } else if let Some(old) = stale {
        format!("stale binary {old} — run lean-ctx setup --fix")
    } else {
        format!("ok ({})", path.display())
    };
    NamedCheck {
        name: name.to_string(),
        ok,
        detail,
    }
}

fn check_claude_hooks(path: &std::path::Path, binary: &str) -> NamedCheck {
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
    let entries_ok = joined.contains(" hook rewrite") && joined.contains(" hook redirect");
    let stale = stale_hook_binary(&joined, binary);
    finalize_hook_check("Hooks", path, entries_ok, stale)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_desktop_note_is_informational_and_never_fails() {
        let note = codex_desktop_note();
        assert!(
            note.ok,
            "the Codex Desktop note is informational, never a failure"
        );
        assert!(
            note.detail.contains("ctx_shell") && note.detail.contains("every surface"),
            "note must steer users to the MCP tools as the reliable cross-surface path: {}",
            note.detail
        );
    }

    #[test]
    fn antigravity_cli_hooks_note_is_informational_and_explains_gating() {
        let note = antigravity_cli_hooks_note();
        assert!(
            note.ok,
            "the Antigravity CLI gating note is informational, never a failure"
        );
        assert!(
            note.detail.contains("enable_json_hooks"),
            "note must name the server-side flag that gates hook execution: {}",
            note.detail
        );
        assert!(
            note.detail.contains("ctx_") && note.detail.contains("regardless"),
            "note must reassure that the MCP tools compress regardless: {}",
            note.detail
        );
    }

    #[test]
    fn hook_binary_refs_extracts_token_before_hook_keyword() {
        let content = r#"{"command": "/opt/lean-ctx hook rewrite"} {"command": "/opt/lean-ctx hook redirect"}"#;
        let refs = hook_binary_refs(content);
        assert_eq!(refs, vec!["/opt/lean-ctx", "/opt/lean-ctx"]);
    }

    #[test]
    fn hook_binary_refs_empty_without_hook_invocation() {
        assert!(hook_binary_refs(r#"{"command": "echo nothing here"}"#).is_empty());
    }

    #[test]
    fn hook_binary_refs_handles_minified_json() {
        // `serde_json::to_string` emits no spaces around keys/values; the binary
        // token must still be extracted cleanly. Regression: the whitespace-only
        // split used to capture the entire JSON prefix as the "binary".
        let content = r#"[{"hooks":[{"command":"lean-ctx hook rewrite"},{"command":"lean-ctx hook redirect"}]}]"#;
        assert_eq!(hook_binary_refs(content), vec!["lean-ctx", "lean-ctx"]);
    }

    #[test]
    fn stale_hook_binary_accepts_minified_bare_command() {
        let content = r#"[{"hooks":[{"command":"lean-ctx hook rewrite"}]}]"#;
        assert!(stale_hook_binary(content, "/anything/lean-ctx").is_none());
    }

    #[test]
    fn stale_hook_binary_flags_minified_foreign_path() {
        let content = r#"[{"hooks":[{"command":"/old/install/lean-ctx hook rewrite"}]}]"#;
        assert_eq!(
            stale_hook_binary(content, "/current/lean-ctx").as_deref(),
            Some("/old/install/lean-ctx")
        );
    }

    #[test]
    fn stale_hook_binary_flags_foreign_path() {
        let content = r#""/nonexistent/old/lean-ctx hook rewrite""#;
        let stale = stale_hook_binary(content, "/current/install/lean-ctx");
        assert_eq!(stale.as_deref(), Some("/nonexistent/old/lean-ctx"));
    }

    #[test]
    fn stale_hook_binary_accepts_current_binary() {
        let bin = "/current/install/lean-ctx";
        let content = format!(r#""{bin} hook rewrite""#);
        assert!(stale_hook_binary(&content, bin).is_none());
    }

    #[test]
    fn stale_hook_binary_accepts_bare_path_command() {
        // The bare `lean-ctx` PATH form is always considered current.
        let content = r#""lean-ctx hook rewrite""#;
        assert!(stale_hook_binary(content, "/anything/lean-ctx").is_none());
    }

    #[test]
    fn finalize_hook_check_reports_drift_missing_and_stale() {
        let p = std::path::Path::new("/tmp/hooks.json");

        let missing = finalize_hook_check("Hooks", p, false, None);
        assert!(!missing.ok);
        assert!(missing.detail.contains("drift"));

        let stale = finalize_hook_check("Hooks", p, true, Some("/old/lean-ctx".to_string()));
        assert!(!stale.ok);
        assert!(stale.detail.contains("stale binary"));
        assert!(stale.detail.contains("setup --fix"));

        let healthy = finalize_hook_check("Hooks", p, true, None);
        assert!(healthy.ok);
        assert!(healthy.detail.contains("ok"));
    }

    #[test]
    fn check_antigravity_cli_verifies_self_contained_bundle() {
        let dir = tempfile::tempdir().expect("tempdir");
        let home = dir.path();
        let plugin_dir = crate::hooks::agents::antigravity_cli_plugin_dir(home);
        std::fs::create_dir_all(plugin_dir.join("hooks")).unwrap();

        std::fs::write(
            plugin_dir.join("plugin.json"),
            r#"{"name":"lean-ctx","version":"0.0.1"}"#,
        )
        .unwrap();
        std::fs::write(
            plugin_dir.join("hooks").join("hooks.json"),
            r#"{"hooks":{"PostToolUse":[{"matcher":"*","hooks":[{"type":"command","command":"lean-ctx hook observe"}]}]}}"#,
        )
        .unwrap();
        // The self-contained, spec-compliant piece (#284): plugin-local MCP config.
        std::fs::write(
            plugin_dir.join("mcp_config.json"),
            r#"{"mcpServers":{"lean-ctx":{"command":"lean-ctx"}}}"#,
        )
        .unwrap();
        let cfg_dir = crate::hooks::agents::antigravity_cli_config_dir(home);
        std::fs::create_dir_all(&cfg_dir).unwrap();
        std::fs::write(
            cfg_dir.join("import_manifest.json"),
            r#"{"imports":[{"name":"lean-ctx"}]}"#,
        )
        .unwrap();

        let full = check_antigravity_cli_hooks(home, "lean-ctx");
        assert!(
            full.ok,
            "full self-contained bundle must pass: {}",
            full.detail
        );

        // Drop the plugin-local mcp_config.json -> the check must fail and name it
        // (so `doctor --fix`, which re-runs the installer, knows what to repair).
        std::fs::remove_file(plugin_dir.join("mcp_config.json")).unwrap();
        let drift = check_antigravity_cli_hooks(home, "lean-ctx");
        assert!(!drift.ok, "missing plugin-local mcp_config.json must fail");
        assert!(
            drift.detail.contains("mcp_config.json"),
            "detail must point at the missing mcp_config.json: {}",
            drift.detail
        );
    }

    #[test]
    fn check_cursor_hooks_detects_stale_binary() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("hooks.json");
        std::fs::write(
            &path,
            r#"{
  "hooks": {
    "preToolUse": [
      { "matcher": "Shell", "command": "/old/bin/lean-ctx hook rewrite" },
      { "matcher": "Read|Grep", "command": "/old/bin/lean-ctx hook redirect" }
    ]
  }
}"#,
        )
        .unwrap();
        let check = check_cursor_hooks(&path, "/new/bin/lean-ctx");
        assert!(!check.ok, "stale binary path must fail the hook check");
        assert!(check.detail.contains("stale binary"));
    }

    #[test]
    fn check_cursor_hooks_ok_for_bare_command() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("hooks.json");
        std::fs::write(
            &path,
            r#"{
  "hooks": {
    "preToolUse": [
      { "matcher": "Shell", "command": "lean-ctx hook rewrite" },
      { "matcher": "Read|Grep", "command": "lean-ctx hook redirect" }
    ]
  }
}"#,
        )
        .unwrap();
        let check = check_cursor_hooks(&path, "/new/bin/lean-ctx");
        assert!(
            check.ok,
            "bare lean-ctx command is PATH-resolved and current"
        );
    }
}
