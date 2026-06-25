//! Non-blocking, single-flight build coordinator for the dashboard (#452).
//!
//! Mirrors `graph_index::coordinator`: the dashboard's search routes must not
//! block the request thread on a full index build (expensive on large repos).
//! Chunk search uses FTS5 bm25() directly from the SQLite DB, so there's no
//! in-memory BM25 index to cache. This coordinator just triggers a background
//! pipeline build if the DB is missing or stale.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

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

/// Returns `Ok(())` if a fresh index is available, or starts a single background
/// build and returns `Err(progress)` so the caller can answer `202 Accepted`.
pub fn get_or_start_build(root: &Path) -> Result<(), SearchIndexBuildProgress> {
    // Check if the DB exists and has chunks
    let db_path = crate::core::index_namespace::vectors_dir(root).join("code_index.db");
    if db_path.exists() {
        if let Ok(conn) = rusqlite::Connection::open(&db_path) {
            if let Ok(count) = conn.query_row("SELECT COUNT(*) FROM chunks", [], |r| r.get::<_, i64>(0)) {
                if count > 0 {
                    // DB has chunks — ready to serve FTS5 queries
                    return Ok(());
                }
            }
        }
    }

    // `swap(true)` wins exactly once; concurrent callers see `true` and just
    // report progress instead of fanning a second build (single flight).
    if building_flag().swap(true, Ordering::SeqCst) {
        return Err(SearchIndexBuildProgress {
            status: "building",
            files_total: 0,
            files_done: 0,
        });
    }

    let bg_root = root.to_path_buf();
    std::thread::spawn(move || {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if let Ok(pipeline) = IndexPipeline::new(bg_root).build() {
                let _ = pipeline.run();
            }
        }));
        building_flag().store(false, Ordering::SeqCst);
    });

    Err(SearchIndexBuildProgress {
        status: "building",
        files_total: 0,
        files_done: 0,
    })
}
