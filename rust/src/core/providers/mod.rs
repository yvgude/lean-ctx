pub mod cache;
pub mod config;
pub mod config_provider;
pub mod github;
pub mod gitlab;
pub mod init;
pub mod jira;
pub mod jira_oauth;
pub mod mcp_bridge;
pub mod postgres;
pub mod provider_trait;
pub mod registry;
pub mod scaffold;

pub use provider_trait::{ContextPacket, ContextProvider, ProviderParams};
pub use registry::{ProviderRegistry, global_registry};

use serde::{Deserialize, Serialize};

use crate::core::evidence::Claim;

/// Intern a string to a process-global `&'static str`, leaking each *unique* value at
/// most once. Provider constructors run per `ctx_provider`/`ctx_preload` call, so a
/// naive `Box::leak` per construction leaked unboundedly; interning bounds the leak to
/// the finite set of distinct provider ids/names/actions.
pub(crate) fn intern(s: String) -> &'static str {
    use std::collections::HashSet;
    use std::sync::{Mutex, OnceLock};
    static POOL: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();
    let pool = POOL.get_or_init(|| Mutex::new(HashSet::new()));
    let mut guard = pool
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(&existing) = guard.get(s.as_str()) {
        return existing;
    }
    let leaked: &'static str = Box::leak(s.into_boxed_str());
    guard.insert(leaked);
    leaked
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderResult {
    pub provider: String,
    pub resource_type: String,
    pub items: Vec<ProviderItem>,
    pub total_count: Option<usize>,
    pub truncated: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProviderItem {
    pub id: String,
    pub title: String,
    pub state: Option<String>,
    pub author: Option<String>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    pub url: Option<String>,
    pub labels: Vec<String>,
    pub body: Option<String>,
    /// Attributable evidence distilled from this item (confidence + source).
    /// Empty for plain records; populated by research/extraction providers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub claims: Vec<Claim>,
}

impl ProviderResult {
    #[must_use]
    pub fn format_compact(&self) -> String {
        let mut out = format!(
            "{} {} ({}{}):\n",
            self.provider,
            self.resource_type,
            self.items.len(),
            if self.truncated { "+" } else { "" }
        );
        for item in &self.items {
            let state = item.state.as_deref().unwrap_or("");
            let labels = if item.labels.is_empty() {
                String::new()
            } else {
                format!(" [{}]", item.labels.join(","))
            };
            out.push_str(&format!(
                "  #{} {} ({}){}\n",
                item.id, item.title, state, labels
            ));
            for claim in &item.claims {
                out.push_str("    ▸ ");
                out.push_str(&claim.render());
                out.push('\n');
            }
        }
        out
    }
}
