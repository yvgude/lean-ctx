//! Resident graph-index cache (Phase 5 of the efficiency epic).
//!
//! `try_load_graph_index` used to deserialize the on-disk `ProjectIndex`
//! (read + zstd-decompress + serde parse) on *every* query that touches the
//! graph (symbol lookups, related hints, impact). This keeps the deserialized
//! index resident in RAM keyed by project root, invalidated by the on-disk
//! index file's mtime so a background rebuild is picked up immediately (no TTL
//! wait). Callers that need an owned value get a cheap in-memory clone instead
//! of a disk round-trip.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Instant, SystemTime};

/// Max distinct project roots kept resident. A daemon touching many roots/branches/
/// worktrees would otherwise retain every ProjectIndex (MBs each) forever. LRU-evict
/// beyond this; an evicted root pays one disk reload (read+zstd+serde) on its next
/// query — bounded and self-healing.
const MAX_ROOTS: usize = 8;

use crate::core::graph_index::ProjectIndex;

/// `(mtime, size)` fingerprint of the on-disk index file. Size pairs with mtime
/// to catch same-second rebuilds that coarse (1–2 s) filesystem mtime would
/// otherwise hide — cheap, no file read.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
struct Fingerprint {
    mtime: Option<SystemTime>,
    size: u64,
}

struct Entry {
    index: Arc<ProjectIndex>,
    fingerprint: Fingerprint,
    last_access: Instant,
}

static CACHE: OnceLock<Mutex<HashMap<String, Entry>>> = OnceLock::new();

fn cache() -> &'static Mutex<HashMap<String, Entry>> {
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// `(mtime, size)` of the persisted graph index file (zst preferred), if any.
fn index_fingerprint(project_root: &str) -> Fingerprint {
    let Some(dir) = ProjectIndex::index_dir(project_root) else {
        return Fingerprint::default();
    };
    for name in ["index.json.zst", "index.json"] {
        if let Ok(meta) = std::fs::metadata(dir.join(name)) {
            return Fingerprint {
                mtime: meta.modified().ok(),
                size: meta.len(),
            };
        }
    }
    Fingerprint::default()
}

/// Returns the resident `ProjectIndex` for `project_root`, loading from disk
/// only when absent or when the on-disk index file changed. `None` when no
/// non-empty index exists on disk.
pub fn get_cached(project_root: &str) -> Option<Arc<ProjectIndex>> {
    let fingerprint = index_fingerprint(project_root);

    {
        let mut map = cache()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(entry) = map.get_mut(project_root) {
            if entry.fingerprint == fingerprint {
                entry.last_access = Instant::now();
                return Some(Arc::clone(&entry.index));
            }
        }
    }

    let idx = ProjectIndex::load(project_root).filter(|i| !i.files.is_empty())?;
    let arc = Arc::new(idx);

    let mut map = cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // LRU-evict before inserting a *new* root so the cap holds. Re-inserting an
    // existing root (fingerprint changed) just overwrites and doesn't grow the map.
    if !map.contains_key(project_root) && map.len() >= MAX_ROOTS {
        if let Some(lru_key) = map
            .iter()
            .min_by_key(|(_, e)| e.last_access)
            .map(|(k, _)| k.clone())
        {
            map.remove(&lru_key);
        }
    }
    map.insert(
        project_root.to_string(),
        Entry {
            index: Arc::clone(&arc),
            fingerprint,
            last_access: Instant::now(),
        },
    );
    Some(arc)
}

/// Drops the cached graph index for a root (or all roots when `None`).
pub fn invalidate(project_root: Option<&str>) {
    let mut map = cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match project_root {
        Some(root) => {
            map.remove(root);
        }
        None => map.clear(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_none_without_index() {
        let tmp = tempfile::tempdir().unwrap();
        invalidate(None);
        assert!(get_cached(tmp.path().to_str().unwrap()).is_none());
    }
}
