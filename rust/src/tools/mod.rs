use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;

use crate::core::cache::SessionCache;
use crate::core::session::SessionState;

pub mod autonomy;
pub mod ctx_agent;
pub mod ctx_analyze;
pub mod ctx_architecture;
pub mod ctx_benchmark;
pub mod ctx_callees;
pub mod ctx_callers;
pub mod ctx_compress;
pub mod ctx_compress_memory;
pub mod ctx_context;
pub mod ctx_cost;
pub mod ctx_dedup;
pub mod ctx_delta;
pub mod ctx_discover;
pub mod ctx_edit;
pub mod ctx_execute;
pub mod ctx_expand;
pub mod ctx_feedback;
pub mod ctx_fill;
pub mod ctx_gain;
pub mod ctx_graph;
pub mod ctx_graph_diagram;
pub mod ctx_handoff;
pub mod ctx_heatmap;
pub mod ctx_impact;
pub mod ctx_intent;
pub mod ctx_knowledge;
pub mod ctx_metrics;
pub mod ctx_multi_read;
pub mod ctx_outline;
pub mod ctx_overview;
pub mod ctx_prefetch;
pub mod ctx_preload;
pub mod ctx_read;
pub mod ctx_response;
pub mod ctx_routes;
pub mod ctx_search;
pub mod ctx_semantic_search;
pub mod ctx_session;
pub mod ctx_share;
pub mod ctx_shell;
pub mod ctx_smart_read;
pub mod ctx_symbol;
pub mod ctx_task;
pub mod ctx_tree;
pub mod ctx_workflow;
pub mod ctx_wrapped;

const DEFAULT_CACHE_TTL_SECS: u64 = 300;

struct CepComputedStats {
    cep_score: u32,
    cache_util: u32,
    mode_diversity: u32,
    compression_rate: u32,
    total_original: u64,
    total_compressed: u64,
    total_saved: u64,
    mode_counts: std::collections::HashMap<String, u64>,
    complexity: String,
    cache_hits: u64,
    total_reads: u64,
    tool_call_count: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CrpMode {
    Off,
    Compact,
    Tdd,
}

impl CrpMode {
    pub fn from_env() -> Self {
        match std::env::var("LEAN_CTX_CRP_MODE")
            .unwrap_or_default()
            .to_lowercase()
            .as_str()
        {
            "off" => Self::Off,
            "compact" => Self::Compact,
            _ => Self::Tdd,
        }
    }

    pub fn is_tdd(&self) -> bool {
        *self == Self::Tdd
    }
}

pub type SharedCache = Arc<RwLock<SessionCache>>;

#[derive(Clone)]
pub struct LeanCtxServer {
    pub cache: SharedCache,
    pub session: Arc<RwLock<SessionState>>,
    pub tool_calls: Arc<RwLock<Vec<ToolCallRecord>>>,
    pub call_count: Arc<AtomicUsize>,
    pub checkpoint_interval: usize,
    pub cache_ttl_secs: u64,
    pub last_call: Arc<RwLock<Instant>>,
    pub crp_mode: CrpMode,
    pub agent_id: Arc<RwLock<Option<String>>>,
    pub client_name: Arc<RwLock<String>>,
    pub autonomy: Arc<autonomy::AutonomyState>,
    pub loop_detector: Arc<RwLock<crate::core::loop_detection::LoopDetector>>,
    pub workflow: Arc<RwLock<Option<crate::core::workflow::WorkflowRun>>>,
    pub ledger: Arc<RwLock<crate::core::context_ledger::ContextLedger>>,
    pub pipeline_stats: Arc<RwLock<crate::core::pipeline::PipelineStats>>,
    startup_project_root: Option<String>,
    startup_shell_cwd: Option<String>,
}

#[derive(Clone, Debug)]
pub struct ToolCallRecord {
    pub tool: String,
    pub original_tokens: usize,
    pub saved_tokens: usize,
    pub mode: Option<String>,
    pub duration_ms: u64,
    pub timestamp: String,
}

impl Default for LeanCtxServer {
    fn default() -> Self {
        Self::new()
    }
}

impl LeanCtxServer {
    pub fn new() -> Self {
        Self::new_with_project_root(None)
    }

