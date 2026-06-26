//! Retrieval evaluation harness for lean-ctx hybrid search.
//!
//! Runs a standardized `query→expected_file` benchmark to measure Recall@k,
//! MRR (Mean Reciprocal Rank), and latency. Outputs NDJSON scorecards.
//!
//! Usage: `lean-ctx benchmark --eval [path]`

use std::path::Path;
use std::time::Instant;

use crate::core::chunk_data::ChunkData;
use crate::core::hybrid_search::HybridConfig;
use crate::core::tokens::count_tokens;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EvalQuery {
    pub query: String,
    pub expected_files: Vec<String>,
    #[serde(default)]
    pub category: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct EvalResult {
    pub query: String,
    pub category: String,
    pub recall_at_5: f64,
    pub recall_at_10: f64,
    pub mrr: f64,
    pub latency_us: u64,
    pub retrieved_files: Vec<String>,
    pub expected_files: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct EvalScorecard {
    pub project: String,
    pub total_queries: usize,
    pub avg_recall_at_5: f64,
    pub avg_recall_at_10: f64,
    pub avg_mrr: f64,
    pub avg_latency_us: u64,
    pub per_category: Vec<CategoryScore>,
    pub results: Vec<EvalResult>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct CategoryScore {
    pub category: String,
    pub count: usize,
    pub avg_recall_at_5: f64,
    pub avg_mrr: f64,
}

impl std::fmt::Display for EvalScorecard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "Eval: {} ({} queries)", self.project, self.total_queries)?;
        writeln!(f, "  R@5:  {:.1}%", self.avg_recall_at_5 * 100.0)?;
        writeln!(f, "  R@10: {:.1}%", self.avg_recall_at_10 * 100.0)?;
        writeln!(f, "  MRR:  {:.3}", self.avg_mrr)?;
        writeln!(f, "  Latency: {}µs avg", self.avg_latency_us)?;
        for cat in &self.per_category {
            writeln!(
                f,
                "  [{:12}] R@5={:.1}% MRR={:.3} (n={})",
                cat.category,
                cat.avg_recall_at_5 * 100.0,
                cat.avg_mrr,
                cat.count
            )?;
        }
        Ok(())
    }
}

/// Run evaluation using the full hybrid search pipeline (BM25 + embeddings + SPLADE).
/// Falls back to BM25-only if embeddings are not available.
#[must_use]
pub fn run_eval(
    project_root: &Path,
    queries: &[EvalQuery],
    index: &ChunkData,
    config: &HybridConfig,
) -> EvalScorecard {
    let label = project_root
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();

    let mut results = Vec::with_capacity(queries.len());

    for q in queries {
        let start = Instant::now();
        let retrieved = hybrid_eval_search(project_root, &q.query, index, config);
        let latency = start.elapsed().as_micros() as u64;

        let recall_5 = recall_at_k(&retrieved, &q.expected_files, 5);
        let recall_10 = recall_at_k(&retrieved, &q.expected_files, 10);
        let mrr = mean_reciprocal_rank(&retrieved, &q.expected_files);

        results.push(EvalResult {
            query: q.query.clone(),
            category: q.category.clone(),
            recall_at_5: recall_5,
            recall_at_10: recall_10,
            mrr,
            latency_us: latency,
            retrieved_files: retrieved.into_iter().take(10).collect(),
            expected_files: q.expected_files.clone(),
        });
    }

    let total = results.len();
    let avg_r5 = results.iter().map(|r| r.recall_at_5).sum::<f64>() / total.max(1) as f64;
    let avg_r10 = results.iter().map(|r| r.recall_at_10).sum::<f64>() / total.max(1) as f64;
    let avg_mrr = results.iter().map(|r| r.mrr).sum::<f64>() / total.max(1) as f64;
    let avg_lat = results.iter().map(|r| r.latency_us).sum::<u64>() / total.max(1) as u64;

    let per_category = build_category_scores(&results);

    EvalScorecard {
        project: label,
        total_queries: total,
        avg_recall_at_5: avg_r5,
        avg_recall_at_10: avg_r10,
        avg_mrr,
        avg_latency_us: avg_lat,
        per_category,
        results,
    }
}

