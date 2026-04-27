//! Hybrid search combining BM25 (lexical) with dense vector search.
//!
//! Uses Reciprocal Rank Fusion (RRF) to merge results from both retrieval
//! methods into a single ranked list. The RRF formula ensures that documents
//! appearing high in both rankings get boosted, while documents appearing
//! in only one ranking still contribute.
//!
//! Formula: score(d) = Σ 1/(k + rank_i(d))
//! where k=60 (standard constant), and i ranges over retrieval methods.
//!
//! Reference: Cormack, Clarke & Buettcher (2009), "Reciprocal Rank Fusion
//! outperforms Condorcet and individual Rank Learning Methods"

use std::collections::HashMap;

use super::vector_index::{BM25Index, ChunkKind, SearchResult};

#[cfg(feature = "embeddings")]
use super::embeddings::{cosine_similarity, EmbeddingEngine};

const RRF_K: f64 = 60.0;

const DEFAULT_BM25_WEIGHT: f64 = 0.4;
const DEFAULT_DENSE_WEIGHT: f64 = 0.6;

/// Configuration for hybrid search behavior.
pub struct HybridConfig {
    pub bm25_weight: f64,
    pub dense_weight: f64,
    pub bm25_candidates: usize,
    pub dense_candidates: usize,
}

impl Default for HybridConfig {
    fn default() -> Self {
        Self {
            bm25_weight: DEFAULT_BM25_WEIGHT,
            dense_weight: DEFAULT_DENSE_WEIGHT,
            bm25_candidates: 50,
            dense_candidates: 50,
        }
    }
}

/// Fuse two ranked result lists using Reciprocal Rank Fusion.
///
/// Each result is identified by `(file_path, start_line)` as a unique key.
/// Returns a merged list sorted by combined RRF score.
pub fn reciprocal_rank_fusion(
    bm25_results: &[SearchResult],
    dense_results: &[DenseSearchResult],
    config: &HybridConfig,
    top_k: usize,
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

    let mut results: Vec<HybridResult> = scores.into_values().collect();
    results.sort_by(|a, b| {
        b.rrf_score
            .partial_cmp(&a.rrf_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    results.truncate(top_k);
    results
}

/// Run hybrid search: BM25 + dense retrieval with RRF fusion.
/// Falls back to BM25-only if embedding engine is not available.
#[cfg(feature = "embeddings")]
pub fn hybrid_search(
    query: &str,
    index: &BM25Index,
    engine: Option<&EmbeddingEngine>,
    chunk_embeddings: Option<&[Vec<f32>]>,
    top_k: usize,
    config: &HybridConfig,
) -> Vec<HybridResult> {
    let bm25_results = index.search(query, config.bm25_candidates);

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

    if dense_results.is_empty() {
        return bm25_results
            .into_iter()
            .take(top_k)
            .map(HybridResult::from_bm25)
            .collect();
    }

    reciprocal_rank_fusion(&bm25_results, &dense_results, config, top_k)
}

#[cfg(not(feature = "embeddings"))]
pub fn hybrid_search(query: &str, index: &BM25Index, top_k: usize) -> Vec<HybridResult> {
    index
        .search(query, top_k)
        .into_iter()
        .map(HybridResult::from_bm25)
        .collect()
}

/// Dense vector search over pre-computed chunk embeddings.
#[cfg(feature = "embeddings")]
fn dense_search(
    query: &str,
    engine: &EmbeddingEngine,
    chunks: &[super::vector_index::CodeChunk],
    embeddings: &[Vec<f32>],
    top_k: usize,
) -> Vec<DenseSearchResult> {
    let Ok(query_embedding) = engine.embed(query) else {
        return Vec::new();
    };

    let mut scored: Vec<(usize, f32)> = embeddings
        .iter()
        .enumerate()
        .map(|(i, emb)| (i, cosine_similarity(&query_embedding, emb)))
        .collect();

    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(top_k);

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
            let source_info = match (r.bm25_rank, r.dense_rank) {
                (Some(bm), Some(dn)) => format!("bm25:#{bm} + dense:#{dn}"),
                (Some(bm), None) => format!("bm25:#{bm}"),
                (None, Some(dn)) => format!("dense:#{dn}"),
                _ => String::new(),
            };
            out.push_str(&format!(
                "\n--- Result {} (rrf: {:.4}, {}) ---\n{} :: {} [{:?}] (L{}-{})\n{}\n",
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
        let results = reciprocal_rank_fusion(&bm25, &dense, &config, 10);

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
        let results = reciprocal_rank_fusion(&bm25, &dense, &config, 10);

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

        let results = reciprocal_rank_fusion(&bm25, &[], &HybridConfig::default(), 5);
        assert_eq!(results.len(), 5);
    }

    #[test]
    fn rrf_empty_inputs() {
        let results = reciprocal_rank_fusion(&[], &[], &HybridConfig::default(), 10);
        assert!(results.is_empty());
    }

    #[test]
    fn rrf_bm25_only() {
        let bm25 = vec![make_bm25_result("a.rs", "alpha", 1, 5.0)];
        let results = reciprocal_rank_fusion(&bm25, &[], &HybridConfig::default(), 10);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].source_label(), "bm25");
    }

    #[test]
    fn rrf_dense_only() {
        let dense = vec![make_dense_result("a.rs", "alpha", 1, 0.95)];
        let results = reciprocal_rank_fusion(&[], &dense, &HybridConfig::default(), 10);
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
}
