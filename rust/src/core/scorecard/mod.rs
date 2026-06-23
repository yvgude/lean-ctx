//! Reproducible scorecard (#211): one command → compression savings + retrieval
//! recall/MRR + latency over a fixed, deterministic scenario matrix.
//!
//! The quality metrics (savings, recall, MRR) are reproducible run-to-run and
//! machine-to-machine because the corpus is generated deterministically and the
//! retrieval path is pure BM25. Latency is measured wall-clock and therefore
//! reported but not part of the determinism contract (see `determinism_digest`).

pub mod dual_arm;
mod scenarios;

use std::collections::BTreeMap;
use std::time::Instant;

use serde::Serialize;

use crate::core::{benchmark, bm25_index::BM25Index};

/// Per-scenario result.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ScenarioScore {
    pub name: String,
    pub files: usize,
    pub raw_tokens: usize,
    pub best_mode: String,
    pub best_savings_pct: f64,
    pub savings_by_mode: BTreeMap<String, f64>,
    pub queries: usize,
    pub recall_at_5: f64,
    pub recall_at_10: f64,
    pub mrr: f64,
    /// Wall-clock; informational only (not part of the determinism contract).
    pub search_latency_us_p50: u64,
}

/// Cross-scenario averages.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Aggregate {
    pub avg_savings_pct: f64,
    pub avg_recall_at_5: f64,
    pub avg_recall_at_10: f64,
    pub avg_mrr: f64,
}

/// The full scorecard.
#[derive(Debug, Clone, Serialize)]
pub struct Scorecard {
    pub schema_version: u32,
    pub tokenizer: String,
    /// Stable fingerprint of the reproducible (latency-free) metrics. Serialized
    /// so the JSON artifact is self-verifying: two runs on the same code (any
    /// machine) yield the same digest.
    pub determinism_digest: String,
    pub scenarios: Vec<ScenarioScore>,
    pub aggregate: Aggregate,
}

impl Scorecard {
    /// Compute the stable fingerprint from scenario scores. Latency is excluded
    /// by construction, so two runs on the same code must produce the same value.
    fn compute_digest(scenarios: &[ScenarioScore]) -> String {
        let mut parts: Vec<String> = Vec::new();
        for s in scenarios {
            let modes: Vec<String> = s
                .savings_by_mode
                .iter()
                .map(|(m, v)| format!("{m}={v:.2}"))
                .collect();
            parts.push(format!(
                "{}|raw={}|best={}:{:.2}|r5={:.4}|r10={:.4}|mrr={:.4}|{}",
                s.name,
                s.raw_tokens,
                s.best_mode,
                s.best_savings_pct,
                s.recall_at_5,
                s.recall_at_10,
                s.mrr,
                modes.join(",")
            ));
        }
        parts.join(";")
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".to_string())
    }

    /// Human-readable scorecard table.
    pub fn to_human(&self) -> String {
        let mut out = String::new();
        out.push_str("lean-ctx scorecard\n");
        out.push_str(&format!("tokenizer: {}\n", self.tokenizer));
        out.push_str(&format!("digest:    {}\n\n", self.determinism_digest));
        out.push_str(
            "scenario   files  raw_tokens  best_mode      savings%  R@5    R@10   MRR    p50(us)\n",
        );
        out.push_str(
            "--------------------------------------------------------------------------------\n",
        );
        for s in &self.scenarios {
            out.push_str(&format!(
                "{:<10} {:>5}  {:>10}  {:<13}  {:>7.2}  {:>4.2}  {:>4.2}  {:>4.2}  {:>7}\n",
                s.name,
                s.files,
                s.raw_tokens,
                s.best_mode,
                s.best_savings_pct,
                s.recall_at_5,
                s.recall_at_10,
                s.mrr,
                s.search_latency_us_p50,
            ));
        }
        out.push_str(
            "--------------------------------------------------------------------------------\n",
        );
        out.push_str(&format!(
            "aggregate                            {:<13}  {:>7.2}  {:>4.2}  {:>4.2}  {:>4.2}\n",
            "",
            self.aggregate.avg_savings_pct,
            self.aggregate.avg_recall_at_5,
            self.aggregate.avg_recall_at_10,
            self.aggregate.avg_mrr,
        ));
        out
    }
}

/// Run the full scenario matrix and assemble the scorecard.
pub fn run_scorecard() -> std::io::Result<Scorecard> {
    let mut scenarios = Vec::with_capacity(scenarios::SCENARIOS.len());
    for sc in scenarios::SCENARIOS {
        scenarios.push(run_one(sc)?);
    }
    let aggregate = aggregate(&scenarios);
    let determinism_digest = Scorecard::compute_digest(&scenarios);
    Ok(Scorecard {
        schema_version: 1,
        tokenizer: crate::core::tokens::counting_family_label(),
        determinism_digest,
        scenarios,
        aggregate,
    })
}