/// Which retrieval pipeline an eval arm exercises (#686 default-flip decision).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchArm {
    /// Full default pipeline: BM25 + dense embeddings + SPLADE + RRF + rerank.
    Hybrid,
    /// Pure lexical BM25 — the **conservative lower bound** of the lean
    /// (`dense_enabled = false`) path, which additionally keeps graph proximity,
    /// reranking and SPLADE on top. If pure BM25 already matches hybrid, the real
    /// lean path is ≥ that, so flipping the default cannot regress quality.
    Bm25Only,
    /// FastContext-style `ctx_explore`: a bounded multi-turn loop (BM25 anchor +
    /// static graph BFS + AST symbols) returning `path:line` citations. Reported
    /// as a peer arm so the scorecard shows its recall **and** its citation-level
    /// token footprint — the value prop is locating the answer across files at a
    /// fraction of the tokens a body-read would cost.
    Explore,
}

impl SearchArm {
    fn label(self) -> &'static str {
        match self {
            SearchArm::Hybrid => "hybrid (dense on)",
            SearchArm::Bm25Only => "bm25-only (lean lower bound)",
            SearchArm::Explore => "explore (citations)",
        }
    }
}

/// Full hybrid search for eval: BM25 + dense embeddings + SPLADE + RRF.
/// Falls back to BM25-only when embeddings are unavailable.
fn hybrid_eval_search(
    project_root: &Path,
    query: &str,
    index: &ChunkData,
    config: &HybridConfig,
) -> Vec<String> {
    search_arm(project_root, query, index, config, SearchArm::Hybrid).files
}

/// One arm's run: the ranked repo-relative files, whether the dense pipeline
/// actually contributed, and the token footprint of the arm's native output (a
/// newline-joined path list for the search arms; the `<final_answer>` citation
/// block for the explore arm). The token field powers a *recall-per-token* view.
struct ArmRun {
    files: Vec<String>,
    dense_active: bool,
    output_tokens: usize,
}

/// Runs one retrieval arm. Returns the ranked repo-relative file paths, whether
/// the dense pipeline actually contributed (so an A/B run can flag an
/// environment without working embeddings instead of silently comparing BM25 to
/// itself), and the arm's output token footprint.
fn search_arm(
    project_root: &Path,
    query: &str,
    index: &ChunkData,
    config: &HybridConfig,
    arm: SearchArm,
) -> ArmRun {
    if arm == SearchArm::Explore {
        return explore_arm(project_root, query);
    }
    if arm == SearchArm::Hybrid {
        #[cfg(feature = "embeddings")]
        {
            if let Ok(results) = try_hybrid_search(project_root, query, index, config) {
                let output_tokens = count_tokens(&results.join("\n"));
                return ArmRun {
                    files: results,
                    dense_active: true,
                    output_tokens,
                };
            }
        }
    }
    let _ = project_root;
    let files = bm25_only_search(index, query, config);
    let output_tokens = count_tokens(&files.join("\n"));
    ArmRun {
        files,
        dense_active: false,
        output_tokens,
    }
}

/// Run the real `ctx_explore` tool and reduce it to (distinct cited files in
/// citation order, citation-block token count). Uses citation-only mode so the
/// token footprint is exactly what an agent would receive to locate the answer.
fn explore_arm(project_root: &Path, query: &str) -> ArmRun {
    use std::collections::HashSet;
    let opts = crate::tools::ctx_explore::ExploreOptions::new(None, true);
    let outcome = crate::tools::ctx_explore::handle(
        query,
        &project_root.to_string_lossy(),
        crate::tools::CrpMode::Off,
        &opts,
    );
    let mut files = Vec::new();
    let mut seen = HashSet::new();
    for c in &outcome.citations {
        if seen.insert(c.file.clone()) {
            files.push(c.file.clone());
        }
    }
    ArmRun {
        files,
        dense_active: false,
        output_tokens: outcome.tokens,
    }
}

