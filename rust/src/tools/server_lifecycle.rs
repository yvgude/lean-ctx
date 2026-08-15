use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::time::Instant;
use tokio::sync::RwLock;

use crate::core::cache::SessionCache;
use crate::core::session::SessionState;

use super::server::{LeanCtxServer, SessionMode};
use super::startup::detect_startup_context;

const MAX_WARM_SESSIONS: usize = 8;

fn collect_warming_history(
    project_root: Option<&str>,
) -> Vec<crate::core::cache::warming::RecentFile> {
    let Some(project_root) = project_root else {
        return Vec::new();
    };

    let sessions = SessionState::list_sessions()
        .into_iter()
        .filter(|summary| summary.project_root.as_deref() == Some(project_root))
        .take(MAX_WARM_SESSIONS)
        .filter_map(|summary| SessionState::load_by_id(&summary.id))
        .collect::<Vec<_>>();
    crate::core::cache::warming::collect_recent_files(&sessions)
}

fn warm_cache_in_background(
    cache: Arc<RwLock<SessionCache>>,
    history: Vec<crate::core::cache::warming::RecentFile>,
) {
    if history.is_empty() {
        return;
    }

    if let Err(error) = std::thread::Builder::new()
        .name("lean-ctx-cache-warm".to_string())
        .spawn(move || {
            let mut cache = cache.blocking_write();
            crate::core::cache::warming::warm_cache(&mut cache, &history);
        })
    {
        tracing::debug!("lean-ctx: failed to start cache warming task: {error}");
    }
}

impl Default for LeanCtxServer {
    fn default() -> Self {
        Self::new()
    }
}

impl LeanCtxServer {
    /// Creates a new server with default settings, auto-detecting the project root.
    pub fn new() -> Self {
        Self::new_with_project_root(None)
    }

    /// Creates a new server rooted at the given project directory.
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
                // A personal MCP server owns one fresh, PID-qualified session.
                // Reusing a persisted project "latest" session lets concurrent
                // servers inject each other's state during initialize.
                let mut session = SessionState::new();
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

        // Rehydrate the persistent stub index (#955) so the first unchanged
        // re-read after this restart can collapse to the `[unchanged]` stub
        // instead of re-delivering the whole file — gated by conversation +
        // mtime/md5 so it can never serve a stale or cross-chat stub.
        crate::core::read_stub_index::load();

        let warming_history = collect_warming_history(startup.project_root.as_deref());
        let cache = Arc::new(RwLock::new(SessionCache::new()));
        let bm25_cache: Arc<std::sync::Mutex<Option<crate::core::bm25_cache::Bm25CacheEntry>>> =
            Arc::new(std::sync::Mutex::new(None));

        // Register every server-local cache with the single process-wide guardian.
        // The registry stores weak targets, so closed HTTP/MCP sessions are not retained.
        let eviction_target = std::sync::Arc::new(
            crate::core::eviction_orchestrator::EvictionOrchestrator::new(
                cache.clone(),
                bm25_cache.clone(),
            ),
        );
        crate::core::eviction_orchestrator::register(&eviction_target);
        crate::core::memory_guard::start_guard(std::sync::Arc::new(
            crate::core::eviction_orchestrator::on_memory_pressure,
        ));
        warm_cache_in_background(cache.clone(), warming_history);

        let presence_root = startup
            .project_root
            .as_deref()
            .or(startup.shell_cwd.as_deref())
            .unwrap_or(".");
        let presence_agent_id =
            match crate::core::agents::AgentRegistry::register_mcp_process(presence_root) {
                Ok(agent_id) => Some(agent_id),
                Err(error) => {
                    tracing::warn!("lean-ctx: failed to register MCP agent presence: {error}");
                    None
                }
            };

        Self {
            cache,
            session,
            tool_calls: Arc::new(RwLock::new(Vec::new())),
            call_count: Arc::new(AtomicUsize::new(0)),
            pro_trigger_check_count: Arc::new(AtomicUsize::new(0)),
            cache_ttl_secs: ttl,
            last_call: Arc::new(RwLock::new(Instant::now())),
            agent_id: Arc::new(RwLock::new(None)),
            task_envelope: Arc::new(RwLock::new(None)),
            presence_agent_id: Arc::new(RwLock::new(presence_agent_id)),
            client_name: Arc::new(RwLock::new(String::new())),
            autonomy: Arc::new(crate::core::autonomy::AutonomyState::new()),
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
            roots_list_attempts: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            bm25_cache,
            progress_sender: Arc::new(std::sync::Mutex::new(None)),
            _eviction_target: eviction_target,
            last_tools_config_hash: Arc::new(std::sync::atomic::AtomicU64::new(
                crate::server::tools_config_watch::current_hash(),
            )),
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
            let redelivered = cache.count_full_delivered();
            let count = cache.clear();
            crate::core::cache_telemetry::record_idle(redelivered as u64);
            // The persisted stub index outlives the warm-cache clear, so a
            // same-conversation re-read after idle still collapses to the stub
            // via the cold fallback (#955). Flush it now for durability.
            crate::core::read_stub_index::persist();
            if count > 0 {
                tracing::info!(
                    "Cache auto-cleared after {}s idle ({count} file(s), {redelivered} forced re-delivery)",
                    self.cache_ttl_secs
                );
            }
        }
        *self.last_call.write().await = Instant::now();
    }

    async fn record_shutdown_episode(&self) {
        let tool_calls: Vec<(String, u64)> = self
            .tool_calls
            .read()
            .await
            .iter()
            .map(|call| (call.tool.clone(), call.duration_ms))
            .collect();
        if tool_calls.is_empty() {
            return;
        }

        let session = self.session.read().await.clone();
        let Some(project_root) = session
            .project_root
            .clone()
            .or_else(|| self.startup_project_root.clone())
        else {
            return;
        };
        let agent_id = self.agent_id.read().await.clone();
        let agent_id = match agent_id {
            Some(agent_id) => Some(agent_id),
            None => self.presence_agent_id.read().await.clone(),
        };
        let policy = crate::core::config::Config::load()
            .memory_policy_effective()
            .unwrap_or_default();
        let project_hash = crate::core::project_hash::hash_project_root(&project_root);

        match crate::core::episodic_memory::record_session_episode(
            &project_hash,
            &session,
            &tool_calls,
            agent_id.as_deref(),
            &policy.episodic,
            true,
        ) {
            Ok(Some(id)) => tracing::info!("lean-ctx: recorded shutdown episode {id}"),
            Ok(None) => {}
            Err(error) => tracing::warn!("lean-ctx: failed to record shutdown episode: {error}"),
        }
    }

    /// Aggressive cleanup on connection drop: save session, consolidate knowledge, clear caches.
    pub async fn shutdown(&self) {
        self.record_shutdown_episode().await;
        crate::core::savings_tracker::persist_session_summary();
        if let Some(agent_id) = self.presence_agent_id.read().await.clone()
            && let Err(error) = crate::core::agents::AgentRegistry::finish_persistent(&agent_id)
        {
            tracing::warn!("lean-ctx: failed to finish MCP agent presence: {error}");
        }
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
        // Flush the persistent stub index (#955) so an unchanged re-read survives
        // this restart as a cheap stub instead of a full re-delivery.
        crate::core::read_stub_index::persist();
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
