use super::{ProviderItem, ProviderResult};

/// Plugin-ready trait for external context providers.
///
/// A `ContextProvider` connects `LeanCTX` to external data sources (issue trackers,
/// CI systems, documentation, etc.) and returns structured context that flows
/// through the standard compression and IR pipeline.
///
/// The existing GitLab provider implements this interface pattern.
/// Future plugins will register implementations dynamically via the provider
/// framework contract.
pub trait ContextProvider: Send + Sync {
    /// Unique identifier for this provider (e.g. "gitlab", "github", "jira").
    fn id(&self) -> &'static str;

    /// Human-readable display name.
    fn display_name(&self) -> &'static str;

    /// Returns the actions this provider supports (e.g. "issues", "mrs", "pipelines").
    fn supported_actions(&self) -> &[&str];

    /// Execute a provider action and return structured results.
    fn execute(&self, action: &str, params: &ProviderParams) -> Result<ProviderResult, String>;

    /// TTL for caching results from this provider (in seconds).
    fn cache_ttl_secs(&self) -> u64 {
        120
    }

    /// Whether this provider requires authentication.
    fn requires_auth(&self) -> bool {
        true
    }

    /// Check if the provider is configured and ready to serve requests.
    fn is_available(&self) -> bool;
}

/// Parameters passed to a provider action.
#[derive(Debug, Clone, Default)]
pub struct ProviderParams {
    pub project: Option<String>,
    pub state: Option<String>,
    pub limit: Option<usize>,
    pub query: Option<String>,
    pub id: Option<String>,
}

/// A context packet produced by a provider, ready for the IR pipeline.
#[derive(Debug, Clone)]
pub struct ContextPacket {
    pub provider_id: String,
    pub action: String,
    pub items: Vec<ProviderItem>,
    pub token_count_raw: usize,
    pub token_count_compressed: usize,
    pub cache_hit: bool,
}
