//! Persistent per-extension signature-backend telemetry (GH #690 Phase 2).
//!
//! The tiering cut ("which of the ~27 static tree-sitter grammars are
//! actually used, and how often?") needs real usage history, but the only
//! existing signal — `signatures::signature_backend_stats()` — is a pair of
//! process-lifetime `AtomicU64`s with no language dimension: it resets on
//! every restart and cannot say *which* grammar earned its binary bytes.
//!
//! This store records, per file extension, how often the tree-sitter path vs
//! the regex fallback produced a signature set, persisted across sessions
//! (mirroring `path_mode_memory`'s store shape: load-once, atomic tmp+rename
//! writes, periodic flush via `tool_lifecycle::flush_all`).
//!
//! Storage: `<cache_dir>/grammar_usage.json`. Aggregate counters only — no
//! paths, no file names, nothing project-identifying — so the store is safe
//! to inspect and never a privacy concern.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};

const STORE_FILE: &str = "grammar_usage.json";
/// Extensions unseen for this long are dropped on load — the project mix
/// changed and stale rows would distort a tiering decision.
const DECAY_SECS: u64 = 180 * 24 * 3600;
/// Hard cap; least-recently-used extensions are evicted first. Real projects
/// see a few dozen extensions, so the cap only guards against pathological
/// callers feeding junk "extensions".
const MAX_EXTENSIONS: usize = 300;
const FLUSH_EVERY: usize = 50;

static STORE: OnceLock<Mutex<GrammarUsage>> = OnceLock::new();
static RECORD_CALLS: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ExtUsage {
    pub tree_sitter_hits: u64,
    pub regex_hits: u64,
    pub last_used_unix: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GrammarUsage {
    pub extensions: HashMap<String, ExtUsage>,
    #[serde(skip)]
    dirty: bool,
}

impl GrammarUsage {
    fn load_from_disk() -> Self {
        let Ok(raw) = std::fs::read_to_string(store_path()) else {
            return Self::default();
        };
        let mut store: Self = serde_json::from_str(&raw).unwrap_or_default();
        store.decay(now_unix());
        store
    }

    fn decay(&mut self, now: u64) {
        let before = self.extensions.len();
        self.extensions
            .retain(|_, u| now.saturating_sub(u.last_used_unix) <= DECAY_SECS);
        if self.extensions.len() != before {
            self.dirty = true;
        }
    }

    fn evict_to_cap(&mut self) {
        if self.extensions.len() <= MAX_EXTENSIONS {
            return;
        }
        let mut items: Vec<(String, u64)> = self
            .extensions
            .iter()
            .map(|(e, u)| (e.clone(), u.last_used_unix))
            .collect();
        items.sort_by_key(|(_, ts)| *ts);
        let drop_n = self.extensions.len() - MAX_EXTENSIONS;
        for (ext, _) in items.into_iter().take(drop_n) {
            self.extensions.remove(&ext);
        }
        self.dirty = true;
    }

    pub fn record(&mut self, ext: &str, tree_sitter: bool, now: u64) {
        let entry = self.extensions.entry(normalize_ext(ext)).or_default();
        if tree_sitter {
            entry.tree_sitter_hits = entry.tree_sitter_hits.saturating_add(1);
        } else {
            entry.regex_hits = entry.regex_hits.saturating_add(1);
        }
        entry.last_used_unix = now;
        self.dirty = true;
        self.evict_to_cap();
    }

    /// Extensions sorted by total hits (desc), then name (asc) for a stable
    /// order — the input to any tiering decision.
    pub fn ranked(&self) -> Vec<(String, ExtUsage)> {
        let mut rows: Vec<(String, ExtUsage)> = self
            .extensions
            .iter()
            .map(|(e, u)| (e.clone(), u.clone()))
            .collect();
        rows.sort_by(|(ea, ua), (eb, ub)| {
            let ta = ua.tree_sitter_hits + ua.regex_hits;
            let tb = ub.tree_sitter_hits + ub.regex_hits;
            tb.cmp(&ta).then_with(|| ea.cmp(eb))
        });
        rows
    }

    pub fn save(&self) -> std::io::Result<()> {
        let path = store_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string(self)?;
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, &path)
    }
}

/// Extensions arrive from callers in mixed shapes (`rs`, `.rs`, `RS`); the
/// store keys must not fragment across them.
fn normalize_ext(ext: &str) -> String {
    ext.trim_start_matches('.').to_ascii_lowercase()
}

