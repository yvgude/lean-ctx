//! Per-agent MCP configuration (configure/disable, target resolution).
//!
//! Split out of `setup/mod.rs`; `use super::*` re-imports the parent module’s
//! aliases and sibling helpers. Public fns are re-exported via `pub(crate) use`.

#[allow(clippy::wildcard_imports)]
use super::*;

/// Result of setting up a single agent with all steps.
#[derive(Debug, Default)]
pub struct AgentSetupResult {
    pub mcp_ok: bool,
    /// MCP registration was intentionally skipped because `[setup]
    /// auto_update_mcp = false` (#281), not because it failed.
    pub mcp_skipped: bool,
    pub rules: crate::rules_inject::InjectResult,
    pub skill_installed: bool,
    pub errors: Vec<String>,
}

/// Complete per-agent setup: MCP config + global rules + skill + hook.
/// Single source of truth — called by both `init --agent` and `setup`.
#[must_use]
pub fn setup_single_agent(
    agent_name: &str,
    global: bool,
    mode: crate::hooks::HookMode,
) -> AgentSetupResult {
    let home = dirs::home_dir().unwrap_or_default();
    let mut result = AgentSetupResult::default();

    crate::hooks::install_agent_hook_with_mode(agent_name, global, mode);

    // #281: honor `[setup] auto_update_mcp = false` — skip MCP registration but
    // still install the hook, rules and skill. Locked-down environments can keep
    // the MCP server out of agent settings without losing the CLI integration.
    if crate::core::config::Config::load()
        .setup
        .should_update_mcp()
    {
        match configure_agent_mcp(agent_name) {
            Ok(()) => result.mcp_ok = true,
            Err(e) => result.errors.push(format!("MCP config: {e}")),
        }
    } else {
        result.mcp_skipped = true;
    }

    result.rules = crate::rules_inject::inject_rules_for_agent(&home, agent_name);

    if let Ok(path) = crate::rules_inject::install_skill_for_agent(&home, agent_name) {
        result.skill_installed = path.exists();
    }

    result
}

pub fn configure_agent_mcp(agent: &str) -> Result<(), String> {
    let home = dirs::home_dir().ok_or_else(|| "Cannot determine home directory".to_string())?;
    let binary = resolve_portable_binary();

    let targets = agent_mcp_targets(agent, &home)?;

    let mut errors = Vec::new();
    for t in &targets {
        if let Err(e) = crate::core::editor_registry::write_config_with_options(
            t,
            &binary,
            WriteOptions {
                overwrite_invalid: true,
            },
        ) {
            eprintln!(
                "\x1b[33m⚠\x1b[0m  Could not configure {}: {}",
                t.config_path.display(),
                e
            );
            errors.push(e);
        }
    }

    if agent == "kiro" {
        install_kiro_steering(&home);
    }

    if (agent == "vscode" || agent == "copilot")
        && let Err(e) = crate::core::editor_registry::plan_mode::write_vscode_plan_settings()
    {
        eprintln!("\x1b[33m⚠\x1b[0m  VS Code plan mode: {e}");
    }
    if (agent == "claude" || agent == "claude-code")
        && let Err(e) =
            crate::core::editor_registry::plan_mode::write_claude_code_plan_permissions()
    {
        eprintln!("\x1b[33m⚠\x1b[0m  Claude Code plan mode: {e}");
    }
    if agent == "codebuddy"
        && let Err(e) =
            crate::core::editor_registry::plan_mode::write_claude_code_plan_permissions()
    {
        eprintln!("\x1b[33m⚠\x1b[0m  CodeBuddy plan mode: {e}");
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{} config(s) could not be written. See warnings above.",
            errors.len()
        ))
    }
}

