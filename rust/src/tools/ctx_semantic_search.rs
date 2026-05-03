use std::collections::HashSet;
use std::path::Path;
use std::sync::OnceLock;

use crate::core::embedding_index::EmbeddingIndex;
#[cfg(feature = "embeddings")]
use crate::core::embeddings::EmbeddingEngine;
use crate::core::hybrid_search::{format_hybrid_results, HybridConfig, HybridResult};
use crate::core::vector_index::{format_search_results, BM25Index};
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

    let filter = match SearchFilter::new(languages, path_glob) {
        Ok(f) => f,
        Err(e) => return format!("ERR: invalid filter: {e}"),
    };

    let compact = crp_mode.is_tdd();
    let mode = mode.unwrap_or("hybrid").to_lowercase();
    let workspace = workspace.unwrap_or(false);
    let artifacts = artifacts.unwrap_or(false);

    if artifacts {
        return artifacts_search(query, root, top_k, compact, &filter, workspace);
    }
    if workspace {
        return workspace_search(query, root, top_k, compact, &filter, &mode);
    }

    let index = load_or_refresh_bm25(root);
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
        "dense" => dense_search_mode(query, root, &index, top_k, compact, &filter),
        _ => hybrid_search_mode(query, root, &index, top_k, compact, &filter),
    }
}

/// Rebuilds the BM25 search index for the given directory from scratch.
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
    let files = idx.files.len();
    let chunks = idx.doc_count;
    let _ = idx.save(root);

    format!("Reindexed {path}: {files} files, {chunks} chunks")
}

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

fn truncate_query(q: &str, max: usize) -> &str {
    if q.len() <= max {
        return q;
    }
    match q.char_indices().nth(max) {
        Some((byte_idx, _)) => &q[..byte_idx],
        None => q,
    }
}

fn load_or_refresh_bm25(root: &Path) -> BM25Index {
    BM25Index::load_or_build(root)
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

    let mut per_project: Vec<(String, Vec<crate::core::vector_index::SearchResult>)> = Vec::new();
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

    let mut fused: Vec<crate::core::vector_index::SearchResult> = if per_project.len() <= 1 {
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
        out.push_str(&format!("\nWarnings ({}):\n", warnings.len()));
        for w in warnings.iter().take(20) {
            out.push_str(&format!("- {w}\n"));
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
        let index = BM25Index::load_or_build(r);
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
    });
    out.truncate(top_k);
    out
}

fn rrf_merge_bm25(
    lists: Vec<(String, Vec<crate::core::vector_index::SearchResult>)>,
    top_k: usize,
) -> Vec<crate::core::vector_index::SearchResult> {
    use std::collections::HashMap;

    let mut acc: HashMap<String, (crate::core::vector_index::SearchResult, f64)> = HashMap::new();
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

    let mut out: Vec<crate::core::vector_index::SearchResult> = acc
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
    let (aligned, coverage, changed_files) =
        ensure_embeddings(root, index, engine, &mut embed_idx)?;

    let backend = crate::core::dense_backend::DenseBackendKind::try_from_env()?;
    let cfg = HybridConfig::default();
    let filter_fn = |p: &str| filter.matches(p);
    let filter_pred: Option<&dyn Fn(&str) -> bool> = filter
        .is_active()
        .then_some(&filter_fn as &dyn Fn(&str) -> bool);
    let candidate_k = filtered_candidate_k(top_k, filter.is_active());
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
    )?;
    results.truncate(top_k);
    Ok((results, coverage))
}

fn label_for_root(root: &Path) -> String {
    root.file_name()
        .and_then(|s| s.to_str())
        .map(str::to_string)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| root.to_string_lossy().to_string())
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
        let (engine, mut embed_idx) = match load_engine_and_index(root) {
            Ok(v) => v,
            Err(e) => return format!("ERR: {e}"),
        };

        let (aligned, coverage, changed_files) =
            match ensure_embeddings(root, index, engine, &mut embed_idx) {
                Ok(v) => v,
                Err(e) => return format!("ERR: {e}"),
            };

        let backend = match crate::core::dense_backend::DenseBackendKind::try_from_env() {
            Ok(v) => v,
            Err(e) => return format!("ERR: {e}"),
        };

        let cfg = HybridConfig::default();
        let filter_fn = |p: &str| filter.matches(p);
        let filter_pred: Option<&dyn Fn(&str) -> bool> = filter
            .is_active()
            .then_some(&filter_fn as &dyn Fn(&str) -> bool);
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
        ) {
            Ok(v) => v,
            Err(e) => return format!("ERR: {e}"),
        };
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
    static ENGINE: OnceLock<anyhow::Result<EmbeddingEngine>> = OnceLock::new();
    let engine = ENGINE
        .get_or_init(EmbeddingEngine::load_default)
        .as_ref()
        .map_err(|e| format!("embedding engine load failed: {e}"))?;

    let mut idx =
        EmbeddingIndex::load(root).unwrap_or_else(|| EmbeddingIndex::new(engine.dimensions()));
    if idx.dimensions != engine.dimensions() {
        idx = EmbeddingIndex::new(engine.dimensions());
    }
    Ok((engine, idx))
}

#[cfg(feature = "embeddings")]
fn ensure_embeddings(
    root: &Path,
    index: &BM25Index,
    engine: &EmbeddingEngine,
    embed_idx: &mut EmbeddingIndex,
) -> Result<(Vec<Vec<f32>>, f64, Vec<String>), String> {
    let mut changed_files = embed_idx.files_needing_update(&index.chunks);
    changed_files.sort();
    changed_files.dedup();

    if !changed_files.is_empty() {
        let changed_set: std::collections::HashSet<&str> = changed_files
            .iter()
            .map(std::string::String::as_str)
            .collect();
        let mut new_embeddings: Vec<(usize, Vec<f32>)> = Vec::new();
        for (i, c) in index.chunks.iter().enumerate() {
            if !changed_set.contains(c.file_path.as_str()) {
                continue;
            }
            let emb = engine
                .embed(&c.content)
                .map_err(|e| format!("embed failed for {}: {e}", c.file_path))?;
            new_embeddings.push((i, emb));
        }
        embed_idx.update(&index.chunks, &new_embeddings, &changed_files);
        embed_idx
            .save(root)
            .map_err(|e| format!("save embeddings failed: {e}"))?;
    }

    if let Some(aligned) = embed_idx.get_aligned_embeddings(&index.chunks) {
        let coverage = embed_idx.coverage(index.chunks.len());
        return Ok((aligned, coverage, changed_files));
    }

    // Alignment missing: rebuild everything once.
    let mut all_files: Vec<String> = index.chunks.iter().map(|c| c.file_path.clone()).collect();
    all_files.sort();
    all_files.dedup();

    let mut new_embeddings: Vec<(usize, Vec<f32>)> = Vec::with_capacity(index.chunks.len());
    for (i, c) in index.chunks.iter().enumerate() {
        let emb = engine
            .embed(&c.content)
            .map_err(|e| format!("embed failed for {}: {e}", c.file_path))?;
        new_embeddings.push((i, emb));
    }

    embed_idx.update(&index.chunks, &new_embeddings, &all_files);
    embed_idx
        .save(root)
        .map_err(|e| format!("save embeddings failed: {e}"))?;

    let aligned = embed_idx
        .get_aligned_embeddings(&index.chunks)
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
        if let Some(p) = &self.path_glob {
            if !p.matches(&rel_path) {
                return false;
            }
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