    pub fn new_with_project_root(project_root: Option<String>) -> Self {
        Self::new_with_startup(project_root, std::env::current_dir().ok())
    }

    fn new_with_startup(project_root: Option<String>, startup_cwd: Option<PathBuf>) -> Self {
        let config = crate::core::config::Config::load();

        let interval = std::env::var("LEAN_CTX_CHECKPOINT_INTERVAL")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(config.checkpoint_interval as usize);

        let ttl = std::env::var("LEAN_CTX_CACHE_TTL")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_CACHE_TTL_SECS);

        let crp_mode = CrpMode::from_env();

        let startup = detect_startup_context(project_root.as_deref(), startup_cwd.as_deref());
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

        Self {
            cache: Arc::new(RwLock::new(SessionCache::new())),
            session: Arc::new(RwLock::new(session)),
            tool_calls: Arc::new(RwLock::new(Vec::new())),
            call_count: Arc::new(AtomicUsize::new(0)),
            checkpoint_interval: interval,
            cache_ttl_secs: ttl,
            last_call: Arc::new(RwLock::new(Instant::now())),
            crp_mode,
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
                crate::core::context_ledger::ContextLedger::new(),
            )),
            pipeline_stats: Arc::new(RwLock::new(crate::core::pipeline::PipelineStats::new())),
            startup_project_root: startup.project_root,
            startup_shell_cwd: startup.shell_cwd,
        }
    }

    /// Resolves a (possibly relative) tool path against the session's project_root.
    /// Absolute paths and "." are returned as-is. Relative paths like "src/main.rs"
    /// are joined with project_root so tools work regardless of the server's cwd.
    pub async fn resolve_path(&self, path: &str) -> Result<String, String> {
        let normalized = crate::hooks::normalize_tool_path(path);
        if normalized.is_empty() || normalized == "." {
            return Ok(normalized);
        }
        let p = std::path::Path::new(&normalized);

        let (resolved, jail_root) = {
            let session = self.session.read().await;
            let jail_root = session
                .project_root
                .as_deref()
                .or(session.shell_cwd.as_deref())
                .unwrap_or(".")
                .to_string();

            let resolved = if p.is_absolute() || p.exists() {
                std::path::PathBuf::from(&normalized)
            } else if let Some(ref root) = session.project_root {
                let joined = std::path::Path::new(root).join(&normalized);
                if joined.exists() {
                    joined
                } else if let Some(ref cwd) = session.shell_cwd {
                    std::path::Path::new(cwd).join(&normalized)
                } else {
                    std::path::Path::new(&jail_root).join(&normalized)
                }
            } else if let Some(ref cwd) = session.shell_cwd {
                std::path::Path::new(cwd).join(&normalized)
            } else {
                std::path::Path::new(&jail_root).join(&normalized)
            };

            (resolved, jail_root)
        };

        let jail_root_path = std::path::Path::new(&jail_root);
        let jailed = match crate::core::pathjail::jail_path(&resolved, jail_root_path) {
            Ok(p) => p,
            Err(e) => {
                if p.is_absolute() {
                    if let Some(new_root) = maybe_derive_project_root_from_absolute(&resolved) {
                        let candidate_under_jail = resolved.starts_with(jail_root_path);
                        let allow_reroot = if !candidate_under_jail {
                            if let Some(ref trusted_root) = self.startup_project_root {
                                std::path::Path::new(trusted_root) == new_root.as_path()
                            } else {
                                !has_project_marker(jail_root_path)
                                    || is_suspicious_root(jail_root_path)
                            }
                        } else {
                            false
                        };

                        if allow_reroot {
                            let mut session = self.session.write().await;
                            let new_root_str = new_root.to_string_lossy().to_string();
                            session.project_root = Some(new_root_str.clone());
                            session.shell_cwd = self
                                .startup_shell_cwd
                                .as_ref()
                                .filter(|cwd| std::path::Path::new(cwd).starts_with(&new_root))
                                .cloned()
                                .or_else(|| Some(new_root_str.clone()));
                            let _ = session.save();

                            crate::core::pathjail::jail_path(&resolved, &new_root)?
                        } else {
                            return Err(e);
                        }
                    } else {
                        return Err(e);
                    }
                } else {
                    return Err(e);
                }
            }
        };

        Ok(crate::hooks::normalize_tool_path(
            &jailed.to_string_lossy().replace('\\', "/"),
        ))
    }

    pub async fn resolve_path_or_passthrough(&self, path: &str) -> String {
        self.resolve_path(path)
            .await
            .unwrap_or_else(|_| path.to_string())
    }

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

    pub async fn record_call(
        &self,
        tool: &str,
        original: usize,
        saved: usize,
        mode: Option<String>,
    ) {
        self.record_call_with_timing(tool, original, saved, mode, 0)
            .await;
    }

    pub async fn record_call_with_timing(
        &self,
        tool: &str,
        original: usize,
        saved: usize,
        mode: Option<String>,
        duration_ms: u64,
    ) {
        let ts = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
        let mut calls = self.tool_calls.write().await;
        calls.push(ToolCallRecord {
            tool: tool.to_string(),
            original_tokens: original,
            saved_tokens: saved,
            mode: mode.clone(),
            duration_ms,
            timestamp: ts.clone(),
        });

        if duration_ms > 0 {
            Self::append_tool_call_log(tool, duration_ms, original, saved, mode.as_deref(), &ts);
        }

        crate::core::events::emit_tool_call(
            tool,
            original as u64,
            saved as u64,
            mode.clone(),
            duration_ms,
            None,
        );

        let output_tokens = original.saturating_sub(saved);
        crate::core::stats::record(tool, original, output_tokens);

        let mut session = self.session.write().await;
        session.record_tool_call(saved as u64, original as u64);
        if tool == "ctx_shell" {
            session.record_command();
        }
        if session.should_save() {
            let _ = session.save();
        }
        drop(calls);
        drop(session);

        self.write_mcp_live_stats().await;
    }

    pub async fn is_prompt_cache_stale(&self) -> bool {
        let last = *self.last_call.read().await;
        last.elapsed().as_secs() > 3600
    }

    pub fn upgrade_mode_if_stale(mode: &str, stale: bool) -> &str {
        if !stale {
            return mode;
        }
        match mode {
            "full" => "full",
            "map" => "signatures",
            m => m,
        }
    }

    pub fn increment_and_check(&self) -> bool {
        let count = self.call_count.fetch_add(1, Ordering::Relaxed) + 1;
        self.checkpoint_interval > 0 && count.is_multiple_of(self.checkpoint_interval)
    }

    pub async fn auto_checkpoint(&self) -> Option<String> {
        let cache = self.cache.read().await;
        if cache.get_all_entries().is_empty() {
            return None;
        }
        let complexity = crate::core::adaptive::classify_from_context(&cache);
        let checkpoint = ctx_compress::handle(&cache, true, self.crp_mode);
        drop(cache);

        let mut session = self.session.write().await;
        let _ = session.save();
        let session_summary = session.format_compact();
        let has_insights = !session.findings.is_empty() || !session.decisions.is_empty();
        let project_root = session.project_root.clone();
        drop(session);

        if has_insights {
            if let Some(ref root) = project_root {
                let root = root.clone();
                std::thread::spawn(move || {
                    auto_consolidate_knowledge(&root);
                });
            }
        }

        let multi_agent_block = self.auto_multi_agent_checkpoint(&project_root).await;

        self.record_call("ctx_compress", 0, 0, Some("auto".to_string()))
            .await;

        self.record_cep_snapshot().await;

        Some(format!(
            "{checkpoint}\n\n--- SESSION STATE ---\n{session_summary}\n\n{}{multi_agent_block}",
            complexity.instruction_suffix()
        ))
    }

    async fn auto_multi_agent_checkpoint(&self, project_root: &Option<String>) -> String {
        let root = match project_root {
            Some(r) => r,
            None => return String::new(),
        };

        let registry = crate::core::agents::AgentRegistry::load_or_create();
        let active = registry.list_active(Some(root));
        if active.len() <= 1 {
            return String::new();
        }

        let agent_id = self.agent_id.read().await;
        let my_id = match agent_id.as_deref() {
            Some(id) => id.to_string(),
            None => return String::new(),
        };
        drop(agent_id);

        let cache = self.cache.read().await;
        let entries = cache.get_all_entries();
        if !entries.is_empty() {
            let mut by_access: Vec<_> = entries.iter().collect();
            by_access.sort_by_key(|x| std::cmp::Reverse(x.1.read_count));
            let top_paths: Vec<&str> = by_access
                .iter()
                .take(5)
                .map(|(key, _)| key.as_str())
                .collect();
            let paths_csv = top_paths.join(",");

            let _ = ctx_share::handle("push", Some(&my_id), None, Some(&paths_csv), None, &cache);
        }
        drop(cache);

        let pending_count = registry
            .scratchpad
            .iter()
            .filter(|e| !e.read_by.contains(&my_id) && e.from_agent != my_id)
            .count();

        let shared_dir = crate::core::data_dir::lean_ctx_data_dir()
            .unwrap_or_default()
            .join("agents")
            .join("shared");
        let shared_count = if shared_dir.exists() {
            std::fs::read_dir(&shared_dir)
                .map(|rd| rd.count())
                .unwrap_or(0)
        } else {
            0
        };

        let agent_names: Vec<String> = active
            .iter()
            .map(|a| {
                let role = a.role.as_deref().unwrap_or(&a.agent_type);
                format!("{role}({})", &a.agent_id[..8.min(a.agent_id.len())])
            })
            .collect();

        format!(
            "\n\n--- MULTI-AGENT SYNC ---\nAgents: {} | Pending msgs: {} | Shared contexts: {}\nAuto-shared top-5 cached files.\n--- END SYNC ---",
            agent_names.join(", "),
            pending_count,
            shared_count,
        )
    }

    pub fn append_tool_call_log(
        tool: &str,
        duration_ms: u64,
        original: usize,
        saved: usize,
        mode: Option<&str>,
        timestamp: &str,
    ) {
        const MAX_LOG_LINES: usize = 50;
        if let Ok(dir) = crate::core::data_dir::lean_ctx_data_dir() {
            let log_path = dir.join("tool-calls.log");
            let mode_str = mode.unwrap_or("-");
            let slow = if duration_ms > 5000 { " **SLOW**" } else { "" };
            let line = format!(
                "{timestamp}\t{tool}\t{duration_ms}ms\torig={original}\tsaved={saved}\tmode={mode_str}{slow}\n"
            );

            let mut lines: Vec<String> = std::fs::read_to_string(&log_path)
                .unwrap_or_default()
                .lines()
                .map(|l| l.to_string())
                .collect();

            lines.push(line.trim_end().to_string());
            if lines.len() > MAX_LOG_LINES {
                lines.drain(0..lines.len() - MAX_LOG_LINES);
            }

            let _ = std::fs::write(&log_path, lines.join("\n") + "\n");
        }
    }

    fn compute_cep_stats(
        calls: &[ToolCallRecord],
        stats: &crate::core::cache::CacheStats,
        complexity: &crate::core::adaptive::TaskComplexity,
    ) -> CepComputedStats {
        let total_original: u64 = calls.iter().map(|c| c.original_tokens as u64).sum();
        let total_saved: u64 = calls.iter().map(|c| c.saved_tokens as u64).sum();
        let total_compressed = total_original.saturating_sub(total_saved);
        let compression_rate = if total_original > 0 {
            total_saved as f64 / total_original as f64
        } else {
            0.0
        };

        let modes_used: std::collections::HashSet<&str> =
            calls.iter().filter_map(|c| c.mode.as_deref()).collect();
        let mode_diversity = (modes_used.len() as f64 / 10.0).min(1.0);
        let cache_util = stats.hit_rate() / 100.0;
        let cep_score = cache_util * 0.3 + mode_diversity * 0.2 + compression_rate * 0.5;

        let mut mode_counts: std::collections::HashMap<String, u64> =
            std::collections::HashMap::new();
        for call in calls {
            if let Some(ref mode) = call.mode {
                *mode_counts.entry(mode.clone()).or_insert(0) += 1;
            }
        }

        CepComputedStats {
            cep_score: (cep_score * 100.0).round() as u32,
            cache_util: (cache_util * 100.0).round() as u32,
            mode_diversity: (mode_diversity * 100.0).round() as u32,
            compression_rate: (compression_rate * 100.0).round() as u32,
            total_original,
            total_compressed,
            total_saved,
            mode_counts,
            complexity: format!("{:?}", complexity),
            cache_hits: stats.cache_hits,
            total_reads: stats.total_reads,
            tool_call_count: calls.len() as u64,
        }
    }

    async fn write_mcp_live_stats(&self) {
        let cache = self.cache.read().await;
        let calls = self.tool_calls.read().await;
        let stats = cache.get_stats();
        let complexity = crate::core::adaptive::classify_from_context(&cache);

        let cs = Self::compute_cep_stats(&calls, stats, &complexity);
        let started_at = calls
            .first()
            .map(|c| c.timestamp.clone())
            .unwrap_or_default();

        drop(cache);
        drop(calls);
        let live = serde_json::json!({
            "cep_score": cs.cep_score,
            "cache_utilization": cs.cache_util,
            "mode_diversity": cs.mode_diversity,
            "compression_rate": cs.compression_rate,
            "task_complexity": cs.complexity,
            "files_cached": cs.total_reads,
            "total_reads": cs.total_reads,
            "cache_hits": cs.cache_hits,
            "tokens_saved": cs.total_saved,
            "tokens_original": cs.total_original,
            "tool_calls": cs.tool_call_count,
            "started_at": started_at,
            "updated_at": chrono::Local::now().to_rfc3339(),
        });

        if let Ok(dir) = crate::core::data_dir::lean_ctx_data_dir() {
            let _ = std::fs::write(dir.join("mcp-live.json"), live.to_string());
        }
    }

    pub async fn record_cep_snapshot(&self) {
        let cache = self.cache.read().await;
        let calls = self.tool_calls.read().await;
        let stats = cache.get_stats();
        let complexity = crate::core::adaptive::classify_from_context(&cache);

        let cs = Self::compute_cep_stats(&calls, stats, &complexity);

        drop(cache);
        drop(calls);

        crate::core::stats::record_cep_session(
            cs.cep_score,
            cs.cache_hits,
            cs.total_reads,
            cs.total_original,
            cs.total_compressed,
            &cs.mode_counts,
            cs.tool_call_count,
            &cs.complexity,
        );
    }
}

