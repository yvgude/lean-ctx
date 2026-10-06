use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

mod shared_sessions;
pub use shared_sessions::{SharedSessionKey, SharedSessionStore};

mod context_bus;
pub mod observation;
pub use context_bus::{
    ConsistencyLevel, ContextBus, ContextEventKindV1, ContextEventV1, FilteredSubscription,
    TopicFilter,
};
pub use observation::{
    LocalObservationPayload, LocalObservationSource, LocalObservationV1,
    ObservationPersistenceError,
};

pub mod redaction;
pub use redaction::{RedactionLevel, redact_event_payload, redact_payload_value};

/// Wraps either a plain `broadcast::Receiver` or a `FilteredSubscription`
/// so the SSE route can handle both with the same code path.
pub enum SubscriptionKind {
    Unfiltered(tokio::sync::broadcast::Receiver<ContextEventV1>),
    Filtered(FilteredSubscription),
}

impl SubscriptionKind {
    pub async fn recv(
        &mut self,
    ) -> Result<ContextEventV1, tokio::sync::broadcast::error::RecvError> {
        match self {
            Self::Unfiltered(rx) => rx.recv().await,
            Self::Filtered(fs) => fs.recv_filtered().await,
        }
    }
}

mod metrics;
pub use metrics::{ContextOsMetrics, MetricsSnapshot};

/// Shared runtime backing Context OS features (shared sessions + event bus).
///
/// This is intentionally process-local: it enables multi-client coordination
/// for HTTP/daemon/team-server deployments (one process handling many clients).
#[derive(Clone)]
pub struct ContextOsRuntime {
    pub shared_sessions: Arc<SharedSessionStore>,
    pub bus: Arc<ContextBus>,
    pub metrics: Arc<ContextOsMetrics>,
}

impl Default for ContextOsRuntime {
    fn default() -> Self {
        Self::try_new().expect("open context-os runtime")
    }
}

impl ContextOsRuntime {
    fn try_new() -> Result<Self, ObservationPersistenceError> {
        #[cfg(test)]
        let path = crate::core::data_dir::test_sandbox_dir()
            .join("context-os")
            .join("context-os.db");
        #[cfg(not(test))]
        let path = crate::core::data_dir::lean_ctx_data_dir()
            .map_err(std::io::Error::other)?
            .join("context-os")
            .join("context-os.db");
        let bus = ContextBus::try_open_at(path)?;

        Ok(Self {
            shared_sessions: Arc::new(SharedSessionStore::new()),
            bus: Arc::new(bus),
            metrics: Arc::new(ContextOsMetrics::default()),
        })
    }

    pub fn new() -> Self {
        Self::default()
    }

    pub fn data_dir() -> Option<PathBuf> {
        crate::core::data_dir::lean_ctx_data_dir().ok()
    }
}

static RUNTIME: OnceLock<Arc<ContextOsRuntime>> = OnceLock::new();
static RUNTIME_INIT: Mutex<()> = Mutex::new(());

pub fn runtime() -> Arc<ContextOsRuntime> {
    try_runtime().expect("open context-os runtime")
}

/// Fallible access to the same runtime authority. Failed initialization is not
/// cached, so restoring the configured storage permits a subsequent retry.
pub fn try_runtime() -> Result<Arc<ContextOsRuntime>, ObservationPersistenceError> {
    if let Some(runtime) = RUNTIME.get() {
        return Ok(Arc::clone(runtime));
    }
    // Build only one SQLite-backed runtime at a time. Competing first callers
    // used to open and migrate the same database concurrently, which can make
    // a valid Windows startup fail with a transient sharing/lock error.
    let _initialization = RUNTIME_INIT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(runtime) = RUNTIME.get() {
        return Ok(Arc::clone(runtime));
    }
    let candidate = Arc::new(ContextOsRuntime::try_new()?);
    let _ = RUNTIME.set(candidate);
    RUNTIME.get().cloned().ok_or_else(|| {
        ObservationPersistenceError::InvalidRow("runtime initialization did not complete".into())
    })
}

/// Convenience: append an event to the global bus with metrics tracking.
pub fn emit_event(
    workspace_id: &str,
    channel_id: &str,
    kind: &ContextEventKindV1,
    actor: Option<&str>,
    payload: serde_json::Value,
) {
    let rt = runtime();
    if rt
        .bus
        .append(workspace_id, channel_id, kind, actor, payload)
        .is_some()
    {
        rt.metrics.record_event_appended();
        rt.metrics.record_event_broadcast();
        rt.metrics.record_workspace_active(workspace_id);
    }
}

/// Emit an event directed at specific agents only.
pub fn emit_directed_event(
    workspace_id: &str,
    channel_id: &str,
    kind: &ContextEventKindV1,
    actor: Option<&str>,
    payload: serde_json::Value,
    target_agents: Vec<String>,
) {
    let rt = runtime();
    if rt
        .bus
        .append_directed(
            workspace_id,
            channel_id,
            kind,
            actor,
            payload,
            target_agents,
        )
        .is_some()
    {
        rt.metrics.record_event_appended();
        rt.metrics.record_event_broadcast();
        rt.metrics.record_workspace_active(workspace_id);
    }
}

/// Classify a tool name into a secondary event kind (beyond ToolCallRecorded).
pub fn secondary_event_kind(tool: &str, action: Option<&str>) -> Option<ContextEventKindV1> {
    match tool {
        "ctx_session" => {
            let a = action.unwrap_or("");
            if matches!(
                a,
                "save"
                    | "set_task"
                    | "task"
                    | "checkpoint"
                    | "finding"
                    | "decision"
                    | "reset"
                    | "import"
                    | "export"
            ) {
                Some(ContextEventKindV1::SessionMutated)
            } else {
                None
            }
        }
        "ctx_handoff" | "ctx_workflow" | "ctx_share" => Some(ContextEventKindV1::SessionMutated),
        "ctx_knowledge" | "ctx_knowledge_relations" => {
            let a = action.unwrap_or("");
            if matches!(
                a,
                "remember"
                    | "relate"
                    | "unrelate"
                    | "feedback"
                    | "remove"
                    | "consolidate"
                    | "import"
            ) {
                Some(ContextEventKindV1::KnowledgeRemembered)
            } else {
                None
            }
        }
        "ctx_artifacts" => {
            let a = action.unwrap_or("");
            if matches!(a, "reindex" | "remove") {
                Some(ContextEventKindV1::ArtifactStored)
            } else {
                None
            }
        }
        "ctx_graph" => {
            let a = action.unwrap_or("");
            if matches!(
                a,
                "index-build"
                    | "index-build-full"
                    | "index-build-background"
                    | "index-build-full-background"
            ) {
                Some(ContextEventKindV1::GraphBuilt)
            } else {
                None
            }
        }
        "ctx_proof" | "ctx_verify" => {
            let a = action.unwrap_or("");
            if matches!(a, "generate" | "export" | "verify") {
                Some(ContextEventKindV1::ProofAdded)
            } else {
                None
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_runtime_bus_uses_stable_process_sandbox() {
        let _isolated = crate::core::data_dir::isolated_data_dir();
        let runtime = ContextOsRuntime::new();
        let expected = crate::core::data_dir::test_sandbox_dir()
            .join("context-os")
            .join("context-os.db");

        assert_eq!(runtime.bus.db_path(), expected);
    }
}
