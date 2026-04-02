//! Heuristic attention prediction model for LLM context optimization.
//!
//! Based on empirical findings from "Lost in the Middle" (Liu et al., 2023):
//! - Transformers attend strongly to begin and end positions
//! - Middle positions receive ~50% less attention
//! - Structural markers (definitions, errors) attract attention regardless of position
//!
//! This module provides a position + structure based attention estimator
//! that can be used to reorder or filter context for maximum LLM utilization.

/// Compute a U-shaped attention weight for a given position.
/// position: normalized 0.0 (begin) to 1.0 (end)
/// Returns attention weight in [0, 1].
///
/// Uses a quadratic U-curve that better models the empirical findings from
/// Liu et al. (2023) "Lost in the Middle" — attention drops more steeply
/// toward the middle than a linear model predicts.
///
/// Formula: f(x) = α·(1-2x)² + γ·(2x-1)² + β·(1 - (1-2x)² - (2x-1)²)
///        simplified for piecewise: quadratic decay from edges toward center.
pub fn positional_attention(position: f64, alpha: f64, beta: f64, gamma: f64) -> f64 {
    if position <= 0.0 {
        return alpha;
    }
    if position >= 1.0 {
        return gamma;
    }

    if position <= 0.5 {
        let t = position / 0.5;
        let t2 = t * t;
        alpha * (1.0 - t2) + beta * t2
    } else {
        let t = (position - 0.5) / 0.5;
        let t2 = t * t;
        beta * (1.0 - t2) + gamma * t2
    }
}

/// Estimate the structural importance of a line.
/// Returns a multiplier [0.1, 2.0] based on syntactic patterns.
///
/// Weights updated 2026-04-02 based on empirical attention analysis
/// (Lab Experiment B: TinyLlama 1.1B on 106 Rust files):
///   import  → 0.0285 mean attn (was rated 0.6, now 1.6)
///   comment → 0.0123 mean attn (was rated 0.4, now 1.2)
///   definition → 0.0038 (was 1.8, adjusted to 1.5)
///   test/assert → 0.0004 (was 1.5, lowered to 0.8)
pub fn structural_importance(line: &str) -> f64 {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return 0.1;
    }

    if trimmed.starts_with("error")
        || trimmed.starts_with("Error")
        || trimmed.contains("ERROR")
        || trimmed.starts_with("panic")
        || trimmed.starts_with("FAIL")
    {
        return 2.0;
    }

    // Lab finding: imports get 3x more attention than definitions.
    // They establish namespace context the model needs for all subsequent code.
    if trimmed.starts_with("use ")
        || trimmed.starts_with("import ")
        || trimmed.starts_with("from ")
        || trimmed.starts_with("#include")
    {
        return 1.6;
    }

    if is_definition(trimmed) {
        return 1.5;
    }

    // Lab finding: comments are semantic anchors — 3x more attention than logic.
    if trimmed.starts_with("//")
        || trimmed.starts_with("#")
        || trimmed.starts_with("/*")
        || trimmed.starts_with("*")
    {
        return 1.2;
    }

    if trimmed.starts_with("return ") || trimmed.starts_with("yield ") {
        return 1.0;
    }

    if trimmed.starts_with("if ")
        || trimmed.starts_with("match ")
        || trimmed.starts_with("for ")
        || trimmed.starts_with("while ")
    {
        return 0.9;
    }

    // Lab finding: test assertions get minimal attention (0.0004) —
    // lowest of all line types unless the task is about testing.
    if trimmed.starts_with("assert")
        || trimmed.starts_with("expect(")
        || trimmed.starts_with("#[test]")
        || trimmed.starts_with("@Test")
    {
        return 0.8;
    }

    if trimmed == "}" || trimmed == "};" || trimmed == "})" {
        return 0.3;
    }

    0.8
}

/// Compute combined attention score for a line at a given position.
/// Combines positional U-curve with structural importance.
pub fn combined_attention(line: &str, position: f64, alpha: f64, beta: f64, gamma: f64) -> f64 {
    let pos_weight = positional_attention(position, alpha, beta, gamma);
    let struct_weight = structural_importance(line);
    // Geometric mean balances both factors
    (pos_weight * struct_weight).sqrt()
}

