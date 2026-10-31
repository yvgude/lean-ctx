//! Provider Registry — dynamic registration and discovery of context providers.
//!
//! Every external data source registers itself here. The registry provides:
//!   - Dynamic provider lookup by ID
//!   - Discovery (list all available providers and their actions)
//!   - Chunking bridge: converts `ProviderResult` → `Vec<ContentChunk>`
//!   - Health checks across all providers
//!
//! Follows the Neocortical Column metaphor: each registered provider is a
//! processing column that converts its native format into the universal
//! `ContentChunk` format for the shared cortical pipeline.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use super::ProviderResult;
use super::provider_trait::{ContextProvider, ProviderParams};
use crate::core::bm25_index::ChunkKind;
use crate::core::content_chunk::ContentChunk;

/// Central registry for all context providers.
pub struct ProviderRegistry {
    providers: RwLock<HashMap<String, Entry>>,
}

struct Entry {
    provider: Arc<dyn ContextProvider>,
    pinned: bool,
}

impl std::fmt::Debug for ProviderRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let count = self.providers.read().map(|p| p.len()).unwrap_or(0);
        f.debug_struct("ProviderRegistry")
            .field("providers_count", &count)
            .finish()
    }
}

impl ProviderRegistry {
    pub fn new() -> Self {
        Self {
            providers: RwLock::new(HashMap::new()),
        }
    }

    pub fn register(&self, provider: Arc<dyn ContextProvider>) {
        let id = provider.id().to_string();
        if let Ok(mut map) = self.providers.write() {
            if !map.get(&id).is_some_and(|entry| entry.pinned) {
                map.insert(
                    id,
                    Entry {
                        provider,
                        pinned: false,
                    },
                );
            }
        }
    }

    /// Startup-only authority. Later built-in/config/WASM initialization cannot
    /// replace this entry, even when it registers the same ID.
    #[cfg(any(unix, windows, test))]
    pub(crate) fn pin(&self, provider: Arc<dyn ContextProvider>) -> Result<(), String> {
        let mut map = self
            .providers
            .write()
            .map_err(|_| "provider registry unavailable")?;
        let id = provider.id().to_string();
        if map.contains_key(&id) {
            return Err("cannot install selected provider after registry initialization".into());
        }
        map.insert(
            id,
            Entry {
                provider,
                pinned: true,
            },
        );
        Ok(())
    }

    pub fn get(&self, id: &str) -> Option<Arc<dyn ContextProvider>> {
        self.providers
            .read()
            .ok()
            .and_then(|map| map.get(id).map(|entry| Arc::clone(&entry.provider)))
    }

    /// Reuse startup-selected authority without trusting a later registry entry.
    pub(crate) fn get_pinned(&self, id: &str) -> Option<Arc<dyn ContextProvider>> {
        self.providers.read().ok().and_then(|map| {
            map.get(id)
                .filter(|entry| entry.pinned)
                .map(|entry| Arc::clone(&entry.provider))
        })
    }

    pub fn execute(
        &self,
        provider_id: &str,
        action: &str,
        params: &ProviderParams,
    ) -> Result<ProviderResult, String> {
        let provider = self
            .get(provider_id)
            .ok_or_else(|| format!("Provider '{provider_id}' not registered"))?;

        if !provider.is_available() {
            return Err(format!(
                "Provider '{provider_id}' is not available (check config/auth)"
            ));
        }

        if !provider.supported_actions().contains(&action) {
            return Err(format!(
                "Provider '{provider_id}' does not support action '{action}'. Supported: {:?}",
                provider.supported_actions()
            ));
        }

        provider.execute(action, params)
    }

