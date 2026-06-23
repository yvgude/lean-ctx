//! Non-blocking, single-flight build coordinator for the dashboard (#452).
//!
//! Mirrors `graph_index::coordinator`: the dashboard's search routes must not
//! block the request thread on a full BM25 build (expensive on large repos).
//! A fresh on-disk index is returned immediately via the cheap sentinel
//! staleness check; otherwise a single background build is started and the
//! caller gets `Err(progress)` so the handler can answer `202 Accepted`.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use super::{BM25Index, bm25_index_looks_stale_fast};
use crate::core::index_pipeline::dump_engine::DumpEngine;
use crate::core::index_pipeline::pipeline::IndexPipeline;

/// Build progress, serialised to the dashboard as the `202` body.
#[derive(serde::Serialize)]
pub struct SearchIndexBuildProgress {
    pub status: &'static str,
    pub files_total: usize,
    pub files_done: usize,
}

/// One in-flight build at a time (single flight). The dashboard serves a single
/// project, matching the call-graph/graph-index coordinators.
static BUILDING: OnceLock<AtomicBool> = OnceLock::new();

fn building_flag() -> &'static AtomicBool {
    BUILDING.get_or_init(|| AtomicBool::new(false))
}

/// Returns a fresh index immediately, or starts a single background build and
/// returns `Err(progress)` so the caller can answer `202 Accepted`.
pub fn get_or_start_build(root: &Path) -> Result<Arc<BM25Index>, SearchIndexBuildProgress> {
    // Fast path: a fresh on-disk index loads cheaply (sentinel staleness check,
    // no full directory walk).
    if let Some(idx) = BM25Index::load(root)
        && !idx.chunks.is_empty()
        && !bm25_index_looks_stale_fast(&idx, root)
    {
        return Ok(Arc::new(idx));
    }

    let files_total = BM25Index::load(root).map_or(0, |i| i.doc_count);

    // `swap(true)` wins exactly once; concurrent callers see `true` and just
    // report progress instead of fanning a second build (single flight).
    if building_flag().swap(true, Ordering::SeqCst) {
        return Err(SearchIndexBuildProgress {
            status: "building",
            files_total,
            files_done: 0,
        });
    }

    let bg_root = root.to_path_buf();
    std::thread::spawn(move || {
        // Use IndexPipeline to build+persist the index; the next poll then
        // loads it via the fast path. Catch panics so the single-flight flag is
        // always cleared and the coordinator never wedges.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if let Ok(pipeline) = IndexPipeline::new(bg_root.clone()).build() {
                let _ = pipeline.run();
            }
            // Load result from disk so DumpEngine dumps are visible to fast-path load
            let _ = DumpEngine::load_with_integrity_check(&bg_root);
        }));
        building_flag().store(false, Ordering::SeqCst);
    });

    Err(SearchIndexBuildProgress {
        status: "building",
        files_total,
        files_done: 0,
    })
}
