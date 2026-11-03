//! Persistent, conversation-scoped index of fully-delivered file reads (#955).
//!
//! The in-memory [`SessionCache`](crate::core::cache::SessionCache) is wiped on
//! every daemon restart (and emptied by the idle-TTL clear), so a re-read of an
//! unchanged file afterwards re-delivers the whole body. This index persists the
//! *minimal bookkeeping* needed to serve the `[unchanged]` stub — **never the
//! content** — so a re-read in the SAME conversation collapses to the cheap stub
//! even across a restart. Content is always re-read from disk; only delivery
//! bookkeeping persists, so tool-output determinism (#498) is untouched.
//!
//! ## Correctness
//! A cold stub (served when the live cache has no entry) is gated harder than a
//! warm one: it requires a *known, matching* conversation
//! (`crate::core::conversation::conversation_allows_cold_stub`) plus an
//! mtime+md5 match against the current file. A new chat after a restart cannot
//! prove the content is in its context, so it gets a cold full read — never a
//! misleading stub. A host compaction drops the whole index (the conversation's
//! context was summarised away), mirroring `SessionCache::reset_delivery_flags`.
//!
//! Disabled with `LEAN_CTX_STUB_PERSIST=0`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Max records retained (LRU by `updated_at`). Bounds disk + RAM (~200 KB).
const MAX_RECORDS: usize = 1024;

/// Serializable mtime: exact `SystemTime` round-trip via (secs, nanos) since the
/// Unix epoch, so the reconstructed value compares equal to a fresh `mtime()`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
struct SerMtime {
    secs: u64,
    nanos: u32,
}

impl SerMtime {
    fn from_system(t: SystemTime) -> Option<Self> {
        t.duration_since(UNIX_EPOCH).ok().map(|d| Self {
            secs: d.as_secs(),
            nanos: d.subsec_nanos(),
        })
    }

    fn to_system(self) -> SystemTime {
        UNIX_EPOCH + Duration::new(self.secs, self.nanos)
    }
}

/// One persisted delivery: everything `try_stub_hit_readonly` needs to emit the
/// `[unchanged]` stub, minus the content.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StubRecord {
    /// Canonical (normalized) path key.
    pub path: String,
    /// md5 of the delivered content (lossy-UTF-8 view, matching `SessionCache`).
    pub hash: String,
    mtime: Option<SerMtime>,
    pub line_count: usize,
    /// Display label (`F1`, `F2`, …) — reused so the stub matches the label the
    /// model already saw for this file.
    pub file_ref: String,
    /// Conversation that received the full content (always `Some` for a stored
    /// record — `None`-conversation deliveries are never recorded).
    pub delivered_conversation: Option<String>,
    /// Unix seconds of the last write, for LRU eviction.
    pub updated_at: u64,
}

impl StubRecord {
    pub fn new(
        path: String,
        hash: String,
        stored_mtime: Option<SystemTime>,
        line_count: usize,
        file_ref: String,
        delivered_conversation: Option<String>,
    ) -> Self {
        Self {
            path,
            hash,
            mtime: stored_mtime.and_then(SerMtime::from_system),
            line_count,
            file_ref,
            delivered_conversation,
            updated_at: now_unix(),
        }
    }

    /// The delivered file's mtime as a `SystemTime` for staleness verification.
    pub fn stored_mtime(&self) -> Option<SystemTime> {
        self.mtime.map(SerMtime::to_system)
    }
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// On-disk shape: a sorted record list (sorted for a stable file, though the
/// file is internal state and not a cacheable tool output).
#[derive(Debug, Default, Serialize, Deserialize)]
struct IndexFile {
    records: Vec<StubRecord>,
}

#[derive(Debug, Default)]
struct ReadStubIndex {
    records: HashMap<String, StubRecord>,
}

impl ReadStubIndex {
    fn upsert(&mut self, rec: StubRecord) {
        self.records.insert(rec.path.clone(), rec);
        self.enforce_cap();
    }

    fn enforce_cap(&mut self) {
        if self.records.len() <= MAX_RECORDS {
            return;
        }
        let mut by_age: Vec<(String, u64)> = self
            .records
            .iter()
            .map(|(k, v)| (k.clone(), v.updated_at))
            .collect();
        by_age.sort_by_key(|(_, age)| *age);
        let remove = self.records.len() - MAX_RECORDS;
        for (key, _) in by_age.into_iter().take(remove) {
            self.records.remove(&key);
        }
    }

    fn to_file(&self) -> IndexFile {
        let mut records: Vec<StubRecord> = self.records.values().cloned().collect();
        records.sort_by(|a, b| a.path.cmp(&b.path));
        IndexFile { records }
    }

    fn from_file(file: IndexFile) -> Self {
        let mut idx = Self {
            records: file
                .records
                .into_iter()
                .map(|r| (r.path.clone(), r))
                .collect(),
        };
        idx.enforce_cap();
        idx
    }
}

fn global() -> &'static RwLock<ReadStubIndex> {
    static G: OnceLock<RwLock<ReadStubIndex>> = OnceLock::new();
    G.get_or_init(|| RwLock::new(ReadStubIndex::default()))
}

fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        !matches!(
            std::env::var("LEAN_CTX_STUB_PERSIST")
                .ok()
                .as_deref()
                .map(str::trim),
            Some("0" | "false" | "off")
        )
    })
}

fn index_path_in(data_dir: &Path) -> Option<PathBuf> {
    let dir = data_dir.join("read_cache");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("stub_index.json"))
}

