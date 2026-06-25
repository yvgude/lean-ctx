//! Hybrid search combining BM25 (lexical) with dense vector search.
//!
//! Uses Reciprocal Rank Fusion (RRF) to merge BM25, dense embeddings, and optional
//! property-graph proximity (session neighborhood) into one ranked list.
//!
//! Formula: score(d) = Σ 1/(k + `rank_i(d)`)
//! where k=60 (standard constant), and i ranges over retrieval methods.
//!
//! Reference: Cormack, Clarke & Buettcher (2009), "Reciprocal Rank Fusion
//! outperforms Condorcet and individual Rank Learning Methods"

use std::collections::HashMap;

use super::chunk_data::{ChunkData, ChunkKind, SearchResult};
#[cfg(feature = "embeddings")]
use super::embeddings::EmbeddingEngine;
#[cfg(feature = "embeddings")]
use super::hnsw::FlatEmbeddings;

const RRF_K: f64 = 60.0;

/// Default weights for standard RRF: equal contribution per ranking (`Σ weight/(k+r)` with weight=1).
const DEFAULT_BM25_WEIGHT: f64 = 1.0;
const DEFAULT_DENSE_WEIGHT: f64 = 1.0;

/// Configuration for hybrid search behavior.
/// Configurable via `[search]` in `.lean-ctx.toml`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct HybridConfig {
    pub bm25_weight: f64,
    pub dense_weight: f64,
    pub bm25_candidates: usize,
    pub dense_candidates: usize,
    /// Weight for SPLADE expansion signal. 0.0 disables SPLADE.
    #[serde(default = "default_splade_weight")]
    pub splade_weight: f64,
    /// Master switch for the dense (embedding) retrieval path (#686). When
    /// `false`, the default `hybrid` semantic search skips loading the embedding
    /// engine and building/persisting `embeddings.json` entirely, and ranks with
    /// BM25 + graph proximity + reranking (+ SPLADE) only. This is the same
    /// ranking the pipeline already falls back to when no embeddings are present,
    /// so results stay coherent while the on-disk vector footprint and per-query
    /// embed latency disappear. `true` by default (unchanged behavior); an
    /// explicit `mode=dense` query still forces dense regardless.
    #[serde(default = "default_dense_enabled")]
    pub dense_enabled: bool,
}

fn default_splade_weight() -> f64 {
    0.5
}

fn default_dense_enabled() -> bool {
    true
}

impl Default for HybridConfig {
    fn default() -> Self {
        Self {
            bm25_weight: DEFAULT_BM25_WEIGHT,
            dense_weight: DEFAULT_DENSE_WEIGHT,
            bm25_candidates: 75,
            dense_candidates: 75,
            splade_weight: 0.5,
            dense_enabled: true,
        }
    }
}

impl HybridConfig {
    /// Load from the global config, falling back to defaults.
    #[must_use]
    pub fn from_config() -> Self {
        crate::core::config::Config::load().search
    }
}

