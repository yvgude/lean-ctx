// SPDX-License-Identifier: Apache-2.0
//! Per-editor integration status builders (Cursor/Claude/CodeBuddy + the
//! generic registry-driven path).

#[allow(clippy::wildcard_imports)]
use super::*;

pub(crate) fn integration_generic(
    home: &std::path::Path,
    binary: &str,
    data_dir: &str,
    target: &lean_ctx::core::editor_registry::types::EditorTarget,
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
        lean_ctx::core::editor_registry::types::ConfigType::McpJson
        | lean_ctx::core::editor_registry::types::ConfigType::OmpMcp
        | lean_ctx::core::editor_registry::types::ConfigType::CodeWhale
        | lean_ctx::core::editor_registry::types::ConfigType::MuseSettings
        | lean_ctx::core::editor_registry::types::ConfigType::CommandCode
        | lean_ctx::core::editor_registry::types::ConfigType::QoderSettings => {
            checks.push(check_mcp_json(&target.config_path, binary, data_dir));
            // The Antigravity CLI also installs observe hooks as a plugin under
            // ~/.gemini/config/plugins/lean-ctx (registered in import_manifest.json,
            // NOT in any settings.json — see GH #284); verify that plugin too.
            if target.agent_key == "antigravity-cli" {
                checks.push(check_antigravity_cli_hooks(home, binary));
                checks.push(antigravity_cli_hooks_note());
            }
        }
        lean_ctx::core::editor_registry::types::ConfigType::ClineCli => {
            checks.push(check_cline_cli_config(
                &target.config_path,
                binary,
                data_dir,
            ));
        }
        lean_ctx::core::editor_registry::types::ConfigType::JetBrains => {
            checks.push(check_jetbrains_snippet(
                &target.config_path,
                binary,
                data_dir,
            ));
        }
        lean_ctx::core::editor_registry::types::ConfigType::Zed => {
            checks.push(check_zed_settings(&target.config_path, binary));
        }
        lean_ctx::core::editor_registry::types::ConfigType::Codex => {
            checks.push(check_codex_toml(&target.config_path, binary));
            checks.push(check_codex_history_visibility(home));
            checks.push(check_codex_hooks_enabled(home));
            checks.push(check_codex_hooks_json(home, binary));
            checks.push(codex_desktop_note());
        }
        lean_ctx::core::editor_registry::types::ConfigType::VsCodeMcp => {
            checks.push(check_vscode_mcp(&target.config_path, binary, data_dir));
        }
        lean_ctx::core::editor_registry::types::ConfigType::CopilotCli => {
            checks.push(check_copilot_cli_mcp(&target.config_path, binary, data_dir));
        }
        lean_ctx::core::editor_registry::types::ConfigType::OpenCode => {
            checks.push(check_opencode_config(&target.config_path, binary, data_dir));
        }
        lean_ctx::core::editor_registry::types::ConfigType::Crush => {
            checks.push(check_crush_config(&target.config_path, binary, data_dir));
        }
        lean_ctx::core::editor_registry::types::ConfigType::Amp => {
            checks.push(check_amp_config(&target.config_path, binary, data_dir));
        }
        lean_ctx::core::editor_registry::types::ConfigType::HermesYaml => {
            checks.push(check_hermes_yaml(&target.config_path, binary, data_dir));
        }
        lean_ctx::core::editor_registry::types::ConfigType::GeminiSettings => {
            checks.push(check_mcp_json(&target.config_path, binary, data_dir));
            checks.push(check_gemini_trust_and_hooks(home, binary));
        }
        lean_ctx::core::editor_registry::types::ConfigType::AugmentVsCode => {
            checks.push(check_augment_vscode_mcp(
                &target.config_path,
                binary,
                data_dir,
            ));
        }
        lean_ctx::core::editor_registry::types::ConfigType::OpenClaw => {
            checks.push(check_openclaw_config(&target.config_path, binary, data_dir));
        }
        lean_ctx::core::editor_registry::types::ConfigType::VibeToml => {
            // Vibe uses TOML config, check if lean-ctx is in mcp_servers
            checks.push(check_vibe_config(&target.config_path, binary));
        }
    }

    if let Some(rules_path) = rules_path_for(target.name, home) {
        // #1596: honour rules_injection / rules_scope — an intentionally absent
        // rules file is not drift.
        let cfg = lean_ctx::core::config::Config::load();
        checks.push(check_rules_file_for_policy(
            &rules_path,
            cfg.rules_injection_effective(),
            cfg.rules_scope_effective(),
        ));
    }

    // #593: Windsurf is wired through MCP + dedicated rules + Cascade hooks, but
    // has NO on-demand skill by design. Surface a consolidated status so an empty
    // `watch` or a "missing skill" is not misread as a broken install, and show
    // the last real ctx_* MCP call so users can see whether Cascade actually
    // drives lean-ctx (GLM 5.2 and other weaker models often call native tools).
    if target.name == "Windsurf" {
        checks.push(check_windsurf_hooks(home));
        checks.push(skill_not_applicable_note());
        checks.push(last_ctx_call_check());
    }

    let ok = checks.iter().all(|c| c.ok);
    IntegrationStatus {
        name: target.name.to_string(),
        detected: true,
        checks,
        ok,
    }
}