fn bm25_only_search(index: &ChunkData, query: &str, config: &HybridConfig) -> Vec<String> {
    crate::core::chunk_data::bm25_search(index, query, config.bm25_candidates)
        .iter()
        .map(|r| r.file_path.clone())
        .collect()
}

#[cfg(feature = "embeddings")]
fn try_hybrid_search(
    project_root: &Path,
    query: &str,
    index: &ChunkData,
    config: &HybridConfig,
) -> Result<Vec<String>, String> {
    use crate::core::dense_backend;
    use crate::tools::ctx_semantic_search;

    let (engine, mut embed_idx) = ctx_semantic_search::load_engine_and_index_pub(project_root)?;

    let (aligned, _coverage, changed_files) = ctx_semantic_search::ensure_embeddings_for_eval(
        project_root,
        index,
        engine,
        &mut embed_idx,
    )?;

    let backend = dense_backend::DenseBackendKind::try_from_env()?;
    let candidate_k = config.bm25_candidates.max(config.dense_candidates);

    let mut results = dense_backend::hybrid_results(
        backend,
        project_root,
        index,
        engine,
        &aligned,
        &changed_files,
        query,
        candidate_k,
        config,
        None,
        None,
    )?;

    if config.splade_weight > 0.0 {
        let splade = crate::core::splade_retrieval::hybrid_retrieve(query, index, candidate_k);
        if !splade.is_empty() {
            ctx_semantic_search::boost_with_splade_pub(&mut results, &splade, config.splade_weight);
        }
    }

    results.truncate(10);
    Ok(results.iter().map(|r| r.file_path.clone()).collect())
}

/// Generate self-eval queries from an indexed codebase.
/// Picks random symbols/files and constructs retrieval queries.
#[must_use]
pub fn generate_self_eval(index: &ChunkData, max_queries: usize) -> Vec<EvalQuery> {
    let mut queries = Vec::new();

    for chunk in index.chunks.iter().take(max_queries * 2) {
        if queries.len() >= max_queries {
            break;
        }
        if chunk.symbol_name.is_empty() || chunk.file_path.is_empty() {
            continue;
        }

        let category = if chunk.symbol_name.starts_with("fn ") || chunk.symbol_name.contains("()") {
            "function"
        } else if chunk.symbol_name.starts_with("struct ")
            || chunk.symbol_name.starts_with("class ")
        {
            "type"
        } else {
            "symbol"
        };

        let clean_name = chunk
            .symbol_name
            .replace("fn ", "")
            .replace("struct ", "")
            .replace("class ", "")
            .replace("()", "");

        queries.push(EvalQuery {
            query: format!("where is {clean_name} defined"),
            expected_files: vec![chunk.file_path.clone()],
            category: category.to_string(),
        });
    }

    queries
}

// ── A/B retrieval comparison (#686): dense default vs lean lower bound ────────

/// Recall slack (absolute, 0–1) within which the lean arm counts as "matching"
/// the dense default. 0.02 ≈ two percentage points of recall@5.
const AB_MARGIN: f64 = 0.02;

/// Verdict of a dense-vs-lean retrieval A/B.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AbVerdict {
    /// Pure BM25 already matches hybrid within `AB_MARGIN` on both recall@5 and
    /// MRR. The richer lean path is ≥ pure BM25, so flipping the default to
    /// dense-off cannot regress retrieval quality.
    FlipSafe,
    /// Dense adds recall beyond the margin. Evaluate the full lean path
    /// (BM25+graph+rerank+SPLADE) before flipping; keep hybrid as the default.
    KeepHybrid,
    /// The dense pipeline never actually ran (no working embeddings in this
    /// environment), so the two arms are identical and the run proves nothing.
    Inconclusive,
}

impl AbVerdict {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            AbVerdict::FlipSafe => "FLIP-SAFE",
            AbVerdict::KeepHybrid => "KEEP-HYBRID",
            AbVerdict::Inconclusive => "INCONCLUSIVE",
        }
    }
}

