//! Workspace/artifacts search across roots + RRF fusion (hybrid & BM25),
//! graph-signal ranks and SPLADE boosting.

use std::fmt::Write;
use std::path::Path;

use crate::core::bm25_index::{BM25Index, format_search_results};
#[cfg(feature = "embeddings")]
use crate::core::hybrid_search::HybridConfig;
use crate::core::hybrid_search::{HybridResult, format_hybrid_results};

#[allow(clippy::wildcard_imports)]
use super::*;

pub(crate) const WORKSPACE_RRF_K: f64 = 60.0;

pub(crate) fn artifacts_search(
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

pub(crate) fn workspace_search(
    query: &str,
    root: &Path,
    top_k: usize,
    compact: bool,
    filter: &SearchFilter,
    mode: &str,
) -> String {
    let linked = crate::core::workspace_config::load_linked_projects(root);
    #[cfg(feature = "embeddings")]
    let mut warnings = linked.warnings;
    #[cfg(not(feature = "embeddings"))]
    let warnings = linked.warnings;

    let mut roots: Vec<std::path::PathBuf> = vec![root.to_path_buf()];
    roots.extend(linked.roots);
    roots.sort();
    roots.dedup();

    let mut per_project: Vec<(String, Vec<HybridResult>)> = Vec::new();
    #[cfg(feature = "embeddings")]
    let mut avg_cov: Option<f64> = None;
    #[cfg(not(feature = "embeddings"))]
    let avg_cov: Option<f64> = None;
    #[cfg(feature = "embeddings")]
    let mut cov_count = 0usize;
    #[cfg(not(feature = "embeddings"))]
    let cov_count = 0usize;

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

pub(crate) fn rrf_merge_hybrid(
    lists: Vec<(String, Vec<HybridResult>)>,
    top_k: usize,
) -> Vec<HybridResult> {
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

pub(crate) fn rrf_merge_bm25(
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
pub(crate) fn dense_results_for_root(
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
pub(crate) fn hybrid_results_for_root(
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

    boost_with_complexity(&mut results, root.to_str().unwrap_or(""), 0.3);

    results.truncate(top_k);
    Ok((results, coverage))
}

/// Boost existing hybrid results with SPLADE expansion scores.
pub(crate) fn boost_with_splade(
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

/// Boost results by code-health complexity signal (#877, #882).
///
/// Symbols with higher cognitive complexity (handle, dispatch, orchestration
/// functions) are more likely edit targets than small leaf helpers sharing
/// vocabulary with the query.
#[cfg(feature = "embeddings")]
pub(crate) fn boost_with_complexity(results: &mut [HybridResult], project_root: &str, weight: f64) {
    if weight <= 0.0 || results.is_empty() {
        return;
    }
    for r in results.iter_mut() {
        if r.symbol_name.is_empty() {
            continue;
        }
        if let Some(cc) = crate::core::code_health::fabric::hotspot_cc(project_root, &r.symbol_name)
        {
            let boost = weight * (1.0 + cc as f64).ln() / 100.0;
            r.rrf_score += boost;
        }
    }
    results.sort_by(|a, b| {
        b.rrf_score
            .partial_cmp(&a.rrf_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
}

pub(crate) fn label_for_root(root: &Path) -> String {
    root.file_name()
        .and_then(|s| s.to_str())
        .map(str::to_string)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| root.to_string_lossy().to_string())
}

pub(crate) fn graph_rrf_ranks_for_search_root(
    root: &Path,
) -> Option<std::collections::HashMap<String, usize>> {
    let root_s = root.to_string_lossy().to_string();
    // The caller's own working set, never another agent's (I07/S03).
    let recent: Vec<String> = crate::core::session::caller::with_for_project(&root_s, |session| {
        session
            .files_touched
            .iter()
            .rev()
            .filter(|f| path_under_search_root(&f.path, root))
            .take(12)
            .map(|f| f.path.clone())
            .collect()
    })?;

    if recent.is_empty() {
        return None;
    }

    crate::core::graph_context::graph_neighbor_ranks_for_recent_files(&root_s, &recent, 40, 120)
}

pub(crate) fn path_under_search_root(path: &str, root: &Path) -> bool {
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
pub(crate) fn bm25_graph_search(
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
    boost_with_complexity(&mut results, root.to_str().unwrap_or(""), 0.3);

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
