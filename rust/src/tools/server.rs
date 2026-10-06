use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
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
    /// Effective CRP mode for the active profile — see [`CrpMode::resolve`].
    pub fn effective() -> Self {
        Self::resolve(
            crate::core::profiles::active_profile()
                .compression
                .crp_mode
                .as_deref(),
        )
    }

    /// Returns true if the mode is TDD (maximum compression).
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

/// IDE interaction mode communicated via `_meta.interactionMode` in MCP requests.
/// Controls which tools are advertised and callable — Plan mode restricts to
/// read-only tools (see `plan_mode_tools()`), Agent mode exposes the full set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum InteractionMode {
    Agent = 0,
    Plan = 1,
}

impl InteractionMode {
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Plan,
            _ => Self::Agent,
        }
    }

    /// Parse the string value from `_meta.interactionMode`.
    /// Accepts multiple conventions across IDEs (OpenCode, Cursor, Claude Code).
    pub fn from_meta_str(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "plan" | "readonly" | "read-only" => Some(Self::Plan),
            "agent" | "normal" | "edit" | "act" => Some(Self::Agent),
            _ => None,
        }
    }
}

/// Central MCP server state: cache, session, metrics, and autonomy runtime.
#[derive(Clone)]
pub struct LeanCtxServer {
    pub cache: SharedCache,
    pub session: Arc<RwLock<SessionState>>,
    pub tool_calls: Arc<RwLock<Vec<ToolCallRecord>>>,
    pub call_count: Arc<AtomicUsize>,
    /// Every tool call, counted unconditionally at the top of
    /// `call_tool_guarded`.
    ///
    /// `call_count` only advances inside `record_checkpoint`, which returns
    /// early whenever checkpointing is skipped — and `minimal_overhead`
    /// (default `true`) skips it for every tool. Anything scheduled off
    /// `call_count` therefore never runs in a default install. The daily
    /// telemetry flush was scheduled that way and consequently never sent.
    pub background_tick: Arc<AtomicUsize>,
    pub pro_trigger_check_count: Arc<AtomicUsize>,
    pub cache_ttl_secs: u64,
    pub last_call: Arc<RwLock<Instant>>,
    pub agent_id: Arc<RwLock<Option<String>>>,
    pub task_envelope: Arc<RwLock<Option<lean_ctx_protocol::TaskEnvelopeV1>>>,
    /// Installed by the trusted host before serving; never sourced from tool arguments.
    pub(crate) native_receipt_authority:
        Option<Arc<crate::core::execution_ledger::host::HostReceiptAuthority>>,
    pub(crate) presence_agent_id: Arc<RwLock<Option<String>>>,
    /// The role this session resolved at `initialize` (#1766). The fail-closed
    /// presence retry re-registers with it instead of the construction-time
    /// `context-engine` placeholder, which silently turned a `reviewer` (or a
    /// `coder`) into a context engine.
    pub(crate) presence_role: Arc<RwLock<Option<String>>>,
    pub client_name: Arc<RwLock<String>>,
    pub autonomy: Arc<crate::core::autonomy::AutonomyState>,
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
    /// Failed `roots/list` attempts (GH #694): transient failures re-arm
    /// `roots_resolved` until a small budget is exhausted.
    pub(crate) roots_list_attempts: Arc<std::sync::atomic::AtomicU32>,
    pub(crate) bm25_cache: Arc<std::sync::Mutex<Option<crate::core::bm25_cache::Bm25CacheEntry>>>,
    pub(crate) _eviction_target: Arc<crate::core::eviction_orchestrator::EvictionOrchestrator>,
    pub(crate) progress_sender: crate::server::progress::SharedProgressSender,
    pub(crate) last_tools_config_hash: Arc<std::sync::atomic::AtomicU64>,
    /// Current IDE interaction mode (Agent/Plan). Updated from `_meta.interactionMode`
    /// on each tool call; controls tool visibility and destructive-tool guards.
    pub(crate) interaction_mode: Arc<std::sync::atomic::AtomicU8>,
}

pub use crate::core::protocol::ToolCallRecord;

#[cfg(test)]
mod tests {
    use super::InteractionMode;

    #[test]
    fn from_u8_round_trips() {
        assert_eq!(InteractionMode::from_u8(0), InteractionMode::Agent);
        assert_eq!(InteractionMode::from_u8(1), InteractionMode::Plan);
        assert_eq!(InteractionMode::from_u8(42), InteractionMode::Agent);
    }

    #[test]
    fn from_meta_str_plan_variants() {
        for s in ["plan", "Plan", "PLAN", "readonly", "read-only"] {
            assert_eq!(
                InteractionMode::from_meta_str(s),
                Some(InteractionMode::Plan),
                "failed for: {s}"
            );
        }
    }

    #[test]
    fn from_meta_str_agent_variants() {
        for s in ["agent", "Agent", "normal", "edit", "act"] {
            assert_eq!(
                InteractionMode::from_meta_str(s),
                Some(InteractionMode::Agent),
                "failed for: {s}"
            );
        }
    }

    #[test]
    fn from_meta_str_unknown_returns_none() {
        assert_eq!(InteractionMode::from_meta_str("debug"), None);
        assert_eq!(InteractionMode::from_meta_str(""), None);
    }

    #[test]
    fn default_mode_is_agent() {
        let mode = std::sync::atomic::AtomicU8::new(InteractionMode::Agent as u8);
        assert_eq!(
            InteractionMode::from_u8(mode.load(std::sync::atomic::Ordering::Relaxed)),
            InteractionMode::Agent,
        );
    }
}
