//! Non-blocking, PropertyGraph-first build coordinator for the dashboard graph
//! routes (#696 phase C).
//!
//! Supersedes the JSON-coupled `graph_index::coordinator`: instead of loading
//! the on-disk `ProjectIndex` and scanning into it, this resolves a
//! [`GraphProvider`](super::graph_provider::GraphProvider) through
//! [`open_best_effort`](super::graph_provider::open_best_effort) (PropertyGraph
//! first, with the legacy JSON index only as a transition fallback) and kicks
//! the shared single-flight background builder when nothing is ready yet.
//!
//! The request thread is never blocked on a full project scan: a populated
//! store is returned immediately; otherwise the caller gets `Err(progress)` so
//! the HTTP handler answers `202 Accepted` and the dashboard polls until the
//! background build finishes and a `200` carries the data.

use crate::core::graph_provider::{self, OpenGraphProvider};

/// Build progress serialised to the dashboard as the `202` body. Kept
/// field-compatible with the legacy `graph_index` coordinator so the front-end
/// polling contract is unchanged.
#[derive(serde::Serialize)]
pub(crate) struct GraphBuildProgress {
    pub status: &'static str,
    pub files_total: usize,
    pub files_done: usize,
}

impl GraphBuildProgress {
    /// The graph scan's file counts for `project_root` while this process
    /// runs it (`0/0` while it still walks the tree, or when another process,
    /// such as the daemon, builds the shared index).
    fn building(project_root: &str) -> Self {
        let graph = crate::core::index_orchestrator::progress_view(project_root).graph;
        Self {
            status: "building",
            files_total: usize::try_from(graph.total).unwrap_or(usize::MAX),
            files_done: usize::try_from(graph.done).unwrap_or(usize::MAX),
        }
    }
}

/// Resolve a graph provider for `project_root` without blocking on a scan, or
/// start the single-flight background build and report progress.
///
/// Fast path: a populated PropertyGraph (or, during the #696 transition, the
/// legacy JSON index) is returned immediately. Otherwise the shared background
/// indexer is (idempotently) started and `Err(progress)` is returned.
pub(crate) fn get_or_start_build(
    project_root: &str,
) -> Result<OpenGraphProvider, GraphBuildProgress> {
    if let Some(open) = graph_provider::open_best_effort(project_root) {
        return Ok(open);
    }
    // Unit tests share one process-global data dir that each test rewrites; a
    // detached build would outlive the test that started it (the same reason
    // `graph_provider` skips its lazy build under `cfg!(test)`).
    if !cfg!(test) {
        crate::core::index_orchestrator::ensure_all_background(project_root);
    }
    Err(GraphBuildProgress::building(project_root))
}
