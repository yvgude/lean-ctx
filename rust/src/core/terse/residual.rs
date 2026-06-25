//! Layer 2: Pattern-aware residual compression.
//!
//! Runs AFTER pattern compression (95+ command-specific patterns) to apply
//! terse compression only on the residual text that patterns didn't touch.
//! Tracks attribution: how much came from patterns vs. terse.

use super::counter;
use super::engine;
use crate::core::config::CompressionLevel;

/// Result of residual compression with attribution split.
#[derive(Debug)]
pub struct ResidualResult {
    pub output: String,
    pub tokens_before_patterns: u32,
    pub tokens_after_patterns: u32,
    pub tokens_after_terse: u32,
    pub pattern_savings: u32,
    pub terse_savings: u32,
    pub total_savings_pct: f32,
    pub quality_passed: bool,
}

/// Applies Layer 2 residual compression.
///
/// Takes the output AFTER pattern compression and applies terse on top,
/// tracking the attribution split between pattern and terse savings.
#[must_use]
pub fn compress_residual(
    original_text: &str,
    pattern_compressed: &str,
    level: &CompressionLevel,
) -> ResidualResult {
    let tokens_before = counter::count(original_text);
    let tokens_after_patterns = counter::count(pattern_compressed);
    let pattern_savings = tokens_before.saturating_sub(tokens_after_patterns);

    if !level.is_active() {
        return ResidualResult {
            output: pattern_compressed.to_string(),
            tokens_before_patterns: tokens_before,
            tokens_after_patterns,
            tokens_after_terse: tokens_after_patterns,
            pattern_savings,
            terse_savings: 0,
            total_savings_pct: counter::savings_pct(tokens_before, tokens_after_patterns),
            quality_passed: true,
        };
    }

    let engine_result = engine::compress(pattern_compressed, level);
    let tokens_after_terse = engine_result.tokens_after;
    let terse_savings = tokens_after_patterns.saturating_sub(tokens_after_terse);

    let total_savings_pct = counter::savings_pct(tokens_before, tokens_after_terse);

    ResidualResult {
        output: engine_result.output,
        tokens_before_patterns: tokens_before,
        tokens_after_patterns,
        tokens_after_terse,
        pattern_savings,
        terse_savings,
        total_savings_pct,
        quality_passed: engine_result.quality.passed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn residual_off_passes_through() {
        let result = compress_residual("hello world", "hello world", &CompressionLevel::Off);
        assert_eq!(result.output, "hello world");
        assert_eq!(result.terse_savings, 0);
    }

    #[test]
    fn residual_tracks_pattern_savings() {
        let original = "This is a long verbose original text with many tokens and words";
        let after_patterns = "Short text";
        let result = compress_residual(original, after_patterns, &CompressionLevel::Standard);
        assert!(result.pattern_savings > 0, "should track pattern savings");
    }

    #[test]
    fn attribution_split_sums_correctly() {
        let original =
            "line one\n\n\nline two\n\n\nline three\nand more verbose content here please";
        let after_patterns =
            "line one\n\n\nline two\n\n\nline three\nand more verbose content here please";
        let result = compress_residual(original, after_patterns, &CompressionLevel::Standard);
        let total = result.pattern_savings + result.terse_savings;
        let expected = result
            .tokens_before_patterns
            .saturating_sub(result.tokens_after_terse);
        assert_eq!(
            total, expected,
            "pattern + terse should equal total savings"
        );
    }
}
