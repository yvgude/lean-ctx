//! BLAKE3-based edit snapshot store for safe native read compression.
//!
//! When a native Read is served in compressed (lossy) form, the original bytes
//! are stored here so a subsequent StrReplace can validate `old_string` against
//! the actual file content — not the compressed view the agent saw.
//!
//! Design principles (from Agent 9 research, Phase B):
//! - Hash canonical uncompressed bytes with BLAKE3
//! - Snapshot and lossy view are always separate objects
//! - Validate disk content against snapshot before allowing edits
//! - Fail-safe: missing/expired snapshot → deny edit (require fresh read)

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

const MAX_SNAPSHOTS: usize = 500;
const DEFAULT_TTL_SECS: u64 = 1800; // 30 minutes

static STORE: OnceLock<Mutex<EditSnapshotStore>> = OnceLock::new();

#[derive(Debug, Clone)]
pub(crate) struct EditSnapshot {
    pub digest: String,
    pub path: String,
    pub canonical_bytes: Vec<u8>,
    pub mtime: u64,
    pub created_at: u64,
}

#[derive(Debug, Default)]
pub(crate) struct EditSnapshotStore {
    by_digest: HashMap<String, EditSnapshot>,
    by_path: HashMap<String, String>, // path → most recent digest
}

impl EditSnapshotStore {
    fn store_inner(&mut self, path: &str, bytes: &[u8]) -> String {
        let digest = blake3::hash(bytes).to_hex().to_string();
        let now = now_unix();
        let mtime = file_mtime(path);

        let snapshot = EditSnapshot {
            digest: digest.clone(),
            path: path.to_string(),
            canonical_bytes: bytes.to_vec(),
            mtime,
            created_at: now,
        };

        self.by_digest.insert(digest.clone(), snapshot);
        self.by_path.insert(path.to_string(), digest.clone());
        self.evict_if_needed();
        digest
    }

    fn get_inner(&self, digest: &str) -> Option<&EditSnapshot> {
        self.by_digest.get(digest)
    }

    fn get_for_path_inner(&self, path: &str) -> Option<&EditSnapshot> {
        self.by_path.get(path).and_then(|d| self.by_digest.get(d))
    }

    /// Validates that the current file on disk matches the stored snapshot.
    fn validate_inner(&self, path: &str, digest: &str) -> bool {
        let Some(snapshot) = self.by_digest.get(digest) else {
            return false;
        };
        let current_mtime = file_mtime(path);
        if current_mtime != snapshot.mtime {
            return false;
        }
        match std::fs::read(path) {
            Ok(bytes) => blake3::hash(&bytes).to_hex().to_string() == digest,
            Err(_) => false,
        }
    }

    fn remove_path(&mut self, path: &str) {
        if let Some(digest) = self.by_path.remove(path) {
            self.by_digest.remove(&digest);
        }
    }

    fn evict_stale(&mut self, max_age_secs: u64) {
        let now = now_unix();
        let stale_digests: Vec<String> = self
            .by_digest
            .iter()
            .filter(|(_, s)| now.saturating_sub(s.created_at) > max_age_secs)
            .map(|(d, _)| d.clone())
            .collect();

        for digest in &stale_digests {
            if let Some(snap) = self.by_digest.remove(digest) {
                if self
                    .by_path
                    .get(&snap.path)
                    .is_some_and(|d| d.as_str() == digest)
                {
                    self.by_path.remove(&snap.path);
                }
            }
        }
    }

    fn evict_if_needed(&mut self) {
        if self.by_digest.len() <= MAX_SNAPSHOTS {
            return;
        }
        self.evict_stale(DEFAULT_TTL_SECS);
        if self.by_digest.len() <= MAX_SNAPSHOTS {
            return;
        }
        let mut items: Vec<(String, u64)> = self
            .by_digest
            .iter()
            .map(|(d, s)| (d.clone(), s.created_at))
            .collect();
        items.sort_by_key(|(_, ts)| *ts);
        let drop_n = self.by_digest.len() - MAX_SNAPSHOTS;
        for (digest, _) in items.into_iter().take(drop_n) {
            if let Some(snap) = self.by_digest.remove(&digest) {
                if self.by_path.get(&snap.path) == Some(&digest) {
                    self.by_path.remove(&snap.path);
                }
            }
        }
    }
}

fn global() -> &'static Mutex<EditSnapshotStore> {
    STORE.get_or_init(|| Mutex::new(EditSnapshotStore::default()))
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn file_mtime(path: &str) -> u64 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs())
}

