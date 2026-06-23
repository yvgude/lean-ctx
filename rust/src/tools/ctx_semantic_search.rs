use std::collections::HashSet;
use std::fmt::Write;
use std::path::Path;

use crate::core::bm25_index::{BM25Index, format_search_results};
use crate::core::embedding_index::EmbeddingIndex;
#[cfg(feature = "embeddings")]
use crate::core::embeddings::EmbeddingEngine;
use crate::core::hnsw::FlatEmbeddings;
use crate::core::hybrid_search::{HybridConfig, HybridResult, format_hybrid_results};
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
    let root = Path::new(path);
    if !root.exists() {
        return format!("ERR: path does not exist: {path}");
    }

    let root = if root.is_file() {
        root.parent().unwrap_or(root)
    } else {
        root
    };

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
        Ok(f) => f,
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
    let root = Path::new(path);
    if !root.exists() {
        return Err(format!("path does not exist: {path}"));
    }
    let root = if root.is_file() {
        root.parent().unwrap_or(root)
    } else {
        root
    };

    let filter =
        SearchFilter::new(languages, path_glob).map_err(|e| format!("invalid filter: {e}"))?;

    let index = crate::core::index_orchestrator::load_or_build_bm25(root);
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
    let root = Path::new(path);
    if !root.exists() {
        return format!("ERR: path does not exist: {path}");
    }
    let root = if root.is_file() {
        root.parent().unwrap_or(root)
    } else {
        root
    };

    let idx = crate::core::index_orchestrator::load_or_build_bm25(root);
    let files = idx.files.len();
    let chunks = idx.doc_count;
    let _ = idx.save(root);

    format!("Reindexed {path}: {files} files, {chunks} chunks")
}

