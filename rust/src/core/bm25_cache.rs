use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Instant, SystemTime};

use super::bm25_index::BM25Index;

const DEFAULT_TTL_SECS: u64 = 60;

/// Cheap content fingerprint of the persisted index file: `(mtime, size)`.
///
/// mtime alone is not enough — many filesystems only resolve mtime to 1–2 s, so
/// a background rebuild that lands in the same tick as the load would be missed.
/// Pairing it with the file size catches those same-second rewrites without the
/// cost of hashing a multi-MB index file on every per-query freshness check.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct IndexFingerprint {
    mtime: Option<SystemTime>,
    size: u64,
}

pub struct Bm25CacheEntry {
    pub root: PathBuf,
    pub index: Arc<BM25Index>,
    pub loaded_at: Instant,
    /// Fingerprint of the persisted index file when this entry was loaded.
    pub fingerprint: IndexFingerprint,
}

impl Bm25CacheEntry {
    pub fn is_fresh(&self) -> bool {
        if self.loaded_at.elapsed().as_secs() >= ttl_secs() {
            return false;
        }
        // Precise invalidation: if a background rebuild changed the index file
        // on disk, the resident copy is stale even within the TTL window.
        index_fingerprint(&self.root) == self.fingerprint
    }
}

/// `(mtime, size)` fingerprint of the persisted BM25 index file for `root`.
pub(crate) fn index_fingerprint(root: &Path) -> IndexFingerprint {
    match std::fs::metadata(BM25Index::index_file_path(root)) {
        Ok(m) => IndexFingerprint {
            mtime: m.modified().ok(),
            size: m.len(),
        },
        Err(_) => IndexFingerprint::default(),
    }
}

fn ttl_secs() -> u64 {
    std::env::var("LEAN_CTX_BM25_CACHE_TTL")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_TTL_SECS)
}

pub type SharedBm25Cache = std::sync::Arc<std::sync::Mutex<Option<Bm25CacheEntry>>>;

/// Get the BM25 index from cache if available and fresh, otherwise load/build,
/// cache it, and return. Uses Arc to avoid cloning the entire index.
pub fn get_or_load(cache: &SharedBm25Cache, root: &Path) -> Arc<BM25Index> {
    {
        let guard = cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(ref entry) = *guard
            && entry.root == root
            && entry.is_fresh()
        {
            return Arc::clone(&entry.index);
        }
    }

    let index = Arc::new(crate::core::index_orchestrator::load_or_build_bm25(root));

    let mut guard = cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *guard = Some(Bm25CacheEntry {
        root: root.to_path_buf(),
        index: Arc::clone(&index),
        loaded_at: Instant::now(),
        fingerprint: index_fingerprint(root),
    });

    index
}

/// Get index from cache (fresh or stale), triggering background rebuild if stale.
/// Returns None only if no cache entry exists at all.
pub fn get_or_background(cache: &SharedBm25Cache, root: &Path) -> Option<Arc<BM25Index>> {
    let guard = cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let entry = guard.as_ref()?;
    if entry.root != root {
        return None;
    }

    let idx = Arc::clone(&entry.index);

    if !entry.is_fresh() {
        let root_str = root.to_string_lossy().to_string();
        let cache_clone = cache.clone();
        let root_clone = root.to_path_buf();
        std::thread::spawn(move || {
            // Isolate panics (corrupt index file, FS race): a panic here must not
            // kill the worker silently — the stale index keeps serving and the
            // next call retries the refresh.
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let rebuilt = crate::core::index_orchestrator::load_or_build_bm25(&root_clone);
                let rebuilt_fp = index_fingerprint(&root_clone);
                let mut g = cache_clone
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                *g = Some(Bm25CacheEntry {
                    root: root_clone,
                    index: Arc::new(rebuilt),
                    loaded_at: Instant::now(),
                    fingerprint: rebuilt_fp,
                });
            }));
            if result.is_ok() {
                tracing::debug!("[bm25_cache: background refresh done for {root_str}]");
            } else {
                tracing::warn!(
                    "[bm25_cache: background refresh panicked for {root_str}; serving stale index]"
                );
            }
        });
    }

    Some(idx)
}

/// Drops the cached BM25 index, freeing its heap memory.
/// The index will be rebuilt from disk on the next search.
pub fn unload(cache: &SharedBm25Cache) {
    let mut guard = cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if guard.is_some() {
        *guard = None;
        tracing::info!("[bm25_cache] unloaded index to free memory");
    }
}

