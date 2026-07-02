use std::path::Path;

use crate::core::bm25_index::{BM25Index, format_search_results};
#[cfg(feature = "embeddings")]
use crate::core::embedding_index::EmbeddingIndex;
#[cfg(feature = "embeddings")]
use crate::core::embeddings::EmbeddingEngine;
use crate::core::hybrid_search::HybridResult;
use crate::tools::CrpMode;

/// Performs semantic code search using BM25, dense embeddings, or hybrid ranking.
#[allow(clippy::too_many_arguments)]
pub fn handle(
    query: &str,
    path: &str,
    top_k: usize,
    crp_mode: CrpMode,
    languages: Option<&[String]>,
    path_glob: Option<&str>,
    mode: Option<&str>,
    workspace: Option<bool>,
    artifacts: Option<bool>,
) -> String {
    let (root_buf, subdir) = match resolve_search_root(path) {
        Ok(v) => v,
        Err(e) => return format!("ERR: {e}"),
    };
    let root = root_buf.as_path();

    // Query-conditioned IB (#542): remember the latest search query as a
    // fallback relevance signal for subsequent compressed reads.
    if !query.trim().is_empty()
        && let Some(mut session) = crate::core::session::SessionState::load_latest()
        && session.last_semantic_query.as_deref() != Some(query)
    {
        session.last_semantic_query = Some(query.to_string());
        let _ = session.save();
    }

    let filter = match SearchFilter::new(languages, path_glob) {
        Ok(f) => f.with_subdir(subdir),
        Err(e) => return format!("ERR: invalid filter: {e}"),
    };

    let compact = crp_mode.is_tdd();
    let mode = mode.unwrap_or("bm25").to_lowercase();
    let workspace = workspace.unwrap_or(false);
    let artifacts = artifacts.unwrap_or(false);

    if artifacts {
        return artifacts_search(query, root, top_k, compact, &filter, workspace);
    }
    if workspace {
        return workspace_search(query, root, top_k, compact, &filter, &mode);
    }

    let index = match load_or_refresh_bm25(root) {
        Bm25LoadResult::Ready(idx) => idx,
        Bm25LoadResult::Building => {
            return "BM25 index is being built in the background. \
                    Run ctx_semantic_search again in ~30s, or use action=reindex to wait for completion."
                .to_string();
        }
    };
    if index.doc_count == 0 {
        return "No code files found to index.".to_string();
    }

    match mode.as_str() {
        "bm25" => {
            let mut results = index.search(query, filtered_candidate_k(top_k, filter.is_active()));
            if filter.is_active() {
                results.retain(|x| filter.matches(&x.file_path));
            }
            results.truncate(top_k);

            let header = if compact {
                format!(
                    "semantic_search(bm25,{top_k}) → {} results, {} chunks indexed\n",
                    results.len(),
                    index.doc_count
                )
            } else {
                format!(
                    "Semantic search (BM25): \"{}\" ({} results from {} indexed chunks)\n",
                    truncate_query(query, 60),
                    results.len(),
                    index.doc_count,
                )
            };
            format!("{header}{}", format_search_results(&results, compact))
        }
        "dense" => {
            let out = dense_search_mode(query, root, &index, top_k, compact, &filter);
            shrink_resident_after_embedding(root, index);
            out
        }
        _ => {
            let out = hybrid_search_mode(query, root, &index, top_k, compact, &filter);
            shrink_resident_after_embedding(root, index);
            out
        }
    }
}

/// Reclaim the RAM held by full chunk bodies in the resident BM25 cache once the
/// dense/hybrid embedding pass has consumed and persisted them. Drops this
/// handler's `Arc` clone first so the cache becomes the sole owner and the trim
/// is zero-copy (see `bm25_cache::shrink_resident_to_snippet`).
///
/// `keep_lines = 5` matches the snippet window used everywhere results are
/// rendered (`bm25_index::search`, `dense_backend`, `hybrid_search`). Only fires
/// when embeddings are actually built (feature-gated); a BM25-only fallback build
/// must keep full bodies for a later real embedding pass.
fn shrink_resident_after_embedding(root: &Path, index: std::sync::Arc<BM25Index>) {
    #[cfg(feature = "embeddings")]
    {
        // Release our clone so the cache is the sole Arc owner; otherwise the
        // in-place trim is skipped and retried on the next search.
        drop(index);
        if let Some(cache) = get_thread_cache() {
            let freed = crate::core::bm25_cache::shrink_resident_to_snippet(&cache, root, 5);
            if freed > 0 {
                tracing::info!(
                    "[bm25_cache] reclaimed ~{:.1}MB of resident chunk bodies post-embedding",
                    freed as f64 / 1_048_576.0
                );
            }
        }
    }
    #[cfg(not(feature = "embeddings"))]
    {
        let _ = (root, index);
    }
}

