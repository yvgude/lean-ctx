//! Layer 1: Deterministic output compression.
//!
//! Replaces the legacy `compress_terse`/`compress_ultra` with a scoring-based
//! approach that preserves information-dense lines and removes low-value content.

use super::counter;
use super::dictionaries::{self, DictLevel};
use super::quality::{self, QualityConfig, QualityReport};
use super::scoring;
use crate::core::config::CompressionLevel;

/// Threshold below which a line is considered low-information and may be removed.
const LOW_SCORE_THRESHOLD: f32 = 2.5;

const STANDARD_SCORE_THRESHOLD: f32 = 3.0;
const MAX_SCORE_THRESHOLD: f32 = 3.5;

/// Result of Layer 1 compression.
#[derive(Debug)]
pub struct EngineResult {
    pub output: String,
    pub tokens_before: u32,
    pub tokens_after: u32,
    pub quality: QualityReport,
    pub lines_removed: usize,
    pub lines_total: usize,
}

const MIN_LINES_FOR_COMPRESSION: usize = 5;

/// Runs Layer 1 deterministic compression on the input text.
#[must_use]
pub fn compress(text: &str, level: &CompressionLevel) -> EngineResult {
    let tokens_before = counter::count(text);
    let lines_total = text.lines().count();

    if !level.is_active() || text.is_empty() || lines_total < MIN_LINES_FOR_COMPRESSION {
        return EngineResult {
            output: text.to_string(),
            tokens_before,
            tokens_after: tokens_before,
            quality: quality::check(
                text,
                text,
                tokens_before,
                tokens_before,
                &QualityConfig::default(),
            ),
            lines_removed: 0,
            lines_total,
        };
    }

    let result = compress_at_level(text, tokens_before, level);

    if result.quality.passed {
        return result;
    }

    if *level == CompressionLevel::Max {
        let fallback = compress_at_level(text, tokens_before, &CompressionLevel::Standard);
        if fallback.quality.passed {
            return fallback;
        }
    }

    EngineResult {
        output: text.to_string(),
        tokens_before,
        tokens_after: tokens_before,
        quality: result.quality,
        lines_removed: 0,
        lines_total: text.lines().count(),
    }
}

fn compress_at_level(text: &str, tokens_before: u32, level: &CompressionLevel) -> EngineResult {
    let scores = scoring::score_lines(text);
    let lines: Vec<&str> = text.lines().collect();
    let lines_total = lines.len();

    let threshold = match level {
        CompressionLevel::Max => MAX_SCORE_THRESHOLD,
        CompressionLevel::Standard => STANDARD_SCORE_THRESHOLD,
        CompressionLevel::Lite | CompressionLevel::Off => LOW_SCORE_THRESHOLD,
    };

    let mut kept_lines = Vec::new();
    let mut lines_removed = 0;

    for (score, line) in scores.iter().zip(lines.iter()) {
        let trimmed = line.trim();

        if trimmed.is_empty() {
            lines_removed += 1;
            continue;
        }

        if is_pure_decoration(trimmed) {
            lines_removed += 1;
            continue;
        }

        if is_filler_line(trimmed) && !score.has_structural_marker {
            lines_removed += 1;
            continue;
        }

        if score.combined < threshold && !score.has_structural_marker {
            lines_removed += 1;
            continue;
        }

        kept_lines.push(*line);
    }

    let filtered = kept_lines.join("\n");

    let quality_config = match level {
        CompressionLevel::Max => QualityConfig {
            min_identifier_preservation: 0.80,
            ..QualityConfig::default()
        },
        _ => QualityConfig::default(),
    };

    let filtered_tokens = counter::count(&filtered);
    let quality_report = quality::check(
        text,
        &filtered,
        tokens_before,
        filtered_tokens,
        &quality_config,
    );

    if !quality_report.passed {
        return EngineResult {
            output: text.to_string(),
            tokens_before,
            tokens_after: tokens_before,
            quality: quality_report,
            lines_removed: 0,
            lines_total,
        };
    }

    let dict_level = match level {
        CompressionLevel::Max | CompressionLevel::Standard => DictLevel::Full,
        CompressionLevel::Lite | CompressionLevel::Off => DictLevel::General,
    };
    let compressed = dictionaries::apply_dictionaries(&filtered, dict_level);
    let tokens_after = counter::count(&compressed);

    EngineResult {
        output: compressed,
        tokens_before,
        tokens_after,
        quality: quality_report,
        lines_removed,
        lines_total,
    }
}

fn is_filler_line(line: &str) -> bool {
    let trimmed = line.trim();

    if trimmed == "|" || trimmed == "| " {
        return true;
    }

    let lower = line.to_lowercase();
    const FILLER_PATTERNS: &[&str] = &[
        "use \"git add",
        "use \"git restore",
        "(use \"git",
        "run with `rust_backtrace",
        "for more information about this error",
        "try `rustc --explain",
        "run `npm fund`",
        "run `npm audit`",
        "to address all issues",
        "sending build context",
        "using cache",
        "packages are looking for funding",
        "no changes added to commit",
        "help: ",
        "= note: ",
        "---> running in",
    ];
    FILLER_PATTERNS.iter().any(|p| lower.contains(p))
}

fn is_pure_decoration(line: &str) -> bool {
    let trimmed = line.trim();

    if trimmed.is_empty() {
        return true;
    }

    if trimmed.chars().all(|c| c == '|' || c.is_whitespace()) {
        return true;
    }

    if line.len() < 3 {
        return false;
    }

    if line.starts_with("//") || line.starts_with('#') || line.starts_with("--") {
        let content = line
            .trim_start_matches('/')
            .trim_start_matches('#')
            .trim_start_matches('-')
            .trim();
        return content.is_empty() || is_banner_chars(content);
    }

    is_banner_chars(line)
}

fn is_banner_chars(line: &str) -> bool {
    let chars: Vec<char> = line.chars().collect();
    if chars.len() < 4 {
        return false;
    }
    let first = chars[0];
    if matches!(
        first,
        '=' | '-' | '*' | '─' | '━' | '▀' | '▄' | '╔' | '╚' | '║' | '░' | '█' | '═'
    ) {
        let same_count = chars.iter().filter(|c| **c == first).count();
        return same_count as f64 / chars.len() as f64 > 0.6;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compress_off_returns_original() {
        let text = "hello world\n\nsome blank lines\n\n";
        let result = compress(text, &CompressionLevel::Off);
        assert_eq!(result.output, text);
        assert_eq!(result.lines_removed, 0);
    }

    #[test]
    fn compress_lite_removes_blank_lines() {
        let text = "line one\n\n\nline two\n\n";
        let result = compress(text, &CompressionLevel::Lite);
        assert!(
            !result.output.contains("\n\n"),
            "blank lines should be removed"
        );
    }

    #[test]
    fn compress_preserves_paths() {
        let text = "error in src/main.rs at line 42\n\nsome blank\n\n";
        let result = compress(text, &CompressionLevel::Standard);
        assert!(
            result.output.contains("src/main.rs"),
            "path must be preserved"
        );
    }

    #[test]
    fn decoration_detection() {
        assert!(is_pure_decoration("════════════════════"));
        assert!(is_pure_decoration("--------------------"));
        assert!(is_pure_decoration("// ================"));
        assert!(!is_pure_decoration("error: mismatched types"));
    }

    #[test]
    fn compress_returns_token_counts() {
        let text = "Hello world from the compression engine test";
        let result = compress(text, &CompressionLevel::Lite);
        assert!(result.tokens_before > 0);
    }
}
