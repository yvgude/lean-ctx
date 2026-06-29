//! L1 output compression (#1093): shrink downstream tool output to a token
//! budget before it reaches the model.
//!
//! Deterministic (#498): the output is a pure function of (content, budget) —
//! the same input always yields the same bytes, so it never defeats provider
//! prompt-caching. The result is capped at the input (it never inflates).

use crate::core::entropy::entropy_compress_to_density;
use crate::core::tokens::count_tokens;

/// Compress `text` down to roughly `budget_tokens`. Returns the input unchanged
/// when it already fits (no inflation), and only ever returns something smaller.
///
/// Two passes, cheapest first:
///   1. **Lossless**: if the body is JSON, minify it (whitespace-only removal).
///   2. **Lossy**: keep the highest-entropy lines down to the token budget.
#[must_use]
pub fn to_budget(text: &str, budget_tokens: usize) -> String {
    let original = count_tokens(text);
    if original == 0 || original <= budget_tokens {
        return text.to_string();
    }

    // Pass 1 — lossless JSON minify. Often enough on whitespace-heavy payloads.
    let minified = minify_if_json(text);
    let after_minify = count_tokens(&minified);
    if after_minify <= budget_tokens {
        return minified;
    }

    // Pass 2 — lossy, entropy-ranked line selection to the budget.
    #[allow(clippy::cast_precision_loss)]
    let target = (budget_tokens as f64 / after_minify.max(1) as f64).clamp(0.05, 1.0);
    let compressed = entropy_compress_to_density(&minified, target).output;

    // Anti-inflation guard (#361): never hand back more than we received.
    if count_tokens(&compressed) >= original {
        text.to_string()
    } else {
        compressed
    }
}

/// Minify a JSON body by re-serializing it without insignificant whitespace.
/// Lossless and deterministic (object key order is preserved by the parser).
/// Returns the input unchanged when it is not valid JSON.
fn minify_if_json(text: &str) -> String {
    let trimmed = text.trim();
    if !(trimmed.starts_with('{') || trimmed.starts_with('[')) {
        return text.to_string();
    }
    match serde_json::from_str::<serde_json::Value>(trimmed) {
        Ok(v) => serde_json::to_string(&v).unwrap_or_else(|_| text.to_string()),
        Err(_) => text.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn under_budget_is_unchanged() {
        let text = "short output";
        assert_eq!(to_budget(text, 1000), text);
    }

    #[test]
    fn over_budget_shrinks() {
        let big: String = (0..4000)
            .map(|i| format!("metric {i} = {}\n", i * 7))
            .collect();
        let out = to_budget(&big, 200);
        assert!(count_tokens(&out) < count_tokens(&big));
        assert!(!out.is_empty());
    }

    #[test]
    fn is_deterministic() {
        let big: String = (0..3000)
            .map(|i| format!("row {i} payload {}\n", i % 13))
            .collect();
        let a = to_budget(&big, 150);
        let b = to_budget(&big, 150);
        assert_eq!(a, b, "compression must be byte-stable across calls (#498)");
    }

    #[test]
    fn json_is_minified_losslessly_first() {
        // Whitespace-heavy JSON that fits the budget once minified stays valid
        // JSON (lossless pass), rather than being entropy-shredded.
        let pretty = "{\n    \"a\":    1,\n    \"b\":    2,\n    \"c\":    3\n}";
        let out = to_budget(pretty, 8);
        let parsed: serde_json::Value = serde_json::from_str(&out).expect("still valid JSON");
        assert_eq!(parsed["a"], 1);
        assert_eq!(parsed["c"], 3);
        assert!(!out.contains("    "), "insignificant whitespace removed");
    }
}