#[derive(Clone, Debug, Default)]
struct StartupContext {
    project_root: Option<String>,
    shell_cwd: Option<String>,
}

pub fn create_server() -> LeanCtxServer {
    LeanCtxServer::new()
}

const PROJECT_ROOT_MARKERS: &[&str] = &[
    ".git",
    ".lean-ctx.toml",
    "Cargo.toml",
    "package.json",
    "go.mod",
    "pyproject.toml",
    "pom.xml",
    "build.gradle",
    "Makefile",
    ".planning",
];

fn has_project_marker(dir: &std::path::Path) -> bool {
    PROJECT_ROOT_MARKERS.iter().any(|m| dir.join(m).exists())
}

fn is_suspicious_root(dir: &std::path::Path) -> bool {
    let s = dir.to_string_lossy();
    s.contains("/.claude")
        || s.contains("/.codex")
        || s.contains("\\.claude")
        || s.contains("\\.codex")
}

fn canonicalize_path(path: &std::path::Path) -> String {
    crate::core::pathutil::safe_canonicalize_or_self(path)
        .to_string_lossy()
        .to_string()
}

fn detect_startup_context(
    explicit_project_root: Option<&str>,
    startup_cwd: Option<&std::path::Path>,
) -> StartupContext {
    let shell_cwd = startup_cwd.map(canonicalize_path);
    let project_root = explicit_project_root
        .map(|root| canonicalize_path(std::path::Path::new(root)))
        .or_else(|| {
            startup_cwd
                .and_then(maybe_derive_project_root_from_absolute)
                .map(|p| canonicalize_path(&p))
        });

    let shell_cwd = match (shell_cwd, project_root.as_ref()) {
        (Some(cwd), Some(root))
            if std::path::Path::new(&cwd).starts_with(std::path::Path::new(root)) =>
        {
            Some(cwd)
        }
        (Some(_), Some(root)) => Some(root.clone()),
        (Some(cwd), None) => Some(cwd),
        (None, Some(root)) => Some(root.clone()),
        (None, None) => None,
    };

    StartupContext {
        project_root,
        shell_cwd,
    }
}

