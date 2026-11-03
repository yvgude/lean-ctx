// SPDX-License-Identifier: Apache-2.0
use std::path::Path;

#[must_use]
pub fn handle(action: &str, project_root: &Path) -> String {
    match action {
        "status" => {
            lean_ctx::core::index_orchestrator::status_json(project_root.to_string_lossy().as_ref())
        }
        "build" => {
            lean_ctx::core::index_orchestrator::ensure_all_background(
                project_root.to_string_lossy().as_ref(),
            );
            "started".to_string()
        }
        "build-full" => {
            // Force rebuild by deleting existing on-disk indexes first.
            let bm25 = lean_ctx::core::bm25_index::BM25Index::index_file_path(project_root);
            let _ = std::fs::remove_file(&bm25);
            // #696 C4: purge the property graph (graph.db + wal/shm + meta) and
            // any retired JSON/call-graph artifacts so the rebuild starts clean.
            lean_ctx::core::graph_index::purge_index(project_root.to_string_lossy().as_ref());
            // Purge old embeddings so the full rebuild starts from scratch.
            let vectors_dir = lean_ctx::core::index_namespace::vectors_dir(project_root);
            let _ = std::fs::remove_file(vectors_dir.join("embeddings.bin"));
            let _ = std::fs::remove_file(vectors_dir.join("embeddings.json"));
            lean_ctx::core::index_orchestrator::ensure_all_background(
                project_root.to_string_lossy().as_ref(),
            );
            // Build semantic index once the fresh BM25 is on disk.
            lean_ctx::core::index_orchestrator::build_semantic_background(
                project_root.to_string_lossy().as_ref(),
            );
            // #420: a forced rebuild must drop the in-process call-graph cache so
            // ctx_impact/graph reads re-derive from the fresh on-disk index
            // instead of the pre-rebuild snapshot (the CLI path does the same).
            lean_ctx::core::graph_cache::invalidate(Some(project_root.to_string_lossy().as_ref()));
            "started".to_string()
        }
        "build-semantic" => {
            // Builds BM25 first if missing. Runs in the background: a CPU-only
            // embed can take far longer than a tool call may block.
            lean_ctx::core::index_orchestrator::build_semantic_background(
                project_root.to_string_lossy().as_ref(),
            );
            "semantic index build started in the background; progress is saved as it goes — \
             check with ctx_index action=status"
                .to_string()
        }
        _ => "Unknown action. Use: status, build, build-full, build-semantic, why".to_string(),
    }
}