pub(crate) fn integration_cursor(
    home: &std::path::Path,
    binary: &str,
    data_dir: &str,
) -> IntegrationStatus {
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

pub(crate) fn integration_claude(
    home: &std::path::Path,
    binary: &str,
    data_dir: &str,
) -> IntegrationStatus {
    let target = lean_ctx::core::editor_registry::build_targets(home)
        .into_iter()
        .find(|t| t.agent_key == "claude");
    let detected = target.as_ref().is_some_and(|t| t.detect_path.exists())
        || lean_ctx::core::editor_registry::claude_state_dir(home).exists()
        || claude_binary_exists();

    if !detected {
        return IntegrationStatus {
            name: "Claude Code".to_string(),
            detected: false,
            checks: Vec::new(),
            ok: true,
        };
    }
    if let Some(arm) = lean_ctx_hooks::hooks::agents::claude_governor::installed_arm(home) {
        return integration_claude_governor(home, binary, arm);
    }

    let mut checks = Vec::new();
    // The governor is the default Claude Code setup; an unpinned standard
    // setup gets one informational line here (doctor output only, never in
    // hooks or sessions). A pinned choice gets none.
    if let Some(host) = lean_ctx_hooks::hooks::governor_default::host("claude")
        && lean_ctx_hooks::hooks::governor_default::pinned_mode(host).is_none()
        && lean_ctx_hooks::hooks::agents::claude_governor::has_standard_setup(home)
    {
        checks.push(NamedCheck {
            name: "Setup".to_string(),
            ok: true,
            detail: "standard (ctx_* MCP tools). The governor setup is the cheaper default: \
                     lean-ctx init --agent claude --mode governor (keep this one: --mode hybrid)"
                .to_string(),
        });
    }
    let mcp_path = lean_ctx::core::editor_registry::claude_mcp_json_path(home);
    let mcp_check = check_mcp_json(&mcp_path, binary, data_dir);
    let mcp_registered = mcp_check.ok;
    checks.push(mcp_check);

    let settings_path =
        lean_ctx::core::editor_registry::claude_state_dir(home).join("settings.json");
    checks.push(check_claude_hooks(&settings_path, binary));

    // #719: the generated wrapper scripts can carry a stale machine-absolute
    // binary even when settings.json is healthy (synced multi-machine setups).
    let hooks_dir = lean_ctx::core::editor_registry::claude_state_dir(home).join("hooks");
    checks.push(check_hook_wrapper_scripts(&hooks_dir, binary, home));

    // v3 layout (GL #555, GH #396): instructions live in the CLAUDE.md block +
    // on-demand skill; `setup` removes the legacy rules file. Same detector as
    // the main doctor check, so the two views can never disagree again.
    {
        use crate::doctor::common::ClaudeInstructionsState as S;
        let cfg = lean_ctx::core::config::Config::load();
        let state = crate::doctor::common::claude_instructions_state(
            home,
            cfg.rules_scope_effective(),
            cfg.rules_injection_effective(),
        );
        let claude_md = lean_ctx::core::editor_registry::claude_state_dir(home).join("CLAUDE.md");
        let detail = match state {
            S::ProjectScope => "project scope (global instructions intentionally absent)".into(),
            S::InjectionOff => "rules injection off (intentionally not installed)".into(),
            S::DedicatedWithSkill => "dedicated injection + skill".into(),
            S::DedicatedMissingSkill => "skill missing (run: lean-ctx setup)".into(),
            S::BlockAndSkill => format!("{} block + skill", claude_md.display()),
            S::BlockOnly => format!("{} block", claude_md.display()),
            S::LegacyRules => "legacy rules file (migrates on next setup)".into(),
            S::Governor => "governor setup (no MCP tools, zero prefix)".into(),
            S::Missing => "missing (run: lean-ctx setup)".into(),
        };
        let advertises_ctx_tools = matches!(state, S::BlockAndSkill | S::BlockOnly);
        checks.push(NamedCheck {
            name: "Instructions".to_string(),
            ok: state.ok(),
            detail,
        });

        // GH #637 (second half) / GL #1139: a CLAUDE.md block that advertises
        // ctx_* tools while no lean-ctx MCP server is registered strands the
        // agent — it chases fallbacks (`ctx_edit`) that do not exist in the
        // session. Surface the *combination* explicitly; the bare "MCP config"
        // failure above does not tell the user the instructions are the hazard.
        if advertises_ctx_tools && !mcp_registered {
            checks.push(NamedCheck {
                name: "Instructions/MCP consistency".to_string(),
                ok: false,
                detail: format!(
                    "CLAUDE.md advertises ctx_* tools but no lean-ctx MCP server is registered in {} — run: lean-ctx setup",
                    mcp_path.display()
                ),
            });
        }
    }

    // #637: surface the Read-redirect posture so a Claude Code user understands why
    // native reads are (not) transparently compressed here. Purely informational —
    // every mode is a valid choice, so it never fails the integration.
    {
        use lean_ctx::core::config::ReadRedirect;
        let cfg = lean_ctx::core::config::Config::load();
        let detail = match ReadRedirect::effective(&cfg) {
            ReadRedirect::Auto => {
                "auto — native Read passes through Claude Code's read-before-write guard; re-reads dedup via PostToolUse, ctx_read + Grep/Glob still compress (#637)"
            }
            ReadRedirect::On => {
                "on — native Read redirected to ctx_read; can retrigger #637 here — switch to auto if native Write/Edit fails"
            }
            ReadRedirect::Off => "off — native Read not redirected; ctx_read compresses on request",
        };
        checks.push(NamedCheck {
            name: "Read redirect".to_string(),
            ok: true,
            detail: detail.to_string(),
        });
    }

    let ok = checks.iter().all(|c| c.ok);
    IntegrationStatus {
        name: "Claude Code".to_string(),
        detected: true,
        checks,
        ok,
    }
}

/// The governor setup: hooks and the rules block, no MCP server by design.
fn integration_claude_governor(
    home: &std::path::Path,
    binary: &str,
    arm: lean_ctx_hooks::hooks::agents::claude_governor::GovernorArm,
) -> IntegrationStatus {
    let settings_path =
        lean_ctx::core::editor_registry::claude_state_dir(home).join("settings.json");
    let settings = std::fs::read_to_string(&settings_path).unwrap_or_default();
    let hooks_ok =
        settings.contains(" hook govern\"") && settings.contains(" hook rewrite-enforce\"");
    let stale = stale_hook_binary(&settings, binary);
    let hooks = finalize_hook_check("Hooks", &settings_path, hooks_ok, stale);
    let claude_md = lean_ctx::core::editor_registry::claude_state_dir(home).join("CLAUDE.md");
    let md = std::fs::read_to_string(&claude_md).unwrap_or_default();
    // The no-MCP arm writes no block (zero prefix); the MCP arm needs its own.
    let block_ok = match arm {
        lean_ctx_hooks::hooks::agents::claude_governor::GovernorArm::NoMcp => {
            !md.contains("<!-- lean-ctx")
        }
        lean_ctx_hooks::hooks::agents::claude_governor::GovernorArm::McpDeferred => {
            md.contains("<!-- lean-ctx-governor-v1 -->")
        }
    };
    let checks = vec![
        NamedCheck {
            name: "Setup".to_string(),
            ok: true,
            detail: format!(
                "{} — native tools + hook govern; ctx_* MCP tools: lean-ctx init --agent claude --mode hybrid",
                arm.label()
            ),
        },
        hooks,
        NamedCheck {
            name: "Instructions".to_string(),
            ok: block_ok,
            detail: match (block_ok, arm) {
                (true, lean_ctx_hooks::hooks::agents::claude_governor::GovernorArm::NoMcp) => {
                    "no lean-ctx rules block (zero prefix)".into()
                }
                (true, _) => format!("{} governor block", claude_md.display()),
                (false, _) => format!(
                    "rules block out of date (run: lean-ctx init --agent claude --mode {})",
                    arm.label()
                ),
            },
        },
    ];
    let ok = checks.iter().all(|c| c.ok);
    IntegrationStatus {
        name: "Claude Code".to_string(),
        detected: true,
        checks,
        ok,
    }
}

pub(crate) fn integration_codebuddy(
    home: &std::path::Path,
    binary: &str,
    data_dir: &str,
) -> IntegrationStatus {
    let target = lean_ctx::core::editor_registry::build_targets(home)
        .into_iter()
        .find(|t| t.agent_key == "codebuddy");
    let detected = target.as_ref().is_some_and(|t| t.detect_path.exists())
        || lean_ctx::core::editor_registry::codebuddy_state_dir(home).exists()
        || codebuddy_binary_exists();

    if !detected {
        return IntegrationStatus {
            name: "CodeBuddy".to_string(),
            detected: false,
            checks: Vec::new(),
            ok: true,
        };
    }

    let mut checks = Vec::new();
    let mcp_path = lean_ctx::core::editor_registry::codebuddy_mcp_json_path(home);
    checks.push(check_mcp_json(&mcp_path, binary, data_dir));

    let settings_path =
        lean_ctx::core::editor_registry::codebuddy_state_dir(home).join("settings.json");
    checks.push(check_claude_hooks(&settings_path, binary));

    // #719: wrapper staleness, same rationale as the Claude check.
    let hooks_dir = lean_ctx::core::editor_registry::codebuddy_state_dir(home).join("hooks");
    checks.push(check_hook_wrapper_scripts(&hooks_dir, binary, home));

    // CodeBuddy uses the same block + skill pattern as Claude Code.
    {
        use crate::doctor::common::ClaudeInstructionsState as S;
        let cfg = lean_ctx::core::config::Config::load();
        let state = crate::doctor::common::codebuddy_instructions_state(
            home,
            cfg.rules_scope_effective(),
            cfg.rules_injection_effective(),
        );
        let codebuddy_md =
            lean_ctx::core::editor_registry::codebuddy_state_dir(home).join("CODEBUDDY.md");
        let detail = match state {
            S::ProjectScope => "project scope (global instructions intentionally absent)".into(),
            S::InjectionOff => "rules injection off (intentionally not installed)".into(),
            S::DedicatedWithSkill => "dedicated injection + skill".into(),
            S::DedicatedMissingSkill => "skill missing (run: lean-ctx setup)".into(),
            S::BlockAndSkill => format!("{} block + skill", codebuddy_md.display()),
            S::BlockOnly => format!("{} block", codebuddy_md.display()),
            S::LegacyRules => "legacy rules file (migrates on next setup)".into(),
            S::Governor => "governor setup (no MCP tools, zero prefix)".into(),
            S::Missing => "missing (run: lean-ctx setup)".into(),
        };
        checks.push(NamedCheck {
            name: "Instructions".to_string(),
            ok: state.ok(),
            detail,
        });
    }

    let ok = checks.iter().all(|c| c.ok);
    IntegrationStatus {
        name: "CodeBuddy".to_string(),
        detected: true,
        checks,
        ok,
    }
}
