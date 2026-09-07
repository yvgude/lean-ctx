//! BM25 index lifecycle: per-thread shared cache, load-or-refresh with the
//! cold-build budget, resident-cache storage.

use std::path::Path;

use crate::core::bm25_index::BM25Index;

std::thread_local! {
    static BM25_SHARED_CACHE: std::cell::RefCell<Option<crate::core::bm25_cache::SharedBm25Cache>> =
        const { std::cell::RefCell::new(None) };
}

/// Set the shared BM25 cache for the current thread (called from the registered handler).
pub fn set_thread_cache(cache: crate::core::bm25_cache::SharedBm25Cache) {
    BM25_SHARED_CACHE.with(|c| {
        *c.borrow_mut() = Some(cache);
    });
}

/// Clone the current thread's shared BM25 cache, if any. Lets composer tools
/// propagate the resident cache into a budgeted worker thread so a slow cold
/// build warms the *same* cache instead of being wasted work.
pub fn get_thread_cache() -> Option<crate::core::bm25_cache::SharedBm25Cache> {
    BM25_SHARED_CACHE.with(|c| c.borrow().clone())
}

/// Result of BM25 index loading — may indicate background build in progress.
pub(crate) enum Bm25LoadResult {
    Ready(std::sync::Arc<BM25Index>),
    Building,
}

pub(crate) fn load_or_refresh_bm25(root: &Path) -> Bm25LoadResult {
    let cached = BM25_SHARED_CACHE.with(|c| {
        let borrow = c.borrow();
        borrow
            .as_ref()
            .and_then(|cache| crate::core::bm25_cache::get_or_background(cache, root))
    });
    if let Some(idx) = cached {
        return Bm25LoadResult::Ready(idx);
    }

    let root_str = root.to_string_lossy().to_string();

    // #1724: the resident cache holds ONE root, so every call for a sibling
    // project (`ctx_compose(path=...)`) misses it and lands here. Loading the
    // persisted index unconditionally pinned that project's first, partial tree
    // forever — nothing on this path ever asked whether it was still current.
    // Check staleness (one stat per tracked directory, no ingest walk); a stale
    // index still answers this call, but it now also triggers the refresh that
    // makes the next call complete, and it is not pinned into the resident
    // cache so the rebuilt index is picked up as soon as it is persisted.
    if let Some(idx) = crate::core::index_orchestrator::try_load_bm25_index(&root_str) {
        if crate::core::bm25_index::bm25_index_looks_stale_fast(&idx, root) {
            tracing::debug!("[bm25_store: stale on-disk index for {root_str}; refreshing]");
            crate::core::index_orchestrator::ensure_all_background(&root_str);
            return Bm25LoadResult::Ready(std::sync::Arc::new(idx));
        }
        let idx = std::sync::Arc::new(idx);
        store_in_thread_cache(root, &idx);
        return Bm25LoadResult::Ready(idx);
    }

    if crate::core::index_orchestrator::is_building() {
        return Bm25LoadResult::Building;
    }

    // Cold path: kick off the background build (which persists the index to
    // disk) instead of doing an unbounded synchronous build in the MCP handler.
    // Wait briefly so small/medium repos still return Ready on the first call;
    // larger repos return Building and the agent retries against the warm cache
    // once the worker has persisted the index (#150).
    crate::core::index_orchestrator::ensure_all_background(&root_str);

    let deadline = std::time::Instant::now() + bm25_cold_build_budget();
    loop {
        if let Some(idx) = crate::core::index_orchestrator::try_load_bm25_index(&root_str) {
            let idx = std::sync::Arc::new(idx);
            store_in_thread_cache(root, &idx);
            return Bm25LoadResult::Ready(idx);
        }
        if std::time::Instant::now() >= deadline {
            return Bm25LoadResult::Building;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// Time budget for waiting on a cold BM25 build in the MCP handler before
/// returning `Building`. Overridable via `LEAN_CTX_BM25_COLD_BUDGET_MS`.
pub(crate) fn bm25_cold_build_budget() -> std::time::Duration {
    let ms = std::env::var("LEAN_CTX_BM25_COLD_BUDGET_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(60_000);
    std::time::Duration::from_millis(ms)
}

pub(crate) fn store_in_thread_cache(root: &Path, idx: &std::sync::Arc<BM25Index>) {
    BM25_SHARED_CACHE.with(|c| {
        let borrow = c.borrow();
        if let Some(cache) = borrow.as_ref() {
            let mut guard = cache
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            *guard = Some(crate::core::bm25_cache::Bm25CacheEntry {
                root: root.to_path_buf(),
                index: std::sync::Arc::clone(idx),
                loaded_at: std::time::Instant::now(),
                fingerprint: crate::core::bm25_cache::index_fingerprint(root),
            });
        }
    });
}

pub(crate) fn filtered_candidate_k(top_k: usize, filtered: bool) -> usize {
    if !filtered {
        return top_k;
    }
    let candidates = (top_k.max(10)).saturating_mul(10);
    candidates.clamp(50, 500)
}