fn run_one(sc: &scenarios::Scenario) -> std::io::Result<ScenarioScore> {
    let dir = tempfile::TempDir::new()?;
    let root = dir.path();
    let queries = scenarios::materialize(sc, root)?;

    // --- Compression savings (existing, tested benchmark path) ---
    let bench = benchmark::run_project_benchmark(&root.to_string_lossy());
    let mut savings_by_mode = BTreeMap::new();
    for m in &bench.mode_summaries {
        savings_by_mode.insert(m.mode.clone(), round2(m.avg_savings_pct));
    }
    let (best_mode, best_savings_pct) = savings_by_mode
        .iter()
        .max_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal))
        .map_or_else(|| ("none".to_string(), 0.0), |(m, v)| (m.clone(), *v));

    // --- Retrieval quality (pure BM25 → deterministic, feature-independent) ---
    let index = crate::core::index_orchestrator::load_or_build_bm25(root);
    let mut sum_r5 = 0.0;
    let mut sum_r10 = 0.0;
    let mut sum_mrr = 0.0;
    let mut latencies: Vec<u64> = Vec::with_capacity(queries.len());
    for q in &queries {
        let start = Instant::now();
        let results = index.search(&q.query, 10);
        latencies.push(start.elapsed().as_micros() as u64);
        let files = dedup_files(&results);
        sum_r5 += recall_at_k(&files, &q.expected_file, 5);
        sum_r10 += recall_at_k(&files, &q.expected_file, 10);
        sum_mrr += reciprocal_rank(&files, &q.expected_file);
    }
    let n = queries.len().max(1) as f64;

    Ok(ScenarioScore {
        name: sc.name.to_string(),
        files: sc.files,
        raw_tokens: bench.total_raw_tokens,
        best_mode,
        best_savings_pct,
        savings_by_mode,
        queries: queries.len(),
        recall_at_5: round2(sum_r5 / n),
        recall_at_10: round2(sum_r10 / n),
        mrr: round2(sum_mrr / n),
        search_latency_us_p50: percentile_p50(&mut latencies),
    })
}

fn aggregate(scenarios: &[ScenarioScore]) -> Aggregate {
    if scenarios.is_empty() {
        return Aggregate {
            avg_savings_pct: 0.0,
            avg_recall_at_5: 0.0,
            avg_recall_at_10: 0.0,
            avg_mrr: 0.0,
        };
    }
    let n = scenarios.len() as f64;
    Aggregate {
        avg_savings_pct: round2(scenarios.iter().map(|s| s.best_savings_pct).sum::<f64>() / n),
        avg_recall_at_5: round2(scenarios.iter().map(|s| s.recall_at_5).sum::<f64>() / n),
        avg_recall_at_10: round2(scenarios.iter().map(|s| s.recall_at_10).sum::<f64>() / n),
        avg_mrr: round2(scenarios.iter().map(|s| s.mrr).sum::<f64>() / n),
    }
}

/// Unique file paths in rank order (the first chunk of each file defines rank).
fn dedup_files(results: &[crate::core::bm25_index::SearchResult]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut files = Vec::new();
    for r in results {
        if seen.insert(r.file_path.clone()) {
            files.push(r.file_path.clone());
        }
    }
    files
}

/// Platform-independent suffix match (mirrors `eval_harness` convention).
fn path_matches(retrieved: &str, expected: &str) -> bool {
    let r = retrieved.replace('\\', "/");
    let e = expected.replace('\\', "/");
    r.ends_with(&e) || e.ends_with(&r)
}

fn recall_at_k(files: &[String], expected: &str, k: usize) -> f64 {
    if files.iter().take(k).any(|f| path_matches(f, expected)) {
        1.0
    } else {
        0.0
    }
}

fn reciprocal_rank(files: &[String], expected: &str) -> f64 {
    for (i, f) in files.iter().enumerate() {
        if path_matches(f, expected) {
            return 1.0 / (i as f64 + 1.0);
        }
    }
    0.0
}

fn percentile_p50(latencies: &mut [u64]) -> u64 {
    if latencies.is_empty() {
        return 0;
    }
    latencies.sort_unstable();
    latencies[latencies.len() / 2]
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_matching_is_suffix_based() {
        assert!(path_matches(
            "/tmp/x/src/auth/file_001.rs",
            "src/auth/file_001.rs"
        ));
        assert!(!path_matches(
            "src/auth/file_002.rs",
            "src/auth/file_001.rs"
        ));
    }

    #[test]
    fn recall_and_rr_basics() {
        let files = vec![
            "src/db/file_000.rs".to_string(),
            "src/auth/file_001.rs".to_string(),
        ];
        assert_eq!(recall_at_k(&files, "src/auth/file_001.rs", 5), 1.0);
        assert_eq!(recall_at_k(&files, "src/auth/file_001.rs", 1), 0.0);
        assert_eq!(reciprocal_rank(&files, "src/auth/file_001.rs"), 0.5);
        assert_eq!(reciprocal_rank(&files, "src/missing.rs"), 0.0);
    }

    #[test]
    fn round2_rounds() {
        assert_eq!(round2(12.3456), 12.35);
        assert_eq!(round2(0.0), 0.0);
    }
}
