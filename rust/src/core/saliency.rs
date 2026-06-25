//! Saliency Map — information-theoretic chunk ranking and deduplication.
//!
//! Implements two key algorithms from recent information theory:
//!
//!   **ECS (Entropic Context Shaping, arXiv 2025):**
//!   Scores each chunk by its pragmatic utility — how much it shifts the
//!   LLM's output distribution toward the correct answer. Operationalized as
//!   a composite of task relevance, graph centrality, and information density.
//!
//!   **MIG (Marginal Information Gain, COMI 2025):**
//!   Removes redundant chunks by measuring each chunk's marginal contribution
//!   given the already-selected set: `MIG(c_i)` = `Relevance(c_i)` - `Redundancy(c_i`, selected).
//!
//! Scientific basis: Saliency maps (Itti & Koch 2001) + lateral inhibition
//! (V1 cortex) for competitive chunk selection.

use crate::core::content_chunk::ContentChunk;

/// Saliency score for a single chunk.
#[derive(Debug, Clone)]
pub struct SaliencyScore {
    pub chunk_idx: usize,
    pub ecs_score: f64,
    pub task_relevance: f64,
    pub graph_centrality: f64,
    pub info_density: f64,
    pub final_score: f64,
}

/// Weights for the ECS composite score.
#[derive(Debug, Clone)]
pub struct EcsWeights {
    pub w_task: f64,
    pub w_graph: f64,
    pub w_density: f64,
}

impl Default for EcsWeights {
    fn default() -> Self {
        Self {
            w_task: 0.5,
            w_graph: 0.3,
            w_density: 0.2,
        }
    }
}

/// Compute ECS saliency scores for a set of chunks.
///
/// For each chunk, the score is:
///   ECS = `w_task` * `task_relevance` + `w_graph` * `graph_centrality` + `w_density` * `info_density`
///
/// `task_keywords`: words from the active task description.
/// `graph_edges_per_chunk`: number of graph edges touching each chunk's file.
#[must_use]
pub fn compute_ecs_scores(
    chunks: &[ContentChunk],
    task_keywords: &[String],
    graph_edge_counts: &[usize],
    weights: &EcsWeights,
) -> Vec<SaliencyScore> {
    let max_edges = graph_edge_counts.iter().max().copied().unwrap_or(1).max(1) as f64;

    chunks
        .iter()
        .enumerate()
        .map(|(i, chunk)| {
            let task_relevance = compute_task_relevance(chunk, task_keywords);
            let graph_centrality =
                graph_edge_counts.get(i).copied().unwrap_or(0) as f64 / max_edges;
            let info_density = compute_info_density(chunk);

            let ecs_score = weights.w_task * task_relevance
                + weights.w_graph * graph_centrality
                + weights.w_density * info_density;

            SaliencyScore {
                chunk_idx: i,
                ecs_score,
                task_relevance,
                graph_centrality,
                info_density,
                final_score: ecs_score,
            }
        })
        .collect()
}

/// Task relevance: fraction of task keywords present in the chunk.
fn compute_task_relevance(chunk: &ContentChunk, task_keywords: &[String]) -> f64 {
    if task_keywords.is_empty() {
        return 0.5;
    }
    let content_lower = chunk.content.to_lowercase();
    let title_lower = chunk.symbol_name.to_lowercase();
    let combined = format!("{content_lower} {title_lower}");

    let matches = task_keywords
        .iter()
        .filter(|kw| combined.contains(&kw.to_lowercase()))
        .count();

    matches as f64 / task_keywords.len() as f64
}

/// Information density: unique tokens / total tokens (normalized).
/// Higher density = more diverse information, less repetition.
fn compute_info_density(chunk: &ContentChunk) -> f64 {
    if chunk.token_count == 0 {
        return 0.0;
    }
    let unique: std::collections::HashSet<&str> = chunk.content.split_whitespace().collect();
    let total = chunk.content.split_whitespace().count().max(1);
    (unique.len() as f64 / total as f64).min(1.0)
}

// ---------------------------------------------------------------------------
// MIG: Marginal Information Gain — redundancy-free chunk selection
// ---------------------------------------------------------------------------

