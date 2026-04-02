use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;

use crate::core::cache::SessionCache;
use crate::core::session::SessionState;

pub mod ctx_agent;
pub mod ctx_analyze;
pub mod ctx_benchmark;
pub mod ctx_compress;
pub mod ctx_context;
pub mod ctx_dedup;
pub mod ctx_delta;
pub mod ctx_discover;
pub mod ctx_fill;
pub mod ctx_graph;
pub mod ctx_intent;
pub mod ctx_knowledge;
pub mod ctx_metrics;
pub mod ctx_multi_read;
pub mod ctx_overview;
pub mod ctx_preload;
pub mod ctx_read;
pub mod ctx_response;
pub mod ctx_search;
pub mod ctx_semantic_search;
pub mod ctx_session;
pub mod ctx_shell;
pub mod ctx_smart_read;
pub mod ctx_tree;
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
}

#[derive(Clone, Debug)]
pub struct ToolCallRecord {
    pub tool: String,
    pub original_tokens: usize,
    pub saved_tokens: usize,
    pub mode: Option<String>,
}

impl Default for LeanCtxServer {
    fn default() -> Self {
        Self::new()
    }
}

impl LeanCtxServer {
    pub fn new() -> Self {
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

        let session = SessionState::load_latest().unwrap_or_default();

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
        }
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
        let mut calls = self.tool_calls.write().await;
        calls.push(ToolCallRecord {
            tool: tool.to_string(),
            original_tokens: original,
            saved_tokens: saved,
            mode,
        });

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
            "full" => "aggressive",
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
            if let Some(root) = project_root {
                std::thread::spawn(move || {
                    auto_consolidate_knowledge(&root);
                });
            }
        }

        self.record_call("ctx_compress", 0, 0, Some("auto".to_string()))
            .await;

        self.record_cep_snapshot().await;

        Some(format!(
            "{checkpoint}\n\n--- SESSION STATE ---\n{session_summary}\n\n{}",
            complexity.instruction_suffix()
        ))
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
        let mode_diversity = (modes_used.len() as f64 / 6.0).min(1.0);
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
            "updated_at": chrono::Local::now().to_rfc3339(),
        });

        if let Some(dir) = dirs::home_dir().map(|h| h.join(".lean-ctx")) {
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

pub fn create_server() -> LeanCtxServer {
    LeanCtxServer::new()
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
