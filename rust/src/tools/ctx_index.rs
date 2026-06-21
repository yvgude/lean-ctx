use std::path::Path;

#[must_use]
pub fn handle(action: &str, project_root: &Path) -> String {
    match action {
        "status" => {
            crate::core::index_orchestrator::status_json(project_root.to_string_lossy().as_ref())
        }
        "build" => {
            crate::core::index_orchestrator::ensure_all_background(
                project_root.to_string_lossy().as_ref(),
            );
            "started".to_string()
        }
        "build-full" => {
            // Force rebuild by deleting existing on-disk indexes first.
            let bm25 = crate::core::bm25_index::BM25Index::index_file_path(project_root);
            let _ = std::fs::remove_file(&bm25);
            // #696 C4: purge the property graph (graph.db + wal/shm + meta) and
            // any retired JSON/call-graph artifacts so the rebuild starts clean.
            crate::core::graph_index::purge_index(project_root.to_string_lossy().as_ref());
            // Purge old embeddings so the full rebuild starts from scratch.
            let vectors_dir = crate::core::index_namespace::vectors_dir(project_root);
            let _ = std::fs::remove_file(vectors_dir.join("embeddings.bin"));
            let _ = std::fs::remove_file(vectors_dir.join("embeddings.json"));
            crate::core::index_orchestrator::ensure_all_background(
                project_root.to_string_lossy().as_ref(),
            );
            // Build semantic index on top of the fresh BM25.
            crate::core::index_orchestrator::build_semantic(
                project_root.to_string_lossy().as_ref(),
            );
            // #420: a forced rebuild must drop the in-process call-graph cache so
            // ctx_impact/graph reads re-derive from the fresh on-disk index
            // instead of the pre-rebuild snapshot (the CLI path does the same).
            crate::core::graph_cache::invalidate(Some(project_root.to_string_lossy().as_ref()));
            "started".to_string()
        }
        "build-semantic" => {
            // Build semantic index; auto-build BM25 first if missing.
            let root = project_root.to_string_lossy();
            let disk = crate::core::index_orchestrator::disk_status(&root);
            if !disk.bm25_index.exists {
                crate::core::index_orchestrator::ensure_all_background(&root);
            }
            crate::core::index_orchestrator::build_semantic(&root);
            let sem = crate::core::index_orchestrator::semantic_summary(&root);
            match sem.state {
                "ready" => "semantic index ready".to_string(),
                "failed" => format!(
                    "semantic index failed: {}",
                    sem.last_error.unwrap_or_else(|| "unknown".to_string())
                ),
                _ => sem
                    .note
                    .unwrap_or("semantic index not available".to_string()),
            }
        }
        _ => "Unknown action. Use: status, build, build-full, build-semantic".to_string(),
    }
}
