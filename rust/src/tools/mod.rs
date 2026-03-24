use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::RwLock;

use crate::core::cache::SessionCache;

pub mod ctx_read;
pub mod ctx_tree;
pub mod ctx_shell;
pub mod ctx_search;
pub mod ctx_compress;
pub mod ctx_benchmark;
pub mod ctx_metrics;
pub mod ctx_analyze;

const DEFAULT_CHECKPOINT_INTERVAL: usize = 10;

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

    #[allow(dead_code)]
    pub fn is_compact_or_tdd(&self) -> bool {
        matches!(self, Self::Compact | Self::Tdd)
    }
}

pub type SharedCache = Arc<RwLock<SessionCache>>;

#[derive(Clone)]
pub struct LeanCtxServer {
    pub cache: SharedCache,
    pub tool_calls: Arc<RwLock<Vec<ToolCallRecord>>>,
    pub call_count: Arc<AtomicUsize>,
    pub checkpoint_interval: usize,
    pub crp_mode: CrpMode,
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct ToolCallRecord {
    pub tool: String,
    pub original_tokens: usize,
    pub saved_tokens: usize,
    pub mode: Option<String>,
}

impl LeanCtxServer {
    pub fn new() -> Self {
        let interval = std::env::var("LEAN_CTX_CHECKPOINT_INTERVAL")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_CHECKPOINT_INTERVAL);

        let crp_mode = CrpMode::from_env();

        Self {
            cache: Arc::new(RwLock::new(SessionCache::new())),
            tool_calls: Arc::new(RwLock::new(Vec::new())),
            call_count: Arc::new(AtomicUsize::new(0)),
            checkpoint_interval: interval,
            crp_mode,
        }
    }

    pub async fn record_call(&self, tool: &str, original: usize, saved: usize, mode: Option<String>) {
        let mut calls = self.tool_calls.write().await;
        calls.push(ToolCallRecord {
            tool: tool.to_string(),
            original_tokens: original,
            saved_tokens: saved,
            mode,
        });

        let output_tokens = original.saturating_sub(saved);
        crate::core::stats::record(tool, original, output_tokens);
    }

    pub fn increment_and_check(&self) -> bool {
        let count = self.call_count.fetch_add(1, Ordering::Relaxed) + 1;
        self.checkpoint_interval > 0 && count % self.checkpoint_interval == 0
    }

    pub async fn auto_checkpoint(&self) -> Option<String> {
        let cache = self.cache.read().await;
        if cache.get_all_entries().is_empty() {
            return None;
        }
        let checkpoint = ctx_compress::handle(&cache, true, self.crp_mode);
        drop(cache);
        self.record_call("ctx_compress", 0, 0, Some("auto".to_string())).await;
        Some(checkpoint)
    }
}

pub fn create_server() -> LeanCtxServer {
    LeanCtxServer::new()
}
