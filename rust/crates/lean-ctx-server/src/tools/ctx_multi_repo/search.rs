// SPDX-License-Identifier: Apache-2.0
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

use lean_ctx::core::multi_repo::{FusedSearchResult, format_fused_results, global_manager};

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
    // A malformed call is answered as such, whatever the content policy state.
    let Some(query) = query else {
        return ("ERROR: query is required for search".to_string(), 0);
    };
    let mode = mode.unwrap_or("hybrid").trim().to_ascii_lowercase();
    if mode != "bm25" && mode != "hybrid" {
        return (
            format!("ERROR: unknown search mode '{mode}' (expected 'bm25' or 'hybrid')"),
            0,
        );
    }
    if let Err(refused) = request_admits_other_projects() {
        return (refused, 0);
    }
    let answer = match mode.as_str() {
        "bm25" => bm25_search(query, max_results, roots_filter),
        _ => hybrid_search(query, max_results, roots_filter),
    };
    // Checked again once the roots were read: a policy that came into force
    // on the requesting project meanwhile withholds what was found.
    if let Err(refused) = request_admits_other_projects() {
        return (refused, 0);
    }
    answer
}

/// The requesting project's own policy decides whether results of other
/// projects may enter it at all. A request bound to no project (an MCP
/// client started in a workspace folder that is no project) has no such
/// policy: every root is still judged under its own view (#2244).
fn request_admits_other_projects() -> Result<(), String> {
    use lean_ctx::core::policy::runtime;
    match runtime::with_source_view(runtime::is_active) {
        Ok(false) => Ok(()),
        Err(e) if e == "project_unresolved" => Ok(()),
        Ok(true) => Err(
            "ERROR: cross-project search withheld under content policy until each source has verified provenance; use local ctx_search mode=bm25".into(),
        ),
        Err(e) => Err(format!(
            "ERROR: search source authority changed or is unavailable ({e})"
        )),
    }
}

/// Run one root's search under that root's own source view (#2244): a root
/// whose policy is active, or whose authority cannot be established, is left
/// out with a warning naming why — never searched under another project's
/// view, never failing the roots that are admissible.
fn in_root_view<T: Default>(
    alias: &str,
    root: &std::path::Path,
    warnings: &mut Vec<String>,
    search: impl FnOnce() -> T,
) -> T {
    use lean_ctx::core::policy::runtime;
    match runtime::with_root_source_view(root, || (!runtime::is_active()).then(search)) {
        Ok(Some(found)) => found,
        Ok(None) => {
            warnings.push(format!(
                "[{alias}] withheld under its content policy until its sources have verified provenance"
            ));
            T::default()
        }
        Err(e) => {
            warnings.push(format!("[{alias}] source authority unavailable: {e}"));
            T::default()
        }
    }
}

fn push_warnings(output: &mut String, warnings: &[String]) {
    if !warnings.is_empty() {
        output.push_str(&format!("\nWarnings ({}):\n", warnings.len()));
        for w in warnings.iter().take(8) {
            output.push_str(&format!("- {w}\n"));
        }
    }
}

/// Legacy lexical path — byte-identical output to the pre-hybrid tool when
/// every root is admissible.
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

    let mut warnings = Vec::new();
    let results = mgr.search_with(query, max_results, roots_filter, |alias, root, search| {
        in_root_view(alias, root, &mut warnings, search)
    });
    drop(mgr);
    let mut output = format_fused_results(&results);
    push_warnings(&mut output, &warnings);
    let tokens = lean_ctx::core::tokens::count_tokens(&output);
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
    push_warnings(&mut output, &warnings);
    let tokens = lean_ctx::core::tokens::count_tokens(&output);
    (output, tokens)
}

