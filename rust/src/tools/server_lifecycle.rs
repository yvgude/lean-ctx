use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::time::Instant;
use tokio::sync::RwLock;

use crate::core::cache::SessionCache;
use crate::core::session::SessionState;

use super::autonomy;
use super::server::{LeanCtxServer, SessionMode};
use super::startup::detect_startup_context;

impl Default for LeanCtxServer {
    fn default() -> Self {
        Self::new()
    }
}

impl LeanCtxServer {
    /// Creates a new server with default settings, auto-detecting the project root.
    #[must_use]
    pub fn new() -> Self {
        Self::new_with_project_root(None)
    }

    /// Creates a new server rooted at the given project directory.
    #[must_use]
    pub fn new_with_project_root(project_root: Option<&str>) -> Self {
        Self::new_with_startup(
            project_root,
            std::env::current_dir().ok().as_deref(),
            SessionMode::Personal,
            "default",
            "default",
        )
    }

    /// Creates a new server in Context OS shared mode for a specific workspace/channel.
    #[must_use]
    pub fn new_shared_with_context(
        project_root: &str,
        workspace_id: &str,
        channel_id: &str,
    ) -> Self {
        Self::new_with_startup(
            Some(project_root),
            std::env::current_dir().ok().as_deref(),
            SessionMode::Shared,
            workspace_id,
            channel_id,
        )
    }