fn store_path() -> PathBuf {
    crate::core::paths::cache_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(STORE_FILE)
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn global() -> &'static Mutex<GrammarUsage> {
    STORE.get_or_init(|| Mutex::new(GrammarUsage::load_from_disk()))
}

/// Process-global: record one signature extraction for `ext` via the given
/// backend. Called from the hot `extract_signatures` path — no I/O here;
/// disk writes happen every `FLUSH_EVERY` records or on [`flush`].
pub fn record(ext: &str, tree_sitter: bool) {
    if ext.is_empty() {
        return;
    }
    let Ok(mut store) = global().lock() else {
        return;
    };
    store.record(ext, tree_sitter, now_unix());
    let n = RECORD_CALLS.fetch_add(1, Ordering::Relaxed) + 1;
    if n.is_multiple_of(FLUSH_EVERY) && store.dirty && store.save().is_ok() {
        store.dirty = false;
    }
}

/// Flush pending counts (wired into `tool_lifecycle::flush_all`).
pub fn flush() {
    // Clears `dirty` once saved: flush runs after every tool call, and a
    // store left dirty was rewritten on each one.
    if let Ok(mut store) = global().lock()
        && store.dirty
        && store.save().is_ok()
    {
        store.dirty = false;
    }
}

/// Ranked per-extension usage, read straight from disk so a separate process
/// (dashboard, `doctor`) sees what the MCP/CLI processes persisted.
pub fn disk_ranked() -> Vec<(String, ExtUsage)> {
    GrammarUsage::load_from_disk().ranked()
}

/// Ranked per-extension usage from the live in-process store (includes
/// not-yet-flushed records) — what `ctx_metrics` shows.
pub fn live_ranked() -> Vec<(String, ExtUsage)> {
    global().lock().map(|s| s.ranked()).unwrap_or_default()
}

#[cfg(test)]
pub mod tests {
    use super::*;

    #[test]
    fn record_splits_backends_and_normalizes_ext() {
        let mut u = GrammarUsage::default();
        u.record(".RS", true, 100);
        u.record("rs", true, 101);
        u.record("rs", false, 102);
        assert_eq!(u.extensions.len(), 1, "'.RS' and 'rs' must share one row");
        let row = u.extensions.get("rs").unwrap();
        assert_eq!(row.tree_sitter_hits, 2);
        assert_eq!(row.regex_hits, 1);
        assert_eq!(row.last_used_unix, 102);
    }

    #[test]
    fn ranked_orders_by_total_then_name() {
        let mut u = GrammarUsage::default();
        u.record("py", true, 100);
        u.record("py", true, 101);
        u.record("rs", true, 102);
        u.record("go", true, 103); // same total as rs -> alphabetical
        let ranked = u.ranked();
        assert_eq!(ranked[0].0, "py");
        assert_eq!(ranked[1].0, "go");
        assert_eq!(ranked[2].0, "rs");
    }

    #[test]
    fn decay_drops_stale_extensions() {
        let mut u = GrammarUsage::default();
        u.record("old", true, 1000);
        u.record("fresh", true, 5000);
        u.decay(5000 + DECAY_SECS - 10);
        assert!(!u.extensions.contains_key("old"));
        assert!(u.extensions.contains_key("fresh"));
    }

    #[test]
    fn eviction_keeps_most_recently_used() {
        let mut u = GrammarUsage::default();
        for i in 0..(MAX_EXTENSIONS + 10) {
            u.record(&format!("e{i}"), true, 1000 + i as u64);
        }
        assert_eq!(u.extensions.len(), MAX_EXTENSIONS);
        assert!(!u.extensions.contains_key("e0"), "LRU evicted");
        let newest = format!("e{}", MAX_EXTENSIONS + 9);
        assert!(u.extensions.contains_key(&newest));
    }

    #[test]
    fn roundtrip_serialization() {
        let mut u = GrammarUsage::default();
        u.record("ts", true, 42);
        let json = serde_json::to_string(&u).unwrap();
        let back: GrammarUsage = serde_json::from_str(&json).unwrap();
        assert_eq!(back.extensions.get("ts").unwrap().tree_sitter_hits, 1);
        assert_eq!(back.extensions.get("ts").unwrap().last_used_unix, 42);
    }
}