#[must_use]
pub fn handle_reindex_artifacts(path: &str, workspace: bool) -> String {
    let root = Path::new(path);
    if !root.exists() {
        return format!("ERR: path does not exist: {path}");
    }
    let root = if root.is_file() {
        root.parent().unwrap_or(root)
    } else {
        root
    };

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
    let root = Path::new(project_root);
    if !root.exists() {
        return format!("ERR: path does not exist: {project_root}");
    }

    let index = crate::core::index_orchestrator::load_or_build_bm25(root);
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

std::thread_local! {
    static BM25_SHARED_CACHE: std::cell::RefCell<Option<crate::core::bm25_cache::SharedBm25Cache>> =
        const { std::cell::RefCell::new(None) };
}

/// Set the shared BM25 cache for the current thread (called from the registered handler).
pub fn set_thread_cache(cache: crate::core::bm25_cache::SharedBm25Cache) {
    BM25_SHARED_CACHE.with(|c| {
        *c.borrow_mut() = Some(cache);
    });
}

/// Clone the current thread's shared BM25 cache, if any. Lets composer tools
/// propagate the resident cache into a budgeted worker thread so a slow cold
/// build warms the *same* cache instead of being wasted work.
pub fn get_thread_cache() -> Option<crate::core::bm25_cache::SharedBm25Cache> {
    BM25_SHARED_CACHE.with(|c| c.borrow().clone())
}

/// Result of BM25 index loading — may indicate background build in progress.
pub(crate) enum Bm25LoadResult {
    Ready(std::sync::Arc<BM25Index>),
    Building,
}

fn load_or_refresh_bm25(root: &Path) -> Bm25LoadResult {
    let cached = BM25_SHARED_CACHE.with(|c| {
        let borrow = c.borrow();
        borrow
            .as_ref()
            .and_then(|cache| crate::core::bm25_cache::get_or_background(cache, root))
    });
    if let Some(idx) = cached {
        return Bm25LoadResult::Ready(idx);
    }

    let root_str = root.to_string_lossy().to_string();

    if let Some(idx) = crate::core::index_orchestrator::try_load_bm25_index(&root_str) {
        let idx = std::sync::Arc::new(idx);
        store_in_thread_cache(root, &idx);
        return Bm25LoadResult::Ready(idx);
    }

    if crate::core::index_orchestrator::is_building() {
        return Bm25LoadResult::Building;
    }

    // Cold path: kick off the background build (which persists the index to
    // disk) instead of doing an unbounded synchronous build in the MCP handler.
    // Wait briefly so small/medium repos still return Ready on the first call;
    // larger repos return Building and the agent retries against the warm cache
    // once the worker has persisted the index (#150).
    crate::core::index_orchestrator::ensure_all_background(&root_str);

    let deadline = std::time::Instant::now() + bm25_cold_build_budget();
    loop {
        if let Some(idx) = crate::core::index_orchestrator::try_load_bm25_index(&root_str) {
            let idx = std::sync::Arc::new(idx);
            store_in_thread_cache(root, &idx);
            return Bm25LoadResult::Ready(idx);
        }
        if std::time::Instant::now() >= deadline {
            return Bm25LoadResult::Building;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// Time budget for waiting on a cold BM25 build in the MCP handler before
/// returning `Building`. Overridable via `LEAN_CTX_BM25_COLD_BUDGET_MS`.
fn bm25_cold_build_budget() -> std::time::Duration {
    let ms = std::env::var("LEAN_CTX_BM25_COLD_BUDGET_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(60_000);
    std::time::Duration::from_millis(ms)
}

fn store_in_thread_cache(root: &Path, idx: &std::sync::Arc<BM25Index>) {
    BM25_SHARED_CACHE.with(|c| {
        let borrow = c.borrow();
        if let Some(cache) = borrow.as_ref() {
            let mut guard = cache
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            *guard = Some(crate::core::bm25_cache::Bm25CacheEntry {
                root: root.to_path_buf(),
                index: std::sync::Arc::clone(idx),
                loaded_at: std::time::Instant::now(),
                fingerprint: crate::core::bm25_cache::index_fingerprint(root),
            });
        }
    });
}

fn filtered_candidate_k(top_k: usize, filtered: bool) -> usize {
    if !filtered {
        return top_k;
    }
    let candidates = (top_k.max(10)).saturating_mul(10);
    candidates.clamp(50, 500)
}

const WORKSPACE_RRF_K: f64 = 60.0;

fn artifacts_search(
    query: &str,
    root: &Path,
    top_k: usize,
    compact: bool,
    filter: &SearchFilter,
    workspace: bool,
) -> String {
    let mut roots: Vec<std::path::PathBuf> = vec![root.to_path_buf()];
    let mut warnings: Vec<String> = Vec::new();

    if workspace {
        let linked = crate::core::workspace_config::load_linked_projects(root);
        warnings.extend(linked.warnings);
        roots.extend(linked.roots);
    }
    roots.sort();
    roots.dedup();

    let mut per_project: Vec<(String, Vec<crate::core::bm25_index::SearchResult>)> = Vec::new();
    let mut total_chunks = 0usize;

    for r in &roots {
        let label = label_for_root(r);
        let (idx, w) = crate::core::artifact_index::load_or_build(r);
        warnings.extend(w);
        total_chunks += idx.doc_count;
        if idx.doc_count == 0 {
            continue;
        }

        let mut results = idx.search(query, filtered_candidate_k(top_k, filter.is_active()));
        if filter.is_active() {
            results.retain(|x| filter.matches(&x.file_path));
        }
        results.truncate(top_k);

        for res in &mut results {
            res.file_path = if workspace {
                format!("[project:{label}] [artifact] {}", res.file_path)
            } else {
                format!("[artifact] {}", res.file_path)
            };
        }

        per_project.push((label, results));
    }

    let mut fused: Vec<crate::core::bm25_index::SearchResult> = if per_project.len() <= 1 {
        per_project
            .into_iter()
            .next()
            .map(|(_, v)| v)
            .unwrap_or_default()
    } else {
        rrf_merge_bm25(per_project, top_k)
    };

    if fused.is_empty() {
        return "No artifact files found to index.".to_string();
    }

    fused.truncate(top_k);

    let header = if compact {
        if workspace {
            format!(
                "semantic_search(artifacts,workspace,{top_k}) → {} results, projects={}, {} chunks indexed\n",
                fused.len(),
                roots.len(),
                total_chunks
            )
        } else {
            format!(
                "semantic_search(artifacts,{top_k}) → {} results, {} chunks indexed\n",
                fused.len(),
                total_chunks
            )
        }
    } else if workspace {
        format!(
            "Semantic search (Artifacts/Workspace): \"{}\" ({} results from {} projects)\n",
            truncate_query(query, 60),
            fused.len(),
            roots.len()
        )
    } else {
        format!(
            "Semantic search (Artifacts): \"{}\" ({} results)\n",
            truncate_query(query, 60),
            fused.len()
        )
    };

    let mut out = format!("{header}{}", format_search_results(&fused, compact));
    if !warnings.is_empty() && !compact {
        let _ = writeln!(out, "\nWarnings ({}):", warnings.len());
        for w in warnings.iter().take(20) {
            let _ = writeln!(out, "- {w}");
        }
    }
    out
}

fn workspace_search(
    query: &str,
    root: &Path,
    top_k: usize,
    compact: bool,
    filter: &SearchFilter,
    mode: &str,
) -> String {
    let linked = crate::core::workspace_config::load_linked_projects(root);
    let mut warnings = linked.warnings;

    let mut roots: Vec<std::path::PathBuf> = vec![root.to_path_buf()];
    roots.extend(linked.roots);
    roots.sort();
    roots.dedup();

    let mut per_project: Vec<(String, Vec<HybridResult>)> = Vec::new();
    let mut avg_cov: Option<f64> = None;
    let mut cov_count = 0usize;

    for r in &roots {
        let label = label_for_root(r);
        let index = crate::core::index_orchestrator::load_or_build_bm25(r);
        if index.doc_count == 0 {
            continue;
        }

        let mut results: Vec<HybridResult> = match mode {
            "bm25" => {
                let mut bm25 = index.search(query, filtered_candidate_k(top_k, filter.is_active()));
                if filter.is_active() {
                    bm25.retain(|x| filter.matches(&x.file_path));
                }
                bm25.truncate(top_k);
                bm25.into_iter()
                    .map(HybridResult::from_bm25_public)
                    .collect()
            }
            "dense" => {
                #[cfg(feature = "embeddings")]
                {
                    match dense_results_for_root(query, r, &index, top_k, filter) {
                        Ok((v, cov)) => {
                            avg_cov = Some(avg_cov.unwrap_or(0.0) + cov);
                            cov_count += 1;
                            v
                        }
                        Err(e) => {
                            warnings.push(format!("[{label}] dense search failed: {e}"));
                            let mut bm25 = index
                                .search(query, filtered_candidate_k(top_k, filter.is_active()));
                            if filter.is_active() {
                                bm25.retain(|x| filter.matches(&x.file_path));
                            }
                            bm25.truncate(top_k);
                            bm25.into_iter()
                                .map(HybridResult::from_bm25_public)
                                .collect()
                        }
                    }
                }
                #[cfg(not(feature = "embeddings"))]
                {
                    let _ = (&label, &warnings);
                    let mut bm25 =
                        index.search(query, filtered_candidate_k(top_k, filter.is_active()));
                    if filter.is_active() {
                        bm25.retain(|x| filter.matches(&x.file_path));
                    }
                    bm25.truncate(top_k);
                    bm25.into_iter()
                        .map(HybridResult::from_bm25_public)
                        .collect()
                }
            }
            _ => {
                #[cfg(feature = "embeddings")]
                {
                    match hybrid_results_for_root(query, r, &index, top_k, filter) {
                        Ok((v, cov)) => {
                            avg_cov = Some(avg_cov.unwrap_or(0.0) + cov);
                            cov_count += 1;
                            v
                        }
                        Err(e) => {
                            warnings.push(format!("[{label}] hybrid search failed: {e}"));
                            let mut bm25 = index
                                .search(query, filtered_candidate_k(top_k, filter.is_active()));
                            if filter.is_active() {
                                bm25.retain(|x| filter.matches(&x.file_path));
                            }
                            bm25.truncate(top_k);
                            bm25.into_iter()
                                .map(HybridResult::from_bm25_public)
                                .collect()
                        }
                    }
                }
                #[cfg(not(feature = "embeddings"))]
                {
                    let _ = (&label, &warnings);
                    let mut bm25 =
                        index.search(query, filtered_candidate_k(top_k, filter.is_active()));
                    if filter.is_active() {
                        bm25.retain(|x| filter.matches(&x.file_path));
                    }
                    bm25.truncate(top_k);
                    bm25.into_iter()
                        .map(HybridResult::from_bm25_public)
                        .collect()
                }
            }
        };

        for res in &mut results {
            res.file_path = format!("[project:{label}] {}", res.file_path);
        }
        per_project.push((label, results));
    }

    let mut fused: Vec<HybridResult> = if per_project.len() <= 1 {
        per_project
            .into_iter()
            .next()
            .map(|(_, v)| v)
            .unwrap_or_default()
    } else {
        rrf_merge_hybrid(per_project, top_k)
    };

    if fused.is_empty() {
        return "No code files found to index.".to_string();
    }

    fused.truncate(top_k);
    let cov = avg_cov.and_then(|s| {
        if cov_count == 0 {
            None
        } else {
            Some(s / cov_count as f64)
        }
    });

    let header = if compact {
        match (mode, cov) {
            (_, Some(c)) => format!(
                "semantic_search(workspace,{mode},{top_k}) → {} results, projects={}, embed_cov={:.0}%\n",
                fused.len(),
                roots.len(),
                c * 100.0
            ),
            _ => format!(
                "semantic_search(workspace,{mode},{top_k}) → {} results, projects={}\n",
                fused.len(),
                roots.len()
            ),
        }
    } else {
        format!(
            "Workspace semantic search ({mode}): \"{}\" ({} results from {} projects)\n",
            truncate_query(query, 60),
            fused.len(),
            roots.len()
        )
    };

    let mut out = format!("{header}{}", format_hybrid_results(&fused, compact));
    if !warnings.is_empty() && !compact {
        out.push_str(&format!("\nWarnings ({}):\n", warnings.len()));
        for w in warnings.iter().take(20) {
            out.push_str(&format!("- {w}\n"));
        }
    }
    out
}

fn rrf_merge_hybrid(lists: Vec<(String, Vec<HybridResult>)>, top_k: usize) -> Vec<HybridResult> {
    use std::collections::HashMap;

    let mut acc: HashMap<String, (HybridResult, f64)> = HashMap::new();
    for (label, results) in lists {
        for (rank, r) in results.into_iter().enumerate() {
            let key = format!(
                "{label}|{}|{}|{}|{}",
                r.file_path, r.symbol_name, r.start_line, r.end_line
            );
            let rrf = 1.0 / (WORKSPACE_RRF_K + (rank as f64) + 1.0);
            acc.entry(key)
                .and_modify(|(_, s)| *s += rrf)
                .or_insert((r, rrf));
        }
    }

    let mut out: Vec<HybridResult> = acc
        .into_values()
        .map(|(mut r, s)| {
            r.rrf_score = s;
            r
        })
        .collect();
    out.sort_by(|a, b| {
        b.rrf_score
            .partial_cmp(&a.rrf_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.file_path.cmp(&b.file_path))
            .then_with(|| a.symbol_name.cmp(&b.symbol_name))
            .then_with(|| a.start_line.cmp(&b.start_line))
            .then_with(|| a.end_line.cmp(&b.end_line))
    });
    out.truncate(top_k);
    out
}

fn rrf_merge_bm25(
    lists: Vec<(String, Vec<crate::core::bm25_index::SearchResult>)>,
    top_k: usize,
) -> Vec<crate::core::bm25_index::SearchResult> {
    use std::collections::HashMap;

    let mut acc: HashMap<String, (crate::core::bm25_index::SearchResult, f64)> = HashMap::new();
    for (label, results) in lists {
        for (rank, r) in results.into_iter().enumerate() {
            let key = format!(
                "{label}|{}|{}|{}|{}",
                r.file_path, r.symbol_name, r.start_line, r.end_line
            );
            let rrf = 1.0 / (WORKSPACE_RRF_K + (rank as f64) + 1.0);
            acc.entry(key)
                .and_modify(|(_, s)| *s += rrf)
                .or_insert((r, rrf));
        }
    }

    let mut out: Vec<crate::core::bm25_index::SearchResult> = acc
        .into_values()
        .map(|(mut r, s)| {
            r.score = s;
            r
        })
        .collect();
    out.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.file_path.cmp(&b.file_path))
            .then_with(|| a.symbol_name.cmp(&b.symbol_name))
            .then_with(|| a.start_line.cmp(&b.start_line))
            .then_with(|| a.end_line.cmp(&b.end_line))
    });
    out.truncate(top_k);
    out
}

#[cfg(feature = "embeddings")]
fn dense_results_for_root(
    query: &str,
    root: &Path,
    index: &BM25Index,
    top_k: usize,
    filter: &SearchFilter,
) -> Result<(Vec<HybridResult>, f64), String> {
    let (engine, mut embed_idx) = load_engine_and_index(root)?;
    // #512: cold-start guard for the CLI/editor (`search_hits`) path — the twin of
    // the MCP `dense_search_mode` guard. Explicit dense fails fast on a cold index
    // rather than embed the whole corpus inline under the request.
    if let Some(pending) = cold_start_embed_guard(&embed_idx, index) {
        return Err(dense_build_hint(pending, true));
    }
    let (aligned, coverage, changed_files) =
        ensure_embeddings(root, index, engine, &mut embed_idx)?;

    let backend = crate::core::dense_backend::DenseBackendKind::try_from_env()?;
    let filter_fn = |p: &str| filter.matches(p);
    let filter_pred: Option<&dyn Fn(&str) -> bool> = filter
        .is_active()
        .then_some(&filter_fn as &dyn Fn(&str) -> bool);

    let candidate_k = filtered_candidate_k(top_k, filter.is_active());
    let mut results = crate::core::dense_backend::dense_results_as_hybrid(
        backend,
        root,
        index,
        engine,
        &aligned,
        &changed_files,
        query,
        candidate_k,
        filter_pred,
    )?;
    results.truncate(top_k);

    Ok((results, coverage))
}

#[cfg(feature = "embeddings")]
fn hybrid_results_for_root(
    query: &str,
    root: &Path,
    index: &BM25Index,
    top_k: usize,
    filter: &SearchFilter,
) -> Result<(Vec<HybridResult>, f64), String> {
    let (engine, mut embed_idx) = load_engine_and_index(root)?;
    // #512: cold-start guard for the CLI/editor (`search_hits`) path — the twin of
    // the MCP `hybrid_search_mode` guard. Degrade to BM25 on a cold index rather
    // than embed the whole corpus inline under the request.
    if let Some(pending) = cold_start_embed_guard(&embed_idx, index) {
        tracing::info!(
            pending,
            "hybrid cold-start guard: dense index not built — degrading to BM25 \
             (build once: lean-ctx index build-semantic)"
        );
        return Ok((bm25_hits(index, query, top_k, filter), 0.0));
    }
    let (aligned, coverage, changed_files) =
        ensure_embeddings(root, index, engine, &mut embed_idx)?;

    let backend = crate::core::dense_backend::DenseBackendKind::try_from_env()?;
    let cfg = HybridConfig::from_config();
    let filter_fn = |p: &str| filter.matches(p);
    let filter_pred: Option<&dyn Fn(&str) -> bool> = filter
        .is_active()
        .then_some(&filter_fn as &dyn Fn(&str) -> bool);
    let candidate_k = filtered_candidate_k(top_k, filter.is_active());
    let graph_ranks = graph_rrf_ranks_for_search_root(root);
    let graph_ranks_ref = graph_ranks.as_ref();
    let mut results = crate::core::dense_backend::hybrid_results(
        backend,
        root,
        index,
        engine,
        &aligned,
        &changed_files,
        query,
        candidate_k,
        &cfg,
        filter_pred,
        graph_ranks_ref,
    )?;

    if cfg.splade_weight > 0.0 {
        let splade = crate::core::splade_retrieval::hybrid_retrieve(query, index, candidate_k);
        if !splade.is_empty() {
            boost_with_splade(&mut results, &splade, cfg.splade_weight);
        }
    }

    results.truncate(top_k);
    Ok((results, coverage))
}

/// Boost existing hybrid results with SPLADE expansion scores.
fn boost_with_splade(
    results: &mut [HybridResult],
    splade: &[crate::core::splade_retrieval::SpladeResult],
    weight: f64,
) {
    use std::collections::HashMap;
    let rrf_k = 60.0_f64;

    let boosts: HashMap<&str, f64> = splade
        .iter()
        .enumerate()
        .map(|(rank, sr)| (sr.file_path.as_str(), weight / (rrf_k + rank as f64 + 1.0)))
        .collect();

    for r in results.iter_mut() {
        if let Some(&boost) = boosts.get(r.file_path.as_str()) {
            r.rrf_score += boost;
        }
    }

    results.sort_by(|a, b| {
        b.rrf_score
            .partial_cmp(&a.rrf_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
}

fn label_for_root(root: &Path) -> String {
    root.file_name()
        .and_then(|s| s.to_str())
        .map(str::to_string)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| root.to_string_lossy().to_string())
}

fn graph_rrf_ranks_for_search_root(
    root: &Path,
) -> Option<std::collections::HashMap<String, usize>> {
    let root_s = root.to_string_lossy().to_string();
    let session = crate::core::session::SessionState::load_latest_for_project_root(&root_s)?;

    if session.files_touched.is_empty() {
        return None;
    }

    let recent: Vec<String> = session
        .files_touched
        .iter()
        .rev()
        .filter(|f| path_under_search_root(&f.path, root))
        .take(12)
        .map(|f| f.path.clone())
        .collect();

    if recent.is_empty() {
        return None;
    }

    crate::core::graph_context::graph_neighbor_ranks_for_recent_files(&root_s, &recent, 40, 120)
}

fn path_under_search_root(path: &str, root: &Path) -> bool {
    let p = std::path::Path::new(path);
    if p.is_absolute() {
        let root_norm = crate::core::pathutil::safe_canonicalize_or_self(root);
        let path_norm = crate::core::pathutil::safe_canonicalize_or_self(p);
        path_norm.starts_with(&root_norm)
    } else {
        true
    }
}

/// BM25 + graph + rerank (+ SPLADE) ranking with no dense signal — the body of
/// `hybrid` semantic search when `search.dense_enabled = false` (#686). Mirrors
/// the local dense path (`dense_backend::hybrid_results` + the SPLADE boost in
/// `hybrid_search_mode`) step for step, but feeds `hybrid_search` a `None`
/// engine/embeddings pair, which is the same input the pipeline already handles
/// as its embeddings-absent fallback. Net effect: no `embeddings.json`, no embed
/// latency, identical fusion/rerank/SPLADE stages.
#[cfg(feature = "embeddings")]
fn bm25_graph_search(
    query: &str,
    root: &Path,
    index: &BM25Index,
    top_k: usize,
    compact: bool,
    filter: &SearchFilter,
    cfg: &HybridConfig,
) -> String {
    let graph_ranks = graph_rrf_ranks_for_search_root(root);
    let graph_enhances = graph_ranks.as_ref().is_some_and(|m| !m.is_empty());

    let mut results = crate::core::hybrid_search::hybrid_search(
        query,
        index,
        None,
        None,
        top_k,
        cfg,
        graph_ranks.as_ref(),
    );
    if filter.is_active() {
        results.retain(|r| filter.matches(&r.file_path));
    }
    results.truncate(top_k);

    if cfg.splade_weight > 0.0 {
        let splade = crate::core::splade_retrieval::hybrid_retrieve(query, index, top_k);
        if !splade.is_empty() {
            boost_with_splade(&mut results, &splade, cfg.splade_weight);
        }
    }
    results.truncate(top_k);

    let graph_tag = if graph_enhances { "+graph" } else { "" };
    let header = if compact {
        format!(
            "semantic_search(bm25{graph_tag},{top_k}) → {} results, {} chunks indexed\n",
            results.len(),
            index.doc_count
        )
    } else {
        format!(
            "Semantic search (BM25{graph_tag}): \"{}\" ({} results from {} indexed chunks)\n",
            truncate_query(query, 60),
            results.len(),
            index.doc_count,
        )
    };
    format!("{header}{}", format_hybrid_results(&results, compact))
}

/// #512: max chunks the hybrid/dense path will embed *inline* (under the
/// per-request watchdog) before degrading instead of embedding. A server that
/// started before the on-disk dense index existed would otherwise embed the
/// whole corpus on the first query — observed as a runaway 500%+ CPU child the
/// 120s watchdog abandons but cannot cancel. Tunable via
/// `LEAN_CTX_HYBRID_INLINE_EMBED_MAX`; `0` disables the guard (always embed
/// inline — the pre-#512 behavior).
#[cfg(feature = "embeddings")]
fn inline_embed_max_chunks() -> usize {
    const DEFAULT_MAX: usize = 2000;
    std::env::var("LEAN_CTX_HYBRID_INLINE_EMBED_MAX")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(DEFAULT_MAX)
}

/// Pure budget check for the cold-start guard (#512): `max == 0` disables it,
/// and the budget is inclusive (`pending == max` still embeds inline).
#[cfg(feature = "embeddings")]
fn exceeds_inline_embed_budget(pending: usize, max: usize) -> bool {
    max > 0 && pending > max
}

/// Decide whether this call would trigger a large inline embed the watchdog
/// cannot safely bound (#512). Returns the pending-chunk count when the call
/// should degrade instead of embedding inline; `None` keeps the normal path
/// (warm index, or an incremental embed of only a few changed chunks).
#[cfg(feature = "embeddings")]
fn cold_start_embed_guard(embed_idx: &EmbeddingIndex, index: &BM25Index) -> Option<usize> {
    let pending = embed_idx.pending_chunk_count(&index.chunks);
    exceeds_inline_embed_budget(pending, inline_embed_max_chunks()).then_some(pending)
}

/// One-line, deterministic hint pointing at the out-of-band dense build. Shared
/// by the hybrid fallback and the dense fail-fast so the guidance never drifts.
#[cfg(feature = "embeddings")]
fn dense_build_hint(pending: usize, compact: bool) -> String {
    if compact {
        format!("[dense not built: {pending} chunks pending — run: lean-ctx index build-semantic]")
    } else {
        format!(
            "[lean-ctx: dense index not built ({pending} chunks would embed inline). \
             Build it once — no per-query embed, no cold-start hang: \
             lean-ctx index build-semantic]"
        )
    }
}

fn hybrid_search_mode(
    query: &str,
    root: &Path,
    index: &BM25Index,
    top_k: usize,
    compact: bool,
    filter: &SearchFilter,
) -> String {
    #[cfg(feature = "embeddings")]
    {
        let cfg = HybridConfig::from_config();

        // Dense disabled (#686): skip the embedding engine + index build/persist
        // and rank with BM25 + graph proximity + reranking (+ SPLADE) only — the
        // exact fallback the pipeline uses when embeddings are absent, so results
        // stay coherent while the vector footprint and embed latency disappear.
        if !cfg.dense_enabled {
            return bm25_graph_search(query, root, index, top_k, compact, filter, &cfg);
        }

        let (engine, mut embed_idx) = match load_engine_and_index(root) {
            Ok(v) => v,
            Err(e) => return format!("ERR: {e}"),
        };

        // #512: cold-start guard. Never embed a large corpus inline under the
        // request watchdog (it produces a runaway the watchdog abandons but
        // cannot cancel). Degrade to the BM25+graph path — the same coherent
        // fallback used when dense is disabled — and tell the user to build the
        // dense index once, out of band. Incremental embeds (few changed chunks
        // on a warm index) stay inline and fast.
        if let Some(pending) = cold_start_embed_guard(&embed_idx, index) {
            let base = bm25_graph_search(query, root, index, top_k, compact, filter, &cfg);
            return format!("{base}\n{}", dense_build_hint(pending, compact));
        }

        let (aligned, coverage, changed_files) =
            match ensure_embeddings(root, index, engine, &mut embed_idx) {
                Ok(v) => v,
                Err(e) => return format!("ERR: {e}"),
            };

        let backend = match crate::core::dense_backend::DenseBackendKind::try_from_env() {
            Ok(v) => v,
            Err(e) => return format!("ERR: {e}"),
        };
        let filter_fn = |p: &str| filter.matches(p);
        let filter_pred: Option<&dyn Fn(&str) -> bool> = filter
            .is_active()
            .then_some(&filter_fn as &dyn Fn(&str) -> bool);
        let graph_ranks = graph_rrf_ranks_for_search_root(root);
        let graph_ranks_ref = graph_ranks.as_ref();
        let mut results = match crate::core::dense_backend::hybrid_results(
            backend,
            root,
            index,
            engine,
            &aligned,
            &changed_files,
            query,
            top_k,
            &cfg,
            filter_pred,
            graph_ranks_ref,
        ) {
            Ok(v) => v,
            Err(e) => return format!("ERR: {e}"),
        };

        if cfg.splade_weight > 0.0 {
            let splade = crate::core::splade_retrieval::hybrid_retrieve(query, index, top_k);
            if !splade.is_empty() {
                boost_with_splade(&mut results, &splade, cfg.splade_weight);
            }
        }

        results.truncate(top_k);

        let header = if compact {
            format!(
                "semantic_search(hybrid,{top_k}) → {} results, {} chunks, embed_cov={:.0}%\n",
                results.len(),
                index.doc_count,
                coverage * 100.0
            )
        } else {
            format!(
                "Semantic search (Hybrid): \"{}\" ({} results from {} indexed chunks, embeddings coverage {:.0}%)\n",
                truncate_query(query, 60),
                results.len(),
                index.doc_count,
                coverage * 100.0
            )
        };

        format!("{header}{}", format_hybrid_results(&results, compact))
    }
    #[cfg(not(feature = "embeddings"))]
    {
        let mut results = index.search(query, filtered_candidate_k(top_k, filter.is_active()));
        if filter.is_active() {
            results.retain(|x| filter.matches(&x.file_path));
        }

        let graph_ranks = graph_rrf_ranks_for_search_root(root);
        if let Some(ref graph_ranks) = graph_ranks {
            const GRAPH_RRF_K: f64 = 60.0;
            for r in &mut results {
                if let Some(&rank) = graph_ranks.get(&r.file_path) {
                    r.score += 1.0 / (GRAPH_RRF_K + rank as f64 + 1.0);
                }
            }
            results.sort_by(|a, b| {
                b.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        }

        results.truncate(top_k);
        let graph_tag = if graph_ranks.is_some() { "+graph" } else { "" };
        let header = if compact {
            format!(
                "semantic_search(bm25{graph_tag},{top_k}) → {} results, {} chunks indexed\n",
                results.len(),
                index.doc_count
            )
        } else {
            format!(
                "Semantic search (BM25{graph_tag}): \"{}\" ({} results from {} indexed chunks)\n",
                truncate_query(query, 60),
                results.len(),
                index.doc_count,
            )
        };
        format!("{header}{}", format_search_results(&results, compact))
    }
}

fn dense_search_mode(
    query: &str,
    root: &Path,
    index: &BM25Index,
    top_k: usize,
    compact: bool,
    filter: &SearchFilter,
) -> String {
    #[cfg(feature = "embeddings")]
    {
        let (engine, mut embed_idx) = match load_engine_and_index(root) {
            Ok(v) => v,
            Err(e) => return format!("ERR: {e}"),
        };

        // #512: explicit dense has no BM25 fallback to degrade into, so fail fast
        // with the same actionable hint rather than embed the whole corpus inline
        // under the watchdog (the cold-start runaway). A warm/incremental index
        // passes through untouched.
        if let Some(pending) = cold_start_embed_guard(&embed_idx, index) {
            return dense_build_hint(pending, compact);
        }

        let (aligned, coverage, changed_files) =
            match ensure_embeddings(root, index, engine, &mut embed_idx) {
                Ok(v) => v,
                Err(e) => return format!("ERR: {e}"),
            };

        let backend = match crate::core::dense_backend::DenseBackendKind::try_from_env() {
            Ok(v) => v,
            Err(e) => return format!("ERR: {e}"),
        };

        let filter_fn = |p: &str| filter.matches(p);
        let filter_pred: Option<&dyn Fn(&str) -> bool> = filter
            .is_active()
            .then_some(&filter_fn as &dyn Fn(&str) -> bool);

        let candidate_k = filtered_candidate_k(top_k, filter.is_active());
        let mut results = match crate::core::dense_backend::dense_results_as_hybrid(
            backend,
            root,
            index,
            engine,
            &aligned,
            &changed_files,
            query,
            candidate_k,
            filter_pred,
        ) {
            Ok(v) => v,
            Err(e) => return format!("ERR: {e}"),
        };
        results.truncate(top_k);

        let header = if compact {
            format!(
                "semantic_search(dense,{top_k}) → {} results, {} chunks, embed_cov={:.0}%\n",
                results.len(),
                index.doc_count,
                coverage * 100.0
            )
        } else {
            format!(
                "Semantic search (Dense): \"{}\" ({} results from {} indexed chunks, embeddings coverage {:.0}%)\n",
                truncate_query(query, 60),
                results.len(),
                index.doc_count,
                coverage * 100.0
            )
        };

        format!("{header}{}", format_hybrid_results(&results, compact))
    }
    #[cfg(not(feature = "embeddings"))]
    {
        "ERR: embeddings feature not enabled".to_string()
    }
}

#[cfg(feature = "embeddings")]
fn load_engine_and_index(
    root: &Path,
) -> Result<(&'static EmbeddingEngine, EmbeddingIndex), String> {
    let cfg = crate::core::config::Config::load();
    let profile = crate::core::config::MemoryProfile::effective(&cfg);
    if !profile.embeddings_enabled() {
        return Err("embeddings disabled by memory_profile=low".into());
    }

    let engine = crate::core::embeddings::shared_engine()
        .ok_or_else(|| "embedding engine load failed".to_string())?;

    let model_name = engine.model_name();
    let mut idx = EmbeddingIndex::load(root)
        .unwrap_or_else(|| EmbeddingIndex::new_with_model(engine.dimensions(), model_name));

    if let Some((stored, current)) = idx.model_mismatch(model_name) {
        tracing::warn!(
            "[embeddings] model changed: {stored} → {current}. Re-indexing all embeddings."
        );
        idx = EmbeddingIndex::new_with_model(engine.dimensions(), model_name);
    } else if idx.dimension_mismatch(engine.dimensions()) {
        tracing::warn!(
            "[embeddings] dimension mismatch: index={}d, engine={}d. Re-indexing.",
            idx.dimensions,
            engine.dimensions()
        );
        idx = EmbeddingIndex::new_with_model(engine.dimensions(), model_name);
    }

    if idx.model_id.is_none() {
        idx.model_id = Some(model_name.to_string());
    }

    Ok((engine, idx))
}

/// Aligned embedding corpus as a single contiguous [`FlatEmbeddings`] allocation,
/// plus coverage and the list of files re-embedded this call. The flat row-major
/// layout gives sequential memory access during dot-product scoring — one
/// dereference instead of the two-level indirection of `Arc<[Vec<f32>]>`.
#[cfg(feature = "embeddings")]
type AlignedEmbeddings = (FlatEmbeddings, f64, Vec<String>);

#[cfg(feature = "embeddings")]
fn ensure_embeddings(
    root: &Path,
    index: &BM25Index,
    engine: &EmbeddingEngine,
    embed_idx: &mut EmbeddingIndex,
) -> Result<AlignedEmbeddings, String> {
    // A resident index whose bodies were shrunk to snippets (post-embedding RAM
    // reclaim) must NEVER drive re-embedding: `files_needing_update` hashes
    // `c.content`, so truncated bodies would falsely flag every file as changed
    // and re-embed 5-line snippets over the full-body vectors persisted earlier
    // this session. Embeddings for exactly these chunks were already built and
    // saved before truncation, and alignment is keyed by (path, start, end) —
    // not content — so we just re-align here. If a file genuinely changed, the
    // BM25 cache fingerprint goes stale and a fresh full-content index (reloaded
    // from disk) replaces this one, restoring the normal re-embed path.
    if index.content_truncated {
        let aligned = embed_idx.get_aligned_flat(&index.chunks).ok_or_else(|| {
            "embedding alignment failed on truncated resident index; \
                 refusing to re-embed snippet-only bodies"
                .to_string()
        })?;
        let coverage = embed_idx.coverage(index.chunks.len());
        return Ok((aligned, coverage, Vec::new()));
    }

    let mut changed_files = embed_idx.files_needing_update(&index.chunks);
    changed_files.sort();
    changed_files.dedup();

    if !changed_files.is_empty() {
        let changed_set: std::collections::HashSet<&str> = changed_files
            .iter()
            .map(std::string::String::as_str)
            .collect();

        let mut changed_indices: Vec<usize> = Vec::new();
        let mut changed_texts: Vec<&str> = Vec::new();
        for (i, c) in index.chunks.iter().enumerate() {
            if changed_set.contains(c.file_path.as_str()) {
                changed_indices.push(i);
                changed_texts.push(&c.content);
            }
        }

        let batch_embeddings = engine
            .embed_batch(&changed_texts)
            .map_err(|e| format!("batch embed failed: {e}"))?;

        let new_embeddings: Vec<(usize, Vec<f32>)> =
            changed_indices.into_iter().zip(batch_embeddings).collect();

        embed_idx.update(&index.chunks, &new_embeddings, &changed_files, None);
        embed_idx
            .save(root)
            .map_err(|e| format!("save embeddings failed: {e}"))?;
    }

    if let Some(aligned) = embed_idx.get_aligned_flat(&index.chunks) {
        let coverage = embed_idx.coverage(index.chunks.len());
        return Ok((aligned, coverage, changed_files));
    }

    // Alignment missing: rebuild everything once via batched inference.
    let mut all_files: Vec<String> = index.chunks.iter().map(|c| c.file_path.clone()).collect();
    all_files.sort();
    all_files.dedup();

    let all_texts: Vec<&str> = index.chunks.iter().map(|c| c.content.as_str()).collect();
    let batch_embeddings = engine
        .embed_batch(&all_texts)
        .map_err(|e| format!("batch embed failed: {e}"))?;

    let new_embeddings: Vec<(usize, Vec<f32>)> = batch_embeddings.into_iter().enumerate().collect();

    embed_idx.update(&index.chunks, &new_embeddings, &all_files, None);
    embed_idx
        .save(root)
        .map_err(|e| format!("save embeddings failed: {e}"))?;

    let aligned = embed_idx
        .get_aligned_flat(&index.chunks)
        .ok_or_else(|| "embedding alignment failed after full rebuild".to_string())?;
    let coverage = embed_idx.coverage(index.chunks.len());
    Ok((aligned, coverage, all_files))
}

struct SearchFilter {
    allowed_exts: Option<HashSet<String>>,
    path_glob: Option<glob::Pattern>,
}

impl SearchFilter {
    fn new(languages: Option<&[String]>, path_glob: Option<&str>) -> Result<Self, String> {
        let allowed_exts = languages.map(normalize_languages);
        let path_glob = match path_glob {
            None => None,
            Some(s) if s.trim().is_empty() => None,
            Some(s) => Some(glob::Pattern::new(s).map_err(|e| e.msg.to_string())?),
        };
        Ok(Self {
            allowed_exts,
            path_glob,
        })
    }

    fn is_active(&self) -> bool {
        self.allowed_exts.is_some() || self.path_glob.is_some()
    }

    fn matches(&self, rel_path: &str) -> bool {
        let rel_path = rel_path.replace('\\', "/");
        if let Some(p) = &self.path_glob
            && !p.matches(&rel_path)
        {
            return false;
        }
        if let Some(exts) = &self.allowed_exts {
            let ext = Path::new(&rel_path)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_lowercase();
            if ext.is_empty() || !exts.contains(&ext) {
                return false;
            }
        }
        true
    }
}

fn normalize_languages(langs: &[String]) -> HashSet<String> {
    let mut out = HashSet::new();
    for l in langs {
        let raw = l.trim().trim_start_matches('.').to_lowercase();
        match raw.as_str() {
            "rust" | "rs" => {
                out.insert("rs".to_string());
            }
            "ts" | "typescript" => {
                out.insert("ts".to_string());
                out.insert("tsx".to_string());
            }
            "js" | "javascript" => {
                out.insert("js".to_string());
                out.insert("jsx".to_string());
                out.insert("mjs".to_string());
                out.insert("cjs".to_string());
            }
            "py" | "python" => {
                out.insert("py".to_string());
            }
            "go" => {
                out.insert("go".to_string());
            }
            "java" => {
                out.insert("java".to_string());
            }
            "ruby" | "rb" => {
                out.insert("rb".to_string());
            }
            "php" => {
                out.insert("php".to_string());
            }
            "c" => {
                out.insert("c".to_string());
                out.insert("h".to_string());
            }
            "cpp" | "c++" | "cc" => {
                out.insert("cpp".to_string());
                out.insert("hpp".to_string());
                out.insert("cc".to_string());
                out.insert("hh".to_string());
            }
            "cs" | "csharp" => {
                out.insert("cs".to_string());
            }
            "swift" => {
                out.insert("swift".to_string());
            }
            "kt" | "kotlin" => {
                out.insert("kt".to_string());
                out.insert("kts".to_string());
            }
            "json" => {
                out.insert("json".to_string());
            }
            "yaml" | "yml" => {
                out.insert("yaml".to_string());
                out.insert("yml".to_string());
            }
            other if !other.is_empty() => {
                out.insert(other.to_string());
            }
            _ => {}
        }
    }
    out
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

#[cfg(test)]
mod filter_tests {
    use super::*;

    #[test]
    fn filter_language_rust() {
        let f = SearchFilter::new(Some(&["rust".into()]), None).unwrap();
        assert!(f.matches("src/main.rs"));
        assert!(!f.matches("src/main.ts"));
    }

    #[test]
    fn filter_path_glob() {
        let f = SearchFilter::new(None, Some("rust/src/**")).unwrap();
        assert!(f.matches("rust/src/core/mod.rs"));
        assert!(!f.matches("website/src/pages/index.astro"));
    }
}

#[cfg(all(test, feature = "embeddings"))]
mod cold_start_guard_tests {
    use super::*;

    #[test]
    fn budget_zero_disables_guard() {
        // 0 = "always embed inline" (pre-#512 behavior), regardless of size.
        assert!(!exceeds_inline_embed_budget(1_000_000, 0));
    }

    #[test]
    fn budget_is_inclusive_and_triggers_above_threshold() {
        assert!(!exceeds_inline_embed_budget(0, 2000), "warm index: inline");
        assert!(
            !exceeds_inline_embed_budget(2000, 2000),
            "at the budget: still inline"
        );
        assert!(
            exceeds_inline_embed_budget(2001, 2000),
            "over the budget: degrade"
        );
    }

    #[test]
    fn default_threshold_positive_when_env_unset() {
        // With the env override unset the default must be a real, positive guard.
        if std::env::var_os("LEAN_CTX_HYBRID_INLINE_EMBED_MAX").is_none() {
            assert!(inline_embed_max_chunks() >= 1);
        }
    }

    #[test]
    fn dense_build_hint_always_points_at_the_cli_build() {
        let full = dense_build_hint(22_741, false);
        assert!(full.contains("lean-ctx index build-semantic"));
        assert!(full.contains("22741"));
        let compact = dense_build_hint(22_741, true);
        assert!(compact.contains("lean-ctx index build-semantic"));
        assert!(compact.contains("22741"));
    }
}

#[cfg(test)]
mod determinism_tests {
    use super::*;

    #[test]
    fn rrf_merge_hybrid_is_deterministic_on_ties() {
        let a = HybridResult {
            file_path: "a.rs".to_string(),
            symbol_name: "foo".to_string(),
            kind: crate::core::bm25_index::ChunkKind::Function,
            start_line: 1,
            end_line: 1,
            snippet: "a".to_string(),
            rrf_score: 0.0,
            bm25_score: None,
            dense_score: None,
            bm25_rank: None,
            dense_rank: None,
        };
        let b = HybridResult {
            file_path: "b.rs".to_string(),
            symbol_name: "foo".to_string(),
            kind: crate::core::bm25_index::ChunkKind::Function,
            start_line: 1,
            end_line: 1,
            snippet: "b".to_string(),
            rrf_score: 0.0,
            bm25_score: None,
            dense_score: None,
            bm25_rank: None,
            dense_rank: None,
        };

        // Two lists with swapped ranks yield identical RRF sums for a and b.
        let fused = rrf_merge_hybrid(
            vec![
                ("root".to_string(), vec![a.clone(), b.clone()]),
                ("root".to_string(), vec![b.clone(), a.clone()]),
            ],
            10,
        );

        assert_eq!(fused.len(), 2);
        assert_eq!(fused[0].file_path, "a.rs");
        assert_eq!(fused[1].file_path, "b.rs");
    }
}

#[cfg(test)]
mod dense_config_tests {
    use super::*;

    /// #686: dense stays on by default — the flip is opt-in, no behavior change.
    #[test]
    fn dense_enabled_defaults_true() {
        assert!(HybridConfig::default().dense_enabled);
    }

    /// #686: `[search].dense_enabled = false` parses and leaves siblings at default.
    #[test]
    fn dense_enabled_deserializes_false() {
        let cfg: HybridConfig = toml::from_str("dense_enabled = false").unwrap();
        assert!(!cfg.dense_enabled);
        assert_eq!(cfg.bm25_candidates, 75);
        assert_eq!(cfg.splade_weight, 0.5);
    }
}

#[cfg(all(test, feature = "embeddings"))]
mod dense_toggle_tests {
    use super::*;
    use crate::core::bm25_index::{BM25Index, ChunkKind, CodeChunk, tokenize};

    fn small_index() -> BM25Index {
        BM25Index::from_chunks_for_test(vec![
            CodeChunk {
                file_path: "auth.rs".into(),
                symbol_name: "validate_token".into(),
                kind: ChunkKind::Function,
                start_line: 1,
                end_line: 10,
                content: "fn validate_token(token: &str) -> bool { check_jwt_expiry(token) }"
                    .into(),
                tokens: tokenize("fn validate_token token str bool check_jwt_expiry token"),
                token_count: 0,
            },
            CodeChunk {
                file_path: "db.rs".into(),
                symbol_name: "connect_database".into(),
                kind: ChunkKind::Function,
                start_line: 1,
                end_line: 5,
                content: "fn connect_database(url: &str) -> Pool { create_pool(url) }".into(),
                tokens: tokenize("fn connect_database url str Pool create_pool url"),
                token_count: 0,
            },
        ])
    }

    /// #686: the dense-disabled body ranks via BM25 (+ graph + rerank + SPLADE),
    /// emits a BM25 header, finds the lexical match, and crucially never loads the
    /// embedding engine or writes `embeddings.json` — the on-disk vector footprint
    /// and embed latency disappear.
    #[test]
    fn bm25_graph_search_ranks_without_embeddings() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let index = small_index();
        let cfg = HybridConfig {
            dense_enabled: false,
            ..Default::default()
        };
        let filter = SearchFilter::new(None, None).unwrap();

        let out = bm25_graph_search(
            "jwt token validation",
            root,
            &index,
            5,
            false,
            &filter,
            &cfg,
        );

        assert!(
            out.contains("Semantic search (BM25"),
            "expected BM25 header, got: {out}"
        );
        assert!(
            out.contains("validate_token"),
            "expected lexical match, got: {out}"
        );
        assert!(
            !root.join("embeddings.json").exists(),
            "dense-disabled path must not persist embeddings.json"
        );
    }
}
