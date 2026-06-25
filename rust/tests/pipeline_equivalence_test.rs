//! Equivalence test: compares new pipeline output against old pipeline reference
//! counts from the frozen test repo.
//!
//! The old pipeline used `ProjectIndex`; the new one uses `GraphBuffer`/SQLite.
//! This test verifies the structure-level metrics (nodes, edges, chunks) are
//! equivalent between the two implementations.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use lean_ctx::core::config::IndexingMode;
use lean_ctx::core::index_pipeline::dump_engine::DumpEngine;
use lean_ctx::core::index_pipeline::pipeline::IndexPipeline;

/// Resolve a path relative to the repo root (parent of `CARGO_MANIFEST_DIR`).
fn repo_path(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join(rel)
}

/// Load the old pipeline reference report as a JSON value.
fn read_old_report() -> serde_json::Value {
    let path = repo_path("tests/fixtures/old-pipeline-output/pipeline-report.json");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read old report at {}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("cannot parse old report JSON: {e}"))
}

#[test]
fn new_pipeline_matches_old_pipeline_counts() {
    // Isolate data dir so the pipeline does not pollute real state.
    let data_dir = tempfile::tempdir().expect("temp dir for data isolation");
    unsafe { std::env::set_var("LEAN_CTX_DATA_DIR", data_dir.path()) };

    // ── Read old reference counts ────────────────────────────────────────────
    let old = read_old_report();
    let old_nodes: usize = old["nodes"].as_u64().expect("old nodes") as usize;
    let old_edges: usize = old["edges"].as_u64().expect("old edges") as usize;
    let old_chunks: usize = old["chunks"].as_u64().expect("old chunks") as usize;
    let old_files: usize = old["fixture_files"]
        .as_array()
        .map_or(0, std::vec::Vec::len);

    // ── Run new pipeline on the frozen test repo ─────────────────────────────
    let root = repo_path("tests/fixtures/frozen-test-repo");
    let handle = IndexPipeline::new(root.clone())
        .with_mode(IndexingMode::Full)
        .build()
        .expect("pipeline builder should succeed");
    let report = handle.run().expect("pipeline run should succeed");

    // ── Assert equivalence (within reason) ────────────────────────────────────
    //
    // The old pipeline used ProjectIndex which counted at a coarser granularity
    // (11 nodes ≈ top-level symbols). The new pipeline uses GraphBuffer which
    // creates Project + Folder + File + Symbol nodes → more detailed. We check
    // that the new counts are at least as large (same or finer granularity) and
    // that files_scanned and chunks match exactly (content-derived).

    // Node count: new pipeline creates more detailed nodes (project, folder,
    // file, symbol hierarchy) so it should be >= old count.
    assert!(
        report.nodes >= old_nodes,
        "node count too low: new={}, old={}",
        report.nodes,
        old_nodes,
    );

    // Edge count: similarly more granular.
    assert!(
        report.edges >= old_edges,
        "edge count too low: new={}, old={}",
        report.edges,
        old_edges,
    );

    // Chunk count: derived from file content, should match exactly.
    assert_eq!(
        report.chunks, old_chunks,
        "chunk count mismatch: new={}, old={}",
        report.chunks, old_chunks,
    );

    // Files scanned must match the fixture file list.
    assert_eq!(
        report.files_scanned, old_files,
        "files scanned mismatch: new={}, old={}",
        report.files_scanned, old_files,
    );

    // Sanity-check the report shape.
    assert!(
        matches!(
            report.embedding_ready,
            lean_ctx::core::embedding_index::EmbeddingBuildOutcome::Skipped
                | lean_ctx::core::embedding_index::EmbeddingBuildOutcome::ModelNotAvailable(_)
        ),
        "unexpected embedding outcome: {:?}",
        report.embedding_ready,
    );
    assert!(report.elapsed_ms > 0, "elapsed time should be positive");
    assert!(!report.is_incremental, "new pipeline is not incremental");
    assert_eq!(report.mode, IndexingMode::Full);

    // ── Per-file signature set comparison ──────────────────────────────────
    // Load the dumped BM25 index to access per-file chunk data (≈ signature
    // count per file). Verify every fixture file is represented and that the
    // per-file sum matches the aggregate.
    let (_graph, bm25, _store) = DumpEngine::load_with_integrity_check(&root)
        .expect("should load dumped index after pipeline run");
    let bm25 = bm25.expect("BM25 index should have been created");

    let mut per_file_chunks: HashMap<&str, usize> = HashMap::new();
    for chunk in &bm25.chunks {
        *per_file_chunks.entry(&chunk.file_path).or_insert(0) += 1;
    }

    let fixture_files = old["fixture_files"]
        .as_array()
        .expect("fixture_files array");
    for file_val in fixture_files {
        let file_path = file_val.as_str().expect("fixture file path");
        assert!(
            per_file_chunks.contains_key(file_path),
            "fixture file '{file_path}' not found in new pipeline chunks",
        );
    }

    let sum_per_file: usize = per_file_chunks.values().sum();
    assert_eq!(
        sum_per_file, report.chunks,
        "sum of per-file chunk counts ({sum_per_file}) != total chunks ({})",
        report.chunks,
    );
}