pub(crate) fn agent_mcp_targets(
    agent: &str,
    home: &std::path::Path,
) -> Result<Vec<EditorTarget>, String> {
    let mut targets = Vec::<EditorTarget>::new();

    let push = |targets: &mut Vec<EditorTarget>,
                name: &'static str,
                config_path: PathBuf,
                config_type: ConfigType| {
        targets.push(EditorTarget {
            name,
            agent_key: agent.to_string(),
            detect_path: PathBuf::from("/nonexistent"), // not used in direct agent config
            config_path,
            config_type,
        });
    };

    match agent {
        "cursor" => push(
            &mut targets,
            "Cursor",
            home.join(".cursor/mcp.json"),
            ConfigType::McpJson,
        ),
        "claude" | "claude-code" => push(
            &mut targets,
            "Claude Code",
            crate::core::editor_registry::claude_mcp_json_path(home),
            ConfigType::McpJson,
        ),
        "codebuddy" => push(
            &mut targets,
            "CodeBuddy",
            crate::core::editor_registry::codebuddy_mcp_json_path(home),
            ConfigType::McpJson,
        ),
        "augment" => {
            push(
                &mut targets,
                "Augment CLI",
                crate::core::editor_registry::augment_cli_settings_path(home),
                ConfigType::McpJson,
            );
            push(
                &mut targets,
                "Augment (VS Code)",
                crate::core::editor_registry::augment_vscode_mcp_path(home),
                ConfigType::AugmentVsCode,
            );
        }
        "windsurf" => push(
            &mut targets,
            "Windsurf",
            home.join(".codeium/windsurf/mcp_config.json"),
            ConfigType::McpJson,
        ),
        "codex" => {
            let codex_dir =
                crate::core::home::resolve_codex_dir().unwrap_or_else(|| home.join(".codex"));
            push(
                &mut targets,
                "Codex CLI",
                codex_dir.join("config.toml"),
                ConfigType::Codex,
            );
        }
        "gemini" => {
            push(
                &mut targets,
                "Gemini CLI",
                home.join(".gemini/settings.json"),
                ConfigType::GeminiSettings,
            );
            push(
                &mut targets,
                "Antigravity IDE",
                home.join(".gemini/antigravity/mcp_config.json"),
                ConfigType::McpJson,
            );
            push(
                &mut targets,
                "Antigravity CLI",
                home.join(".gemini/antigravity-cli/mcp_config.json"),
                ConfigType::McpJson,
            );
        }
        "antigravity" => push(
            &mut targets,
            "Antigravity IDE",
            home.join(".gemini/antigravity/mcp_config.json"),
            ConfigType::McpJson,
        ),
        "antigravity-cli" => push(
            &mut targets,
            "Antigravity CLI",
            home.join(".gemini/antigravity-cli/mcp_config.json"),
            ConfigType::McpJson,
        ),
        "copilot" => push(
            &mut targets,
            "Copilot CLI",
            home.join(".copilot/mcp-config.json"),
            ConfigType::CopilotCli,
        ),
        "crush" => push(
            &mut targets,
            "Crush",
            home.join(".config/crush/crush.json"),
            ConfigType::Crush,
        ),
        "qoder" => {
            for path in crate::core::editor_registry::qoder_all_mcp_paths(home) {
                push(&mut targets, "Qoder", path, ConfigType::QoderSettings);
            }
        }
        "qoderwork" => push(
            &mut targets,
            "QoderWork",
            crate::core::editor_registry::qoderwork_mcp_path(home),
            ConfigType::McpJson,
        ),
        "cline" => push(
            &mut targets,
            "Cline",
            crate::core::editor_registry::cline_mcp_path(),
            ConfigType::McpJson,
        ),
        "roo" => push(
            &mut targets,
            "Roo Code",
            crate::core::editor_registry::roo_mcp_path(),
            ConfigType::McpJson,
        ),
        "kiro" => push(
            &mut targets,
            "AWS Kiro",
            home.join(".kiro/settings/mcp.json"),
            ConfigType::McpJson,
        ),
        "verdent" => push(
            &mut targets,
            "Verdent",
            home.join(".verdent/mcp.json"),
            ConfigType::McpJson,
        ),
        // pi: deliberately no MCP target. Pi has no native MCP adapter — a
        // ~/.pi/agent/mcp.json entry is never served and made older pi-lean-ctx
        // versions disable their embedded bridge (GitHub #361). Pi runs through
        // the pi-lean-ctx npm package; install_pi_hook_with_mode removes stale
        // entries instead.
        "pi" | "jetbrains" | "amp" | "openclaw" => {
            // jetbrains/amp/openclaw: handled by dedicated install hooks
            // (servers[] array / amp.mcpServers / mcp.servers).
        }
        "qwen" => push(
            &mut targets,
            "Qwen Code",
            home.join(".qwen/settings.json"),
            ConfigType::McpJson,
        ),
        "trae" => push(
            &mut targets,
            "Trae",
            home.join(".trae/mcp.json"),
            ConfigType::McpJson,
        ),
        "amazonq" => push(
            &mut targets,
            "Amazon Q Developer",
            home.join(".aws/amazonq/default.json"),
            ConfigType::McpJson,
        ),
        "opencode" => {
            #[cfg(windows)]
            let opencode_path = if let Ok(appdata) = std::env::var("APPDATA") {
                std::path::PathBuf::from(appdata)
                    .join("opencode")
                    .join("opencode.json")
            } else {
                home.join(".config/opencode/opencode.json")
            };
            #[cfg(not(windows))]
            let opencode_path = home.join(".config/opencode/opencode.json");
            push(
                &mut targets,
                "OpenCode",
                opencode_path,
                ConfigType::OpenCode,
            );
        }
        "hermes" => push(
            &mut targets,
            "Hermes Agent",
            home.join(".hermes/config.yaml"),
            ConfigType::HermesYaml,
        ),
        "vscode" => push(
            &mut targets,
            "VS Code",
            crate::core::editor_registry::vscode_mcp_path(),
            ConfigType::VsCodeMcp,
        ),
        "zed" => push(
            &mut targets,
            "Zed",
            crate::core::editor_registry::zed_settings_path(home),
            ConfigType::Zed,
        ),
        "aider" => push(
            &mut targets,
            "Aider",
            home.join(".aider/mcp.json"),
            ConfigType::McpJson,
        ),
        "continue" => push(
            &mut targets,
            "Continue",
            home.join(".continue/mcp.json"),
            ConfigType::McpJson,
        ),
        "neovim" => push(
            &mut targets,
            "Neovim (mcphub.nvim)",
            home.join(".config/mcphub/servers.json"),
            ConfigType::McpJson,
        ),
        "emacs" => push(
            &mut targets,
            "Emacs (mcp.el)",
            home.join(".emacs.d/mcp.json"),
            ConfigType::McpJson,
        ),
        "sublime" => push(
            &mut targets,
            "Sublime Text",
            home.join(".config/sublime-text/mcp.json"),
            ConfigType::McpJson,
        ),
        _ => {
            return Err(format!("Unknown agent '{agent}'"));
        }
    }

    Ok(targets)
}

