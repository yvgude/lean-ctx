// SPDX-License-Identifier: Apache-2.0
//! `lean-ctx provider list --json` (`providers.list`): the context providers
//! this Instance has for a project — built-ins, MCP bridges, the linked
//! space and the configured (`providers.toml`) ones — with their status and
//! where their configuration comes from. Configuration only: nothing is
//! contacted (`provider check <id> --json` runs one authenticated request
//! and names a machine-readable `reason` when it fails).

use std::collections::BTreeMap;

use serde::Serialize;

use lean_ctx::core::providers::connection::CHECKABLE;

use super::Failure;

/// Providers the Instance ships.
const BUILTIN: &[&str] = &["github", "gitlab", "jira", "postgres"];

#[derive(Debug, Serialize)]
pub(crate) struct ProvidersList {
    pub project_root: String,
    /// `[providers] enabled`: `false` registers none.
    pub enabled: bool,
    pub providers: Vec<ProviderRow>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct ProviderRow {
    pub id: String,
    pub display_name: String,
    /// `builtin` | `mcp_bridge` | `space` | `config`.
    pub source: &'static str,
    /// The file a `config` provider was read from.
    pub config_path: Option<String>,
    /// `available` (configured and usable) | `not_configured`.
    pub status: &'static str,
    pub requires_auth: bool,
    pub actions: Vec<String>,
    /// `provider check <id>` can test its connection.
    pub checkable: bool,
}

/// Where a registered provider's configuration comes from.
pub(crate) fn source_of(
    id: &str,
    config_paths: &BTreeMap<String, String>,
    bridges: &[String],
) -> (&'static str, Option<String>) {
    if let Some(path) = config_paths.get(id) {
        ("config", Some(path.clone()))
    } else if id
        .strip_prefix("mcp:")
        .is_some_and(|name| bridges.iter().any(|b| b == name))
    {
        ("mcp_bridge", None)
    } else if id == "emspace" {
        ("space", None)
    } else if BUILTIN.contains(&id) {
        ("builtin", None)
    } else {
        ("config", None)
    }
}

/// `provider list --json [--project-root P]`.
pub(crate) fn build(args: &[String]) -> Result<ProvidersList, Failure> {
    let root = super::project_root(args)?;
    let cfg = lean_ctx::core::config::Config::load();
    if !cfg.providers.enabled {
        return Ok(ProvidersList {
            project_root: root,
            enabled: false,
            providers: Vec::new(),
        });
    }
    let path = std::path::Path::new(&root);
    // A registry of this call only: listing never changes the shared one
    // (the daemon serves every project), so one project's configured
    // providers never replace built-ins or reach another project's calls.
    let registry = lean_ctx::core::providers::registry::ProviderRegistry::new();
    lean_ctx::core::providers::init::populate(&registry, Some(path));
    let config_paths: BTreeMap<String, String> =
        lean_ctx::core::providers::config_provider::discovery::discover_configs(Some(path))
            .into_iter()
            .map(|c| (c.config.id, c.source_path.display().to_string()))
            .collect();
    let bridges: Vec<String> = cfg.providers.mcp_bridges.keys().cloned().collect();
    let providers = registry
        .discover()
        .into_iter()
        .map(|info| {
            let (source, config_path) = source_of(&info.id, &config_paths, &bridges);
            ProviderRow {
                checkable: CHECKABLE.contains(&info.id.as_str()),
                status: if info.available {
                    "available"
                } else {
                    "not_configured"
                },
                source,
                config_path,
                display_name: info.display_name,
                requires_auth: info.requires_auth,
                actions: info.actions,
                id: info.id,
            }
        })
        .collect();
    Ok(ProvidersList {
        project_root: root,
        enabled: true,
        providers,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_provider_names_where_its_configuration_comes_from() {
        let configs = BTreeMap::from([(
            "github".to_owned(),
            "/p/.lean-ctx/providers.toml".to_owned(),
        )]);
        let bridges = vec!["docs-mcp".to_owned()];
        assert_eq!(
            source_of("github", &configs, &bridges),
            ("config", Some("/p/.lean-ctx/providers.toml".to_owned())),
            "a configured file replaces the built-in"
        );
        assert_eq!(source_of("gitlab", &configs, &bridges), ("builtin", None));
        assert_eq!(
            source_of("mcp:docs-mcp", &configs, &bridges),
            ("mcp_bridge", None)
        );
        assert_eq!(source_of("emspace", &configs, &bridges), ("space", None));
    }

    #[test]
    fn listing_leaves_the_shared_provider_registry_untouched() {
        let _data = lean_ctx::core::data_dir::isolated_data_dir();
        let project = tempfile::tempdir().unwrap();
        let shared = lean_ctx::core::providers::registry::global_registry();
        let before = shared.discover().len();
        let listed = build(&[
            "--project-root".to_owned(),
            project.path().to_string_lossy().into_owned(),
        ])
        .unwrap();
        assert!(
            listed.providers.iter().any(|p| p.id == "emspace"),
            "the project-scoped provider is listed"
        );
        assert_eq!(shared.discover().len(), before, "nothing was registered");
        assert!(
            shared.get("emspace").is_none(),
            "a project's provider never reaches the shared registry"
        );
    }
}