    /// Execute and convert results to ContentChunks for BM25/embedding ingest.
    pub(crate) fn execute_bound(
        &self,
        provider_id: &str,
        action: &str,
        params: &ProviderParams,
    ) -> Result<super::provenance::BoundResult, String> {
        let provider = self.get(provider_id).ok_or("provider unavailable")?;
        if !provider.is_available() || !provider.supported_actions().contains(&action) {
            return Err("provider action is unavailable".into());
        }
        super::provenance::BoundResult::execute(provider.as_ref(), action, params)
    }

    pub fn execute_as_chunks(
        &self,
        provider_id: &str,
        action: &str,
        params: &ProviderParams,
    ) -> Result<Vec<ContentChunk>, String> {
        let bound = self.execute_bound(provider_id, action, params)?;
        Ok(bound.chunks(&bound.result))
    }

    /// List all registered providers with their availability and actions.
    pub fn discover(&self) -> Vec<ProviderInfo> {
        let Ok(map) = self.providers.read() else {
            return Vec::new();
        };

        let mut infos: Vec<ProviderInfo> = map
            .values()
            .map(|entry| &entry.provider)
            .map(|p| ProviderInfo {
                id: p.id().to_string(),
                display_name: p.display_name().to_string(),
                available: p.is_available(),
                requires_auth: p.requires_auth(),
                actions: p
                    .supported_actions()
                    .iter()
                    .map(std::string::ToString::to_string)
                    .collect(),
                cache_ttl_secs: p.cache_ttl_secs(),
                connector: p.descriptor(),
            })
            .collect();

        infos.sort_by(|a, b| a.id.cmp(&b.id));
        infos
    }

    pub fn provider_count(&self) -> usize {
        self.providers.read().map_or(0, |m| m.len())
    }

    pub fn available_provider_ids(&self) -> Vec<String> {
        self.providers
            .read()
            .map(|m| {
                m.values()
                    .map(|entry| &entry.provider)
                    .filter(|p| p.is_available())
                    .map(|p| p.id().to_string())
                    .collect()
            })
            .unwrap_or_default()
    }
}

impl Default for ProviderRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Discovery info for a single provider.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ProviderInfo {
    pub id: String,
    pub display_name: String,
    pub available: bool,
    pub requires_auth: bool,
    pub actions: Vec<String>,
    pub cache_ttl_secs: u64,
    pub connector: super::provider_trait::ConnectorDescriptor,
}

/// The connector capability matrix (doc 17) as published in
/// `docs/contracts/connector-capability-matrix-v1.md`: one row per connector
/// kind, rendered from the connectors' own declarations.
pub fn capability_matrix_markdown() -> String {
    use super::provider_trait::{CapabilityLevel, ConnectorDescriptor};
    let level = |l: CapabilityLevel| match l {
        CapabilityLevel::Supported => "yes",
        CapabilityLevel::Partial => "partial",
        CapabilityLevel::Unsupported => "no",
    };
    let rows: [(&str, ConnectorDescriptor); 8] = [
        (
            "GitLab (project configured)",
            super::gitlab::descriptor(true),
        ),
        ("GitLab (no project)", super::gitlab::descriptor(false)),
        ("GitHub", super::github::descriptor()),
        ("Jira", super::jira::descriptor()),
        ("MCP bridge", super::mcp_bridge::descriptor()),
        ("Emspace", super::emspace::descriptor()),
        (
            "Config provider (GET resources)",
            super::config_provider::descriptor(false),
        ),
        (
            "Other providers (default)",
            ConnectorDescriptor::list_only("token or none"),
        ),
    ];
    let mut out = String::from(
        "| Connector | search | list | get | delta | revalidate ACL | revalidate revision | delete event | shared reuse |\n\
         |---|---|---|---|---|---|---|---|---|\n",
    );
    for (name, d) in &rows {
        out.push_str(&format!(
            "| {name} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
            level(d.search),
            level(d.list),
            level(d.get),
            level(d.delta),
            level(d.revalidate_acl),
            level(d.revalidate_revision),
            level(d.delete_event),
            if d.allows_shared_reuse() { "yes" } else { "no" },
        ));
    }
    out.push('\n');
    for (name, d) in &rows {
        out.push_str(&format!(
            "- **{name}** — principal: {}; content: {}; writes: {}; on failure: {}. {}\n",
            d.principal_binding, d.content_trust, d.write_surface, d.failure_policy, d.notes
        ));
    }
    out
}