pub fn disable_agent_mcp(agent: &str, overwrite_invalid: bool) -> Result<(), String> {
    let home = dirs::home_dir().ok_or_else(|| "Cannot determine home directory".to_string())?;

    let mut targets = Vec::<EditorTarget>::new();

    let push = |targets: &mut Vec<EditorTarget>,
                name: &'static str,
                config_path: PathBuf,
                config_type: ConfigType| {
        targets.push(EditorTarget {
            name,
            agent_key: agent.to_string(),
            detect_path: PathBuf::from("/nonexistent"),
            config_path,
            config_type,
        });
    };

    let pi_cfg = home.join(".pi").join("agent").join("mcp.json");

    match agent {
        "cursor" => push(
            &mut targets,
            "Cursor",
            home.join(".cursor/mcp.json"),
            ConfigType::McpJson,
        ),
        "claude" | "claude-code" => push(
            &mut targets,
            "Claude Code",
            crate::core::editor_registry::claude_mcp_json_path(&home),
            ConfigType::McpJson,
        ),
        "codebuddy" => push(
            &mut targets,
            "CodeBuddy",
            crate::core::editor_registry::codebuddy_mcp_json_path(&home),
            ConfigType::McpJson,
        ),
        "augment" => {
            push(
                &mut targets,
                "Augment CLI",
                crate::core::editor_registry::augment_cli_settings_path(&home),
                ConfigType::McpJson,
            );
            push(
                &mut targets,
                "Augment (VS Code)",
                crate::core::editor_registry::augment_vscode_mcp_path(&home),
                ConfigType::AugmentVsCode,
            );
        }
        "windsurf" => push(
            &mut targets,
            "Windsurf",
            home.join(".codeium/windsurf/mcp_config.json"),
            ConfigType::McpJson,
        ),
        "codex" => {
            let codex_dir =
                crate::core::home::resolve_codex_dir().unwrap_or_else(|| home.join(".codex"));
            push(
                &mut targets,
                "Codex CLI",
                codex_dir.join("config.toml"),
                ConfigType::Codex,
            );
        }
        "gemini" => {
            push(
                &mut targets,
                "Gemini CLI",
                home.join(".gemini/settings.json"),
                ConfigType::GeminiSettings,
            );
            push(
                &mut targets,
                "Antigravity IDE",
                home.join(".gemini/antigravity/mcp_config.json"),
                ConfigType::McpJson,
            );
            push(
                &mut targets,
                "Antigravity CLI",
                home.join(".gemini/antigravity-cli/mcp_config.json"),
                ConfigType::McpJson,
            );
        }
        "antigravity" => push(
            &mut targets,
            "Antigravity IDE",
            home.join(".gemini/antigravity/mcp_config.json"),
            ConfigType::McpJson,
        ),
        "antigravity-cli" => push(
            &mut targets,
            "Antigravity CLI",
            home.join(".gemini/antigravity-cli/mcp_config.json"),
            ConfigType::McpJson,
        ),
        "copilot" => push(
            &mut targets,
            "Copilot CLI",
            home.join(".copilot/mcp-config.json"),
            ConfigType::CopilotCli,
        ),
        "crush" => push(
            &mut targets,
            "Crush",
            home.join(".config/crush/crush.json"),
            ConfigType::Crush,
        ),
        "pi" => push(&mut targets, "Pi Coding Agent", pi_cfg, ConfigType::McpJson),
        "qoder" => {
            for path in crate::core::editor_registry::qoder_all_mcp_paths(&home) {
                push(&mut targets, "Qoder", path, ConfigType::QoderSettings);
            }
        }
        "qoderwork" => push(
            &mut targets,
            "QoderWork",
            crate::core::editor_registry::qoderwork_mcp_path(&home),
            ConfigType::McpJson,
        ),
        "cline" => push(
            &mut targets,
            "Cline",
            crate::core::editor_registry::cline_mcp_path(),
            ConfigType::McpJson,
        ),
        "roo" => push(
            &mut targets,
            "Roo Code",
            crate::core::editor_registry::roo_mcp_path(),
            ConfigType::McpJson,
        ),
        "kiro" => push(
            &mut targets,
            "AWS Kiro",
            home.join(".kiro/settings/mcp.json"),
            ConfigType::McpJson,
        ),
        "verdent" => push(
            &mut targets,
            "Verdent",
            home.join(".verdent/mcp.json"),
            ConfigType::McpJson,
        ),
        "jetbrains" | "amp" | "openclaw" => {
            // Not supported for disable via this helper.
        }
        "qwen" => push(
            &mut targets,
            "Qwen Code",
            home.join(".qwen/settings.json"),
            ConfigType::McpJson,
        ),
        "trae" => push(
            &mut targets,
            "Trae",
            home.join(".trae/mcp.json"),
            ConfigType::McpJson,
        ),
        "amazonq" => push(
            &mut targets,
            "Amazon Q Developer",
            home.join(".aws/amazonq/default.json"),
            ConfigType::McpJson,
        ),
        "opencode" => {
            #[cfg(windows)]
            let opencode_path = if let Ok(appdata) = std::env::var("APPDATA") {
                std::path::PathBuf::from(appdata)
                    .join("opencode")
                    .join("opencode.json")
            } else {
                home.join(".config/opencode/opencode.json")
            };
            #[cfg(not(windows))]
            let opencode_path = home.join(".config/opencode/opencode.json");
            push(
                &mut targets,
                "OpenCode",
                opencode_path,
                ConfigType::OpenCode,
            );
        }
        "hermes" => push(
            &mut targets,
            "Hermes Agent",
            home.join(".hermes/config.yaml"),
            ConfigType::HermesYaml,
        ),
        "vscode" => push(
            &mut targets,
            "VS Code",
            crate::core::editor_registry::vscode_mcp_path(),
            ConfigType::VsCodeMcp,
        ),
        "zed" => push(
            &mut targets,
            "Zed",
            crate::core::editor_registry::zed_settings_path(&home),
            ConfigType::Zed,
        ),
        "aider" => push(
            &mut targets,
            "Aider",
            home.join(".aider/mcp.json"),
            ConfigType::McpJson,
        ),
        "continue" => push(
            &mut targets,
            "Continue",
            home.join(".continue/mcp.json"),
            ConfigType::McpJson,
        ),
        "neovim" => push(
            &mut targets,
            "Neovim (mcphub.nvim)",
            home.join(".config/mcphub/servers.json"),
            ConfigType::McpJson,
        ),
        "emacs" => push(
            &mut targets,
            "Emacs (mcp.el)",
            home.join(".emacs.d/mcp.json"),
            ConfigType::McpJson,
        ),
        "sublime" => push(
            &mut targets,
            "Sublime Text",
            home.join(".config/sublime-text/mcp.json"),
            ConfigType::McpJson,
        ),
        _ => {
            return Err(format!("Unknown agent '{agent}'"));
        }
    }

    for t in &targets {
        crate::core::editor_registry::remove_lean_ctx_server(
            t,
            WriteOptions { overwrite_invalid },
        )?;
    }

    Ok(())
}
