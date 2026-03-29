use std::path::Path;

use crate::core::vector_index::{format_search_results, BM25Index};
use crate::tools::CrpMode;

pub fn handle(query: &str, path: &str, top_k: usize, crp_mode: CrpMode) -> String {
    let root = Path::new(path);
    if !root.exists() {
        return format!("ERR: path does not exist: {path}");
    }

    let root = if root.is_file() {
        root.parent().unwrap_or(root)
    } else {
        root
    };

    let index = match BM25Index::load(root) {
        Some(idx) if idx.doc_count > 0 => idx,
        _ => {
            let idx = BM25Index::build_from_directory(root);
            if idx.doc_count == 0 {
                return "No code files found to index.".to_string();
            }
            let _ = idx.save(root);
            idx
        }
    };

    let results = index.search(query, top_k);
    let compact = crp_mode.is_tdd();

    let header = if compact {
        format!(
            "semantic_search({top_k}) → {} results, {} chunks indexed\n",
            results.len(),
            index.doc_count
        )
    } else {
        format!(
            "Semantic search: \"{}\" ({} results from {} indexed chunks)\n",
            truncate_query(query, 60),
            results.len(),
            index.doc_count,
        )
    };

    format!("{header}{}", format_search_results(&results, compact))
}

pub fn handle_reindex(path: &str) -> String {
    let root = Path::new(path);
    if !root.exists() {
        return format!("ERR: path does not exist: {path}");
    }
    let root = if root.is_file() {
        root.parent().unwrap_or(root)
    } else {
        root
    };

    let idx = BM25Index::build_from_directory(root);
    let count = idx.doc_count;
    let chunks = idx.chunks.len();
    let _ = idx.save(root);

    format!("Reindexed {path}: {count} files, {chunks} chunks")
}

fn truncate_query(q: &str, max: usize) -> &str {
    if q.len() <= max {
        q
    } else {
        &q[..max]
    }
}
