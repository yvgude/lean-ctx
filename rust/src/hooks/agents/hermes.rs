use super::super::{HookMode, install_project_rules, resolve_binary_path};

/// Produce Hermes rules content: canonical shared rules followed by
/// Hermes-specific extras (available tools, multi-agent notes).
/// The canonical section uses markers so the injection layer can update it;
/// Hermes extras sit after `END_MARK` and are preserved as user content.
pub(super) fn hermes_rules_content() -> String {
    let shadow = crate::core::config::Config::load().shadow_mode;
    let base = crate::core::rules_canonical::render(
        shadow,
        crate::core::rules_canonical::Wrapper::Shared,
        crate::core::config::CompressionLevel::Off,
    );
    format!(
        "{base}\n\
         Available tools: ctx_overview, ctx_preload, ctx_dedup, ctx_compress, \
         ctx_session, ctx_knowledge, ctx_semantic_search.\n\
         Multi-agent: ctx_agent(action=handoff|sync). \
         Diary: ctx_agent(action=diary, category=discovery|decision|blocker|progress|insight).\n"
    )
}

pub(crate) fn install_hermes_hook_with_mode(global: bool, mode: HookMode) {
    let Some(home) = crate::core::home::resolve_home_dir() else {
        tracing::error!("Cannot resolve home directory");
        return;
    };

    let binary = resolve_binary_path();
    let config_path = home.join(".hermes/config.yaml");
    let target = crate::core::editor_registry::EditorTarget {
        name: "Hermes Agent",
        agent_key: "hermes".to_string(),
        config_path: config_path.clone(),
        detect_path: home.join(".hermes"),
        config_type: crate::core::editor_registry::ConfigType::HermesYaml,
    };

    // #281: honor `[setup] auto_update_mcp = false` — skip the Hermes MCP server
    // entry under lock-down; the rules below still install.
    let update_mcp = crate::core::config::Config::load()
        .setup
        .should_update_mcp();
    match mode {
        HookMode::Mcp | HookMode::Hybrid if update_mcp => {
            match crate::core::editor_registry::write_config_with_options(
                &target,
                &binary,
                crate::core::editor_registry::WriteOptions {
                    overwrite_invalid: true,
                },
            ) {
                Ok(res) => match res.action {
                    crate::core::editor_registry::WriteAction::Created => {
                        eprintln!(
                            "  \x1b[32m✓\x1b[0m Hermes Agent MCP configured at ~/.hermes/config.yaml"
                        );
                    }
                    crate::core::editor_registry::WriteAction::Updated => {
                        eprintln!(
                            "  \x1b[32m✓\x1b[0m Hermes Agent MCP updated at ~/.hermes/config.yaml"
                        );
                    }
                    crate::core::editor_registry::WriteAction::Already => {
                        eprintln!("  Hermes Agent MCP already configured at ~/.hermes/config.yaml");
                    }
                },
                Err(e) => {
                    tracing::error!("Failed to configure Hermes Agent MCP: {e}");
                }
            }
        }
        _ => {}
    }

    let scope = crate::core::config::Config::load().rules_scope_effective();

    match scope {
        crate::core::config::RulesScope::Global => {
            install_hermes_rules(&home, mode);
        }
        crate::core::config::RulesScope::Project => {
            if !global {
                install_project_hermes_rules(mode);
                install_project_rules();
            }
        }
        crate::core::config::RulesScope::Both => {
            if global {
                install_hermes_rules(&home, mode);
            } else {
                install_hermes_rules(&home, mode);
                install_project_hermes_rules(mode);
                install_project_rules();
            }
        }
    }
}

fn install_hermes_rules(home: &std::path::Path, _mode: HookMode) {
    let rules_path = home.join(".hermes/HERMES.md");
    let content = hermes_rules_content();

    if rules_path.exists() {
        let existing = std::fs::read_to_string(&rules_path).unwrap_or_default();
        if existing.contains("lean-ctx") {
            eprintln!("  Hermes rules already present in ~/.hermes/HERMES.md");
            return;
        }
        let mut updated = existing;
        if !updated.ends_with('\n') {
            updated.push('\n');
        }
        updated.push('\n');
        updated.push_str(&content);
        let _ = std::fs::write(&rules_path, updated);
        eprintln!("  \x1b[32m✓\x1b[0m Appended lean-ctx rules to ~/.hermes/HERMES.md");
    } else {
        if let Some(parent) = rules_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&rules_path, &content);
        eprintln!("  \x1b[32m✓\x1b[0m Created ~/.hermes/HERMES.md with lean-ctx rules");
    }
}

fn install_project_hermes_rules(_mode: HookMode) {
    let Ok(cwd) = std::env::current_dir() else {
        return;
    };
    let rules_path = cwd.join(".hermes.md");
    let content = hermes_rules_content();
    if rules_path.exists() {
        let existing = std::fs::read_to_string(&rules_path).unwrap_or_default();
        if existing.contains("lean-ctx") {
            eprintln!("  .hermes.md already contains lean-ctx rules");
            return;
        }
        let mut updated = existing;
        if !updated.ends_with('\n') {
            updated.push('\n');
        }
        updated.push('\n');
        updated.push_str(&content);
        let _ = std::fs::write(&rules_path, updated);
        eprintln!("  \x1b[32m✓\x1b[0m Appended lean-ctx rules to .hermes.md");
    } else {
        let _ = std::fs::write(&rules_path, &content);
        eprintln!("  \x1b[32m✓\x1b[0m Created .hermes.md with lean-ctx rules");
    }
}