fn maybe_derive_project_root_from_absolute(abs: &std::path::Path) -> Option<std::path::PathBuf> {
    let mut cur = if abs.is_dir() {
        abs.to_path_buf()
    } else {
        abs.parent()?.to_path_buf()
    };
    loop {
        if has_project_marker(&cur) {
            return Some(crate::core::pathutil::safe_canonicalize_or_self(&cur));
        }
        if !cur.pop() {
            break;
        }
    }
    None
}

fn auto_consolidate_knowledge(project_root: &str) {
    use crate::core::knowledge::ProjectKnowledge;
    use crate::core::session::SessionState;

    let session = match SessionState::load_latest() {
        Some(s) => s,
        None => return,
    };

    if session.findings.is_empty() && session.decisions.is_empty() {
        return;
    }

    let mut knowledge = ProjectKnowledge::load_or_create(project_root);

    for finding in &session.findings {
        let key = if let Some(ref file) = finding.file {
            if let Some(line) = finding.line {
                format!("{file}:{line}")
            } else {
                file.clone()
            }
        } else {
            "finding-auto".to_string()
        };
        knowledge.remember("finding", &key, &finding.summary, &session.id, 0.7);
    }

    for decision in &session.decisions {
        let key = decision
            .summary
            .chars()
            .take(50)
            .collect::<String>()
            .replace(' ', "-")
            .to_lowercase();
        knowledge.remember("decision", &key, &decision.summary, &session.id, 0.85);
    }

    let task_desc = session
        .task
        .as_ref()
        .map(|t| t.description.clone())
        .unwrap_or_default();

    let summary = format!(
        "Auto-consolidate session {}: {} — {} findings, {} decisions",
        session.id,
        task_desc,
        session.findings.len(),
        session.decisions.len()
    );
    knowledge.consolidate(&summary, vec![session.id.clone()]);
    let _ = knowledge.save();
}

