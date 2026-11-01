use super::{ProviderItem, ProviderResult};

/// Plugin-ready trait for external context providers.
///
/// A ContextProvider connects LeanCTX to external data sources (issue trackers,
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

    /// Stable, credential-bound identity of a read-only resource. A name alone
    /// does not authorize reuse; providers without this contract remain unbound.
    fn reuse_binding(&self, _action: &str, _params: &ProviderParams) -> Option<String> {
        None
    }

    /// Fetch current evidence for one previously observed item. Never implement
    /// this with a cached result or a project-only permission probe.
    fn reacquire_item(
        &self,
        _action: &str,
        _params: &ProviderParams,
        _item_id: &str,
    ) -> Result<ProviderResult, String> {
        Err("provider does not support current item authorization".into())
    }

    /// TTL for caching results from this provider (in seconds).
    fn cache_ttl_secs(&self) -> u64 {
        120
    }

    /// Whether a result of `action` is consolidated into the stores (BM25,
    /// cross-source edges, knowledge). A provider whose actions include
    /// reads of something one listing already indexes says `false` for
    /// them, so a read does not add a second copy and its own hints.
    fn consolidates(&self, _action: &str) -> bool {
        true
    }

    /// What this connector actually guarantees (strategy package 2026-10-14,
    /// doc 17). The default claims nothing it cannot prove: it lists, and it
    /// cannot re-check a stored item, so its content stays personal-local and
    /// is withheld from governed reuse.
    fn descriptor(&self) -> ConnectorDescriptor {
        ConnectorDescriptor::list_only("token or none")
    }

    /// Whether this provider requires authentication.
    fn requires_auth(&self) -> bool {
        true
    }

    /// Check if the provider is configured and ready to serve requests.
    fn is_available(&self) -> bool;
}

/// How far a connector supports one capability. `Partial` always names its
/// limit in the descriptor's `notes`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityLevel {
    Supported,
    Partial,
    Unsupported,
}

/// The connector contract of doc 17: what a source connector can do, how it
/// fails and what it may write. Declared by the connector, rendered into the
/// capability matrix and consulted before governed or shared use.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ConnectorDescriptor {
    /// How the principal is bound (token, OAuth data source, none).
    pub principal_binding: &'static str,
    pub search: CapabilityLevel,
    pub list: CapabilityLevel,
    pub get: CapabilityLevel,
    pub delta: CapabilityLevel,
    /// Re-check, with the principal's own credential, that a stored item is
    /// still readable (`reacquire_item`).
    pub revalidate_acl: CapabilityLevel,
    /// Detect that a stored item changed since it was read.
    pub revalidate_revision: CapabilityLevel,
    pub delete_event: CapabilityLevel,
    /// Source text is data, never instructions, whatever its detectors say.
    pub content_trust: &'static str,
    /// Mutations of the source: none. Writes belong to the host application.
    pub write_surface: &'static str,
    /// Error, timeout, rate limit or lost access: what the gateway does.
    pub failure_policy: &'static str,
    pub notes: &'static str,
}

impl ConnectorDescriptor {
    pub(crate) const UNTRUSTED: &'static str = "untrusted data";
    pub(crate) const NO_WRITES: &'static str = "none";
    pub(crate) const FAIL_CLOSED: &'static str = "deny; no stale or cached answer";

    pub(crate) fn list_only(principal_binding: &'static str) -> Self {
        Self {
            principal_binding,
            search: CapabilityLevel::Unsupported,
            list: CapabilityLevel::Supported,
            get: CapabilityLevel::Unsupported,
            delta: CapabilityLevel::Unsupported,
            revalidate_acl: CapabilityLevel::Unsupported,
            revalidate_revision: CapabilityLevel::Unsupported,
            delete_event: CapabilityLevel::Unsupported,
            content_trust: Self::UNTRUSTED,
            write_surface: Self::NO_WRITES,
            failure_policy: Self::FAIL_CLOSED,
            notes: "no item re-check: personal-local use only, withheld from governed reuse",
        }
    }

    /// Governed or shared reuse of a stored item needs a current ACL check.
    pub fn allows_shared_reuse(&self) -> bool {
        self.revalidate_acl == CapabilityLevel::Supported
    }
}

/// Parameters passed to a provider action.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
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