// --- Public API (process-global, fail-safe) ---

/// Store original bytes for a file. Returns the BLAKE3 digest.
pub(crate) fn store(path: &str, bytes: &[u8]) -> Option<String> {
    global().lock().ok().map(|mut s| s.store_inner(path, bytes))
}

/// Get a snapshot by its digest.
pub(crate) fn get(digest: &str) -> Option<EditSnapshot> {
    global()
        .lock()
        .ok()
        .and_then(|s| s.get_inner(digest).cloned())
}

/// Get the most recent snapshot for a path.
pub(crate) fn get_for_path(path: &str) -> Option<EditSnapshot> {
    global()
        .lock()
        .ok()
        .and_then(|s| s.get_for_path_inner(path).cloned())
}

/// Validate that the current file matches the stored snapshot.
pub(crate) fn validate(path: &str, digest: &str) -> bool {
    global()
        .lock()
        .ok()
        .is_some_and(|s| s.validate_inner(path, digest))
}

/// Remove snapshot for a path (called after a full read clears lossy state).
pub(crate) fn remove(path: &str) {
    if let Ok(mut s) = global().lock() {
        s.remove_path(path);
    }
}

/// Check if `old_string` appears exactly once in the stored snapshot bytes.
pub(crate) fn old_string_matches_snapshot(path: &str, old_string: &str) -> bool {
    let Some(snapshot) = get_for_path(path) else {
        return false;
    };
    let Ok(content) = std::str::from_utf8(&snapshot.canonical_bytes) else {
        return false;
    };
    content.matches(old_string).count() == 1
}

/// Number of stored snapshots (for telemetry).
pub(crate) fn count() -> usize {
    global().lock().ok().map_or(0, |s| s.by_digest.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_and_retrieve() {
        let mut store = EditSnapshotStore::default();
        let digest = store.store_inner("/tmp/test.log", b"hello world");
        assert!(!digest.is_empty());
        let snap = store.get_inner(&digest).unwrap();
        assert_eq!(snap.canonical_bytes, b"hello world");
        assert_eq!(snap.path, "/tmp/test.log");
    }

    #[test]
    fn get_for_path_returns_latest() {
        let mut store = EditSnapshotStore::default();
        store.store_inner("/tmp/a.log", b"v1");
        store.store_inner("/tmp/a.log", b"v2");
        let snap = store.get_for_path_inner("/tmp/a.log").unwrap();
        assert_eq!(snap.canonical_bytes, b"v2");
    }

    #[test]
    fn remove_clears_both_maps() {
        let mut store = EditSnapshotStore::default();
        let digest = store.store_inner("/tmp/b.log", b"data");
        store.remove_path("/tmp/b.log");
        assert!(store.get_inner(&digest).is_none());
        assert!(store.get_for_path_inner("/tmp/b.log").is_none());
    }

    #[test]
    fn evict_stale_removes_old() {
        let mut store = EditSnapshotStore::default();
        store.store_inner("/tmp/old.log", b"old");
        if let Some(snap) = store.by_digest.values_mut().next() {
            snap.created_at = 1000;
        }
        store.store_inner("/tmp/new.log", b"new");
        store.evict_stale(100);
        assert!(store.get_for_path_inner("/tmp/old.log").is_none());
        assert!(store.get_for_path_inner("/tmp/new.log").is_some());
    }

    #[test]
    fn lru_eviction_at_cap() {
        let mut store = EditSnapshotStore::default();
        for i in 0..(MAX_SNAPSHOTS + 10) {
            store.store_inner(&format!("/tmp/{i}.log"), format!("data{i}").as_bytes());
        }
        assert!(store.by_digest.len() <= MAX_SNAPSHOTS);
    }

    #[test]
    fn digest_is_blake3() {
        let mut store = EditSnapshotStore::default();
        let digest = store.store_inner("/tmp/hash.log", b"test content");
        let expected = blake3::hash(b"test content").to_hex().to_string();
        assert_eq!(digest, expected);
    }

    #[test]
    fn old_string_match_in_snapshot() {
        let mut store = EditSnapshotStore::default();
        store.store_inner("/tmp/edit.log", b"fn main() {\n    println!(\"hello\");\n}");
        let snap = store.get_for_path_inner("/tmp/edit.log").unwrap();
        let content = std::str::from_utf8(&snap.canonical_bytes).unwrap();
        assert_eq!(content.matches("println!(\"hello\")").count(), 1);
        assert_eq!(content.matches("nonexistent").count(), 0);
    }
}
