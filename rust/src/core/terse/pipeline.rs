//! Central compression pipeline — the single entry point for ALL integration modes.
//!
//! This ensures Hybrid and Full MCP modes get identical compression behavior.
//! The pipeline orchestrates Layer 1 + Layer 2 and produces a `TerseResult`
//! with full attribution.

use super::TerseResult;
use super::counter;
use super::engine;
use super::residual;
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

    // Structured-data fast path: tool/command output that is a single JSON
    // document (API responses, `gh api`, `cargo --message-format=json`, MCP
    // bridge results) compacts losslessly far better than the token engine.
    // lean-ctx read outputs carry a header line, so they never match here —
    // only genuine pure-JSON payloads do, keeping file reads byte-exact.
    if let Some(compact) = crate::core::structured_compact::compact_json(input) {
        let before = counter::count(input);
        let after = counter::count(&compact);
        if after < before {
            return TerseResult {
                output: compact,
                tokens_before: before,
                tokens_after: after,
                savings_pct: counter::savings_pct(before, after),
                layers_applied: vec!["json-compact"],
                pattern_savings: 0,
                terse_savings: before.saturating_sub(after),
                quality_passed: true,
            };
        }
    }

    let deadline = std::time::Instant::now();

    let mut result = match pattern_compressed {
        Some(after_patterns) => compress_with_patterns(input, after_patterns, level),
        // Fenced code blocks (``` / ~~~) carry verbatim source — diffs, file
        // excerpts, edit evidence (#382). The dictionary/line-score layers
        // would corrupt them, so prose is compressed around the fences and
        // fence content stays byte-exact.
        None => match compress_fence_aware(input, level) {
            Some(fenced) => fenced,
            None => compress_direct(input, level),
        },
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

/// Returns true when `line` opens/closes a fenced code block (`CommonMark`:
/// at most 3 leading spaces, then ≥ 3 backticks or tildes).
fn is_fence_delimiter(line: &str) -> Option<char> {
    let trimmed = line.trim_start_matches(' ');
    if line.len() - trimmed.len() > 3 {
        return None;
    }
    ['`', '~']
        .into_iter()
        .find(|&ch| trimmed.chars().take_while(|&c| c == ch).count() >= 3)
}

/// Compress prose around fenced code blocks, keeping fence content (and the
/// fence delimiter lines) byte-exact. Returns `None` when the input contains
/// no fence — callers fall back to the plain path. An unterminated fence
/// protects everything from its opener to the end of input (conservative:
/// never corrupt what might be code).
fn compress_fence_aware(input: &str, level: &CompressionLevel) -> Option<TerseResult> {
    if !input.contains("```") && !input.contains("~~~") {
        return None;
    }

    // Split into alternating prose/fence segments, preserving line endings.
    let mut segments: Vec<(bool, String)> = Vec::new(); // (is_fence, text)
    let mut current = String::new();
    let mut fence_char: Option<char> = None;

    for line in input.split_inclusive('\n') {
        let stripped = line.strip_suffix('\n').unwrap_or(line);
        match fence_char {
            None => {
                if let Some(ch) = is_fence_delimiter(stripped) {
                    if !current.is_empty() {
                        segments.push((false, std::mem::take(&mut current)));
                    }
                    fence_char = Some(ch);
                    current.push_str(line);
                } else {
                    current.push_str(line);
                }
            }
            Some(ch) => {
                current.push_str(line);
                if is_fence_delimiter(stripped) == Some(ch) {
                    segments.push((true, std::mem::take(&mut current)));
                    fence_char = None;
                }
            }
        }
    }
    if !current.is_empty() {
        // Unterminated fence stays protected; trailing prose is compressible.
        segments.push((fence_char.is_some(), current));
    }

    if !segments.iter().any(|(is_fence, _)| *is_fence) {
        return None;
    }

    let mut output = String::with_capacity(input.len());
    for (is_fence, text) in &segments {
        if *is_fence {
            output.push_str(text);
        } else {
            let res = engine::compress(text, level);
            if res.quality.passed && res.tokens_after < res.tokens_before {
                output.push_str(&res.output);
                // Keep prose/fence boundaries on separate lines.
                if !output.ends_with('\n') && text.ends_with('\n') {
                    output.push('\n');
                }
            } else {
                output.push_str(text);
            }
        }
    }

    let tokens_before = counter::count(input);
    let tokens_after = counter::count(&output);
    Some(TerseResult {
        output,
        tokens_before,
        tokens_after,
        savings_pct: counter::savings_pct(tokens_before, tokens_after),
        layers_applied: vec!["deterministic", "fence-guard"],
        pattern_savings: 0,
        terse_savings: tokens_before.saturating_sub(tokens_after),
        quality_passed: true,
    })
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
        quality_passed: res.quality_passed,
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

    /// #382: fenced code (edit evidence diffs) must survive byte-exact —
    /// no dictionary abbreviation, no blank-line stripping, no line drops.
    #[test]
    fn fenced_code_is_byte_exact() {
        let fence = "```diff\n--- a.py\n+++ a.py\n         \"\"\"Опубликовать задачу.\n\n-        return 0\n+        return 1\n\n             flag=True,\n             flag=True,\n```\n";
        let input = format!(
            "edit applied successfully to the file\n\n{fence}\nthe operation finished and the file was written\n"
        );
        let result = compress(&input, &CompressionLevel::Max, None);
        assert!(
            result.output.contains(fence),
            "fence must be untouched, got:\n{}",
            result.output
        );
        assert!(
            result.output.contains("return 0"),
            "no abbreviation inside fence"
        );
    }

    #[test]
    fn unterminated_fence_is_protected() {
        let input = "prose before\n```python\nreturn 0\n\nreturn 1\n";
        let result = compress(input, &CompressionLevel::Max, None);
        assert!(
            result.output.contains("```python\nreturn 0\n\nreturn 1\n"),
            "everything after an unterminated opener stays verbatim, got:\n{}",
            result.output
        );
    }

    #[test]
    fn no_fence_returns_none_from_fence_path() {
        assert!(compress_fence_aware("plain text, no code", &CompressionLevel::Max).is_none());
        assert!(compress_fence_aware("inline `code` only", &CompressionLevel::Max).is_none());
    }
}