/// Pure verdict decision, split out so it is unit-testable without embeddings.
fn decide_verdict(
    delta_recall_at_5: f64,
    delta_mrr: f64,
    dense_active_queries: usize,
) -> AbVerdict {
    if dense_active_queries == 0 {
        AbVerdict::Inconclusive
    } else if delta_recall_at_5 >= -AB_MARGIN && delta_mrr >= -AB_MARGIN {
        AbVerdict::FlipSafe
    } else {
        AbVerdict::KeepHybrid
    }
}

/// Aggregate score for one retrieval arm.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ArmScore {
    pub arm: String,
    pub avg_recall_at_5: f64,
    pub avg_recall_at_10: f64,
    pub avg_mrr: f64,
    pub avg_latency_us: u64,
    /// Number of queries (of `total_queries`) where the dense pipeline actually
    /// contributed. Always 0 for the BM25 and explore arms.
    pub dense_active_queries: usize,
    /// Average token footprint of the arm's native per-query output (path list
    /// for the search arms; `<final_answer>` citation block for explore).
    pub avg_output_tokens: u64,
}

/// Full dense-vs-lean A/B scorecard for the default-flip decision (#686).
#[derive(Debug, Clone, serde::Serialize)]
pub struct AbReport {
    pub project: String,
    pub total_queries: usize,
    pub hybrid: ArmScore,
    pub bm25: ArmScore,
    /// FastContext `ctx_explore` peer arm: recall at a citation-level token cost.
    /// Informational — it does not change the #686 dense-vs-lean `verdict`.
    pub explore: ArmScore,
    /// bm25 − hybrid. Negative ⇒ the lean lower bound trails the dense default.
    pub delta_recall_at_5: f64,
    pub delta_mrr: f64,
    pub verdict: AbVerdict,
}

fn run_arm(
    project_root: &Path,
    queries: &[EvalQuery],
    index: &ChunkData,
    config: &HybridConfig,
    arm: SearchArm,
) -> ArmScore {
    let (mut r5, mut r10, mut mrr) = (0.0, 0.0, 0.0);
    let mut latency = 0u64;
    let mut dense_active = 0usize;
    let mut tokens = 0u64;
    for q in queries {
        let start = Instant::now();
        let run = search_arm(project_root, &q.query, index, config, arm);
        latency += start.elapsed().as_micros() as u64;
        r5 += recall_at_k(&run.files, &q.expected_files, 5);
        r10 += recall_at_k(&run.files, &q.expected_files, 10);
        mrr += mean_reciprocal_rank(&run.files, &q.expected_files);
        if run.dense_active {
            dense_active += 1;
        }
        tokens += run.output_tokens as u64;
    }
    let n = queries.len().max(1) as f64;
    let denom = queries.len().max(1) as u64;
    ArmScore {
        arm: arm.label().to_string(),
        avg_recall_at_5: r5 / n,
        avg_recall_at_10: r10 / n,
        avg_mrr: mrr / n,
        avg_latency_us: latency / denom,
        dense_active_queries: dense_active,
        avg_output_tokens: tokens / denom,
    }
}

/// Run the dense-vs-lean retrieval A/B over `queries` and decide whether the
/// default search path can be flipped to dense-off without losing quality.
#[must_use]
pub fn run_ab(
    project_root: &Path,
    queries: &[EvalQuery],
    index: &ChunkData,
    config: &HybridConfig,
) -> AbReport {
    // Canonicalize first so a relative root like "." still yields a real label.
    let label = project_root
        .canonicalize()
        .ok()
        .as_deref()
        .or(Some(project_root))
        .and_then(|p| p.file_name().map(|s| s.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "unknown".to_string());
    let hybrid = run_arm(project_root, queries, index, config, SearchArm::Hybrid);
    let bm25 = run_arm(project_root, queries, index, config, SearchArm::Bm25Only);
    let explore = run_arm(project_root, queries, index, config, SearchArm::Explore);
    let delta_recall_at_5 = bm25.avg_recall_at_5 - hybrid.avg_recall_at_5;
    let delta_mrr = bm25.avg_mrr - hybrid.avg_mrr;
    let verdict = decide_verdict(delta_recall_at_5, delta_mrr, hybrid.dense_active_queries);
    AbReport {
        project: label,
        total_queries: queries.len(),
        hybrid,
        bm25,
        explore,
        delta_recall_at_5,
        delta_mrr,
        verdict,
    }
}

/// Loads a curated eval suite: one JSON [`EvalQuery`] per line; blank lines and
/// `#` comments are ignored. Real labelled queries — no generation, no mocks.
pub fn load_suite(path: &Path) -> std::io::Result<Vec<EvalQuery>> {
    let text = std::fs::read_to_string(path)?;
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        let q: EvalQuery = serde_json::from_str(t).map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("{}:{}: {e}", path.display(), i + 1),
            )
        })?;
        out.push(q);
    }
    Ok(out)
}

