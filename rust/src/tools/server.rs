use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::time::Instant;
use tokio::sync::RwLock;

use crate::core::cache::SessionCache;
use crate::core::session::SessionState;
use rmcp::service::{Peer, RoleServer};

pub(super) struct CepComputedStats {
    pub(super) cep_score: u32,
    pub(super) cache_util: u32,
    pub(super) mode_diversity: u32,
    pub(super) compression_rate: u32,
    pub(super) total_original: u64,
    pub(super) total_compressed: u64,
    pub(super) total_saved: u64,
    pub(super) mode_counts: std::collections::HashMap<String, u64>,
    pub(super) complexity: String,
    pub(super) cache_hits: u64,
    pub(super) total_reads: u64,
    pub(super) tool_call_count: u64,
}

pub use crate::core::protocol::CrpMode;
// CrpMode is now defined in core::protocol to avoid reverse-dependency.
// Re-exported here for backward compatibility.

impl CrpMode {
    /// Effective CRP mode: explicit env var wins; otherwise derived from `CompressionLevel`.
    #[must_use]
    pub fn effective() -> Self {
        if let Ok(v) = std::env::var("LEAN_CTX_CRP_MODE")
            && !v.trim().is_empty()
        {
            return Self::parse(&v).unwrap_or(Self::Off);
        }
        let config = crate::core::config::Config::load();
        let level = crate::core::config::CompressionLevel::effective(&config);
        let (_, _, crp_str, _) = level.to_components();
        Self::parse(crp_str).unwrap_or(Self::Off)
    }

    /// Returns true if the mode is TDD (maximum compression).
    #[must_use]
    pub fn is_tdd(&self) -> bool {
        *self == Self::Tdd
    }
}

/// Thread-safe handle to the shared file content cache.
pub type SharedCache = Arc<RwLock<SessionCache>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionMode {
    /// Traditional single-client session persistence under `~/.lean-ctx/sessions/`.
    Personal,
    /// Context OS mode: shared sessions + event bus for multi-client HTTP/team-server.
    Shared,
}

/// Central MCP server state: cache, session, metrics, and autonomy runtime.
#[derive(Clone)]
pub struct LeanCtxServer {
    pub cache: SharedCache,
    pub session: Arc<RwLock<SessionState>>,
    pub tool_calls: Arc<RwLock<Vec<ToolCallRecord>>>,
    pub call_count: Arc<AtomicUsize>,
    pub cache_ttl_secs: u64,
    pub last_call: Arc<RwLock<Instant>>,
    pub agent_id: Arc<RwLock<Option<String>>>,
    pub client_name: Arc<RwLock<String>>,
    pub autonomy: Arc<super::autonomy::AutonomyState>,
    pub loop_detector: Arc<RwLock<crate::core::loop_detection::LoopDetector>>,
    pub workflow: Arc<RwLock<Option<crate::core::workflow::WorkflowRun>>>,
    pub ledger: Arc<RwLock<crate::core::context_ledger::ContextLedger>>,
    pub pipeline_stats: Arc<RwLock<crate::core::pipeline::PipelineStats>>,
    pub session_mode: SessionMode,
    pub workspace_id: String,
    pub channel_id: String,
    pub context_os: Option<Arc<crate::core::context_os::ContextOsRuntime>>,
    pub context_ir: Option<Arc<RwLock<crate::core::context_ir::ContextIrV1>>>,
    pub registry: Option<Arc<crate::server::registry::ToolRegistry>>,
    pub(crate) rules_stale_checked: Arc<std::sync::atomic::AtomicBool>,
    pub(crate) rules_tip_shown: Arc<std::sync::atomic::AtomicBool>,
    pub(crate) last_seen_event_id: Arc<std::sync::atomic::AtomicI64>,
    pub(crate) startup_project_root: Option<String>,
    pub(crate) startup_shell_cwd: Option<String>,
    pub(crate) peer: Arc<RwLock<Option<Peer<RoleServer>>>>,
    pub(crate) has_client_roots: Arc<std::sync::atomic::AtomicBool>,
    pub(crate) roots_resolved: Arc<std::sync::atomic::AtomicBool>,
    pub(crate) progress_sender: crate::server::progress::SharedProgressSender,
    pub stop_signal: Arc<AtomicBool>,
}

pub use crate::core::protocol::ToolCallRecord;
