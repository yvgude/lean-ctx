//! Non-blocking, PropertyGraph-first build coordinator for the dashboard graph
//! routes (#696 phase C).
//!
//! Supersedes the JSON-coupled `graph_index::coordinator`: instead of loading
//! the on-disk `ProjectIndex` and scanning into it, this resolves a
//! [`GraphProvider`](super::graph_provider::GraphProvider) through
//! [`open_best_effort`](super::graph_provider::open_best_effort) (`PropertyGraph`
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
pub struct GraphBuildProgress {
    pub status: &'static str,
    pub files_total: usize,
    pub files_done: usize,
}

impl GraphBuildProgress {
    fn building() -> Self {
        Self {
            status: "building",
            files_total: 0,
            files_done: 0,
        }
    }
}

/// Resolve a graph provider for `project_root` without blocking on a scan, or
/// start the single-flight background build and report progress.
///
/// Fast path: a populated `PropertyGraph` (or, during the #696 transition, the
/// legacy JSON index) is returned immediately. Otherwise the shared background
/// indexer is (idempotently) started and `Err(progress)` is returned.
pub fn get_or_start_build(project_root: &str) -> Result<OpenGraphProvider, GraphBuildProgress> {
    if let Some(open) = graph_provider::open_best_effort(project_root) {
        return Ok(open);
    }
    Err(GraphBuildProgress::building())
}
