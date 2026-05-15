//! Central compression pipeline — the single entry point for ALL integration modes.
//!
//! This ensures Hybrid and Full MCP modes get identical compression behavior.
//! The pipeline orchestrates Layer 1 + Layer 2 and produces a `TerseResult`
//! with full attribution.

use super::counter;
use super::engine;
use super::residual;
use super::TerseResult;
use crate::core::config::CompressionLevel;

const MAX_TERSE_INPUT_BYTES: usize = 64_000;
const TERSE_BUDGET_MS: u128 = 500;

/// Runs the full compression pipeline on tool/command output.
///
/// This is the **single entry point** for all integration modes:
/// - Full MCP: called from `server/mod.rs` after tool execution
/// - Hybrid: MCP for reads + CLI for shell compression
///
/// Safety: skips compression for inputs > 64KB and enforces a 500ms budget.
pub fn compress(
    input: &str,
    level: &CompressionLevel,
    pattern_compressed: Option<&str>,
) -> TerseResult {
    if !level.is_active() || input.is_empty() {
        let tokens = counter::count(input);
        return TerseResult::passthrough(input.to_string(), tokens);
    }

    if input.len() > MAX_TERSE_INPUT_BYTES {
        let tokens = counter::count(input);
        return TerseResult::passthrough(input.to_string(), tokens);
    }

    let deadline = std::time::Instant::now();

    let mut result = match pattern_compressed {
        Some(after_patterns) => compress_with_patterns(input, after_patterns, level),
        None => compress_direct(input, level),
    };

    if deadline.elapsed().as_millis() < TERSE_BUDGET_MS {
        use std::sync::OnceLock;
        static CDC_ENABLED: OnceLock<bool> = OnceLock::new();
        let cdc = *CDC_ENABLED
            .get_or_init(|| crate::core::config::Config::load().content_defined_chunking);
        if cdc {
            result.output = reorder_cdc_stable(&result.output);
        }
    }

    result
}

/// Reorders compressed output at CDC boundaries so stable blocks (imports,
/// type definitions, module headers) appear before volatile content.
/// Stable-first ordering improves LLM prompt-cache hit rates because caches
/// are prefix-matched.
fn reorder_cdc_stable(output: &str) -> String {
    let chunks = crate::core::rabin_karp::chunk(output);
    if chunks.len() < 3 {
        return output.to_string();
    }

    let bytes = output.as_bytes();
    let mut stable = Vec::new();
    let mut volatile = Vec::new();

    for c in &chunks {
        let end = (c.offset + c.length).min(bytes.len());
        let text = String::from_utf8_lossy(&bytes[c.offset..end]);
        let first_line = text.lines().next().unwrap_or("").trim();
        let is_stable = first_line.starts_with("use ")
            || first_line.starts_with("import ")
            || first_line.starts_with("from ")
            || first_line.starts_with("#include")
            || first_line.starts_with("require")
            || first_line.starts_with("pub mod ")
            || first_line.starts_with("mod ")
            || first_line.starts_with("export ")
            || first_line.starts_with("//!")
            || first_line.starts_with("///");

        if is_stable {
            stable.push(text.into_owned());
        } else {
            volatile.push(text.into_owned());
        }
    }

    if stable.is_empty() || volatile.is_empty() {
        return output.to_string();
    }

    let mut result = String::with_capacity(output.len());
    for s in &stable {
        result.push_str(s);
    }
    for v in &volatile {
        result.push_str(v);
    }
    result
}

fn compress_direct(input: &str, level: &CompressionLevel) -> TerseResult {
    let result = engine::compress(input, level);

    if !result.quality.passed {
        return TerseResult::passthrough(input.to_string(), result.tokens_before);
    }

    TerseResult {
        output: result.output,
        tokens_before: result.tokens_before,
        tokens_after: result.tokens_after,
        savings_pct: counter::savings_pct(result.tokens_before, result.tokens_after),
        layers_applied: vec!["deterministic"],
        pattern_savings: 0,
        terse_savings: result.tokens_before.saturating_sub(result.tokens_after),
        quality_passed: result.quality.passed,
    }
}

fn compress_with_patterns(
    original: &str,
    after_patterns: &str,
    level: &CompressionLevel,
) -> TerseResult {
    let res = residual::compress_residual(original, after_patterns, level);

    let layers = if res.terse_savings > 0 {
        vec!["patterns", "deterministic"]
    } else {
        vec!["patterns"]
    };

    TerseResult {
        output: res.output,
        tokens_before: res.tokens_before_patterns,
        tokens_after: res.tokens_after_terse,
        savings_pct: res.total_savings_pct,
        layers_applied: layers,
        pattern_savings: res.pattern_savings,
        terse_savings: res.terse_savings,
        quality_passed: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn off_level_passthrough() {
        let result = compress("hello world", &CompressionLevel::Off, None);
        assert_eq!(result.output, "hello world");
        assert_eq!(result.savings_pct, 0.0);
        assert!(result.layers_applied.is_empty());
    }

    #[test]
    fn direct_compression_works() {
        let text = "line one\n\n\nline two\n\n\nline three\n\n\n";
        let result = compress(text, &CompressionLevel::Standard, None);
        assert!(
            !result.output.contains("\n\n\n"),
            "should remove empty lines"
        );
    }

    #[test]
    fn pattern_compression_attribution() {
        let original = "This is a very long and verbose text with many unnecessary words and filler content that serves no real purpose";
        let after_patterns = "Short summary";
        let result = compress(original, &CompressionLevel::Standard, Some(after_patterns));
        assert!(
            result.pattern_savings > 0,
            "should attribute savings to patterns"
        );
    }

    #[test]
    fn empty_input_passthrough() {
        let result = compress("", &CompressionLevel::Max, None);
        assert_eq!(result.output, "");
        assert_eq!(result.tokens_before, 0);
    }
}