#[cfg(test)]
mod resolve_path_tests {
    use super::*;

    fn create_git_root(path: &std::path::Path) -> String {
        std::fs::create_dir_all(path.join(".git")).unwrap();
        canonicalize_path(path)
    }

    #[tokio::test]
    async fn resolve_path_can_reroot_to_trusted_startup_root_when_session_root_is_stale() {
        let tmp = tempfile::tempdir().unwrap();
        let stale = tmp.path().join("stale");
        let real = tmp.path().join("real");
        std::fs::create_dir_all(&stale).unwrap();
        let real_root = create_git_root(&real);
        std::fs::write(real.join("a.txt"), "ok").unwrap();

        let server = LeanCtxServer::new_with_startup(None, Some(real.clone()));
        {
            let mut session = server.session.write().await;
            session.project_root = Some(stale.to_string_lossy().to_string());
            session.shell_cwd = Some(stale.to_string_lossy().to_string());
        }

        let out = server
            .resolve_path(&real.join("a.txt").to_string_lossy())
            .await
            .unwrap();

        assert!(out.ends_with("/a.txt"));

        let session = server.session.read().await;
        assert_eq!(session.project_root.as_deref(), Some(real_root.as_str()));
        assert_eq!(session.shell_cwd.as_deref(), Some(real_root.as_str()));
    }