/// Fuse two ranked result lists using Reciprocal Rank Fusion.
///
/// `graph_file_ranks`: optional repo-relative file path → rank (0-based) for neighbors of
/// recently touched session files; each matching result gets an extra `1/(k+r)` term.
#[must_use]
pub fn reciprocal_rank_fusion(
    bm25_results: &[SearchResult],
    dense_results: &[DenseSearchResult],
    config: &HybridConfig,
    top_k: usize,
    graph_file_ranks: Option<&HashMap<String, usize>>,
) -> Vec<HybridResult> {
    let mut scores: HashMap<String, HybridResult> = HashMap::new();

    for (rank, result) in bm25_results.iter().enumerate() {
        let key = result_key(&result.file_path, result.start_line);
        let rrf_score = config.bm25_weight / (RRF_K + rank as f64 + 1.0);

        let entry = scores.entry(key).or_insert_with(|| HybridResult {
            file_path: result.file_path.clone(),
            symbol_name: result.symbol_name.clone(),
            kind: result.kind.clone(),
            start_line: result.start_line,
            end_line: result.end_line,
            snippet: result.snippet.clone(),
            rrf_score: 0.0,
            bm25_score: Some(result.score),
            dense_score: None,
            bm25_rank: None,
            dense_rank: None,
        });
        entry.rrf_score += rrf_score;
        entry.bm25_rank = Some(rank + 1);
    }

    for (rank, result) in dense_results.iter().enumerate() {
        let key = result_key(&result.file_path, result.start_line);
        let rrf_score = config.dense_weight / (RRF_K + rank as f64 + 1.0);

        let entry = scores.entry(key).or_insert_with(|| HybridResult {
            file_path: result.file_path.clone(),
            symbol_name: result.symbol_name.clone(),
            kind: result.kind.clone(),
            start_line: result.start_line,
            end_line: result.end_line,
            snippet: result.snippet.clone(),
            rrf_score: 0.0,
            bm25_score: None,
            dense_score: None,
            bm25_rank: None,
            dense_rank: None,
        });
        entry.rrf_score += rrf_score;
        entry.dense_score = Some(result.similarity);
        entry.dense_rank = Some(rank + 1);
    }

    if let Some(gr) = graph_file_ranks
        && !gr.is_empty()
    {
        for entry in scores.values_mut() {
            if let Some(&rank) = gr.get(&entry.file_path) {
                entry.rrf_score += 1.0 / (RRF_K + rank as f64 + 1.0);
            }
        }
    }

    let mut results: Vec<HybridResult> = scores.into_values().collect();
    results.sort_by(|a, b| {
        b.rrf_score
            .partial_cmp(&a.rrf_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    results.truncate(top_k);
    results
}

/// Run hybrid search: BM25 + dense retrieval with RRF fusion + post-RRF reranking.
/// Falls back to BM25-only if embedding engine is not available.
#[cfg(feature = "embeddings")]
pub fn hybrid_search(
    query: &str,
    index: &ChunkData,
    engine: Option<&EmbeddingEngine>,
    chunk_embeddings: Option<&FlatEmbeddings>,
    top_k: usize,
    config: &HybridConfig,
    graph_file_ranks: Option<&HashMap<String, usize>>,
) -> Vec<HybridResult> {
    let bm25_results = super::chunk_data::bm25_search(index, query, config.bm25_candidates);

    let dense_results = match (engine, chunk_embeddings) {
        (Some(eng), Some(embeddings)) => dense_search(
            query,
            eng,
            &index.chunks,
            embeddings,
            config.dense_candidates,
        ),
        _ => Vec::new(),
    };

    let graph_enhances = graph_file_ranks.is_some_and(|m| !m.is_empty());

    // Over-fetch candidates for reranking (5x top_k, capped at available)
    let candidate_count = (top_k * 5).min(config.bm25_candidates);

    let mut results = if dense_results.is_empty() {
        if graph_enhances {
            reciprocal_rank_fusion(
                &bm25_results,
                &[],
                config,
                candidate_count,
                graph_file_ranks,
            )
        } else {
            bm25_results
                .into_iter()
                .take(candidate_count)
                .map(HybridResult::from_bm25)
                .collect()
        }
    } else {
        reciprocal_rank_fusion(
            &bm25_results,
            &dense_results,
            config,
            candidate_count,
            graph_file_ranks,
        )
    };

    super::search_reranking::rerank_pipeline(&mut results, query, top_k);
    results
}

#[cfg(not(feature = "embeddings"))]
pub fn hybrid_search(query: &str, index: &ChunkData, top_k: usize) -> Vec<HybridResult> {
    let candidate_count = (top_k * 5).min(50);
    let mut results: Vec<HybridResult> =
        super::chunk_data::bm25_search(index, query, candidate_count)
            .into_iter()
            .map(HybridResult::from_bm25)
            .collect();
    super::search_reranking::rerank_pipeline(&mut results, query, top_k);
    results
}

/// Dense vector search over pre-computed chunk embeddings.
/// Uses O(n log k) binary-heap top-k selection for small indices, HNSW for large ones.
#[cfg(feature = "embeddings")]
fn dense_search(
    query: &str,
    engine: &EmbeddingEngine,
    chunks: &[super::chunk_data::CodeChunk],
    embeddings: &FlatEmbeddings,
    top_k: usize,
) -> Vec<DenseSearchResult> {
    let Ok(query_embedding) = engine.embed_query(query) else {
        return Vec::new();
    };

    // Threshold-gated ANN: exact SIMD brute force for small corpora, cached
    // HNSW (sub-linear) once the corpus is large enough to amortize graph build.
    let scored = super::ann_cache::topk(embeddings, &query_embedding, top_k);

    scored
        .into_iter()
        .filter_map(|(idx, sim)| {
            let chunk = chunks.get(idx)?;
            let snippet = chunk.content.lines().take(5).collect::<Vec<_>>().join("\n");
            Some(DenseSearchResult {
                chunk_idx: idx,
                similarity: sim,
                file_path: chunk.file_path.clone(),
                symbol_name: chunk.symbol_name.clone(),
                kind: chunk.kind.clone(),
                start_line: chunk.start_line,
                end_line: chunk.end_line,
                snippet,
            })
        })
        .collect()
}

fn result_key(file_path: &str, start_line: usize) -> String {
    format!("{file_path}:{start_line}")
}

/// Result from dense (embedding-based) search.
#[derive(Debug, Clone)]
pub struct DenseSearchResult {
    pub chunk_idx: usize,
    pub similarity: f32,
    pub file_path: String,
    pub symbol_name: String,
    pub kind: ChunkKind,
    pub start_line: usize,
    pub end_line: usize,
    pub snippet: String,
}

/// Fused result combining BM25 and dense scores.
#[derive(Debug, Clone)]
pub struct HybridResult {
    pub file_path: String,
    pub symbol_name: String,
    pub kind: ChunkKind,
    pub start_line: usize,
    pub end_line: usize,
    pub snippet: String,
    pub rrf_score: f64,
    pub bm25_score: Option<f64>,
    pub dense_score: Option<f32>,
    pub bm25_rank: Option<usize>,
    pub dense_rank: Option<usize>,
}

impl HybridResult {
    #[must_use]
    pub fn from_bm25_public(result: SearchResult) -> Self {
        Self::from_bm25(result)
    }

    fn from_bm25(result: SearchResult) -> Self {
        Self {
            file_path: result.file_path,
            symbol_name: result.symbol_name,
            kind: result.kind,
            start_line: result.start_line,
            end_line: result.end_line,
            snippet: result.snippet,
            rrf_score: result.score,
            bm25_score: Some(result.score),
            dense_score: None,
            bm25_rank: None,
            dense_rank: None,
        }
    }

    #[must_use]
    pub fn source_label(&self) -> &'static str {
        match (self.bm25_rank.is_some(), self.dense_rank.is_some()) {
            (true, true) => "hybrid",
            (true, false) => "bm25",
            (false, true) => "dense",
            (false, false) => "unknown",
        }
    }
}

/// Format hybrid results for display.
#[must_use]
pub fn format_hybrid_results(results: &[HybridResult], compact: bool) -> String {
    if results.is_empty() {
        return "No results found.".to_string();
    }

    let mut out = String::new();
    for (i, r) in results.iter().enumerate() {
        if compact {
            out.push_str(&format!(
                "{}. {:.4} [{}] {}:{}-{} {:?} {}\n",
                i + 1,
                r.rrf_score,
                r.source_label(),
                r.file_path,
                r.start_line,
                r.end_line,
                r.kind,
                r.symbol_name,
            ));
        } else {
            // Prefix the separator inside each arm so an absent rank-source
            // (e.g. BM25-only fallback when embeddings are off) renders a clean
            // "(rrf: X)" instead of a dangling "(rrf: X, )" (#509).
            let source_info = match (r.bm25_rank, r.dense_rank) {
                (Some(bm), Some(dn)) => format!(", bm25:#{bm} + dense:#{dn}"),
                (Some(bm), None) => format!(", bm25:#{bm}"),
                (None, Some(dn)) => format!(", dense:#{dn}"),
                _ => String::new(),
            };
            out.push_str(&format!(
                "\n--- Result {} (rrf: {:.4}{}) ---\n{} :: {} [{:?}] (L{}-{})\n{}\n",
                i + 1,
                r.rrf_score,
                source_info,
                r.file_path,
                r.symbol_name,
                r.kind,
                r.start_line,
                r.end_line,
                r.snippet,
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_bm25_result(file: &str, name: &str, line: usize, score: f64) -> SearchResult {
        SearchResult {
            chunk_idx: 0,
            score,
            file_path: file.to_string(),
            symbol_name: name.to_string(),
            kind: ChunkKind::Function,
            start_line: line,
            end_line: line + 10,
            snippet: format!("fn {name}() {{ }}"),
        }
    }

    fn make_dense_result(file: &str, name: &str, line: usize, sim: f32) -> DenseSearchResult {
        DenseSearchResult {
            chunk_idx: 0,
            similarity: sim,
            file_path: file.to_string(),
            symbol_name: name.to_string(),
            kind: ChunkKind::Function,
            start_line: line,
            end_line: line + 10,
            snippet: format!("fn {name}() {{ }}"),
        }
    }

    #[test]
    fn rrf_basic_fusion() {
        let bm25 = vec![
            make_bm25_result("a.rs", "alpha", 1, 5.0),
            make_bm25_result("b.rs", "beta", 1, 3.0),
            make_bm25_result("c.rs", "gamma", 1, 1.0),
        ];
        let dense = vec![
            make_dense_result("b.rs", "beta", 1, 0.95),
            make_dense_result("d.rs", "delta", 1, 0.90),
            make_dense_result("a.rs", "alpha", 1, 0.85),
        ];

        let config = HybridConfig::default();
        let results = reciprocal_rank_fusion(&bm25, &dense, &config, 10, None);

        assert!(!results.is_empty());

        let top = &results[0];
        assert!(
            top.bm25_rank.is_some() || top.dense_rank.is_some(),
            "top result should appear in at least one ranking"
        );

        let beta = results.iter().find(|r| r.symbol_name == "beta").unwrap();
        assert!(beta.bm25_rank.is_some() && beta.dense_rank.is_some());
        assert_eq!(beta.source_label(), "hybrid");
    }

    #[test]
    fn rrf_both_rankings_boost() {
        let bm25 = vec![
            make_bm25_result("a.rs", "only_bm25", 1, 5.0),
            make_bm25_result("b.rs", "both", 1, 3.0),
        ];
        let dense = vec![
            make_dense_result("c.rs", "only_dense", 1, 0.99),
            make_dense_result("b.rs", "both", 1, 0.90),
        ];

        let config = HybridConfig {
            bm25_weight: 0.5,
            dense_weight: 0.5,
            ..Default::default()
        };
        let results = reciprocal_rank_fusion(&bm25, &dense, &config, 10, None);

        let both = results.iter().find(|r| r.symbol_name == "both").unwrap();
        let only_bm25 = results
            .iter()
            .find(|r| r.symbol_name == "only_bm25")
            .unwrap();
        let only_dense = results
            .iter()
            .find(|r| r.symbol_name == "only_dense")
            .unwrap();

        assert!(
            both.rrf_score > only_bm25.rrf_score,
            "result in both rankings should score higher than BM25-only"
        );
        assert!(
            both.rrf_score > only_dense.rrf_score,
            "result in both rankings should score higher than dense-only"
        );
    }

    #[test]
    fn rrf_respects_top_k() {
        let bm25: Vec<SearchResult> = (0..20)
            .map(|i| make_bm25_result("a.rs", &format!("fn_{i}"), i * 10 + 1, 10.0 - i as f64))
            .collect();

        let results = reciprocal_rank_fusion(&bm25, &[], &HybridConfig::default(), 5, None);
        assert_eq!(results.len(), 5);
    }

    #[test]
    fn rrf_empty_inputs() {
        let results = reciprocal_rank_fusion(&[], &[], &HybridConfig::default(), 10, None);
        assert!(results.is_empty());
    }

    #[test]
    fn rrf_bm25_only() {
        let bm25 = vec![make_bm25_result("a.rs", "alpha", 1, 5.0)];
        let results = reciprocal_rank_fusion(&bm25, &[], &HybridConfig::default(), 10, None);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].source_label(), "bm25");
    }

    #[test]
    fn rrf_dense_only() {
        let dense = vec![make_dense_result("a.rs", "alpha", 1, 0.95)];
        let results = reciprocal_rank_fusion(&[], &dense, &HybridConfig::default(), 10, None);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].source_label(), "dense");
    }

    #[test]
    fn format_compact() {
        let results = vec![HybridResult {
            file_path: "auth.rs".into(),
            symbol_name: "validate".into(),
            kind: ChunkKind::Function,
            start_line: 10,
            end_line: 20,
            snippet: "fn validate() {}".into(),
            rrf_score: 0.0156,
            bm25_score: Some(4.2),
            dense_score: Some(0.91),
            bm25_rank: Some(1),
            dense_rank: Some(2),
        }];
        let output = format_hybrid_results(&results, true);
        assert!(output.contains("[hybrid]"));
        assert!(output.contains("auth.rs"));
        assert!(output.contains("validate"));
    }

    #[test]
    fn format_verbose() {
        let results = vec![HybridResult {
            file_path: "auth.rs".into(),
            symbol_name: "validate".into(),
            kind: ChunkKind::Function,
            start_line: 10,
            end_line: 20,
            snippet: "fn validate() {}".into(),
            rrf_score: 0.0156,
            bm25_score: Some(4.2),
            dense_score: Some(0.91),
            bm25_rank: Some(1),
            dense_rank: Some(2),
        }];
        let output = format_hybrid_results(&results, false);
        assert!(output.contains("bm25:#1 + dense:#2"));
    }

    #[test]
    fn format_verbose_no_dangling_comma_when_source_absent() {
        // BM25-only fallback (embeddings off) leaves both ranks None; the header
        // must read "(rrf: X)" with no trailing ", )" waste (#509).
        let results = vec![HybridResult {
            file_path: "auth.rs".into(),
            symbol_name: "validate".into(),
            kind: ChunkKind::Function,
            start_line: 10,
            end_line: 20,
            snippet: "fn validate() {}".into(),
            rrf_score: 0.5898,
            bm25_score: Some(4.2),
            dense_score: None,
            bm25_rank: None,
            dense_rank: None,
        }];
        let output = format_hybrid_results(&results, false);
        assert!(
            output.contains("(rrf: 0.5898)"),
            "absent rank-source must render a clean header, got: {output}"
        );
        assert!(
            !output.contains(", )"),
            "must not emit a dangling ', )' (#509)"
        );
    }

    #[test]
    fn source_label_categories() {
        let mut r = HybridResult {
            file_path: String::new(),
            symbol_name: String::new(),
            kind: ChunkKind::Function,
            start_line: 0,
            end_line: 0,
            snippet: String::new(),
            rrf_score: 0.0,
            bm25_score: None,
            dense_score: None,
            bm25_rank: None,
            dense_rank: None,
        };

        r.bm25_rank = Some(1);
        r.dense_rank = Some(1);
        assert_eq!(r.source_label(), "hybrid");

        r.dense_rank = None;
        assert_eq!(r.source_label(), "bm25");

        r.bm25_rank = None;
        r.dense_rank = Some(1);
        assert_eq!(r.source_label(), "dense");
    }

    #[test]
    fn rrf_graph_proximity_boost() {
        let bm25 = vec![
            make_bm25_result("neighbor.rs", "n", 1, 5.0),
            make_bm25_result("weak.rs", "low", 1, 1.0),
        ];
        let dense = vec![
            make_dense_result("weak.rs", "low", 1, 0.99),
            make_dense_result("other.rs", "o", 1, 0.50),
        ];
        let mut graph = HashMap::new();
        graph.insert("neighbor.rs".to_string(), 0usize);

        let results =
            reciprocal_rank_fusion(&bm25, &dense, &HybridConfig::default(), 10, Some(&graph));

        let neighbor = results
            .iter()
            .find(|r| r.file_path == "neighbor.rs")
            .unwrap();
        let weak = results.iter().find(|r| r.file_path == "weak.rs").unwrap();
        assert!(
            neighbor.rrf_score > weak.rrf_score,
            "graph neighbor should outrank when it gets a third RRF signal"
        );
    }
}
