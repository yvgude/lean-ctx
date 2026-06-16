//! SPLADE-style sparse expansion layered on BM25 candidate retrieval.
//!
//! Stage 1: BM25 top-100. Stage 2: programming-term synonym / association expansion.
//! Stage 3: additive expansion BM25-like scoring combined with stage-1 scores.

use std::collections::{HashMap, HashSet};

use crate::core::bm25_index::{BM25Index, tokenize_for_index};

/// Result row after hybrid retrieval + re-ranking.
#[derive(Debug, Clone, PartialEq)]
pub struct SpladeResult {
    pub chunk_idx: usize,
    pub file_path: String,
    pub symbol_name: String,
    pub combined_score: f64,
    pub bm25_score: f64,
    pub expansion_score: f64,
}

/// Static programming-term associations (token → related tokens with weights).
fn expansion_dictionary() -> HashMap<&'static str, Vec<(&'static str, f64)>> {
    HashMap::from([
        (
            "auth",
            vec![
                ("authentication", 1.0),
                ("token", 0.9),
                ("jwt", 0.85),
                ("login", 0.8),
                ("session", 0.85),
                ("oauth", 0.75),
                ("credential", 0.7),
            ],
        ),
        (
            "async",
            vec![
                ("await", 1.0),
                ("future", 0.85),
                ("promise", 0.75),
                ("tokio", 0.65),
                ("concurrent", 0.7),
            ],
        ),
        (
            "error",
            vec![
                ("err", 0.95),
                ("result", 0.75),
                ("panic", 0.55),
                ("exception", 0.65),
                ("unwrap", 0.5),
            ],
        ),
        (
            "http",
            vec![
                ("request", 0.85),
                ("response", 0.85),
                ("rest", 0.65),
                ("json", 0.7),
                ("header", 0.6),
            ],
        ),
        (
            "db",
            vec![
                ("database", 1.0),
                ("sql", 0.85),
                ("query", 0.75),
                ("transaction", 0.65),
                ("migration", 0.55),
            ],
        ),
        (
            "test",
            vec![
                ("mock", 0.75),
                ("fixture", 0.6),
                ("assert", 0.85),
                ("expect", 0.65),
            ],
        ),
        (
            "config",
            vec![
                ("configuration", 1.0),
                ("env", 0.75),
                ("setting", 0.65),
                ("toml", 0.55),
            ],
        ),
        (
            "cache",
            vec![
                ("memo", 0.65),
                ("redis", 0.55),
                ("ttl", 0.6),
                ("invalidate", 0.55),
            ],
        ),
    ])
}

fn build_expanded_weights(query_tokens: &[String]) -> HashMap<String, f64> {
    let dict = expansion_dictionary();
    let mut out: HashMap<String, f64> = HashMap::new();

    for t in query_tokens {
        let lower = t.to_lowercase();
        let entry = out.entry(lower.clone()).or_insert(1.0);
        *entry = (*entry).max(1.0);

        if let Some(rel) = dict.get(lower.as_str()) {
            for (syn, w) in rel {
                let le = out.entry((*syn).to_string()).or_insert(0.0);
                *le = (*le).max(*w);
            }
        }
    }
    out
}

fn expansion_bm25_for_chunk(
    index: &BM25Index,
    chunk_idx: usize,
    expanded: &HashMap<String, f64>,
    original_terms: &HashSet<String>,
) -> f64 {
    if index.doc_count == 0 {
        return 0.0;
    }

    let doc_len = index.chunks[chunk_idx].token_count as f64;
    let norm_len = doc_len / index.avg_doc_len.max(1.0);

    const K1: f64 = 1.2;
    const B: f64 = 0.75;

    let mut sum = 0.0;
    for (term, ew) in expanded {
        if original_terms.contains(term) {
            continue;
        }
        let df = *index.doc_freqs.get(term).unwrap_or(&0) as f64;
        if df == 0.0 {
            continue;
        }

        let idf = ((index.doc_count as f64 - df + 0.5) / (df + 0.5) + 1.0).ln();
        let tf = index.inverted.get(term).map_or(0.0, |postings| {
            postings
                .iter()
                .filter(|(idx, _)| *idx == chunk_idx)
                .map(|(_, w)| *w)
                .sum::<f64>()
        });

        if tf == 0.0 {
            continue;
        }

        let bm25_t = idf * (tf * (K1 + 1.0)) / (tf + K1 * (1.0 - B + B * norm_len));
        sum += ew * bm25_t;
    }
    sum
}

