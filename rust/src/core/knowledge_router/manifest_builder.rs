//! Provider-aware source-manifest construction.
use std::fs;

use super::source_manifest::{CostClass, SourceCapabilities, SourceManifestEntry};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProviderConfig {
    pub jira_configured: bool,
    pub github_configured: bool,
    pub gitlab_configured: bool,
    pub postgres_configured: bool,
    pub custom_providers: Vec<String>,
}

pub fn build_manifests_from_config(config: &ProviderConfig) -> Vec<SourceManifestEntry> {
    let mut provider_ids = config.custom_providers.clone();
    provider_ids.extend(
        [
            (config.jira_configured, "jira"),
            (config.github_configured, "github"),
            (config.gitlab_configured, "gitlab"),
            (config.postgres_configured, "postgres"),
        ]
        .into_iter()
        .filter_map(|(enabled, id)| enabled.then_some(id.to_string())),
    );
    build_manifests_for_provider_ids(&provider_ids)
}

/// Build manifests for every registered provider, including dynamic providers.
pub fn build_manifests_for_provider_ids(provider_ids: &[String]) -> Vec<SourceManifestEntry> {
    let mut manifests = vec![entry(
        "local_files",
        "LocalFiles",
        &["file"],
        CostClass::Negligible,
    )];
    let mut provider_ids = provider_ids.to_vec();
    provider_ids.sort();
    provider_ids.dedup();
    manifests.extend(provider_ids.iter().map(|id| provider_entry(id)));
    manifests
}

pub fn detect_config() -> ProviderConfig {
    let providers = crate::core::config::Config::load().providers;
    let file = crate::core::config::Config::path()
        .and_then(|path| fs::read_to_string(path).ok())
        .unwrap_or_default();
    let configured = |env: &str, value: bool| std::env::var_os(env).is_some() || value;
    ProviderConfig {
        jira_configured: configured("JIRA_URL", file.contains("[providers.jira]")),
        github_configured: configured(
            "GITHUB_TOKEN",
            providers.github.token.is_some() || providers.github.api_url.is_some(),
        ),
        gitlab_configured: configured(
            "GITLAB_TOKEN",
            providers.gitlab.token.is_some() || providers.gitlab.api_url.is_some(),
        ),
        postgres_configured: std::env::var_os("DATABASE_URL").is_some()
            || std::env::var_os("PGDATABASE").is_some(),
        custom_providers: providers.mcp_bridges.keys().cloned().collect(),
    }
}

fn provider_entry(id: &str) -> SourceManifestEntry {
    match id {
        "jira" => entry(id, "Jira", &["issue"], CostClass::Low),
        "github" => entry(id, "GitHub", &["issue", "pull_request"], CostClass::Low),
        "gitlab" => entry(id, "GitLab", &["issue", "pull_request"], CostClass::Low),
        "postgres" => entry(id, "PostgreSQL", &["schema", "table"], CostClass::Medium),
        "emspace" => entry(id, "Emspace", &["page", "task"], CostClass::Low),
        _ => entry(id, id, &["external"], CostClass::Medium),
    }
}

fn entry(id: &str, name: &str, kinds: &[&str], cost_class: CostClass) -> SourceManifestEntry {
    SourceManifestEntry {
        source_id: id.into(),
        display_name: name.into(),
        kinds: kinds.iter().map(|kind| (*kind).into()).collect(),
        capabilities: SourceCapabilities {
            search: true,
            exact_get: true,
            delta_sync: true,
            live_query: id != "local_files",
            graph_edges: true,
        },
        freshness_typical_ms: 60_000,
        cost_class,
    }
}
