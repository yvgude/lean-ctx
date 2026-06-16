//! Provider initialization — auto-registers all built-in + config-based providers.
//!
//! Called once at startup. Respects the `[providers]` config section:
//! - `providers.enabled = false` → skip all registration
//! - `providers.github.enabled = false` → skip GitHub
//! - `providers.gitlab.enabled = false` → skip GitLab
//!
//! After built-in providers, scans well-known directories for user-defined
//! TOML/JSON provider configs and registers them automatically.

use std::path::Path;
use std::sync::Arc;

use super::config_provider::ConfigProvider;
use super::config_provider::discovery::discover_configs;
use super::github::GitHubProvider;
use super::gitlab::GitLabProvider;
use super::jira::JiraProvider;
use super::mcp_bridge::McpBridgeProvider;
use super::postgres::PostgresProvider;
use super::provider_trait::ContextProvider;
use super::registry::global_registry;

/// Register all built-in providers with the global registry.
/// Respects `[providers]` config for enabling/disabling individual providers.
/// Safe to call multiple times (idempotent — overwrites existing entries).
pub fn init_builtin_providers() {
    init_with_project_root(None);
}

/// Register all providers, including config-based ones scoped to `project_root`.
pub fn init_with_project_root(project_root: Option<&Path>) {
    let cfg = crate::core::config::Config::load();

    if !cfg.providers.enabled {
        tracing::debug!("[providers] subsystem disabled via config");
        return;
    }

    let registry = global_registry();

    // --- Built-in providers ---
    if cfg.providers.github.enabled {
        registry.register(Arc::new(GitHubProvider::new()));
    }

    if cfg.providers.gitlab.enabled {
        registry.register(Arc::new(GitLabProvider::new()));
    }

    registry.register(Arc::new(JiraProvider::new()));
    registry.register(Arc::new(PostgresProvider::new()));

    // --- MCP Bridge providers (user-defined external MCP servers) ---
    for (name, entry) in &cfg.providers.mcp_bridges {
        if let Some(url) = &entry.url
            && !url.is_empty()
        {
            registry.register(Arc::new(McpBridgeProvider::new(name, url)));
            continue;
        }
        if let Some(command) = &entry.command
            && !command.is_empty()
        {
            registry.register(Arc::new(McpBridgeProvider::new_stdio(
                name,
                command,
                &entry.args,
            )));
            continue;
        }
        tracing::warn!("[providers] MCP bridge '{name}' has neither url nor command — skipping");
    }

    // --- Config-based providers (user-defined) ---
    let discovered = discover_configs(project_root);
    let mut config_count = 0;
    for entry in discovered {
        match ConfigProvider::from_config(entry.config) {
            Ok(provider) => {
                tracing::info!(
                    "[providers] registered config provider '{}' from {}",
                    provider.id(),
                    entry.source_path.display()
                );
                registry.register(Arc::new(provider));
                config_count += 1;
            }
            Err(e) => {
                tracing::warn!("[providers] skipping {}: {e}", entry.source_path.display());
            }
        }
    }

    // --- WASM providers (opt-in, EPIC 12.10) ---
    // Discovered from `LEAN_CTX_WASM_DIR`; each `<name>.wasm` may ship a
    // `<name>.provider.json` sidecar declaring id/display/actions.
    #[cfg(feature = "wasm")]
    if let Ok(dir) = std::env::var("LEAN_CTX_WASM_DIR") {
        let ids = crate::core::wasm_ext::register_providers_from_dir(registry, &dir);
        if !ids.is_empty() {
            tracing::info!(
                "[providers] registered {} WASM provider(s): {ids:?}",
                ids.len()
            );
        }
    }

    tracing::debug!(
        "[providers] initialized {} provider(s) ({} config-based), {} available",
        registry.provider_count(),
        config_count,
        registry.available_provider_ids().len(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_registers_github_when_enabled() {
        init_builtin_providers();
        let reg = global_registry();
        assert!(reg.get("github").is_some());
    }
}