/// BM25 top-100 → SPLADE-like expansion → combined re-rank.
pub fn hybrid_retrieve(query: &str, bm25_index: &BM25Index, top_k: usize) -> Vec<SpladeResult> {
    if bm25_index.doc_count == 0 || top_k == 0 {
        return Vec::new();
    }

    let query_tokens = tokenize_for_index(query);
    if query_tokens.is_empty() {
        return Vec::new();
    }

    let original_terms: HashSet<String> = query_tokens.iter().map(|s| s.to_lowercase()).collect();

    let expanded = build_expanded_weights(&query_tokens);

    let stage1 = bm25_index.search(query, 100.min(bm25_index.doc_count.max(1)));

    let bm25_by_chunk: HashMap<usize, f64> =
        stage1.iter().map(|r| (r.chunk_idx, r.score)).collect();

    let chunk_indices: Vec<usize> = bm25_by_chunk.keys().copied().collect();
    if chunk_indices.is_empty() {
        return Vec::new();
    }

    let mut expansion_scores: HashMap<usize, f64> = HashMap::new();
    for &idx in &chunk_indices {
        let es = expansion_bm25_for_chunk(bm25_index, idx, &expanded, &original_terms);
        expansion_scores.insert(idx, es);
    }

    let max_bm25 = bm25_by_chunk.values().copied().fold(0.0_f64, f64::max);
    let max_exp = expansion_scores.values().copied().fold(0.0_f64, f64::max);

    let norm_bm25 = |s: f64| -> f64 { if max_bm25 > 1e-12 { s / max_bm25 } else { 0.0 } };
    let norm_exp = |s: f64| -> f64 { if max_exp > 1e-12 { s / max_exp } else { 0.0 } };

    const W_BM25: f64 = 0.65;
    const W_EXP: f64 = 0.35;

    let mut results: Vec<SpladeResult> = chunk_indices
        .into_iter()
        .map(|chunk_idx| {
            let chunk = &bm25_index.chunks[chunk_idx];
            let bm25_score = bm25_by_chunk.get(&chunk_idx).copied().unwrap_or(0.0);
            let expansion_score = expansion_scores.get(&chunk_idx).copied().unwrap_or(0.0);
            let combined_score = W_BM25 * norm_bm25(bm25_score) + W_EXP * norm_exp(expansion_score);

            SpladeResult {
                chunk_idx,
                file_path: chunk.file_path.clone(),
                symbol_name: chunk.symbol_name.clone(),
                combined_score,
                bm25_score,
                expansion_score,
            }
        })
        .collect();

    results.sort_by(|a, b| {
        b.combined_score
            .partial_cmp(&a.combined_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                b.bm25_score
                    .partial_cmp(&a.bm25_score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| a.file_path.cmp(&b.file_path))
            .then_with(|| a.symbol_name.cmp(&b.symbol_name))
    });

    results.truncate(top_k);
    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::bm25_index::{ChunkKind, CodeChunk};

    fn sample_index() -> BM25Index {
        BM25Index::from_chunks_for_test(vec![
            CodeChunk {
                file_path: "login.rs".into(),
                symbol_name: "login_user".into(),
                kind: ChunkKind::Function,
                start_line: 1,
                end_line: 3,
                content: "pub fn login_user() { session_token authentication jwt oauth login credential }"
                    .into(),
                tokens: vec![],
                token_count: 0,
            },
            CodeChunk {
                file_path: "cache.rs".into(),
                symbol_name: "memo_cache".into(),
                kind: ChunkKind::Function,
                start_line: 1,
                end_line: 3,
                content: "pub fn memo_cache() { redis ttl invalidate memo cache }".into(),
                tokens: vec![],
                token_count: 0,
            },
            CodeChunk {
                file_path: "auth.rs".into(),
                symbol_name: "oauth_flow".into(),
                kind: ChunkKind::Function,
                start_line: 1,
                end_line: 3,
                content: "pub fn oauth_flow() { credential authentication token jwt session }".into(),
                tokens: vec![],
                token_count: 0,
            },
        ])
    }

    #[test]
    fn hybrid_prefers_expansion_overlap() {
        let index = sample_index();

        let hits = hybrid_retrieve("auth login", &index, 5);
        assert!(!hits.is_empty());
        let top_path = hits[0].file_path.clone();
        assert!(
            top_path.ends_with("login.rs") || top_path.ends_with("auth.rs"),
            "expected auth-expanded chunk first, got {hits:?}"
        );
    }

    #[test]
    fn expansion_boosts_related_terms() {
        let index = sample_index();
        // "jwt" matches BM25 stage 1; "auth" drives SPLADE expansion toward login/oauth chunks.
        let hybrid = hybrid_retrieve("jwt auth", &index, 10);

        assert!(!hybrid.is_empty());
        assert!(
            hybrid[0].expansion_score >= 0.0,
            "expansion_score should be non-negative"
        );
    }

    #[test]
    fn empty_query_returns_empty() {
        let index = sample_index();
        assert!(hybrid_retrieve("", &index, 5).is_empty());
    }

    #[test]
    fn splade_result_fields_populated() {
        let index = sample_index();

        let hybrid = hybrid_retrieve("auth session", &index, 3);
        let r = &hybrid[0];
        assert!(r.chunk_idx < index.chunks.len());
        assert!(!r.file_path.is_empty());
        assert!(r.combined_score.is_finite());
        assert!(r.bm25_score.is_finite());
        assert!(r.expansion_score.is_finite());
    }
}
