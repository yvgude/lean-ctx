use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::time::Instant;
use tokio::sync::RwLock;

use crate::core::cache::SessionCache;
use crate::core::session::SessionState;

use super::server::{InteractionMode, LeanCtxServer, SessionMode};
use super::startup::detect_startup_context;

const MAX_WARM_SESSIONS: usize = 8;

/// How often a connection re-checks that its bus presence is still listed.
/// Shorter than the default worker lease, so a presence an older binary
/// retired is restored before the dashboard polls twice.
const PRESENCE_VERIFY_INTERVAL: std::time::Duration = std::time::Duration::from_mins(1);

/// Last successful presence verification per presence id. A process holds
/// one presence per connection, so this stays a handful of entries.
fn presence_verifications() -> &'static std::sync::Mutex<std::collections::HashMap<String, Instant>>
{
    static VERIFIED: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, Instant>>,
    > = std::sync::OnceLock::new();
    VERIFIED.get_or_init(Default::default)
}

fn presence_verification_due(agent_id: &str) -> bool {
    presence_verifications()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(agent_id)
        .is_none_or(|at| at.elapsed() >= PRESENCE_VERIFY_INTERVAL)
}

fn mark_presence_verified(agent_id: &str) {
    presence_verifications()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(agent_id.to_string(), Instant::now());
}

fn forget_presence_verification(agent_id: &str) {
    presence_verifications()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(agent_id);
}

fn collect_warming_history(
    project_root: Option<&str>,
) -> Vec<crate::core::cache::warming::RecentFile> {
    let Some(project_root) = project_root else {
        return Vec::new();
    };

    let sessions = SessionState::load_recent_for_project_root(project_root, MAX_WARM_SESSIONS);
    crate::core::cache::warming::collect_recent_files_in_project(&sessions, project_root)
}

