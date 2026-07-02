//! Cross-repo search — BM25 (legacy) and hybrid (dense + SPLADE + RRF) modes.
//!
//! The hybrid path (GL#1133) routes every root through the same per-root
//! retrieval stack as `ctx_search action=semantic` (`hybrid_results_for_root`:
//! BM25 + dense backend + SPLADE boost + graph ranks), then fuses the per-root
//! rankings with Reciprocal Rank Fusion — the multi-repo twin of the linked
//! workspace search in `ctx_semantic_search::multi_root`. A root whose dense
//! index is cold or whose engine is unavailable degrades to its BM25 ranking
//! with a warning; a query never fails or inline-embeds because one root has
//! no vectors yet (#512 semantics).

use std::collections::HashMap;
use std::path::PathBuf;

use crate::core::multi_repo::{FusedSearchResult, format_fused_results, global_manager};

/// Per-root candidate depth: mirror `MultiRepoManager::search` so BM25 and
/// hybrid modes examine equally deep per-root rankings before fusion.
fn per_root_k(max_results: usize) -> usize {
    (max_results * 2).max(20)
}

pub(super) fn handle_search(
    query: Option<&str>,
    max_results: usize,
    roots_filter: Option<&[String]>,
    mode: Option<&str>,
) -> (String, usize) {
    let Some(query) = query else {
        return ("ERROR: query is required for search".to_string(), 0);
    };

    let mode = mode.unwrap_or("hybrid").trim().to_ascii_lowercase();
    match mode.as_str() {
        "bm25" => bm25_search(query, max_results, roots_filter),
        "hybrid" => hybrid_search(query, max_results, roots_filter),
        other => (
            format!("ERROR: unknown search mode '{other}' (expected 'bm25' or 'hybrid')"),
            0,
        ),
    }
}

/// Legacy lexical path — byte-identical output to the pre-hybrid tool.
fn bm25_search(
    query: &str,
    max_results: usize,
    roots_filter: Option<&[String]>,
) -> (String, usize) {
    let manager = global_manager();
    let Ok(mut mgr) = manager.lock() else {
        return ("ERROR: failed to acquire multi-repo lock".to_string(), 0);
    };

    if mgr.root_count() == 0 {
        return (
            "ERROR: no repo roots configured. Use add_root first.".to_string(),
            0,
        );
    }

    let results = mgr.search(query, max_results, roots_filter);
    let output = format_fused_results(&results);
    let tokens = crate::core::tokens::count_tokens(&output);
    (output, tokens)
}

fn hybrid_search(
    query: &str,
    max_results: usize,
    roots_filter: Option<&[String]>,
) -> (String, usize) {
    // Snapshot roots under a short lock; the per-root retrieval below can touch
    // embedding indexes and must never run while holding the manager mutex.
    let (roots, rrf_k) = {
        let manager = global_manager();
        let Ok(mgr) = manager.lock() else {
            return ("ERROR: failed to acquire multi-repo lock".to_string(), 0);
        };
        if mgr.root_count() == 0 {
            return (
                "ERROR: no repo roots configured. Use add_root first.".to_string(),
                0,
            );
        }
        let roots: Vec<(String, PathBuf)> = mgr
            .list_roots()
            .into_iter()
            .filter(|r| {
                roots_filter
                    .is_none_or(|filter| filter.iter().any(|f| f == &r.alias || f == &r.path))
            })
            .map(|r| (r.alias, PathBuf::from(r.path)))
            .collect();
        (roots, mgr.rrf_k())
    };

    let (fused, searched_roots, warnings) = search_roots_hybrid(query, &roots, max_results, rrf_k);

    let mut output = format!(
        "Cross-repo hybrid search (BM25+dense+SPLADE, RRF over {searched_roots} roots):\n{}",
        format_fused_results(&fused)
    );
    if !warnings.is_empty() {
        output.push_str(&format!("\nWarnings ({}):\n", warnings.len()));
        for w in warnings.iter().take(8) {
            output.push_str(&format!("- {w}\n"));
        }
    }
    let tokens = crate::core::tokens::count_tokens(&output);
    (output, tokens)
}

