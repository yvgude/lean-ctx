//! Attention-Weighted Context Assembly.
//!
//! Scientific basis:
//! - Treisman (1980), "A feature-integration theory of attention" — cognitive resources
//!   are allocated proportionally to information density, not uniformly.
//! - Transformer Self-Attention (Vaswani et al., 2017) — weighted focus on informative regions.
//!
//! Applied here: when assembling context from multiple search results, we:
//! 1. Score each chunk's information density (unique tokens / total tokens)
//! 2. Detect redundancy between selected chunks (token-level overlap)
//! 3. Allocate token budget proportionally to density, not uniformly
//! 4. Aggressively truncate low-density chunks

use std::collections::HashSet;

/// Information density metrics for a code chunk.
#[derive(Debug, Clone)]
pub struct ChunkDensity {
    pub chunk_idx: usize,
    /// Lexical diversity: `unique_tokens` / `total_tokens` (0.0 - 1.0)
    pub lexical_diversity: f64,
    /// Structural importance: definitions, exports, public APIs score higher
    pub structural_weight: f64,
    /// Combined attention score
    pub attention_score: f64,
    /// Allocated token budget for this chunk
    pub token_budget: usize,
}

/// Compute information density for a chunk.
#[must_use]
pub fn compute_density(content: &str, is_definition: bool) -> f64 {
    let tokens: Vec<&str> = content.split_whitespace().collect();
    if tokens.is_empty() {
        return 0.0;
    }

    let unique: HashSet<&str> = tokens.iter().copied().collect();
    let lexical_diversity = unique.len() as f64 / tokens.len() as f64;

    // Structural weight: definitions are more information-dense
    let structural = if is_definition { 1.3 } else { 1.0 };

    // Penalize highly repetitive content (boilerplate)
    let repetition_penalty = if lexical_diversity < 0.3 { 0.5 } else { 1.0 };

    lexical_diversity * structural * repetition_penalty
}

/// Compute pairwise redundancy between two chunks (0.0 = unique, 1.0 = identical).
#[must_use]
pub fn compute_redundancy(content_a: &str, content_b: &str) -> f64 {
    let tokens_a: HashSet<&str> = content_a.split_whitespace().collect();
    let tokens_b: HashSet<&str> = content_b.split_whitespace().collect();

    if tokens_a.is_empty() || tokens_b.is_empty() {
        return 0.0;
    }

    let intersection = tokens_a.intersection(&tokens_b).count();
    let union = tokens_a.union(&tokens_b).count();

    if union == 0 {
        0.0
    } else {
        intersection as f64 / union as f64
    }
}

/// Assemble context with attention-weighted budget allocation.
///
/// Given a total token budget and a list of chunks with their content,
/// allocates budget proportionally to information density while penalizing
/// redundancy between chunks.
#[must_use]
pub fn attention_weighted_assembly(
    chunks: &[(usize, &str, bool)], // (chunk_idx, content, is_definition)
    total_budget: usize,
) -> Vec<ChunkDensity> {
    if chunks.is_empty() {
        return Vec::new();
    }

    // Step 1: Compute raw density scores
    let mut densities: Vec<ChunkDensity> = chunks
        .iter()
        .map(|&(idx, content, is_def)| {
            let density = compute_density(content, is_def);
            ChunkDensity {
                chunk_idx: idx,
                lexical_diversity: density,
                structural_weight: if is_def { 1.3 } else { 1.0 },
                attention_score: density,
                token_budget: 0,
            }
        })
        .collect();

    // Step 2: Apply redundancy penalties (MMR-like)
    // For large chunk sets, limit comparisons to a sliding window to keep O(n*w)
    let window_size = 20.min(densities.len());
    for i in 1..densities.len() {
        let mut max_redundancy = 0.0f64;
        let start = i.saturating_sub(window_size);
        for j in start..i {
            let redundancy = compute_redundancy(chunks[i].1, chunks[j].1);
            max_redundancy = max_redundancy.max(redundancy);
        }
        densities[i].attention_score *= 1.0 - (max_redundancy * 0.7);
    }

    // Step 3: Normalize attention scores and allocate budget
    let total_attention: f64 = densities.iter().map(|d| d.attention_score).sum();
    if total_attention > 0.0 {
        for density in &mut densities {
            let fraction = density.attention_score / total_attention;
            // Minimum 10% of equal-share, maximum 300% of equal-share
            let equal_share = total_budget as f64 / chunks.len() as f64;
            let raw_budget = fraction * total_budget as f64;
            let clamped = raw_budget.max(equal_share * 0.1).min(equal_share * 3.0);
            density.token_budget = clamped as usize;
        }
    } else {
        // Fallback: equal distribution
        let per_chunk = total_budget / chunks.len().max(1);
        for density in &mut densities {
            density.token_budget = per_chunk;
        }
    }

    densities
}