// ---------------------------------------------------------------------------
// Chunking bridge: ProviderResult → ContentChunks
// ---------------------------------------------------------------------------

fn action_to_chunk_kind(resource_type: &str) -> ChunkKind {
    match resource_type {
        "issues" => ChunkKind::Issue,
        "merge_requests" | "pull_requests" | "prs" => ChunkKind::PullRequest,
        "wikis" | "pages" => ChunkKind::WikiPage,
        "schemas" | "tables" => ChunkKind::DbSchema,
        "endpoints" | "routes" => ChunkKind::ApiEndpoint,
        "tickets" => ChunkKind::Ticket,
        _ => ChunkKind::ExternalOther,
    }
}

/// Convert a `ProviderResult` into a list of `ContentChunk`s.
pub fn result_to_chunks(result: &ProviderResult) -> Vec<ContentChunk> {
    let kind = action_to_chunk_kind(&result.resource_type);

    result
        .items
        .iter()
        .map(|item| {
            let body = item.body.as_deref().unwrap_or("");
            let content = format!(
                "#{} {}{}\n{}",
                item.id,
                item.title,
                item.state
                    .as_ref()
                    .map(|s| format!(" [{s}]"))
                    .unwrap_or_default(),
                body,
            );

            let mut references = crate::core::content_chunk::extract_file_references(&content);
            if !item.references.is_empty() {
                references.extend(item.references.iter().cloned());
                references.sort();
                references.dedup();
            }

            let metadata = serde_json::json!({
                "state": item.state,
                "author": item.author,
                "created_at": item.created_at,
                "updated_at": item.updated_at,
                "url": item.url,
                "labels": item.labels,
            });

            ContentChunk::from_provider(
                &result.provider,
                &result.resource_type,
                &item.id,
                &item.title,
                kind.clone(),
                content,
                references,
                Some(metadata),
            )
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Global singleton (matches existing pattern in providers/cache.rs)
// ---------------------------------------------------------------------------

static GLOBAL_REGISTRY: std::sync::LazyLock<ProviderRegistry> =
    std::sync::LazyLock::new(ProviderRegistry::new);

pub fn global_registry() -> &'static ProviderRegistry {
    &GLOBAL_REGISTRY
}

/// A built-in provider constructed from the current configuration, for
/// re-authorizing an item it produced. Never a registry entry: a provider
/// registered later under the same id must not answer for a stored origin.
/// Only providers that implement `reacquire_item` are named here.
pub(crate) fn builtin_for_reuse(id: &str) -> Option<Arc<dyn ContextProvider>> {
    match id {
        "gitlab" => Some(Arc::new(super::gitlab::GitLabProvider::new())),
        "github" => Some(Arc::new(super::github::GitHubProvider::new())),
        "jira" => Some(Arc::new(super::jira::JiraProvider::new())),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::providers::{ProviderItem, ProviderResult};

    #[test]
    fn the_published_capability_matrix_is_the_connectors_own_declaration() {
        const BEGIN: &str = "<!-- connector-capability-matrix:begin -->\n";
        const END: &str = "<!-- connector-capability-matrix:end -->";
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../docs/contracts/provider-framework-contract-v1.md");
        // A Windows checkout may hold the document with CRLF line endings.
        let doc = std::fs::read_to_string(&path)
            .unwrap()
            .replace("\r\n", "\n");
        let start = doc.find(BEGIN).unwrap() + BEGIN.len();
        let end = doc.find(END).unwrap();
        let rendered = capability_matrix_markdown();
        if std::env::var_os("LEAN_CTX_UPDATE_GOLDEN").is_some() {
            std::fs::write(&path, format!("{}{rendered}{}", &doc[..start], &doc[end..])).unwrap();
            return;
        }
        assert_eq!(
            &doc[start..end],
            rendered,
            "regenerate with LEAN_CTX_UPDATE_GOLDEN=1"
        );
        // Shared reuse is never claimed without a current ACL re-check.
        assert!(rendered.contains("| MCP bridge | no | yes | yes | no | no | no | no | no |"));
        assert!(
            rendered
                .contains("| GitLab (no project) | partial | yes | yes | no | no | no | no | no |")
        );
    }

    #[test]
    fn result_to_chunks_preserves_provider_id() {
        let result = ProviderResult {
            provider: "github".into(),
            resource_type: "issues".into(),
            items: vec![ProviderItem {
                id: "42".into(),
                title: "Auth bug".into(),
                state: Some("open".into()),
                author: Some("dev".into()),
                created_at: None,
                updated_at: None,
                url: Some("https://github.com/o/r/issues/42".into()),
                labels: vec!["bug".into()],
                body: Some("Fix in src/auth/handler.rs".into()),
                ..Default::default()
            }],
            total_count: Some(1),
            truncated: false,
        };

        let chunks = result_to_chunks(&result);
        assert_eq!(chunks.len(), 1);
        let c = &chunks[0];
        assert_eq!(c.provider_id(), Some("github"));
        assert_eq!(c.kind, ChunkKind::Issue);
        assert!(c.file_path.contains("github://issues/42"));
        assert!(c.references.contains(&"src/auth/handler.rs".to_string()));
    }

    #[test]
    fn action_maps_to_correct_kind() {
        assert_eq!(action_to_chunk_kind("issues"), ChunkKind::Issue);
        assert_eq!(
            action_to_chunk_kind("pull_requests"),
            ChunkKind::PullRequest
        );
        assert_eq!(
            action_to_chunk_kind("merge_requests"),
            ChunkKind::PullRequest
        );
        assert_eq!(action_to_chunk_kind("wikis"), ChunkKind::WikiPage);
        assert_eq!(action_to_chunk_kind("schemas"), ChunkKind::DbSchema);
        assert_eq!(action_to_chunk_kind("endpoints"), ChunkKind::ApiEndpoint);
        assert_eq!(action_to_chunk_kind("tickets"), ChunkKind::Ticket);
        assert_eq!(action_to_chunk_kind("unknown"), ChunkKind::ExternalOther);
    }

    #[test]
    fn registry_discover_returns_sorted() {
        let reg = ProviderRegistry::new();
        let infos = reg.discover();
        assert!(infos.is_empty());
    }

    #[test]
    fn registry_execute_unknown_provider_errors() {
        let reg = ProviderRegistry::new();
        let result = reg.execute("nonexistent", "issues", &ProviderParams::default());
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not registered"));
    }

    #[test]
    fn selected_provider_survives_reinitialization_and_rejects_late_pin() {
        use crate::core::providers::{config::GitLabConfig, gitlab::GitLabProvider};
        let provider = || -> Arc<dyn ContextProvider> {
            Arc::new(GitLabProvider::with_config(GitLabConfig {
                host: "gitlab.example.test".into(),
                token: "synthetic-registry-token".into(),
                project_path: Some("5".into()),
            }))
        };
        let registry = ProviderRegistry::new();
        let selected = provider();
        registry.pin(Arc::clone(&selected)).unwrap();
        for _ in 0..3 {
            registry.register(provider());
        }
        assert!(Arc::ptr_eq(&registry.get("gitlab").unwrap(), &selected));
        assert!(registry.pin(provider()).is_err());
        assert_eq!(registry.provider_count(), 1);
        let ordinary = ProviderRegistry::new();
        ordinary.register(provider());
        assert!(ordinary.pin(provider()).is_err());
    }
}