/// Structured single-root search used by the `semantic-search` CLI (`--json`)
/// and any programmatic caller (editor extensions). Mirrors `handle`'s
/// single-root logic but returns the ranked [`HybridResult`]s instead of a
/// formatted report, so callers control their own serialization. Reuses the
/// exact same hybrid/dense/BM25 ranking as the `ctx_semantic_search` MCP tool —
/// no second code path to drift.
pub fn search_hits(
    query: &str,
    path: &str,
    top_k: usize,
    mode: &str,
    languages: Option<&[String]>,
    path_glob: Option<&str>,
) -> Result<Vec<HybridResult>, String> {
    let (root_buf, subdir) = resolve_search_root(path)?;
    let root = root_buf.as_path();

    let filter = SearchFilter::new(languages, path_glob)
        .map_err(|e| format!("invalid filter: {e}"))?
        .with_subdir(subdir);

    let index = BM25Index::load_or_build(root);
    if index.doc_count == 0 {
        return Ok(Vec::new());
    }

    let results = match mode.to_lowercase().as_str() {
        "bm25" => bm25_hits(&index, query, top_k, &filter),
        "dense" => {
            #[cfg(feature = "embeddings")]
            {
                dense_results_for_root(query, root, &index, top_k, &filter).map(|(v, _)| v)?
            }
            #[cfg(not(feature = "embeddings"))]
            {
                return Err("dense mode requires the embeddings feature".to_string());
            }
        }
        _ => {
            #[cfg(feature = "embeddings")]
            {
                hybrid_results_for_root(query, root, &index, top_k, &filter).map(|(v, _)| v)?
            }
            #[cfg(not(feature = "embeddings"))]
            {
                bm25_hits(&index, query, top_k, &filter)
            }
        }
    };

    Ok(results)
}

fn bm25_hits(
    index: &BM25Index,
    query: &str,
    top_k: usize,
    filter: &SearchFilter,
) -> Vec<HybridResult> {
    let mut results = index.search(query, filtered_candidate_k(top_k, filter.is_active()));
    if filter.is_active() {
        results.retain(|x| filter.matches(&x.file_path));
    }
    results.truncate(top_k);
    results
        .into_iter()
        .map(HybridResult::from_bm25_public)
        .collect()
}

/// Rebuilds the BM25 search index for the given directory from scratch.
#[must_use]
pub fn handle_reindex(path: &str) -> String {
    // Promote to the project root so the rebuilt index lands in the same
    // namespace the search path resolves to (#948) — reindexing a subdirectory
    // would otherwise build an index the search can never find.
    let (root_buf, _subdir) = match resolve_search_root(path) {
        Ok(v) => v,
        Err(e) => return format!("ERR: {e}"),
    };
    let root = root_buf.as_path();

    let idx = BM25Index::build_from_directory(root);
    let files = idx.files.len();
    let chunks = idx.doc_count;
    let _ = idx.save(root);

    format!(
        "Reindexed {}: {files} files, {chunks} chunks",
        root.display()
    )
}

#[must_use]
pub fn handle_reindex_artifacts(path: &str, workspace: bool) -> String {
    let (root_buf, _subdir) = match resolve_search_root(path) {
        Ok(v) => v,
        Err(e) => return format!("ERR: {e}"),
    };
    let root = root_buf.as_path();

    let mut roots: Vec<std::path::PathBuf> = vec![root.to_path_buf()];
    let mut warnings: Vec<String> = Vec::new();

    if workspace {
        let linked = crate::core::workspace_config::load_linked_projects(root);
        warnings.extend(linked.warnings);
        roots.extend(linked.roots);
    }

    let mut total_files = 0usize;
    let mut total_chunks = 0usize;
    for r in roots {
        let (idx, w) = crate::core::artifact_index::rebuild_from_scratch(&r);
        warnings.extend(w);
        total_files += idx.files.len();
        total_chunks += idx.doc_count;
    }

    if warnings.is_empty() {
        format!("Reindexed artifacts: {total_files} files, {total_chunks} chunks")
    } else {
        format!(
            "Reindexed artifacts: {total_files} files, {total_chunks} chunks ({} warning(s))",
            warnings.len()
        )
    }
}

