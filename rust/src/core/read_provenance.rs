//! Tracks the provenance of the last native Read for each file path.
//!
//! When the redirect hook serves a compressed read, the provenance is recorded
//! here. The deny guard checks this before allowing a StrReplace: if the last
//! read was lossy and the snapshot validates, the edit may proceed; otherwise
//! it is blocked with a "re-read the file" hint.
//!
//! Design principles (from Agent 9 research, Phase A):
//! - Fail-open on Mutex errors (never block edits without reason)
//! - Complement (not replace) the existing marker check in deny.rs
//! - TTL-based expiry prevents stale provenance from blocking future edits

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

const MAX_ENTRIES: usize = 1000;
const TTL_SECS: u64 = 1800; // 30 minutes

static STORE: OnceLock<Mutex<ProvenanceStore>> = OnceLock::new();

#[derive(Debug, Clone)]
pub(crate) struct ReadProvenance {
    pub path: String,
    pub mode: String,
    pub was_lossy: bool,
    pub digest: Option<String>,
    pub timestamp: u64,
}

#[derive(Debug, Default)]
struct ProvenanceStore {
    entries: HashMap<String, ReadProvenance>,
}

impl ProvenanceStore {
    fn record(&mut self, path: &str, mode: &str, was_lossy: bool, digest: Option<String>) {
        let norm = normalize(path);
        let now = now_unix();
        self.entries.insert(
            norm.clone(),
            ReadProvenance {
                path: norm,
                mode: mode.to_string(),
                was_lossy,
                digest,
                timestamp: now,
            },
        );
        self.evict_if_needed();
    }

    fn last(&self, path: &str) -> Option<&ReadProvenance> {
        let norm = normalize(path);
        let now = now_unix();
        self.entries
            .get(&norm)
            .filter(|p| now.saturating_sub(p.timestamp) <= TTL_SECS)
    }

    fn clear_path(&mut self, path: &str) {
        self.entries.remove(&normalize(path));
    }

    fn evict_if_needed(&mut self) {
        let now = now_unix();
        self.entries
            .retain(|_, p| now.saturating_sub(p.timestamp) <= TTL_SECS);

        if self.entries.len() <= MAX_ENTRIES {
            return;
        }
        let mut items: Vec<(String, u64)> = self
            .entries
            .iter()
            .map(|(k, p)| (k.clone(), p.timestamp))
            .collect();
        items.sort_by_key(|(_, ts)| *ts);
        let drop_n = self.entries.len() - MAX_ENTRIES;
        for (key, _) in items.into_iter().take(drop_n) {
            self.entries.remove(&key);
        }
    }
}

fn global() -> &'static Mutex<ProvenanceStore> {
    STORE.get_or_init(|| Mutex::new(ProvenanceStore::default()))
}

fn normalize(path: &str) -> String {
    crate::core::pathutil::normalize_tool_path(path)
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

// --- Public API (process-global, fail-open) ---

/// Record the provenance of a native Read redirect.
pub(crate) fn record_read(path: &str, mode: &str, was_lossy: bool, digest: Option<String>) {
    if let Ok(mut s) = global().lock() {
        s.record(path, mode, was_lossy, digest);
    }
}

/// Get the most recent read provenance for a path.
pub(crate) fn last_read(path: &str) -> Option<ReadProvenance> {
    global().lock().ok().and_then(|s| s.last(path).cloned())
}

/// Was the last read for this path lossy (compressed)?
/// Returns `false` on Mutex error (fail-open).
pub(crate) fn was_last_read_lossy(path: &str) -> bool {
    global()
        .lock()
        .ok()
        .and_then(|s| s.last(path).map(|p| p.was_lossy))
        .unwrap_or(false)
}

/// Clear provenance for a path (after a full read restores clean state).
pub(crate) fn clear(path: &str) {
    if let Ok(mut s) = global().lock() {
        s.clear_path(path);
    }
}

/// Number of tracked paths (telemetry).
pub(crate) fn count() -> usize {
    global().lock().ok().map_or(0, |s| s.entries.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_and_retrieve() {
        let mut store = ProvenanceStore::default();
        store.record("/tmp/a.log", "map", true, Some("abc123".to_string()));
        let prov = store.last("/tmp/a.log").unwrap();
        assert!(prov.was_lossy);
        assert_eq!(prov.mode, "map");
        assert_eq!(prov.digest.as_deref(), Some("abc123"));
    }

    #[test]
    fn full_read_clears_lossy() {
        let mut store = ProvenanceStore::default();
        store.record("/tmp/b.log", "map", true, None);
        assert!(store.last("/tmp/b.log").unwrap().was_lossy);
        store.record("/tmp/b.log", "full", false, None);
        assert!(!store.last("/tmp/b.log").unwrap().was_lossy);
    }

    #[test]
    fn clear_removes_entry() {
        let mut store = ProvenanceStore::default();
        store.record("/tmp/c.log", "map", true, None);
        store.clear_path("/tmp/c.log");
        assert!(store.last("/tmp/c.log").is_none());
    }

    #[test]
    fn lru_eviction() {
        let mut store = ProvenanceStore::default();
        for i in 0..(MAX_ENTRIES + 50) {
            store.record(&format!("/tmp/{i}.log"), "map", true, None);
        }
        assert!(store.entries.len() <= MAX_ENTRIES);
    }

    #[test]
    fn expired_entries_not_returned() {
        let mut store = ProvenanceStore::default();
        store.record("/tmp/old.log", "map", true, None);
        if let Some(p) = store.entries.get_mut(&normalize("/tmp/old.log")) {
            p.timestamp = 1000;
        }
        assert!(store.last("/tmp/old.log").is_none());
    }
}
