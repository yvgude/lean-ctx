use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

static REQUESTS_TOTAL: AtomicU64 = AtomicU64::new(0);
static TOKENS_SAVED_TOTAL: AtomicU64 = AtomicU64::new(0);
static BYTES_COMPRESSED: AtomicU64 = AtomicU64::new(0);

/// File holding the cross-process proxy totals. The proxy runs as its own
/// (long-lived) process, so the only way the `gain` CLI / dashboard can learn
/// how many provider turns actually carried the injected prefix is to read a
/// persisted counter. This is what makes the net-of-injection figure honest.
const PROXY_METRICS_FILE: &str = "proxy_metrics.json";

/// Persist every Nth request. The body is tiny but we still avoid a write on
/// every single request under high benchmark throughput; losing a few requests
/// of accuracy between flushes is immaterial for a meter.
const PERSIST_EVERY: u64 = 4;

pub fn record_request(tokens_saved: u64, bytes_compressed: u64) {
    let n = REQUESTS_TOTAL.fetch_add(1, Ordering::Relaxed) + 1;
    TOKENS_SAVED_TOTAL.fetch_add(tokens_saved, Ordering::Relaxed);
    BYTES_COMPRESSED.fetch_add(bytes_compressed, Ordering::Relaxed);
    if n == 1 || n.is_multiple_of(PERSIST_EVERY) {
        persist();
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProxyMetrics {
    pub requests_total: u64,
    pub tokens_saved_total: u64,
    pub bytes_compressed: u64,
}

pub fn snapshot() -> ProxyMetrics {
    ProxyMetrics {
        requests_total: REQUESTS_TOTAL.load(Ordering::Relaxed),
        tokens_saved_total: TOKENS_SAVED_TOTAL.load(Ordering::Relaxed),
        bytes_compressed: BYTES_COMPRESSED.load(Ordering::Relaxed),
    }
}

fn metrics_path() -> Option<std::path::PathBuf> {
    crate::core::data_dir::lean_ctx_data_dir()
        .ok()
        .map(|d| d.join(PROXY_METRICS_FILE))
}

/// Atomically write the current in-process totals to disk. The proxy owns these
/// totals for its lifetime, so a plain overwrite (not an additive merge) keeps
/// the file in lock-step with the live atomics.
pub fn persist() {
    let Some(path) = metrics_path() else {
        return;
    };
    let Ok(json) = serde_json::to_string(&snapshot()) else {
        return;
    };
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, json).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

/// Cross-process read of the persisted proxy totals, used by the `gain`
/// CLI/dashboard to reconcile savings against the real number of provider turns.
#[must_use]
pub fn load_persisted() -> Option<ProxyMetrics> {
    let path = metrics_path()?;
    let data = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&data).ok()
}
