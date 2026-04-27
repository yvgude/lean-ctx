use super::super::{install_project_rules, resolve_binary_path};

pub(super) const HERMES_RULES_TEMPLATE: &str = "\
# lean-ctx — Context Engineering Layer

PREFER lean-ctx MCP tools over native equivalents for token savings:

| PREFER | OVER | Why |
|--------|------|-----|
| `ctx_read(path, mode)` | `Read` / `cat` | Cached, 10 read modes, re-reads ~13 tokens |
| `ctx_shell(command)` | `Shell` / `bash` | Pattern compression for git/npm/cargo output |
| `ctx_search(pattern, path)` | `Grep` / `rg` | Compact search results |
| `ctx_tree(path, depth)` | `ls` / `find` | Compact directory maps |

- Native Edit/StrReplace stay unchanged. If Edit requires Read and Read is unavailable, use `ctx_edit(path, old_string, new_string)`.
- Write, Delete, Glob — use normally.

ctx_read modes: full|map|signatures|diff|task|reference|aggressive|entropy|lines:N-M. Auto-selects optimal mode.
Re-reads cost ~13 tokens (cached).

Available tools: ctx_overview, ctx_preload, ctx_dedup, ctx_compress, ctx_session, ctx_knowledge, ctx_semantic_search.
Multi-agent: ctx_agent(action=handoff|sync). Diary: ctx_agent(action=diary, category=discovery|decision|blocker|progress|insight).
";

pub(crate) fn install_hermes_hook(global: bool) {
    let Some(home) = dirs::home_dir() else {
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

    match crate::core::editor_registry::write_config_with_options(
        &target,
        &binary,
        crate::core::editor_registry::WriteOptions {
            overwrite_invalid: true,
        },
    ) {
        Ok(res) => match res.action {
            crate::core::editor_registry::WriteAction::Created => {
                println!("  \x1b[32m✓\x1b[0m Hermes Agent MCP configured at ~/.hermes/config.yaml");
            }
            crate::core::editor_registry::WriteAction::Updated => {
                println!("  \x1b[32m✓\x1b[0m Hermes Agent MCP updated at ~/.hermes/config.yaml");
            }
            crate::core::editor_registry::WriteAction::Already => {
                println!("  Hermes Agent MCP already configured at ~/.hermes/config.yaml");
            }
        },
        Err(e) => {
            tracing::error!("Failed to configure Hermes Agent MCP: {e}");
        }
    }

    let scope = crate::core::config::Config::load().rules_scope_effective();

    match scope {
        crate::core::config::RulesScope::Global => {
            install_hermes_rules(&home);
        }
        crate::core::config::RulesScope::Project => {
            if !global {
                install_project_hermes_rules();
                install_project_rules();
            }
        }
        crate::core::config::RulesScope::Both => {
            if global {
                install_hermes_rules(&home);
            } else {
                install_hermes_rules(&home);
                install_project_hermes_rules();
                install_project_rules();
            }
        }
    }
}

fn install_hermes_rules(home: &std::path::Path) {
    let rules_path = home.join(".hermes/HERMES.md");
    let content = HERMES_RULES_TEMPLATE;

    if rules_path.exists() {
        let existing = std::fs::read_to_string(&rules_path).unwrap_or_default();
        if existing.contains("lean-ctx") {
            println!("  Hermes rules already present in ~/.hermes/HERMES.md");
            return;
        }
        let mut updated = existing;
        if !updated.ends_with('\n') {
            updated.push('\n');
        }
        updated.push('\n');
        updated.push_str(content);
        let _ = std::fs::write(&rules_path, updated);
        println!("  \x1b[32m✓\x1b[0m Appended lean-ctx rules to ~/.hermes/HERMES.md");
    } else {
        if let Some(parent) = rules_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&rules_path, content);
        println!("  \x1b[32m✓\x1b[0m Created ~/.hermes/HERMES.md with lean-ctx rules");
    }
}

fn install_project_hermes_rules() {
    let Ok(cwd) = std::env::current_dir() else {
        return;
    };
    let rules_path = cwd.join(".hermes.md");
    if rules_path.exists() {
        let existing = std::fs::read_to_string(&rules_path).unwrap_or_default();
        if existing.contains("lean-ctx") {
            println!("  .hermes.md already contains lean-ctx rules");
            return;
        }
        let mut updated = existing;
        if !updated.ends_with('\n') {
            updated.push('\n');
        }
        updated.push('\n');
        updated.push_str(HERMES_RULES_TEMPLATE);
        let _ = std::fs::write(&rules_path, updated);
        println!("  \x1b[32m✓\x1b[0m Appended lean-ctx rules to .hermes.md");
    } else {
        let _ = std::fs::write(&rules_path, HERMES_RULES_TEMPLATE);
        println!("  \x1b[32m✓\x1b[0m Created .hermes.md with lean-ctx rules");
    }
}