/// Hybrid retrieval per root + RRF fusion across roots. Pure with respect to
/// the manager (roots are an explicit snapshot) so tests can drive it directly.
fn search_roots_hybrid(
    query: &str,
    roots: &[(String, PathBuf)],
    max_results: usize,
    rrf_k: f64,
) -> (Vec<FusedSearchResult>, usize, Vec<String>) {
    let k = per_root_k(max_results);
    let mut warnings: Vec<String> = Vec::new();
    let mut per_root: Vec<(
        String,
        String,
        Vec<crate::core::hybrid_search::HybridResult>,
    )> = Vec::new();

    for (alias, path) in roots {
        let index = crate::core::bm25_index::BM25Index::load_or_build(path);
        if index.doc_count == 0 {
            continue;
        }
        let (results, warning) = per_root_hybrid(query, path, &index, k);
        if let Some(w) = warning {
            warnings.push(format!("[{alias}] {w}"));
        }
        per_root.push((alias.clone(), path.to_string_lossy().to_string(), results));
    }

    let searched = per_root.len();
    (
        fuse_repo_lists(per_root, max_results, rrf_k),
        searched,
        warnings,
    )
}

/// One root, ranked: the same stack as single-root semantic search. Falls back
/// to plain BM25 (with a warning) when the hybrid path errors — cold dense
/// index, unavailable engine, low memory profile.
#[cfg(feature = "embeddings")]
fn per_root_hybrid(
    query: &str,
    root: &std::path::Path,
    index: &crate::core::bm25_index::BM25Index,
    k: usize,
) -> (
    Vec<crate::core::hybrid_search::HybridResult>,
    Option<String>,
) {
    use crate::tools::ctx_semantic_search::multi_root;

    let cfg = crate::core::hybrid_search::HybridConfig::from_config();
    if !cfg.dense_enabled {
        // Dense globally disabled (#686): BM25 + SPLADE, no engine, no vectors.
        let mut results =
            crate::core::hybrid_search::hybrid_search(query, index, None, None, k, &cfg, None);
        if cfg.splade_weight > 0.0 {
            let splade = crate::core::splade_retrieval::hybrid_retrieve(query, index, k);
            if !splade.is_empty() {
                multi_root::boost_with_splade(&mut results, &splade, cfg.splade_weight);
            }
        }
        results.truncate(k);
        return (results, None);
    }

    let filter = match crate::tools::ctx_semantic_search::SearchFilter::new(None, None) {
        Ok(f) => f,
        Err(e) => return (bm25_ranking(index, query, k), Some(e)),
    };
    match multi_root::hybrid_results_for_root(query, root, index, k, &filter) {
        Ok((results, _coverage)) => (results, None),
        Err(e) => (
            bm25_ranking(index, query, k),
            Some(format!("hybrid failed: {e}; degraded to BM25")),
        ),
    }
}

#[cfg(not(feature = "embeddings"))]
fn per_root_hybrid(
    query: &str,
    _root: &std::path::Path,
    index: &crate::core::bm25_index::BM25Index,
    k: usize,
) -> (
    Vec<crate::core::hybrid_search::HybridResult>,
    Option<String>,
) {
    (bm25_ranking(index, query, k), None)
}

fn bm25_ranking(
    index: &crate::core::bm25_index::BM25Index,
    query: &str,
    k: usize,
) -> Vec<crate::core::hybrid_search::HybridResult> {
    index
        .search(query, k)
        .into_iter()
        .map(crate::core::hybrid_search::HybridResult::from_bm25_public)
        .collect()
}