/// Truncate content to fit within a token budget (approximate by chars/4).
#[must_use]
pub fn truncate_to_budget(content: &str, token_budget: usize) -> &str {
    let char_budget = token_budget * 4; // rough approximation
    if content.len() <= char_budget {
        return content;
    }

    let safe_end = content.floor_char_boundary(char_budget.min(content.len()));
    let truncated = &content[..safe_end];
    match truncated.rfind('\n') {
        Some(pos) => &content[..=pos],
        None => truncated,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn high_diversity_gets_more_budget() {
        let chunks = vec![
            (
                0,
                "fn unique_function_name() { let x = compute_something(); }",
                true,
            ),
            (
                1,
                "test test test test test test test test test test",
                false,
            ),
        ];

        let result = attention_weighted_assembly(&chunks, 1000);
        assert_eq!(result.len(), 2);
        // High-diversity chunk should get more budget
        assert!(result[0].token_budget > result[1].token_budget);
    }

    #[test]
    fn redundant_chunks_get_less_budget() {
        let chunks = vec![
            (0, "fn auth_login() { validate_token(jwt) }", true),
            (1, "fn auth_login() { validate_token(jwt) }", false), // duplicate
            (2, "fn database_query() { execute_sql(conn) }", true),
        ];

        let result = attention_weighted_assembly(&chunks, 1000);
        // Second chunk (redundant) should get less than first
        assert!(result[1].token_budget < result[0].token_budget);
    }

    #[test]
    fn empty_input_returns_empty() {
        let result = attention_weighted_assembly(&[], 1000);
        assert!(result.is_empty());
    }

    #[test]
    fn compute_density_values_make_sense() {
        let high = compute_density("fn unique name with diverse tokens here now", false);
        let low = compute_density("test test test test test test", false);
        assert!(high > low);
    }

    #[test]
    fn redundancy_of_identical_is_one() {
        let r = compute_redundancy("hello world foo bar", "hello world foo bar");
        assert!((r - 1.0).abs() < 0.001);
    }

    #[test]
    fn redundancy_of_disjoint_is_zero() {
        let r = compute_redundancy("alpha beta gamma", "delta epsilon zeta");
        assert!((r - 0.0).abs() < 0.001);
    }

    #[test]
    fn truncate_respects_budget() {
        let content = "line1\nline2\nline3\nline4\nline5\n";
        let truncated = truncate_to_budget(content, 3); // ~12 chars
        assert!(truncated.len() <= 12);
    }

    #[test]
    fn truncate_utf8_russian_no_panic() {
        let content = "Первая строка\nВторая строка\nТретья строка\nЧетвёртая строка\n";
        let truncated = truncate_to_budget(content, 5);
        assert!(content.is_char_boundary(truncated.len()));
    }

    #[test]
    fn truncate_utf8_cjk_boundary() {
        let content = "日本語\n中文\n한국어\nこんにちは\n";
        let truncated = truncate_to_budget(content, 2);
        assert!(content.is_char_boundary(truncated.len()));
    }
}
