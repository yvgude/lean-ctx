//! Persistent per-path bounce memory (#496).
//!
//! The in-process `BounceTracker` detects bounces (compressed read followed by
//! a full read of the same file within a short window) but forgets everything
//! on restart, and `should_force_full` only knows per-extension rates. This
//! store remembers which *specific files* keep bouncing across sessions so
//! `mode=auto` stops compressing them — compression is a net token loss for
//! a file the agent always re-reads in full.
//!
//! Storage: `~/.lean-ctx/path_mode_memory.json`, atomic write (tmp+rename),
//! loaded once per process, flushed periodically like the heatmap.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};

const STORE_FILE: &str = "path_mode_memory.json";
/// Entries without a bounce for this long are dropped on load — the codebase
/// or the agent's reading pattern has likely changed.
const DECAY_SECS: u64 = 30 * 24 * 3600;
/// Hard cap; oldest-bounce entries are evicted first.
const MAX_PATHS: usize = 500;
const FLUSH_EVERY: usize = 25;

static STORE: OnceLock<Mutex<PathModeMemory>> = OnceLock::new();
static RECORD_CALLS: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PathModeStats {
    pub bounce_count: u32,
    /// Reads observed since the path entered the store (first bounce).
    pub read_count: u32,
    pub last_bounce_unix: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PathModeMemory {
    pub paths: HashMap<String, PathModeStats>,
    #[serde(skip)]
    dirty: bool,
}

impl PathModeMemory {
    fn load_from_disk() -> Self {
        let Ok(raw) = std::fs::read_to_string(store_path()) else {
            return Self::default();
        };
        let mut store: Self = serde_json::from_str(&raw).unwrap_or_default();
        store.decay(now_unix());
        store
    }

    /// Drop entries whose last bounce is older than `DECAY_SECS`.
    fn decay(&mut self, now: u64) {
        let before = self.paths.len();
        self.paths
            .retain(|_, s| now.saturating_sub(s.last_bounce_unix) <= DECAY_SECS);
        if self.paths.len() != before {
            self.dirty = true;
        }
    }

    fn evict_to_cap(&mut self) {
        if self.paths.len() <= MAX_PATHS {
            return;
        }
        let mut items: Vec<(String, u64)> = self
            .paths
            .iter()
            .map(|(p, s)| (p.clone(), s.last_bounce_unix))
            .collect();
        items.sort_by_key(|(_, ts)| *ts);
        let drop_n = self.paths.len() - MAX_PATHS;
        for (path, _) in items.into_iter().take(drop_n) {
            self.paths.remove(&path);
        }
        self.dirty = true;
    }

    pub fn record_bounce(&mut self, norm_path: &str, now: u64) {
        let entry = self.paths.entry(norm_path.to_string()).or_default();
        entry.bounce_count = entry.bounce_count.saturating_add(1);
        // The bounce implies a read happened; count it so the majority rule
        // (`bounce_count * 2 >= read_count`) stays meaningful from day one.
        entry.read_count = entry.read_count.max(entry.bounce_count);
        entry.last_bounce_unix = now;
        self.dirty = true;
        self.evict_to_cap();
    }

    /// Count a read only for paths already being tracked (i.e. that bounced
    /// before). Tracking every read of every file would bloat the store for
    /// zero signal.
    pub fn record_read_if_tracked(&mut self, norm_path: &str) {
        if let Some(entry) = self.paths.get_mut(norm_path) {
            entry.read_count = entry.read_count.saturating_add(1);
            self.dirty = true;
        }
    }

    /// A path is force-full when it bounced at least twice and bounces make up
    /// the majority of its observed reads — compressing it keeps backfiring.
    #[must_use]
    pub fn should_force_full(&self, norm_path: &str) -> bool {
        self.paths.get(norm_path).is_some_and(|s| {
            s.bounce_count >= 2 && u64::from(s.bounce_count) * 2 >= u64::from(s.read_count)
        })
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

fn global() -> &'static Mutex<PathModeMemory> {
    STORE.get_or_init(|| Mutex::new(PathModeMemory::load_from_disk()))
}

/// Process-global: record a confirmed bounce for `path` (already normalized
/// by the caller — `BounceTracker` normalizes via `pathutil`).
pub fn record_bounce(norm_path: &str) {
    let Ok(mut store) = global().lock() else {
        return;
    };
    store.record_bounce(norm_path, now_unix());
    maybe_flush(&mut store);
}

/// Process-global: count a read for an already-tracked path.
pub fn record_read_if_tracked(norm_path: &str) {
    let Ok(mut store) = global().lock() else {
        return;
    };
    store.record_read_if_tracked(norm_path);
    maybe_flush(&mut store);
}

/// Process-global: should `mode=auto` resolve to `full` for this path?
#[must_use]
pub fn should_force_full(path: &str) -> bool {
    let norm = crate::core::pathutil::normalize_tool_path(path);
    global().lock().is_ok_and(|s| s.should_force_full(&norm))
}

pub fn flush() {
    if let Ok(store) = global().lock()
        && store.dirty
    {
        let _ = store.save();
    }
}

/// Dashboard summary: `(tracked_paths, forced_full_paths)`. Reads straight
/// from disk so a separate process (the dashboard) sees the same state the
/// MCP/CLI processes persisted (#505).
#[must_use]
pub fn disk_summary() -> (usize, usize) {
    let store = PathModeMemory::load_from_disk();
    let forced = store
        .paths
        .keys()
        .filter(|p| store.should_force_full(p))
        .count();
    (store.paths.len(), forced)
}

fn maybe_flush(store: &mut PathModeMemory) {
    let n = RECORD_CALLS.fetch_add(1, Ordering::Relaxed) + 1;
    if n.is_multiple_of(FLUSH_EVERY) && store.dirty && store.save().is_ok() {
        store.dirty = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn force_full_after_two_majority_bounces() {
        let mut m = PathModeMemory::default();
        m.record_bounce("a.yml", 1000);
        assert!(!m.should_force_full("a.yml"), "one bounce is not a pattern");
        m.record_bounce("a.yml", 1001);
        assert!(m.should_force_full("a.yml"));
    }

    #[test]
    fn many_clean_reads_outweigh_old_bounces() {
        let mut m = PathModeMemory::default();
        m.record_bounce("b.rs", 1000);
        m.record_bounce("b.rs", 1001);
        assert!(m.should_force_full("b.rs"));
        // 5 clean reads later the bounce majority is gone (2*2 < 7).
        for _ in 0..5 {
            m.record_read_if_tracked("b.rs");
        }
        assert!(!m.should_force_full("b.rs"));
    }

    #[test]
    fn decay_drops_stale_entries() {
        let mut m = PathModeMemory::default();
        m.record_bounce("old.ts", 1000);
        m.record_bounce("old.ts", 1001);
        m.record_bounce("fresh.ts", 5000);
        m.record_bounce("fresh.ts", 5001);
        m.decay(5001 + DECAY_SECS - 10);
        assert!(!m.paths.contains_key("old.ts"));
        assert!(m.paths.contains_key("fresh.ts"));
    }

    #[test]
    fn eviction_keeps_newest_bounces() {
        let mut m = PathModeMemory::default();
        for i in 0..(MAX_PATHS + 20) {
            m.record_bounce(&format!("f{i}.rs"), 1000 + i as u64);
        }
        assert_eq!(m.paths.len(), MAX_PATHS);
        assert!(!m.paths.contains_key("f0.rs"), "oldest evicted");
        let newest = format!("f{}.rs", MAX_PATHS + 19);
        assert!(m.paths.contains_key(&newest));
    }

    #[test]
    fn untracked_reads_are_ignored() {
        let mut m = PathModeMemory::default();
        m.record_read_if_tracked("never_bounced.rs");
        assert!(m.paths.is_empty());
    }

    #[test]
    fn roundtrip_serialization() {
        let mut m = PathModeMemory::default();
        m.record_bounce("x.rs", 42);
        let json = serde_json::to_string(&m).unwrap();
        let back: PathModeMemory = serde_json::from_str(&json).unwrap();
        assert_eq!(back.paths.get("x.rs").unwrap().bounce_count, 1);
        assert_eq!(back.paths.get("x.rs").unwrap().last_bounce_unix, 42);
    }
}