/// Find chunks semantically related to a given file location.
///
/// Marchionini (2006): Exploratory search navigates from known points.
/// This enables "show me similar code" workflows.
pub fn handle_find_related(
    file_path: &str,
    line: usize,
    project_root: &str,
    top_k: usize,
    crp_mode: CrpMode,
) -> String {
    let (root_buf, _subdir) = match resolve_search_root(project_root) {
        Ok(v) => v,
        Err(e) => return format!("ERR: {e}"),
    };
    let root = root_buf.as_path();

    let index = BM25Index::load_or_build(root);
    if index.doc_count == 0 {
        return "ERR: empty index. Try action=reindex first.".to_string();
    }

    let source_chunk = index
        .chunks
        .iter()
        .find(|c| c.file_path == file_path && c.start_line <= line && c.end_line >= line);

    let Some(source_chunk) = source_chunk else {
        return format!(
            "ERR: no indexed chunk found at {file_path}:{line}. Try action=reindex first."
        );
    };

    let query_text = source_chunk.content.clone();
    let source_file = source_chunk.file_path.clone();
    let source_start = source_chunk.start_line;

    let compact = crp_mode != CrpMode::Off;

    let results = find_related_internal(&query_text, root, &index, top_k + 5, compact);

    let mut lines: Vec<String> = results
        .into_iter()
        .filter(|l| !l.contains(&format!("{source_file}:{source_start}-")))
        .take(top_k)
        .collect();

    let header = if compact {
        format!(
            "find_related({file_path}:{line}) → {} results\n",
            lines.len()
        )
    } else {
        format!("Find related to {file_path}:{line} (semantic similarity)\n")
    };

    lines.insert(0, header);
    lines.join("")
}

fn find_related_internal(
    query: &str,
    root: &Path,
    index: &BM25Index,
    top_k: usize,
    compact: bool,
) -> Vec<String> {
    let Ok(filter) = SearchFilter::new(None, None) else {
        return vec!["ERR: filter init failed\n".to_string()];
    };
    let output = hybrid_search_mode(query, root, index, top_k, compact, &filter);
    output.lines().map(|l| format!("{l}\n")).collect()
}

fn truncate_query(q: &str, max: usize) -> &str {
    if q.len() <= max {
        return q;
    }
    match q.char_indices().nth(max) {
        Some((byte_idx, _)) => &q[..byte_idx],
        None => q,
    }
}

/// Public wrapper for eval harness: load embedding engine + index.
#[cfg(feature = "embeddings")]
pub fn load_engine_and_index_pub(
    root: &Path,
) -> Result<(&'static EmbeddingEngine, EmbeddingIndex), String> {
    load_engine_and_index(root)
}

/// Public wrapper for eval harness: prepare embeddings for a project.
#[cfg(feature = "embeddings")]
pub fn ensure_embeddings_for_eval(
    root: &Path,
    index: &BM25Index,
    engine: &EmbeddingEngine,
    embed_idx: &mut EmbeddingIndex,
) -> Result<AlignedEmbeddings, String> {
    ensure_embeddings(root, index, engine, embed_idx)
}

/// Public wrapper for eval harness: apply SPLADE boosting.
pub fn boost_with_splade_pub(
    results: &mut [HybridResult],
    splade: &[crate::core::splade_retrieval::SpladeResult],
    weight: f64,
) {
    boost_with_splade(results, splade, weight);
}

mod bm25_store;
mod dense;
pub(crate) mod multi_root;
mod scope;

pub(crate) use bm25_store::*;
pub use bm25_store::{get_thread_cache, set_thread_cache};
pub(crate) use dense::*;
pub(crate) use multi_root::*;
pub(crate) use scope::*;

#[cfg(test)]
mod tests;