/// Reorder lines to maximize predicted attention utilization.
/// Places high-attention lines at begin and end positions.
pub fn attention_optimize(lines: &[&str], _alpha: f64, _beta: f64, _gamma: f64) -> Vec<String> {
    if lines.len() <= 3 {
        return lines.iter().map(|l| l.to_string()).collect();
    }

    let mut scored: Vec<(usize, f64)> = lines
        .iter()
        .enumerate()
        .map(|(i, line)| {
            let importance = structural_importance(line);
            (i, importance)
        })
        .collect();

    // Sort by importance (high first)
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    // Place most important at begin (alpha) and end (gamma) positions
    let n = scored.len();
    let mut result = vec![String::new(); n];
    let mut begin_idx = 0;
    let mut end_idx = n - 1;
    let mut mid_idx = n / 4; // start mid section after first quarter

    for (i, (orig_idx, _importance)) in scored.iter().enumerate() {
        if i % 3 == 0 && begin_idx < n / 3 {
            result[begin_idx] = lines[*orig_idx].to_string();
            begin_idx += 1;
        } else if i % 3 == 1 && end_idx > 2 * n / 3 {
            result[end_idx] = lines[*orig_idx].to_string();
            end_idx -= 1;
        } else {
            if mid_idx < 2 * n / 3 {
                result[mid_idx] = lines[*orig_idx].to_string();
                mid_idx += 1;
            }
        }
    }

    // Fill any remaining empty slots with original order
    let mut remaining: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
    for slot in &mut result {
        if slot.is_empty() {
            if let Some(line) = remaining.pop() {
                *slot = line;
            }
        }
    }

    result
}

/// Compute the theoretical attention efficiency for a given context layout.
/// Returns a percentage [0, 100] indicating how much of the context
/// is in attention-optimal positions.
pub fn attention_efficiency(line_importances: &[f64], alpha: f64, beta: f64, gamma: f64) -> f64 {
    if line_importances.is_empty() {
        return 0.0;
    }

    let n = line_importances.len();
    let mut weighted_sum = 0.0;
    let mut total_importance = 0.0;

    for (i, &importance) in line_importances.iter().enumerate() {
        let pos = i as f64 / (n - 1).max(1) as f64;
        let pos_weight = positional_attention(pos, alpha, beta, gamma);
        weighted_sum += importance * pos_weight;
        total_importance += importance;
    }

    if total_importance == 0.0 {
        return 0.0;
    }

    (weighted_sum / total_importance) * 100.0
}

fn is_definition(line: &str) -> bool {
    let starts = [
        "fn ",
        "pub fn ",
        "async fn ",
        "pub async fn ",
        "struct ",
        "pub struct ",
        "enum ",
        "pub enum ",
        "trait ",
        "pub trait ",
        "impl ",
        "type ",
        "pub type ",
        "const ",
        "pub const ",
        "static ",
        "class ",
        "export class ",
        "interface ",
        "export interface ",
        "function ",
        "export function ",
        "async function ",
        "def ",
        "async def ",
        "func ",
    ];
    starts.iter().any(|s| line.starts_with(s))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positional_u_curve() {
        let begin = positional_attention(0.0, 0.9, 0.5, 0.85);
        let middle = positional_attention(0.5, 0.9, 0.5, 0.85);
        let end = positional_attention(1.0, 0.9, 0.5, 0.85);

        assert!((begin - 0.9).abs() < 0.01);
        assert!((middle - 0.5).abs() < 0.01);
        assert!((end - 0.85).abs() < 0.01);
        assert!(begin > middle);
        assert!(end > middle);
    }

    #[test]
    fn structural_errors_highest() {
        let error = structural_importance("error[E0433]: failed to resolve");
        let import = structural_importance("use std::collections::HashMap;");
        let def = structural_importance("fn main() {");
        let comment = structural_importance("// just a comment");
        let brace = structural_importance("}");

        assert!(error > import, "errors should be highest");
        assert!(
            import > def,
            "imports should outrank definitions (lab finding)"
        );
        assert!(def > comment, "definitions should outrank comments");
        assert!(comment > brace, "comments should outrank closing braces");
    }

    #[test]
    fn combined_high_at_begin_with_definition() {
        let score_begin = combined_attention("fn main() {", 0.0, 0.9, 0.5, 0.85);
        let score_middle = combined_attention("fn main() {", 0.5, 0.9, 0.5, 0.85);
        assert!(score_begin > score_middle);
    }

    #[test]
    fn efficiency_higher_when_important_at_edges() {
        let good_layout = vec![1.8, 0.3, 0.3, 0.3, 1.5]; // important at begin+end
        let bad_layout = vec![0.3, 0.3, 1.8, 1.5, 0.3]; // important in middle

        let eff_good = attention_efficiency(&good_layout, 0.9, 0.5, 0.85);
        let eff_bad = attention_efficiency(&bad_layout, 0.9, 0.5, 0.85);
        assert!(
            eff_good > eff_bad,
            "edges layout ({eff_good:.1}) should beat middle layout ({eff_bad:.1})"
        );
    }
}