impl AbReport {
    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".to_string())
    }
}

impl std::fmt::Display for AbReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(
            f,
            "Retrieval A/B: {} ({} queries) — #686 default-flip decision",
            self.project, self.total_queries
        )?;
        writeln!(
            f,
            "  {:<30} R@5={:>6.1}%  R@10={:>6.1}%  MRR={:>5.3}  {:>7}µs  {:>5}tok  dense:{}/{}",
            self.hybrid.arm,
            self.hybrid.avg_recall_at_5 * 100.0,
            self.hybrid.avg_recall_at_10 * 100.0,
            self.hybrid.avg_mrr,
            self.hybrid.avg_latency_us,
            self.hybrid.avg_output_tokens,
            self.hybrid.dense_active_queries,
            self.total_queries,
        )?;
        writeln!(
            f,
            "  {:<30} R@5={:>6.1}%  R@10={:>6.1}%  MRR={:>5.3}  {:>7}µs  {:>5}tok",
            self.bm25.arm,
            self.bm25.avg_recall_at_5 * 100.0,
            self.bm25.avg_recall_at_10 * 100.0,
            self.bm25.avg_mrr,
            self.bm25.avg_latency_us,
            self.bm25.avg_output_tokens,
        )?;
        writeln!(
            f,
            "  {:<30} R@5={:>6.1}%  R@10={:>6.1}%  MRR={:>5.3}  {:>7}µs  {:>5}tok",
            self.explore.arm,
            self.explore.avg_recall_at_5 * 100.0,
            self.explore.avg_recall_at_10 * 100.0,
            self.explore.avg_mrr,
            self.explore.avg_latency_us,
            self.explore.avg_output_tokens,
        )?;
        writeln!(
            f,
            "  Δ(bm25−hybrid): R@5={:+.1}pp  MRR={:+.3}",
            self.delta_recall_at_5 * 100.0,
            self.delta_mrr,
        )?;
        writeln!(f, "  Verdict: {}", self.verdict.label())?;
        let note = match self.verdict {
            AbVerdict::FlipSafe => {
                "pure BM25 matches hybrid within margin; the richer lean path is ≥ this \
                 → flipping the default to dense-off is safe."
            }
            AbVerdict::KeepHybrid => {
                "dense adds recall beyond the margin → evaluate the full lean path before \
                 flipping; keep hybrid default."
            }
            AbVerdict::Inconclusive => {
                "dense pipeline did not run (no embeddings here) → both arms identical; \
                 re-run where embeddings are built."
            }
        };
        writeln!(f, "  {note}")
    }
}

/// Normalizes path separators so comparisons are platform-independent (the
/// retrieved paths use the OS separator — `\` on Windows — while expected paths
/// in eval fixtures use `/`).
fn normalize_sep(p: &str) -> String {
    p.replace('\\', "/")
}

fn recall_at_k(retrieved: &[String], expected: &[String], k: usize) -> f64 {
    if expected.is_empty() {
        return 0.0;
    }
    let top_k: Vec<String> = retrieved.iter().take(k).map(|r| normalize_sep(r)).collect();
    let hits = expected
        .iter()
        .filter(|e| {
            let e = normalize_sep(e);
            top_k.iter().any(|r| r.ends_with(&e) || e.ends_with(r))
        })
        .count();
    hits as f64 / expected.len() as f64
}