fn data_dir() -> Option<PathBuf> {
    crate::core::data_dir::lean_ctx_data_dir().ok()
}

// --- Pure file helpers (explicit path → unit-testable without globals) -------

fn load_file(path: &Path) -> ReadStubIndex {
    let Ok(content) = std::fs::read_to_string(path) else {
        return ReadStubIndex::default();
    };
    match serde_json::from_str::<IndexFile>(&content) {
        Ok(file) => ReadStubIndex::from_file(file),
        Err(_) => ReadStubIndex::default(),
    }
}

fn save_file(path: &Path, index: &ReadStubIndex) {
    let Ok(json) = serde_json::to_string(&index.to_file()) else {
        return;
    };
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, &json).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

// --- Global API (used by the server / read path) -----------------------------

/// Load the on-disk index into the process-global store (call once at startup).
pub fn load() {
    if !enabled() {
        return;
    }
    let Some(dir) = data_dir() else { return };
    load_from_dir(&dir);
}

/// Load from an explicit base dir (production: real data dir; tests: tempdir).
pub fn load_from_dir(data_dir: &Path) {
    let Some(path) = index_path_in(data_dir) else {
        return;
    };
    let loaded = load_file(&path);
    if let Ok(mut g) = global().write() {
        *g = loaded;
    }
}

/// Atomically persist the global index to the real data dir (call at save points).
pub fn persist() {
    if !enabled() {
        return;
    }
    let Some(dir) = data_dir() else { return };
    persist_to_dir(&dir);
}

/// Persist the global index to an explicit base dir.
pub fn persist_to_dir(data_dir: &Path) {
    let Some(path) = index_path_in(data_dir) else {
        return;
    };
    if let Ok(g) = global().read() {
        save_file(&path, &g);
    }
}

/// Write-through a full delivery into the global index (in-memory; flushed to
/// disk at the next save point). No-op for `None` conversations — they could
/// never serve a cold stub anyway.
pub fn record(rec: StubRecord) {
    if !enabled() || rec.delivered_conversation.is_none() {
        return;
    }
    if let Ok(mut g) = global().write() {
        g.upsert(rec);
    }
}

/// Look up a record for the cold stub fallback (returns a clone).
pub fn lookup(path: &str) -> Option<StubRecord> {
    if !enabled() {
        return None;
    }
    let key = crate::core::pathutil::normalize_tool_path(path);
    global().read().ok()?.records.get(&key).cloned()
}

/// Drop all records (host compaction: the conversation's context was summarised)
/// and persist the emptied index to the given base dir so a restart can't resurrect
/// pre-compaction stubs.
pub fn reset_in_dir(data_dir: &Path) {
    if let Ok(mut g) = global().write() {
        g.records.clear();
    }
    if enabled() {
        persist_to_dir(data_dir);
    }
}

#[cfg(any(test, feature = "test-support"))]
pub fn clear_for_test() {
    if let Ok(mut g) = global().write() {
        g.records.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    fn rec(path: &str, conv: Option<&str>, age: u64) -> StubRecord {
        let mut r = StubRecord::new(
            path.to_string(),
            "deadbeef".to_string(),
            Some(UNIX_EPOCH + Duration::new(1_000, 500)),
            42,
            "F1".to_string(),
            conv.map(String::from),
        );
        r.updated_at = age;
        r
    }

    #[test]
    fn ser_mtime_round_trips_exactly() {
        let t = SystemTime::now();
        let back = SerMtime::from_system(t).unwrap().to_system();
        assert_eq!(t, back, "reconstructed mtime must equal the original");
    }

    #[test]
    fn save_then_load_round_trips_records() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("read_cache").join("stub_index.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();

        let mut idx = ReadStubIndex::default();
        idx.upsert(rec("/a.rs", Some("conv-a"), 1));
        idx.upsert(rec("/b.rs", Some("conv-b"), 2));
        save_file(&path, &idx);

        let loaded = load_file(&path);
        assert_eq!(loaded.records.len(), 2);
        assert_eq!(
            loaded.records.get("/a.rs").unwrap().delivered_conversation,
            Some("conv-a".to_string())
        );
        assert_eq!(loaded.records.get("/b.rs").unwrap().line_count, 42);
    }

    #[test]
    fn enforce_cap_evicts_oldest_first() {
        let mut idx = ReadStubIndex::default();
        for i in 0..(MAX_RECORDS + 10) {
            // updated_at = i, so the lowest indices are the oldest.
            idx.upsert(rec(&format!("/f{i}.rs"), Some("c"), i as u64));
        }
        assert_eq!(idx.records.len(), MAX_RECORDS);
        // The 10 oldest (/f0../f9) must have been evicted.
        assert!(!idx.records.contains_key("/f0.rs"));
        assert!(!idx.records.contains_key("/f9.rs"));
        assert!(idx.records.contains_key("/f10.rs"));
    }

    #[test]
    fn load_missing_file_is_empty_not_error() {
        let dir = tempfile::tempdir().unwrap();
        let loaded = load_file(&dir.path().join("does-not-exist.json"));
        assert!(loaded.records.is_empty());
    }

    #[test]
    #[serial(stub_index)]
    fn record_skips_none_conversation() {
        clear_for_test();
        record(rec("/no-conv.rs", None, 1));
        assert!(
            lookup("/no-conv.rs").is_none(),
            "a None-conversation delivery must not be persisted (can't serve a cold stub)"
        );
        record(rec("/with-conv.rs", Some("conv-a"), 1));
        assert!(lookup("/with-conv.rs").is_some());
        clear_for_test();
    }
}
