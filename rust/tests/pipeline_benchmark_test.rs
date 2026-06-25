//! Benchmark test: asserts the indexing pipeline completes within a time budget.
//!
//! This test runs the full index pipeline against the frozen test repo and
//! verifies that elapsed time stays under 6600 ms (release build only —
//! `cargo test --release --test pipeline_benchmark_test`).  The budget
//! accounts for CI variability on a 2.6 GHz 12-core Intel Xeon with NVMe SSD.
//!
//! RSS measurement is intentionally omitted (requires OS-specific tooling
//! inside the test process); elapsed time is a strong enough proxy for
//! regression detection.

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
fn pipeline_full_mode_under_6600ms() {
    // Isolate data dir so the pipeline does not pollute real state.
    let data_dir = tempfile::tempdir().expect("temp dir for data isolation");
    unsafe { std::env::set_var("LEAN_CTX_DATA_DIR", data_dir.path()) };

    // Run the pipeline on the frozen test repo.
    let root = repo_path("tests/fixtures/frozen-test-repo");
    let handle = IndexPipeline::new(root)
        .with_mode(IndexingMode::Full)
        .build()
        .expect("pipeline builder should succeed");
    let report = handle.run().expect("pipeline run should succeed");

    // Assert time budget.
    assert!(
        report.elapsed_ms <= 6600,
        "pipeline took too long: {} ms (budget: 6600 ms)",
        report.elapsed_ms,
    );
}