    #[tokio::test]
    async fn resolve_path_rejects_absolute_path_outside_trusted_startup_root() {
        let tmp = tempfile::tempdir().unwrap();
        let stale = tmp.path().join("stale");
        let root = tmp.path().join("root");
        let other = tmp.path().join("other");
        std::fs::create_dir_all(&stale).unwrap();
        create_git_root(&root);
        let _other_value = create_git_root(&other);
        std::fs::write(other.join("b.txt"), "no").unwrap();

        let server = LeanCtxServer::new_with_startup(None, Some(root.clone()));
        {
            let mut session = server.session.write().await;
            session.project_root = Some(stale.to_string_lossy().to_string());
            session.shell_cwd = Some(stale.to_string_lossy().to_string());
        }

        let err = server
            .resolve_path(&other.join("b.txt").to_string_lossy())
            .await
            .unwrap_err();
        assert!(err.contains("path escapes project root"));

        let session = server.session.read().await;
        assert_eq!(
            session.project_root.as_deref(),
            Some(stale.to_string_lossy().as_ref())
        );
    }

    #[tokio::test]
    async fn startup_prefers_workspace_scoped_session_over_global_latest() {
        let _data = tempfile::tempdir().unwrap();
        let _tmp = tempfile::tempdir().unwrap();

        let (server, root_b) = {
            let _lock = crate::core::data_dir::test_env_lock();
            std::env::set_var("LEAN_CTX_DATA_DIR", _data.path());

            let repo_a = _tmp.path().join("repo-a");
            let repo_b = _tmp.path().join("repo-b");
            let root_a = create_git_root(&repo_a);
            let root_b = create_git_root(&repo_b);

            let mut session_b = SessionState::new();
            session_b.project_root = Some(root_b.clone());
            session_b.shell_cwd = Some(root_b.clone());
            session_b.set_task("repo-b task", None);
            session_b.save().unwrap();

            let mut session_a = SessionState::new();
            session_a.project_root = Some(root_a.clone());
            session_a.shell_cwd = Some(root_a.clone());
            session_a.set_task("repo-a latest task", None);
            session_a.save().unwrap();

            let server = LeanCtxServer::new_with_startup(None, Some(repo_b.clone()));
            std::env::remove_var("LEAN_CTX_DATA_DIR");
            (server, root_b)
        };

        let session = server.session.read().await;
        assert_eq!(session.project_root.as_deref(), Some(root_b.as_str()));
        assert_eq!(session.shell_cwd.as_deref(), Some(root_b.as_str()));
        assert_eq!(
            session.task.as_ref().map(|t| t.description.as_str()),
            Some("repo-b task")
        );
    }