/// Returns the approximate heap memory used by the cached BM25 index, or 0.
pub fn memory_usage(cache: &SharedBm25Cache) -> usize {
    let guard = cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    guard.as_ref().map_or(0, |e| e.index.memory_usage_bytes())
}

/// Trims the RESIDENT cached index for `root` so each chunk keeps only its first
/// `keep_lines` lines of `content`, reclaiming the RAM held by full source
/// bodies once the embedding pass has consumed them.
///
/// Call this ONLY after embeddings for the current fingerprint are built and
/// persisted (see `ctx_semantic_search::ensure_embeddings`). The on-disk index
/// is untouched, so a reload restores full bodies; the resident `content_truncated`
/// flag guards a later embedding pass against re-embedding the trimmed bodies.
///
/// Truncation happens in place via `Arc::get_mut`, so it is a no-op (and costs
/// nothing) whenever another owner still holds the Arc — e.g. the search handler
/// has not yet dropped its clone, or a background refresh is in flight. The next
/// search call retries against the then-sole-owner cache entry. Returns the bytes
/// reclaimed (0 if skipped).
pub fn shrink_resident_to_snippet(
    cache: &SharedBm25Cache,
    root: &Path,
    keep_lines: usize,
) -> usize {
    let mut guard = cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(entry) = guard.as_mut() else {
        return 0;
    };
    if entry.root != root || entry.index.content_truncated {
        return 0;
    }
    // Only mutate when the cache is the sole owner — cloning a multi-MB index
    // just to trim it would defeat the purpose.
    let Some(index) = Arc::get_mut(&mut entry.index) else {
        tracing::debug!(
            "[bm25_cache] resident index still shared; skipping content shrink for now"
        );
        return 0;
    };
    let before = index.memory_usage_bytes();
    index.shrink_resident_content_to_snippet(keep_lines);
    before.saturating_sub(index.memory_usage_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn fresh_cache_returns_same_instance() {
        let cache: SharedBm25Cache = Arc::new(std::sync::Mutex::new(None));
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("main.rs"), "fn main() {}\n").unwrap();

        let idx1 = get_or_load(&cache, root);
        assert!(idx1.doc_count > 0);

        let idx2 = get_or_load(&cache, root);
        assert_eq!(idx1.doc_count, idx2.doc_count);
    }

    #[test]
    fn different_root_invalidates() {
        let cache: SharedBm25Cache = Arc::new(std::sync::Mutex::new(None));
        let tmp1 = tempfile::tempdir().unwrap();
        let tmp2 = tempfile::tempdir().unwrap();
        std::fs::write(tmp1.path().join("a.rs"), "fn a() {}\n").unwrap();
        std::fs::write(tmp2.path().join("b.rs"), "fn b() {}\n").unwrap();

        let _ = get_or_load(&cache, tmp1.path());
        let idx2 = get_or_load(&cache, tmp2.path());

        let guard = cache.lock().unwrap();
        let entry = guard.as_ref().unwrap();
        assert_eq!(entry.root, tmp2.path());
        assert_eq!(entry.index.doc_count, idx2.doc_count);
    }

    #[test]
    fn get_or_background_returns_none_on_empty() {
        let cache: SharedBm25Cache = Arc::new(std::sync::Mutex::new(None));
        let tmp = tempfile::tempdir().unwrap();
        assert!(get_or_background(&cache, tmp.path()).is_none());
    }

    #[test]
    fn fingerprint_default_when_index_file_absent() {
        let tmp = tempfile::tempdir().unwrap();
        // No persisted index file → default (None, 0) fingerprint.
        assert_eq!(index_fingerprint(tmp.path()), IndexFingerprint::default());
    }

    #[test]
    fn fingerprint_detects_size_change_under_equal_mtime() {
        // Two fingerprints with the same mtime but different size must differ,
        // proving size catches same-second rewrites that mtime alone misses.
        let mtime = Some(SystemTime::UNIX_EPOCH);
        let a = IndexFingerprint { mtime, size: 100 };
        let b = IndexFingerprint { mtime, size: 200 };
        assert_ne!(a, b);
        assert_eq!(a, IndexFingerprint { mtime, size: 100 });
    }
}