/// Hybrid retrieval per root + RRF fusion across roots. Pure with respect to
/// the manager (roots are an explicit snapshot) so tests can drive it directly.
/// Every root is ranked under its own source view ([`in_root_view`]).
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
        Vec<lean_ctx::core::hybrid_search::HybridResult>,
    )> = Vec::new();

    for (alias, path) in roots {
        let ranked = in_root_view(alias, path, &mut warnings, || {
            let index = lean_ctx::core::bm25_index::BM25Index::load_or_build(path);
            if index.doc_count == 0 {
                return None;
            }
            Some(per_root_hybrid(query, path, &index, k))
        });
        let Some((results, warning)) = ranked else {
            continue;
        };
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
    index: &lean_ctx::core::bm25_index::BM25Index,
    k: usize,
) -> (
    Vec<lean_ctx::core::hybrid_search::HybridResult>,
    Option<String>,
) {
    use crate::tools::ctx_semantic_search::multi_root;

    let cfg = lean_ctx::core::hybrid_search::HybridConfig::from_config();
    if !cfg.dense_enabled {
        // Dense globally disabled (#686): BM25 + SPLADE, no engine, no vectors.
        let mut results =
            lean_ctx::core::hybrid_search::hybrid_search(query, index, None, None, k, &cfg, None);
        if cfg.splade_weight > 0.0 {
            let splade = lean_ctx::core::splade_retrieval::hybrid_retrieve(query, index, k);
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
    index: &lean_ctx::core::bm25_index::BM25Index,
    k: usize,
) -> (
    Vec<lean_ctx::core::hybrid_search::HybridResult>,
    Option<String>,
) {
    (bm25_ranking(index, query, k), None)
}

fn bm25_ranking(
    index: &lean_ctx::core::bm25_index::BM25Index,
    query: &str,
    k: usize,
) -> Vec<lean_ctx::core::hybrid_search::HybridResult> {
    index
        .search(query, k)
        .into_iter()
        .map(lean_ctx::core::hybrid_search::HybridResult::from_bm25_public)
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
        Vec<lean_ctx::core::hybrid_search::HybridResult>,
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
    use lean_ctx::core::hybrid_search::HybridResult;

    fn hit(file: &str, symbol: &str, start: usize) -> HybridResult {
        HybridResult {
            file_path: file.to_string(),
            symbol_name: symbol.to_string(),
            kind: lean_ctx::core::bm25_index::ChunkKind::Function,
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
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn handle_search_rejects_unknown_mode() {
        // The search answers under the process environment and policy (a
        // pinned protected session, an active pack): both held, in the
        // order environment then policy, so no other test's state decides.
        // Name the request project instead of reading the shared cwd.
        let _env = lean_ctx::core::data_dir::test_env_lock();
        let _community = lean_ctx::core::policy::runtime::TestPolicyOverride::set(None);
        let project = tempfile::tempdir().unwrap();
        let (out, _) = lean_ctx::core::policy::runtime::REQUEST_PROJECT.sync_scope(
            std::cell::RefCell::new(Some(project.path().to_path_buf())),
            || handle_search(Some("q"), 10, None, Some("vector")),
        );
        assert!(out.contains("unknown search mode"), "{out}");
    }

    /// #2244: a search over two registered roots answered every call with
    /// "search source authority changed or is unavailable" — the request
    /// project's source view was held over the other root, whose index gate
    /// rebinds the request project and so ends that view. Both modes, with the
    /// request bound to one of the roots and to a workspace folder that is
    /// neither, must return hits of both roots.
    #[test]
    fn search_spans_every_registered_root_gh2244() {
        let _env = lean_ctx::core::data_dir::test_env_lock();
        let _community = lean_ctx::core::policy::runtime::TestPolicyOverride::set(None);
        let ws = tempfile::tempdir().unwrap();
        let mut roots = Vec::new();
        for name in ["gh2244-web", "gh2244-api"] {
            let root = ws.path().join(name);
            std::fs::create_dir_all(root.join("src")).unwrap();
            std::fs::write(
                root.join("src/hub.ts"),
                format!("export function startHub() {{ return \"signalrneedle {name}\"; }}\n"),
            )
            .unwrap();
            roots.push(root.canonicalize().unwrap());
        }
        {
            let mut mgr = global_manager().lock().unwrap();
            for root in &roots {
                mgr.add_root(&root.to_string_lossy(), None).unwrap();
            }
        }
        let mut failures = Vec::new();
        for bound in [
            Some(roots[0].clone()),
            Some(ws.path().canonicalize().unwrap()),
            None,
        ] {
            for mode in ["bm25", "hybrid"] {
                let (out, _) = lean_ctx::core::policy::runtime::REQUEST_PROJECT
                    .sync_scope(std::cell::RefCell::new(bound.clone()), || {
                        handle_search(Some("signalrneedle"), 10, None, Some(mode))
                    });
                if !(out.contains("[gh2244-web]") && out.contains("[gh2244-api]")) {
                    failures.push(format!("bound={bound:?} mode={mode}:\n{out}"));
                }
            }
        }
        {
            let mut mgr = global_manager().lock().unwrap();
            for root in &roots {
                let _ = mgr.remove_root(&root.to_string_lossy());
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n---\n"));
    }

    /// #2244: the per-root views never weaken the policy gate. A request
    /// bound to a protected project is refused as a whole; a request bound to
    /// no project reads no protected root and names each one in a warning.
    #[test]
    fn protected_roots_stay_withheld_gh2244() {
        let _env = lean_ctx::core::data_dir::test_env_lock();
        let _protected = lean_ctx::core::policy::content::protected_for_test("");
        let ws = tempfile::tempdir().unwrap();
        let mut roots = Vec::new();
        for name in ["gh2244-locked-a", "gh2244-locked-b"] {
            let root = ws.path().join(name);
            std::fs::create_dir_all(root.join("src")).unwrap();
            std::fs::write(
                root.join("src/vault.ts"),
                "export const secret = \"lockedneedle\";\n",
            )
            .unwrap();
            roots.push(root.canonicalize().unwrap());
        }
        {
            let mut mgr = global_manager().lock().unwrap();
            for root in &roots {
                mgr.add_root(&root.to_string_lossy(), None).unwrap();
            }
        }
        let mut failures = Vec::new();
        for mode in ["bm25", "hybrid"] {
            let (bound, _) = lean_ctx::core::policy::runtime::REQUEST_PROJECT
                .sync_scope(std::cell::RefCell::new(Some(roots[0].clone())), || {
                    handle_search(Some("lockedneedle"), 10, None, Some(mode))
                });
            if !bound.contains("withheld under content policy") || bound.contains("vault.ts") {
                failures.push(format!("bound mode={mode}:\n{bound}"));
            }
            let (unbound, _) = lean_ctx::core::policy::runtime::REQUEST_PROJECT
                .sync_scope(std::cell::RefCell::new(None), || {
                    handle_search(Some("lockedneedle"), 10, None, Some(mode))
                });
            if unbound.contains("vault.ts")
                || !unbound.contains("[gh2244-locked-a] withheld")
                || !unbound.contains("[gh2244-locked-b] withheld")
            {
                failures.push(format!("unbound mode={mode}:\n{unbound}"));
            }
        }
        {
            let mut mgr = global_manager().lock().unwrap();
            for root in &roots {
                let _ = mgr.remove_root(&root.to_string_lossy());
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n---\n"));
    }

    #[test]
    fn per_root_k_has_floor() {
        assert_eq!(per_root_k(5), 20);
        assert_eq!(per_root_k(50), 100);
    }
}