/// Reciprocal Rank Fusion across per-root ranked lists. Key + score semantics
/// mirror `MultiRepoManager::search` (`alias:file:start_line`, `1/(k+rank+1)`),
/// so BM25 and hybrid modes fuse identically — only the per-root ranking
/// signal differs. Ties break deterministically (#498).
fn fuse_repo_lists(
    lists: Vec<(
        String,
        String,
        Vec<crate::core::hybrid_search::HybridResult>,
    )>,
    max_results: usize,
    rrf_k: f64,
) -> Vec<FusedSearchResult> {
    let mut acc: HashMap<String, FusedSearchResult> = HashMap::new();

    for (alias, repo_path, results) in lists {
        for (rank, r) in results.into_iter().enumerate() {
            let contribution = 1.0 / (rrf_k + rank as f64 + 1.0);
            let key = format!("{alias}:{}:{}", r.file_path, r.start_line);
            acc.entry(key)
                .and_modify(|existing| existing.rrf_score += contribution)
                .or_insert_with(|| FusedSearchResult {
                    repo_alias: alias.clone(),
                    repo_path: repo_path.clone(),
                    file_path: r.file_path,
                    symbol_name: r.symbol_name,
                    content: r.snippet,
                    start_line: r.start_line,
                    end_line: r.end_line,
                    rrf_score: contribution,
                });
        }
    }

    let mut fused: Vec<FusedSearchResult> = acc.into_values().collect();
    fused.sort_by(|a, b| {
        b.rrf_score
            .partial_cmp(&a.rrf_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.repo_alias.cmp(&b.repo_alias))
            .then_with(|| a.file_path.cmp(&b.file_path))
            .then_with(|| a.start_line.cmp(&b.start_line))
    });
    fused.truncate(max_results);
    fused
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::hybrid_search::HybridResult;

    fn hit(file: &str, symbol: &str, start: usize) -> HybridResult {
        HybridResult {
            file_path: file.to_string(),
            symbol_name: symbol.to_string(),
            kind: crate::core::bm25_index::ChunkKind::Function,
            start_line: start,
            end_line: start + 5,
            snippet: format!("fn {symbol}() {{}}"),
            rrf_score: 0.0,
            bm25_score: None,
            dense_score: None,
            bm25_rank: None,
            dense_rank: None,
        }
    }

    #[test]
    fn fuse_repo_lists_ranks_shared_hits_first() {
        let backend = (
            "backend".to_string(),
            "/repos/backend".to_string(),
            vec![hit("src/auth.rs", "login", 1), hit("src/db.rs", "pool", 1)],
        );
        let frontend = (
            "frontend".to_string(),
            "/repos/frontend".to_string(),
            vec![hit("src/api.ts", "login", 1)],
        );

        let fused = fuse_repo_lists(vec![backend, frontend], 10, 60.0);

        assert_eq!(fused.len(), 3);
        // Rank-0 hits from both repos tie on score; alias breaks the tie
        // deterministically (backend < frontend).
        assert_eq!(fused[0].repo_alias, "backend");
        assert_eq!(fused[0].file_path, "src/auth.rs");
        assert_eq!(fused[1].repo_alias, "frontend");
        assert!((fused[0].rrf_score - fused[1].rrf_score).abs() < f64::EPSILON);
        assert!(fused[1].rrf_score > fused[2].rrf_score);
    }

    #[test]
    fn fuse_repo_lists_deduplicates_same_chunk_within_repo() {
        let lists = vec![(
            "app".to_string(),
            "/repos/app".to_string(),
            vec![hit("src/a.rs", "f", 1), hit("src/a.rs", "f", 1)],
        )];

        let fused = fuse_repo_lists(lists, 10, 60.0);

        assert_eq!(fused.len(), 1);
        let expected = 1.0 / 61.0 + 1.0 / 62.0;
        assert!((fused[0].rrf_score - expected).abs() < 1e-12);
    }

    #[test]
    fn fuse_repo_lists_truncates_to_max_results() {
        let lists = vec![(
            "app".to_string(),
            "/repos/app".to_string(),
            (0..30).map(|i| hit(&format!("f{i}.rs"), "f", 1)).collect(),
        )];
        let fused = fuse_repo_lists(lists, 5, 60.0);
        assert_eq!(fused.len(), 5);
    }

    #[test]
    fn search_roots_hybrid_skips_empty_roots_and_warns_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let roots = vec![("empty".to_string(), dir.path().to_path_buf())];
        let (fused, searched, warnings) = search_roots_hybrid("query", &roots, 10, 60.0);
        assert!(fused.is_empty());
        assert_eq!(searched, 0);
        assert!(warnings.is_empty());
    }

    #[test]
    fn handle_search_rejects_unknown_mode() {
        let (out, _) = handle_search(Some("q"), 10, None, Some("vector"));
        assert!(out.contains("unknown search mode"));
    }

    #[test]
    fn per_root_k_has_floor() {
        assert_eq!(per_root_k(5), 20);
        assert_eq!(per_root_k(50), 100);
    }
}