    #[tokio::test]
    async fn startup_creates_fresh_session_for_new_workspace_and_preserves_subdir_cwd() {
        let _data = tempfile::tempdir().unwrap();
        let _tmp = tempfile::tempdir().unwrap();

        let (server, root_b, repo_b_src_value, old_id) = {
            let _lock = crate::core::data_dir::test_env_lock();
            std::env::set_var("LEAN_CTX_DATA_DIR", _data.path());

            let repo_a = _tmp.path().join("repo-a");
            let repo_b = _tmp.path().join("repo-b");
            let repo_b_src = repo_b.join("src");
            let root_a = create_git_root(&repo_a);
            let root_b = create_git_root(&repo_b);
            std::fs::create_dir_all(&repo_b_src).unwrap();
            let repo_b_src_value = canonicalize_path(&repo_b_src);

            let mut session_a = SessionState::new();
            session_a.project_root = Some(root_a.clone());
            session_a.shell_cwd = Some(root_a.clone());
            session_a.set_task("repo-a latest task", None);
            let old_id = session_a.id.clone();
            session_a.save().unwrap();

            let server = LeanCtxServer::new_with_startup(None, Some(repo_b_src.clone()));
            std::env::remove_var("LEAN_CTX_DATA_DIR");
            (server, root_b, repo_b_src_value, old_id)
        };

        let session = server.session.read().await;
        assert_eq!(session.project_root.as_deref(), Some(root_b.as_str()));
        assert_eq!(
            session.shell_cwd.as_deref(),
            Some(repo_b_src_value.as_str())
        );
        assert!(session.task.is_none());
        assert_ne!(session.id, old_id);
    }

    #[tokio::test]
    async fn resolve_path_does_not_auto_update_when_current_root_is_real_project() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        let other = tmp.path().join("other");
        let root_value = create_git_root(&root);
        create_git_root(&other);
        std::fs::write(other.join("b.txt"), "no").unwrap();

        let server = LeanCtxServer::new_with_project_root(Some(root.to_string_lossy().to_string()));

        let err = server
            .resolve_path(&other.join("b.txt").to_string_lossy())
            .await
            .unwrap_err();
        assert!(err.contains("path escapes project root"));

        let session = server.session.read().await;
        assert_eq!(session.project_root.as_deref(), Some(root_value.as_str()));
    }
}