    pub(crate) fn new_with_startup(
        project_root: Option<&str>,
        startup_cwd: Option<&Path>,
        session_mode: SessionMode,
        workspace_id: &str,
        channel_id: &str,
    ) -> Self {
        let ttl = std::env::var("LEAN_CTX_CACHE_TTL")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(|| {
                let cfg = crate::core::config::Config::load();
                crate::core::config::MemoryCleanup::effective(&cfg).idle_ttl_secs()
            });

        // Purge stale graph indices on startup to prevent serving outdated data
        crate::core::graph_index::ProjectIndex::purge_stale_indices();

        let startup = detect_startup_context(project_root, startup_cwd);
        let (session, context_os) = match session_mode {
            SessionMode::Personal => {
                let mut session = if let Some(ref root) = startup.project_root {
                    SessionState::load_latest_for_project_root(root).unwrap_or_default()
                } else {
                    SessionState::load_latest().unwrap_or_default()
                };
                if let Some(ref root) = startup.project_root {
                    session.project_root = Some(root.clone());
                }
                if let Some(ref cwd) = startup.shell_cwd {
                    session.shell_cwd = Some(cwd.clone());
                }
                (Arc::new(RwLock::new(session)), None)
            }
            SessionMode::Shared => {
                let Some(ref root) = startup.project_root else {
                    // Shared mode without a project root is not useful; fall back to personal.
                    return Self::new_with_startup(
                        project_root,
                        startup_cwd,
                        SessionMode::Personal,
                        workspace_id,
                        channel_id,
                    );
                };
                let rt = crate::core::context_os::runtime();
                let session = rt
                    .shared_sessions
                    .get_or_load(root, workspace_id, channel_id);
                rt.metrics.record_session_loaded();
                // Ensure shell_cwd is refreshed (best-effort).
                if let Some(ref cwd) = startup.shell_cwd
                    && let Ok(mut s) = session.try_write()
                {
                    s.shell_cwd = Some(cwd.clone());
                }
                (session, Some(rt))
            }
        };

        // Indices are NOT built eagerly here. A freshly connected agent that sits
        // idle — or only uses ctx_read/ctx_shell/ctx_tree — must pay zero indexing
        // cost. Heavy/search tools warm their indices lazily on first use via
        // `index_orchestrator::ensure_warm_for_tool`, driven from dispatch (#152).
        // An eager full graph + BM25 scan on every `new()` pegged a CPU core on
        // each server start; multiplied across multiple agents and stdio respawns
        // it was the root cause of the idle-high-CPU report (#453).

        let cache = Arc::new(RwLock::new(SessionCache::new()));

        // Start the RAM guardian with real eviction via EvictionOrchestrator.
        // Bridges memory_guard (RSS monitoring) → HomeostasisController (graduated actions).
        let orchestrator = std::sync::Arc::new(
            crate::core::eviction_orchestrator::EvictionOrchestrator::new(cache.clone()),
        );
        crate::core::memory_guard::start_guard(std::sync::Arc::new(move |level| {
            orchestrator.on_pressure(level);
        }));

        Self {
            cache,
            session,
            tool_calls: Arc::new(RwLock::new(Vec::new())),
            call_count: Arc::new(AtomicUsize::new(0)),
            cache_ttl_secs: ttl,
            last_call: Arc::new(RwLock::new(Instant::now())),
            agent_id: Arc::new(RwLock::new(None)),
            client_name: Arc::new(RwLock::new(String::new())),
            autonomy: Arc::new(autonomy::AutonomyState::new()),
            loop_detector: Arc::new(RwLock::new(
                crate::core::loop_detection::LoopDetector::with_config(
                    &crate::core::config::Config::load().loop_detection,
                ),
            )),
            workflow: Arc::new(RwLock::new(
                crate::core::workflow::load_active().ok().flatten(),
            )),
            ledger: Arc::new(RwLock::new(
                crate::core::context_ledger::ContextLedger::load(),
            )),
            pipeline_stats: Arc::new(RwLock::new(crate::core::pipeline::PipelineStats::new())),
            session_mode,
            workspace_id: if workspace_id.trim().is_empty() {
                "default".to_string()
            } else {
                workspace_id.trim().to_string()
            },
            channel_id: if channel_id.trim().is_empty() {
                "default".to_string()
            } else {
                channel_id.trim().to_string()
            },
            context_os,
            context_ir: Some(std::sync::Arc::new(tokio::sync::RwLock::new(
                crate::core::context_ir::ContextIrV1::load(),
            ))),
            registry: Some(std::sync::Arc::new(
                crate::server::registry::build_registry(),
            )),
            rules_stale_checked: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            rules_tip_shown: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            last_seen_event_id: Arc::new(std::sync::atomic::AtomicI64::new(0)),
            startup_project_root: startup.project_root,
            startup_shell_cwd: startup.shell_cwd,
            peer: Arc::new(tokio::sync::RwLock::new(None)),
            has_client_roots: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            roots_resolved: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            progress_sender: Arc::new(std::sync::Mutex::new(None)),
            stop_signal: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Clears the cache and saves the session if the TTL idle threshold has been exceeded.
    pub async fn check_idle_expiry(&self) {
        if self.cache_ttl_secs == 0 {
            return;
        }
        let last = *self.last_call.read().await;
        if last.elapsed().as_secs() >= self.cache_ttl_secs {
            {
                let mut session = self.session.write().await;
                let _ = session.save();
            }
            let mut cache = self.cache.write().await;
            let count = cache.clear();
            if count > 0 {
                tracing::info!(
                    "Cache auto-cleared after {}s idle ({count} file(s))",
                    self.cache_ttl_secs
                );
            }
        }
        *self.last_call.write().await = Instant::now();
    }

    /// Aggressive cleanup on connection drop: save session, consolidate knowledge, clear caches.
    pub async fn shutdown(&self) {
        {
            let session = self.session.read().await;
            let has_insights = !session.findings.is_empty() || !session.decisions.is_empty();
            let root = session.project_root.clone();
            drop(session);

            if has_insights && let Some(ref root) = root {
                crate::tools::startup::auto_consolidate_knowledge(root);
            }
        }
        {
            let mut session = self.session.write().await;
            let _ = session.save();
        }
        // Persist buffered stats (incl. CEP cache-hit/session counters) before
        // the process exits. Short bridge sessions — e.g. a phase-isolated
        // benchmark harness that spawns a fresh server per phase — may never
        // reach the 30s live-stats flush cadence, which left
        // `cep.sessions`/`total_cache_hits` at 0 in stats.json despite real
        // cache hits (#361).
        crate::core::stats::flush();
        {
            let mut cache = self.cache.write().await;
            let count = cache.clear();
            if count > 0 {
                tracing::info!("[shutdown] cleared {count} cached file(s)");
            }
        }
        crate::core::memory_guard::force_purge();
    }
}