fn mean_reciprocal_rank(retrieved: &[String], expected: &[String]) -> f64 {
    for (rank, r) in retrieved.iter().enumerate() {
        let r = normalize_sep(r);
        if expected.iter().any(|e| {
            let e = normalize_sep(e);
            r.ends_with(&e) || e.ends_with(&r)
        }) {
            return 1.0 / (rank as f64 + 1.0);
        }
    }
    0.0
}

fn build_category_scores(results: &[EvalResult]) -> Vec<CategoryScore> {
    use std::collections::HashMap;
    let mut cat_map: HashMap<&str, Vec<&EvalResult>> = HashMap::new();
    for r in results {
        cat_map.entry(r.category.as_str()).or_default().push(r);
    }

    let mut scores: Vec<CategoryScore> = cat_map
        .into_iter()
        .map(|(cat, items)| {
            let n = items.len();
            CategoryScore {
                category: cat.to_string(),
                count: n,
                avg_recall_at_5: items.iter().map(|r| r.recall_at_5).sum::<f64>() / n as f64,
                avg_mrr: items.iter().map(|r| r.mrr).sum::<f64>() / n as f64,
            }
        })
        .collect();
    scores.sort_by(|a, b| a.category.cmp(&b.category));
    scores
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recall_at_k_full_match() {
        let retrieved = vec!["a.rs".into(), "b.rs".into(), "c.rs".into()];
        let expected = vec!["a.rs".into()];
        assert_eq!(recall_at_k(&retrieved, &expected, 5), 1.0);
    }

    #[test]
    fn recall_at_k_matches_across_path_separators() {
        // Retrieved paths may use the OS separator (backslash on Windows) while
        // expected fixtures use '/'. They must still match.
        let retrieved = vec!["proj\\src\\auth.rs".into(), "proj\\src\\db.rs".into()];
        let expected = vec!["src/auth.rs".into()];
        assert_eq!(recall_at_k(&retrieved, &expected, 5), 1.0);
        assert_eq!(mean_reciprocal_rank(&retrieved, &expected), 1.0);
    }

    #[test]
    fn recall_at_k_no_match() {
        let retrieved = vec!["x.rs".into(), "y.rs".into()];
        let expected = vec!["a.rs".into()];
        assert_eq!(recall_at_k(&retrieved, &expected, 5), 0.0);
    }

    #[test]
    fn recall_at_k_partial() {
        let retrieved = vec!["a.rs".into(), "x.rs".into()];
        let expected = vec!["a.rs".into(), "b.rs".into()];
        assert_eq!(recall_at_k(&retrieved, &expected, 5), 0.5);
    }

    #[test]
    fn mrr_first_hit() {
        let retrieved = vec!["a.rs".into(), "b.rs".into()];
        let expected = vec!["a.rs".into()];
        assert_eq!(mean_reciprocal_rank(&retrieved, &expected), 1.0);
    }

    #[test]
    fn mrr_second_hit() {
        let retrieved = vec!["x.rs".into(), "a.rs".into()];
        let expected = vec!["a.rs".into()];
        assert_eq!(mean_reciprocal_rank(&retrieved, &expected), 0.5);
    }

    #[test]
    fn mrr_no_hit() {
        let retrieved = vec!["x.rs".into()];
        let expected = vec!["a.rs".into()];
        assert_eq!(mean_reciprocal_rank(&retrieved, &expected), 0.0);
    }

    #[test]
    fn empty_expected() {
        assert_eq!(recall_at_k(&["a.rs".into()], &[], 5), 0.0);
    }

    #[test]
    fn scorecard_display() {
        let sc = EvalScorecard {
            project: "test".into(),
            total_queries: 10,
            avg_recall_at_5: 0.8,
            avg_recall_at_10: 0.9,
            avg_mrr: 0.75,
            avg_latency_us: 100,
            per_category: vec![],
            results: vec![],
        };
        let s = format!("{sc}");
        assert!(s.contains("80.0%"));
        assert!(s.contains("0.750"));
    }

    #[test]
    fn verdict_flip_safe_when_lean_matches() {
        // Lean equal to hybrid, within margin, and even ahead → all flip-safe.
        assert_eq!(decide_verdict(0.0, 0.0, 5), AbVerdict::FlipSafe);
        assert_eq!(decide_verdict(-0.01, -0.005, 5), AbVerdict::FlipSafe);
        assert_eq!(decide_verdict(0.05, 0.03, 5), AbVerdict::FlipSafe);
    }

    #[test]
    fn verdict_keep_hybrid_when_dense_helps() {
        // Dense ahead beyond the margin on either metric → keep hybrid.
        assert_eq!(decide_verdict(-0.10, 0.0, 5), AbVerdict::KeepHybrid);
        assert_eq!(decide_verdict(0.0, -0.10, 3), AbVerdict::KeepHybrid);
    }

    #[test]
    fn verdict_inconclusive_without_dense() {
        // No dense-active query ⇒ arms are identical regardless of the deltas.
        assert_eq!(decide_verdict(-0.5, -0.5, 0), AbVerdict::Inconclusive);
        assert_eq!(decide_verdict(0.0, 0.0, 0), AbVerdict::Inconclusive);
    }

    #[test]
    fn load_suite_parses_and_skips_comments() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.ndjson");
        std::fs::write(
            &p,
            "# header comment\n\n\
             {\"query\":\"reciprocal rank fusion\",\"expected_files\":[\"core/hybrid_search.rs\"]}\n",
        )
        .unwrap();
        let q = load_suite(&p).unwrap();
        assert_eq!(q.len(), 1);
        assert_eq!(q[0].query, "reciprocal rank fusion");
        assert_eq!(
            q[0].expected_files,
            vec!["core/hybrid_search.rs".to_string()]
        );
    }

    #[test]
    fn load_suite_rejects_bad_json() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("bad.ndjson");
        std::fs::write(&p, "{not valid json}\n").unwrap();
        assert!(load_suite(&p).is_err());
    }

    #[test]
    fn run_ab_plumbing_on_synthetic_index() {
        use crate::core::chunk_data::{ChunkData, ChunkKind, CodeChunk, tokenize};

        let index = ChunkData::from_chunks_for_test(vec![CodeChunk {
            file_path: "core/hybrid_search.rs".into(),
            symbol_name: "reciprocal_rank_fusion".into(),
            kind: ChunkKind::Function,
            start_line: 1,
            end_line: 20,
            // Natural-language body so the query tokens match (the indexer keeps
            // `snake_case` identifiers as single tokens, so a bare symbol name
            // would not match the space-separated query).
            content: "Combine two ranked result lists using reciprocal rank fusion scoring.".into(),
            tokens: tokenize("combine two ranked result lists reciprocal rank fusion scoring"),
            token_count: 0,
        }]);
        let queries = vec![EvalQuery {
            query: "reciprocal rank fusion".into(),
            expected_files: vec!["core/hybrid_search.rs".into()],
            category: "test".into(),
        }];

        // Isolate any embedding side effects to a throwaway root.
        let dir = tempfile::tempdir().unwrap();
        let report = run_ab(dir.path(), &queries, &index, &HybridConfig::default());

        assert_eq!(report.total_queries, 1);
        // The BM25 arm must find the lexical match and never reports dense activity.
        assert!(report.bm25.avg_recall_at_5 > 0.0);
        assert_eq!(report.bm25.dense_active_queries, 0);
        // The verdict follows the (separately unit-tested) contract for whatever
        // dense availability this environment happens to have.
        let expected = decide_verdict(
            report.delta_recall_at_5,
            report.delta_mrr,
            report.hybrid.dense_active_queries,
        );
        assert_eq!(report.verdict, expected);
        // The explore peer arm ran and never reports dense activity.
        assert_eq!(report.explore.arm, "explore (citations)");
        assert_eq!(report.explore.dense_active_queries, 0);
        // Serialization stays valid JSON and exposes the explore arm.
        let v: serde_json::Value = serde_json::from_str(&report.to_json()).unwrap();
        assert_eq!(v["total_queries"], 1);
        assert!(v["explore"].is_object());
    }
}