/// GH #2006: collecting the history resolves every touched file of the recent
/// sessions inside the project root. On a network drive that is a round trip
/// per path, and it ran synchronously before the server logged or answered
/// `initialize` (95-150 s on SMB). It now runs on the warming thread, before
/// that thread takes the cache lock, so neither the handshake nor the first
/// tool call waits for it.
fn warm_cache_in_background(cache: Arc<RwLock<SessionCache>>, project_root: Option<String>) {
    if project_root.is_none() {
        return;
    }

    if let Err(error) = std::thread::Builder::new()
        .name("lean-ctx-cache-warm".to_string())
        .spawn(move || {
            let history = collect_warming_history(project_root.as_deref());
            if history.is_empty() {
                return;
            }
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

    /// Ensures this MCP connection owns a durable local bus presence before
    /// executing a tool. Construction registers eagerly; this is the
    /// fail-closed retry for a transient startup lock or registry failure.
    pub(crate) async fn ensure_session_presence(&self) -> Result<(), String> {
        if self.presence_is_current().await {
            return Ok(());
        }
        // Concurrent first calls (16 parallel reads on a fresh engine) each
        // registered their own agent and timed out on the registry file lock
        // on Windows. One registers; the rest wait and reuse its id.
        let _init = self.presence_init.lock().await;
        if self.presence_is_current().await {
            return Ok(());
        }

        let project_root = self.presence_root();
        // #1766: once `initialize` has resolved the session's role, the retry
        // must register with it — falling back to the `context-engine`
        // placeholder rewrote a `reviewer` presence.
        let requested_role = self.presence_role.read().await.clone();
        let registration = match requested_role.as_deref() {
            Some(role) => {
                let role_source = if crate::core::agents::declared_session_role().is_some() {
                    crate::core::agents::RoleSource::Declared
                } else {
                    crate::core::agents::RoleSource::Assumed
                };
                crate::core::agents::AgentRegistry::register_mcp_process_as(
                    project_root,
                    role,
                    role_source,
                    None,
                )
            }
            None => crate::core::agents::AgentRegistry::register_mcp_process(project_root, None),
        };
        let retired = self.presence_agent_id.read().await.clone();
        match registration {
            Ok(agent_id) => {
                if let Some(retired) = retired.as_deref()
                    && retired != agent_id
                {
                    tracing::info!("lean-ctx: MCP presence {retired} was retired; now {agent_id}");
                    forget_presence_verification(retired);
                }
                mark_presence_verified(&agent_id);
                *self.presence_agent_id.write().await = Some(agent_id);
                Ok(())
            }
            // Re-registering a retired presence is best effort: a transient
            // registry failure must not block a connection that already
            // passed the fail-closed gate once. Retry after the interval.
            Err(error) if retired.is_some() => {
                tracing::warn!("lean-ctx: failed to re-register retired MCP presence: {error}");
                if let Some(retired) = retired.as_deref() {
                    mark_presence_verified(retired);
                }
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    /// True while this connection holds a presence that is still listed.
    /// The registry is shared with other processes: an older binary finishes
    /// quiet presences after the worker lease, and finished history is
    /// pruned. A retired presence reports `false` so the caller re-registers
    /// instead of heartbeating a dead id forever (`/api/agents` went empty
    /// while five MCP servers were connected). Verification reads the
    /// registry at most once per `PRESENCE_VERIFY_INTERVAL` per presence.
    async fn presence_is_current(&self) -> bool {
        let Some(agent_id) = self.presence_agent_id.read().await.clone() else {
            return false;
        };
        if !presence_verification_due(&agent_id) {
            return true;
        }
        let id = agent_id.clone();
        let live = tokio::task::spawn_blocking(move || {
            crate::core::agents::AgentRegistry::transport_presence_is_live(&id)
        })
        .await
        // A failed check must not knock out a working presence.
        .unwrap_or(true);
        if live {
            mark_presence_verified(&agent_id);
        }
        live
    }

    fn presence_root(&self) -> &str {
        self.startup_project_root
            .as_deref()
            .or(self.startup_shell_cwd.as_deref())
            .unwrap_or(".")
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

        let mut startup = detect_startup_context(project_root, startup_cwd);
        // Launchd daemons commonly start in `/`. A broad root must never become
        // session state: it creates a cross-project mega-session and, before
        // the bounded project index, also made every later MCP launch scan it.
        if startup.project_root.as_deref().is_some_and(|root| {
            crate::core::pathutil::is_broad_or_unsafe_root(std::path::Path::new(root))
        }) {
            tracing::debug!("lean-ctx: ignoring broad startup project root");
            startup.project_root = None;
        }
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
                // Every client of a shared session works in its root, so it
                // must be an existing absolute directory (AP04-G5).
                let Some(root) = startup.project_root.as_ref().filter(|root| {
                    let root = std::path::Path::new(root);
                    root.is_absolute() && root.is_dir()
                }) else {
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
        warm_cache_in_background(cache.clone(), startup.project_root.clone());

        let presence_root = startup
            .project_root
            .as_deref()
            .or(startup.shell_cwd.as_deref())
            .unwrap_or(".");
        let presence_agent_id =
            match crate::core::agents::AgentRegistry::register_mcp_process(presence_root, None) {
                Ok(agent_id) => Some(agent_id),
                Err(error) => {
                    tracing::warn!("lean-ctx: failed to register MCP agent presence: {error}");
                    None
                }
            };

        Self {
            cache,
            session: crate::core::session::SessionSlot::new(session),
            tool_calls: Arc::new(RwLock::new(Vec::new())),
            call_count: Arc::new(AtomicUsize::new(0)),
            background_tick: Arc::new(AtomicUsize::new(0)),
            cache_ttl_secs: ttl,
            last_call: Arc::new(RwLock::new(Instant::now())),
            agent_id: Arc::new(RwLock::new(None)),
            task_envelope: Arc::new(RwLock::new(None)),
            native_receipt_authority: None,
            presence_agent_id: Arc::new(RwLock::new(presence_agent_id)),
            presence_init: Arc::new(tokio::sync::Mutex::new(())),
            presence_role: Arc::new(RwLock::new(None)),
            client_name: Arc::new(RwLock::new(String::new())),
            autonomy: Arc::new(crate::core::autonomy::AutonomyState::new()),
            loop_detector: Arc::new(RwLock::new(
                crate::core::loop_detection::LoopDetector::with_config(
                    &crate::core::config::Config::load().loop_detection,
                ),
            )),
            // ctx_workflow was removed (focus release): nothing can start or stop
            // a run, so a run left on disk by an older version must not gate
            // tools here. The gate code goes with the workflow store.
            workflow: Arc::new(RwLock::new(None)),
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
            shell_approvals: Arc::default(),
            has_client_roots: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            roots_resolved: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            roots_list_attempts: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            bm25_cache,
            progress_sender: Arc::new(std::sync::Mutex::new(None)),
            _eviction_target: eviction_target,
            last_tools_config_hash: Arc::new(std::sync::atomic::AtomicU64::new(
                crate::server::tools_config_watch::current_hash(),
            )),
            interaction_mode: Arc::new(std::sync::atomic::AtomicU8::new(
                InteractionMode::Agent as u8,
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
            self.clear_idle_cache().await;
        }
        *self.last_call.write().await = Instant::now();
    }

    /// Idle TTL of the read cache (`memory_cleanup` / `LEAN_CTX_CACHE_TTL`); 0 = off.
    pub fn idle_ttl(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.cache_ttl_secs)
    }

    async fn clear_idle_cache(&self) {
        tokio::spawn(crate::core::session::SessionState::save_shared_logged(
            self.session.instance(),
        ));
        let mut cache = self.cache.write().await;
        let count = cache.clear();
        // The persisted stub index outlives the warm-cache clear, so a
        // same-conversation re-read after idle still collapses to the stub
        // via the cold fallback (#955). Flush it now for durability.
        crate::core::read_stub_index::persist();
        if count > 0 {
            tracing::info!(
                "Cache auto-cleared after {}s idle ({count} file(s))",
                self.cache_ttl_secs
            );
        }
    }

    /// Release what an idle server holds without waiting for the next call:
    /// the read cache (as `check_idle_expiry` would), the resident indexes and
    /// the allocator's free pages. Everything reloads on demand. Restarts the
    /// TTL so the next call does not clear (and count) the cache a second time.
    pub async fn release_idle_memory(&self) {
        use crate::core::memory_guard;
        let before = (
            memory_guard::get_rss_bytes(),
            memory_guard::allocator_stats(),
        );
        self.clear_idle_cache().await;
        *self.last_call.write().await = Instant::now();
        let _ = tokio::task::spawn_blocking(|| {
            crate::core::eviction_orchestrator::on_memory_pressure(
                memory_guard::PressureLevel::Hard,
            );
            memory_guard::jemalloc_purge();
            memory_guard::release_system_heap();
        })
        .await;
        let after = (
            memory_guard::get_rss_bytes(),
            memory_guard::allocator_stats(),
        );
        let mb = |bytes: Option<u64>| bytes.map_or_else(|| "?".into(), |b| format!("{}", b >> 20));
        tracing::info!(
            "[mcp-idle] released: RSS {} → {} MB, heap {} → {} MB",
            mb(before.0),
            mb(after.0),
            mb(before.1.map(|s| s.0)),
            mb(after.1.map(|s| s.0)),
        );
    }

    async fn record_shutdown_episode(&self, until: Instant) {
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

        // Storage admission of the session's text: off the async executor and
        // within the shutdown's enrichment time.
        let recorded = within_shutdown("episode", until, move || {
            crate::core::episodic_memory::record_session_episode(
                &project_hash,
                &session,
                &tool_calls,
                agent_id.as_deref(),
                &policy.episodic,
                true,
            )
        })
        .await;
        match recorded {
            Some(Ok(Some(id))) => tracing::info!("lean-ctx: recorded shutdown episode {id}"),
            Some(Ok(None)) | None => {}
            Some(Err(error)) => {
                tracing::warn!("lean-ctx: failed to record shutdown episode: {error}");
            }
        }
    }

    /// Aggressive cleanup on connection drop: save session, consolidate knowledge, clear caches.
    ///
    /// Order and bounds: the host that closed the connection waits a bounded
    /// time and then kills the process. What must not be lost comes first —
    /// the session's last state — and what only enriches later sessions (the
    /// shutdown episode, the knowledge consolidation) runs afterwards within
    /// `SHUTDOWN_ENRICHMENT`. With a semantic detector bound each of the
    /// two re-admits the session's text through the model; run first and
    /// unbounded, they kept the server alive beyond the host's wait while
    /// the last state was still to be saved. The consolidation reads the
    /// SAVED session, so it also belongs after the save.
    pub async fn shutdown(&self) {
        let stage = Instant::now();
        let last_state_durable =
            crate::core::session::SessionState::save_shared_at_shutdown(self.session.instance())
                .await;
        let stage = shutdown_stage("session save", stage);
        if !last_state_durable {
            // The save's own warning says why. Its changes stay pending as
            // after any failed save, and nothing later in this process saves
            // them. What follows must not make that worse: the consolidation
            // imports from the SAVED session and advances its watermark, so
            // on a state that is not the last one it is not run.
            tracing::error!(
                "lean-ctx: the session's last state is NOT durable at shutdown; \
                 knowledge consolidation is skipped"
            );
        }
        crate::core::savings_tracker::persist_session_summary();
        if let Some(agent_id) = self.presence_agent_id.read().await.clone() {
            forget_presence_verification(&agent_id);
            if let Err(error) = crate::core::agents::AgentRegistry::finish_persistent(&agent_id) {
                tracing::warn!("lean-ctx: failed to finish MCP agent presence: {error}");
            }
        }
        let stage = shutdown_stage("summary and presence", stage);

        let enrichment_ends = Instant::now() + SHUTDOWN_ENRICHMENT;
        self.record_shutdown_episode(enrichment_ends).await;
        let stage = shutdown_stage("episode", stage);
        {
            let session = self.session.read().await;
            let has_insights = !session.findings.is_empty() || !session.decisions.is_empty();
            let root = session.project_root.clone();
            let hive_target = root.clone().map(|root| (root, session.id.clone()));
            drop(session);

            if last_state_durable
                && has_insights
                && let Some(root) = root
            {
                // Incremental, and only this server's own session — never the
                // project's latest saved one, which may be another agent's
                // (I07/S03). What is not consolidated now stays newer than the
                // session's watermark, which advances only after the knowledge
                // write: when this agent resumes the session explicitly
                // (`ctx_session` load with `session_id`, or a handoff), its next
                // consolidation (checkpoint, compaction or shutdown) imports
                // those items. A new server starts its own fresh session and
                // never takes this one up by itself; nothing consolidates at
                // start-up.
                let own = self.session.instance();
                within_shutdown("knowledge consolidation", enrichment_ends, move || {
                    crate::core::session::caller::scope(Some(own), || {
                        crate::tools::startup::auto_consolidate_knowledge(&root);
                    });
                })
                .await;
            }
            // Queue this session's learnings for Hive (local only, no network).
            if last_state_durable && let Some((root, session_id)) = hive_target {
                crate::core::hive::knowledge::draft::on_session_event(
                    &root,
                    &session_id,
                    crate::core::hive::knowledge::draft::Trigger::Shutdown,
                );
            }
        }
        // Give back this process's Hive task claims (each bounded; one
        // Hive cannot be reached for ends at its expiry there).
        within_shutdown("hive task claims", enrichment_ends, || {
            crate::core::hive::tasks::release_own_claims("the agent session ended")
        })
        .await;
        shutdown_stage("knowledge consolidation", stage);
        // Persist buffered stats (incl. CEP cache-hit/session counters) before
        // the process exits. Short bridge sessions — e.g. a phase-isolated
        // benchmark harness that spawns a fresh server per phase — may never
        // reach the 30s live-stats flush cadence, which left
        // `cep.sessions`/`total_cache_hits` at 0 in stats.json despite real
        // cache hits (#361).
        crate::core::stats::flush();
        // A dropped connection ends its read/edit sequence: book the bounces
        // still waiting for their edit window instead of losing them.
        crate::core::bounce_tracker::flush_pending();
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

/// Time the shutdown gives to what only enriches later sessions, AFTER the
/// session's last state is durable. An engineering bound, not a contract of
/// any host: hosts wait differently long before they kill a server.
const SHUTDOWN_ENRICHMENT: std::time::Duration = std::time::Duration::from_secs(8);

/// Run blocking shutdown work off the async executor until `until` at the
/// latest. `None`: it was not started (no time left) or did not finish in
/// time; the log names the stage.
///
/// The bound ends the WAIT, not the work: a blocking task cannot be stopped.
/// Work that did not finish keeps running until the process ends (or to its
/// end, when the server object lives on). Both stages write their store in
/// one atomic replace at the end (`episodic_memory::mutate_locked_admitted`,
/// `knowledge::persist::write_json_atomic`), so a process that ends first
/// leaves the store as it was — this session's episode is then missing, and
/// the consolidation's items stay ahead of the session's watermark for the
/// next consolidation of the continued session.
async fn within_shutdown<T: Send + 'static>(
    stage: &'static str,
    until: Instant,
    work: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    let left = until.saturating_duration_since(Instant::now());
    if left.is_zero() {
        tracing::info!("lean-ctx: shutdown stage `{stage}` skipped: no time left for it");
        return None;
    }
    match tokio::time::timeout(left, tokio::task::spawn_blocking(work)).await {
        Ok(Ok(value)) => Some(value),
        Ok(Err(error)) => {
            tracing::warn!("lean-ctx: shutdown stage `{stage}` failed: {error}");
            None
        }
        Err(_) => {
            tracing::info!(
                "lean-ctx: shutdown stage `{stage}` not finished within its time and left behind"
            );
            None
        }
    }
}

/// Name a shutdown stage that took a second or more, and return the start of
/// the next one. A host waits a bounded time for the server to end after it
/// closed the connection and kills it afterwards; without these lines a
/// shutdown that outlives that wait leaves no trace of where the time went.
pub(crate) fn shutdown_stage(stage: &'static str, started: Instant) -> Instant {
    let milliseconds = started.elapsed().as_millis();
    if milliseconds >= 1000 {
        tracing::info!("lean-ctx: shutdown stage `{stage}` took {milliseconds} ms");
    } else {
        tracing::debug!("lean-ctx: shutdown stage `{stage}` took {milliseconds} ms");
    }
    Instant::now()
}

#[cfg(test)]
mod tests {
    use super::LeanCtxServer;

    #[tokio::test]
    async fn missing_presence_is_registered_before_first_tool() {
        let _isolated_data_dir = crate::core::data_dir::isolated_data_dir();
        let server = LeanCtxServer::new_shared_with_context("/workspace/lean-ctx", "ws", "ch");
        *server.presence_agent_id.write().await = None;

        server
            .ensure_session_presence()
            .await
            .expect("registration retry succeeds");
        let agent_id = server
            .presence_agent_id
            .read()
            .await
            .clone()
            .expect("presence id is retained");
        let registry = crate::core::agents::AgentRegistry::load().expect("registry persisted");
        assert!(
            registry
                .agents
                .iter()
                .any(|agent| agent.agent_id == agent_id)
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_presence_retries_register_once() {
        let _isolated_data_dir = crate::core::data_dir::isolated_data_dir();
        let server = LeanCtxServer::new_shared_with_context("/workspace/lean-ctx", "ws", "ch");
        *server.presence_agent_id.write().await = None;
        let before = crate::core::agents::AgentRegistry::load()
            .map(|registry| registry.agents.len())
            .unwrap_or(0);

        let calls: Vec<_> = (0..16)
            .map(|_| {
                let server = server.clone();
                tokio::spawn(async move { server.ensure_session_presence().await })
            })
            .collect();
        for call in calls {
            call.await
                .expect("task joined")
                .expect("presence retry succeeds");
        }

        let agent_id = server
            .presence_agent_id
            .read()
            .await
            .clone()
            .expect("presence id is retained");
        let registry = crate::core::agents::AgentRegistry::load().expect("registry persisted");
        assert!(
            registry.agents.len() <= before + 1,
            "no duplicate registrations"
        );
        assert!(
            registry
                .agents
                .iter()
                .any(|agent| agent.agent_id == agent_id)
        );
    }

    /// Audit bug 1: another process (an older binary's lease expiry) finished
    /// this live connection's presence; the server kept heartbeating the dead
    /// id and the dashboard listed no agents. The next tool call re-registers.
    #[tokio::test]
    async fn retired_presence_is_replaced_on_next_tool_call() {
        let _isolated_data_dir = crate::core::data_dir::isolated_data_dir();
        let server = LeanCtxServer::new_shared_with_context("/workspace/lean-ctx", "ws", "ch");
        server
            .ensure_session_presence()
            .await
            .expect("initial presence");
        let retired = server
            .presence_agent_id
            .read()
            .await
            .clone()
            .expect("presence id");
        crate::core::agents::AgentRegistry::mutate_locked(|registry| {
            registry.set_status(
                &retired,
                crate::core::agents::AgentStatus::Finished,
                Some("worker lease expired; register again before continuing"),
            )
        })
        .expect("registry writable")
        .1
        .expect("presence exists");
        super::forget_presence_verification(&retired);

        server
            .ensure_session_presence()
            .await
            .expect("re-registration succeeds");

        let current = server
            .presence_agent_id
            .read()
            .await
            .clone()
            .expect("presence id");
        assert_ne!(current, retired, "a retired presence id must be replaced");
        assert!(crate::core::agents::AgentRegistry::transport_presence_is_live(&current));
        let registry = crate::core::agents::AgentRegistry::load().expect("registry persisted");
        let entry = registry
            .agents
            .iter()
            .find(|agent| agent.agent_id == current)
            .expect("new presence persisted");
        assert!(entry.transport.is_some(), "re-registered as a transport");
    }

    /// Verification is throttled: a live presence is not re-read per call.
    #[test]
    fn presence_verification_is_throttled_per_presence() {
        let id = "throttle-test-presence";
        super::forget_presence_verification(id);
        assert!(super::presence_verification_due(id));
        super::mark_presence_verified(id);
        assert!(!super::presence_verification_due(id));
        super::forget_presence_verification(id);
        assert!(super::presence_verification_due(id));
    }

    #[tokio::test]
    async fn session_presence_failure_is_observable_to_tool_gate() {
        let _isolated_data_dir = crate::core::data_dir::isolated_data_dir();
        let data_dir = crate::core::data_dir::lean_ctx_data_dir().expect("data dir");
        std::fs::create_dir_all(&data_dir).expect("data dir exists");
        std::fs::write(data_dir.join("agents"), "not a directory").expect("poison agents path");

        let server = LeanCtxServer::new_shared_with_context("/workspace/lean-ctx", "ws", "ch");
        assert!(server.presence_agent_id.read().await.is_none());
        assert!(server.ensure_session_presence().await.is_err());
    }
}