/// Select top-k chunks using MIG: Marginal Information Gain.
///
/// Greedily selects chunks that maximize relevance while minimizing
/// redundancy with already-selected chunks.
///
/// `lambda`: trade-off between relevance and diversity (0.0 = pure relevance,
/// 1.0 = pure diversity). Default: 0.6.
#[must_use]
pub fn mig_select(
    scores: &[SaliencyScore],
    chunks: &[ContentChunk],
    top_k: usize,
    lambda: f64,
) -> Vec<usize> {
    if scores.is_empty() || top_k == 0 {
        return Vec::new();
    }

    let mut selected: Vec<usize> = Vec::with_capacity(top_k);
    let mut available: Vec<usize> = (0..scores.len()).collect();

    // First: pick highest ECS score.
    available.sort_by(|a, b| {
        scores[*b]
            .ecs_score
            .partial_cmp(&scores[*a].ecs_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    if let Some(&first) = available.first() {
        selected.push(first);
        available.retain(|&i| i != first);
    }

    // Greedy MIG selection.
    while selected.len() < top_k && !available.is_empty() {
        let mut best_idx = available[0];
        let mut best_mig = f64::NEG_INFINITY;

        for &candidate in &available {
            let relevance = scores[candidate].ecs_score;
            let redundancy = max_similarity_to_selected(candidate, &selected, chunks);
            let mig = (1.0 - lambda) * relevance - lambda * redundancy;

            if mig > best_mig {
                best_mig = mig;
                best_idx = candidate;
            }
        }

        selected.push(best_idx);
        available.retain(|&i| i != best_idx);
    }

    selected
}

/// Jaccard similarity between two chunks' token sets (fast approximation).
fn chunk_similarity(a: &ContentChunk, b: &ContentChunk) -> f64 {
    let tokens_a: std::collections::HashSet<&str> = a.content.split_whitespace().collect();
    let tokens_b: std::collections::HashSet<&str> = b.content.split_whitespace().collect();

    if tokens_a.is_empty() && tokens_b.is_empty() {
        return 1.0;
    }

    let intersection = tokens_a.intersection(&tokens_b).count();
    let union = tokens_a.union(&tokens_b).count().max(1);

    intersection as f64 / union as f64
}

/// Max similarity between a candidate and all already-selected chunks.
fn max_similarity_to_selected(
    candidate: usize,
    selected: &[usize],
    chunks: &[ContentChunk],
) -> f64 {
    selected
        .iter()
        .map(|&s| chunk_similarity(&chunks[candidate], &chunks[s]))
        .fold(0.0, f64::max)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::chunk_data::ChunkKind;

    fn make_chunk(title: &str, content: &str) -> ContentChunk {
        ContentChunk::from_provider(
            "test",
            "issues",
            title,
            title,
            ChunkKind::Issue,
            content.into(),
            vec![],
            None,
        )
    }

    #[test]
    fn ecs_score_higher_for_relevant_chunk() {
        let chunks = vec![
            make_chunk("auth-bug", "authentication token expiry broken"),
            make_chunk("css-issue", "sidebar layout broken on mobile"),
        ];
        let keywords = vec!["authentication".into(), "token".into()];
        let edge_counts = vec![0, 0];

        let scores = compute_ecs_scores(&chunks, &keywords, &edge_counts, &EcsWeights::default());
        assert!(scores[0].ecs_score > scores[1].ecs_score);
        assert!(scores[0].task_relevance > scores[1].task_relevance);
    }

    #[test]
    fn ecs_score_boosts_high_graph_centrality() {
        let chunks = vec![
            make_chunk("hub-file", "important module"),
            make_chunk("leaf-file", "minor utility"),
        ];
        let keywords: Vec<String> = vec![];
        let edge_counts = vec![10, 1];

        let scores = compute_ecs_scores(&chunks, &keywords, &edge_counts, &EcsWeights::default());
        assert!(scores[0].graph_centrality > scores[1].graph_centrality);
    }

    #[test]
    fn info_density_higher_for_diverse_content() {
        let diverse = make_chunk(
            "diverse",
            "authentication token validation expiry check refresh",
        );
        let repetitive = make_chunk("repetitive", "token token token token token token token");

        let d_density = compute_info_density(&diverse);
        let r_density = compute_info_density(&repetitive);
        assert!(d_density > r_density);
    }

    #[test]
    fn mig_select_picks_diverse_chunks() {
        let chunks = vec![
            make_chunk("auth-1", "authentication token expiry validation"),
            make_chunk("auth-2", "authentication token expiry check"),
            make_chunk("db-issue", "database connection pool exhausted timeout"),
        ];
        let keywords = vec!["authentication".into(), "database".into()];
        let edge_counts = vec![0, 0, 0];

        let scores = compute_ecs_scores(&chunks, &keywords, &edge_counts, &EcsWeights::default());
        let selected = mig_select(&scores, &chunks, 2, 0.6);

        assert_eq!(selected.len(), 2);
        // Should select auth-1 (highest relevance) and db-issue (diverse),
        // NOT auth-1 + auth-2 (redundant).
        assert!(selected.contains(&0));
        assert!(selected.contains(&2));
    }

    #[test]
    fn mig_select_respects_top_k() {
        let chunks = vec![
            make_chunk("a", "content a"),
            make_chunk("b", "content b"),
            make_chunk("c", "content c"),
        ];
        let scores = compute_ecs_scores(&chunks, &[], &[0, 0, 0], &EcsWeights::default());

        let selected = mig_select(&scores, &chunks, 1, 0.6);
        assert_eq!(selected.len(), 1);

        let selected = mig_select(&scores, &chunks, 10, 0.6);
        assert_eq!(selected.len(), 3);
    }

    #[test]
    fn mig_select_empty_input() {
        let selected = mig_select(&[], &[], 5, 0.6);
        assert!(selected.is_empty());
    }

    #[test]
    fn chunk_similarity_identical() {
        let a = make_chunk("a", "same content here");
        assert!((chunk_similarity(&a, &a) - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn chunk_similarity_disjoint() {
        let a = make_chunk("a", "authentication token validation");
        let b = make_chunk("b", "database connection pool exhausted");
        let sim = chunk_similarity(&a, &b);
        assert!(sim < 0.2);
    }

    #[test]
    fn default_weights_sum_to_one() {
        let w = EcsWeights::default();
        assert!((w.w_task + w.w_graph + w.w_density - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn no_task_keywords_gives_neutral_relevance() {
        let chunk = make_chunk("test", "some content");
        let relevance = compute_task_relevance(&chunk, &[]);
        assert!((relevance - 0.5).abs() < f64::EPSILON);
    }
}
