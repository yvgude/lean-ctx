//! Determinism test: run pipeline twice on the same repo with the same
//! configuration, assert that node/edge/chunk/file counts are identical.
//!
//! Determinism is critical for cached/reproducible indices. Any
//! non-determinism here indicates a bug in the indexing pipeline.

use std::path::{Path, PathBuf};

use lean_ctx::core::config::IndexingMode;
use lean_ctx::core::index_pipeline::pipeline::IndexPipeline;

/// Resolve a path relative to the repo root (parent of `CARGO_MANIFEST_DIR`).
fn repo_path(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join(rel)
}

#[test]
fn pipeline_runs_are_deterministic() {
    let data_dir = tempfile::tempdir().expect("temp dir for data isolation");
    unsafe { std::env::set_var("LEAN_CTX_DATA_DIR", data_dir.path()) };

    let root = repo_path("tests/fixtures/frozen-test-repo");

    // ── Run 1 ────────────────────────────────────────────────────────────────
    let handle1 = IndexPipeline::new(root.clone())
        .with_mode(IndexingMode::Full)
        .build()
        .expect("pipeline 1 builder should succeed");
    let report1 = handle1.run().expect("pipeline 1 run should succeed");

    // ── Run 2 — same repo, same mode, same data dir ──────────────────────────
    let handle2 = IndexPipeline::new(root)
        .with_mode(IndexingMode::Full)
        .build()
        .expect("pipeline 2 builder should succeed");
    let report2 = handle2.run().expect("pipeline 2 run should succeed");

    // ── Assert identical counts ──────────────────────────────────────────────
    assert_eq!(
        report1.nodes, report2.nodes,
        "node count mismatch: run1={}, run2={}",
        report1.nodes, report2.nodes,
    );
    assert_eq!(
        report1.edges, report2.edges,
        "edge count mismatch: run1={}, run2={}",
        report1.edges, report2.edges,
    );
    assert_eq!(
        report1.chunks, report2.chunks,
        "chunk count mismatch: run1={}, run2={}",
        report1.chunks, report2.chunks,
    );
    assert_eq!(
        report1.files_scanned, report2.files_scanned,
        "files_scanned mismatch: run1={}, run2={}",
        report1.files_scanned, report2.files_scanned,
    );
}
