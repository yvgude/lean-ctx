//! Frozen-region prose compression for the proxy (#710).
//!
//! The proxy already prunes OLD tool-result content at a frozen, cache-aware
//! boundary (see [`super::history_prune`]). This module adds the *prose*
//! counterpart: when an operator opts in via `[proxy.role_aggressiveness]`,
//! system and user free-text is squeezed with a deterministic, anti-inflation
//! pass. Because the output is a pure function of `(text, aggressiveness)`, a
//! frozen-region rewrite is byte-identical on every later turn, so the provider
//! prompt-cache prefix stays valid (#498).
//!
//! Assistant turns are never passed to this module — the passthrough guarantee
//! lives at the call sites, which only invoke it for system/user roles.

use serde_json::Value;

use crate::core::aggressiveness::AggressivenessProfile;
use crate::core::tokens::count_tokens;
use crate::core::web::distill::squeeze_prose;

/// Below this many chars a string is never worth a prose pass — the squeeze can
/// only add risk, not save meaningful tokens. Keeps short instructions intact.
const MIN_PROSE_CHARS: usize = 400;

/// Code/structured symbols whose density cleanly separates source, JSON, logs
/// and tables from natural-language prose.
const CODE_SYMBOLS: &str = "{}<>;=|\\$`[]";

/// Compress a single prose string at `aggressiveness` (`0.0–1.0`).
///
/// Returns `Some(compressed)` only when the text is long enough, *looks like*
/// prose, and the squeeze actually saves tokens; otherwise `None` (leave the
/// original intact — the anti-inflation guard). Deterministic: the result is a
/// pure function of `(text, aggressiveness)`.
#[must_use]
pub fn compress_prose(text: &str, aggressiveness: f64) -> Option<String> {
    if text.len() < MIN_PROSE_CHARS || !looks_like_prose(text) {
        return None;
    }
    let profile = AggressivenessProfile::from_level(aggressiveness);
    // `density_target` is the fraction of content to keep; map it to a char
    // budget. Below the budget the squeeze is a near-lossless dedup pass; when it
    // must actually shrink (budget < len) we use cache-safe extractive ranking
    // (#895) — keeping the most central sentences instead of just the prefix —
    // which falls back to truncation when the embedding engine is unavailable.
    let budget = ((text.len() as f64) * profile.density_target).ceil() as usize;
    let squeezed = if budget < text.len() {
        crate::proxy::prose_ranker::squeeze(text, budget)
    } else {
        squeeze_prose(text, budget)
    };
    let before = count_tokens(text);
    let after = count_tokens(&squeezed);
    (after < before).then_some(squeezed)
}

/// Conservative prose gate: substantial, letter-dense, low on code symbols.
/// Excludes source code, JSON, logs and tables while accepting natural-language
/// system prompts and user turns (including bulleted instructions).
fn looks_like_prose(text: &str) -> bool {
    let sample: String = text.chars().take(4000).collect();
    let total = sample.chars().count();
    if total < 200 {
        return false;
    }
    let total_f = total as f32;
    let alpha = sample.chars().filter(|c| c.is_alphabetic()).count() as f32;
    let spaces = sample.chars().filter(|c| c.is_whitespace()).count() as f32;
    let symbols = sample.chars().filter(|c| CODE_SYMBOLS.contains(*c)).count() as f32;
    // Real prose has sentences; source code, JSON, logs and tables largely do
    // not. This is the signal that separates `let x = f(a);`-dense code (which
    // slips under a pure symbol-ratio gate) from natural-language instructions.
    let sentences = sample.matches(['.', '!', '?']).count();
    alpha / total_f >= 0.5
        && spaces / total_f >= 0.08
        && symbols / total_f <= 0.06
        && sentences >= 3
}

/// `true` if a content block carries a `cache_control` breakpoint — such a
/// block anchors the client's prompt cache and must never be rewritten.
fn block_has_cache_control(block: &Value) -> bool {
    block.get("cache_control").is_some()
}

/// `true` if a `system` value (string or array of blocks) carries any
/// `cache_control` breakpoint. Then it anchors the client's prompt cache and
/// the whole field must be left verbatim. A plain string system prompt never
/// carries one (Anthropic places `cache_control` on blocks), so it is safe.
#[must_use]
pub fn value_has_cache_control(v: &Value) -> bool {
    match v {
        Value::Array(blocks) => blocks.iter().any(block_has_cache_control),
        _ => false,
    }
}

/// Compress a JSON *string* field in place (e.g. `OpenAI` message `content`).
/// Returns `true` if it was rewritten.
pub fn compress_string_field(obj: &mut Value, field: &str, aggressiveness: f64) -> bool {
    let Some(text) = obj.get(field).and_then(Value::as_str) else {
        return false;
    };
    if let Some(compressed) = compress_prose(text, aggressiveness) {
        obj[field] = Value::String(compressed);
        return true;
    }
    false
}

/// Compress every `{ "type": "text", "text": … }` block in a content array
/// (Anthropic message content / system blocks). Blocks carrying a
/// `cache_control` breakpoint are skipped so client cache anchors survive.
/// Returns the number of blocks rewritten.
pub fn compress_text_blocks(blocks: &mut [Value], aggressiveness: f64) -> u32 {
    let mut count = 0;
    for block in blocks.iter_mut() {
        if block.get("type").and_then(Value::as_str) != Some("text")
            || block_has_cache_control(block)
        {
            continue;
        }
        let Some(text) = block.get("text").and_then(Value::as_str) else {
            continue;
        };
        if let Some(compressed) = compress_prose(text, aggressiveness) {
            block["text"] = Value::String(compressed);
            count += 1;
        }
    }
    count
}

/// Compress an Anthropic top-level `system` field, which may be a plain string
/// or an array of text blocks. Returns the number of segments rewritten.
pub fn compress_system_value(system: &mut Value, aggressiveness: f64) -> u32 {
    match system {
        Value::String(s) => {
            if let Some(compressed) = compress_prose(s, aggressiveness) {
                *s = compressed;
                return 1;
            }
            0
        }
        Value::Array(blocks) => compress_text_blocks(blocks, aggressiveness),
        _ => 0,
    }
}

/// Compress an `OpenAI` chat message's `content`, which is either a plain string
/// or an array of `{type:"text", text}` parts (multimodal). Returns the number
/// of segments rewritten.
pub fn compress_message_content(msg: &mut Value, aggressiveness: f64) -> u32 {
    match msg.get_mut("content") {
        Some(Value::String(s)) => {
            if let Some(compressed) = compress_prose(s, aggressiveness) {
                *s = compressed;
                return 1;
            }
            0
        }
        Some(Value::Array(parts)) => compress_text_blocks(parts, aggressiveness),
        _ => 0,
    }
}

/// Compress the plain-`text` parts of a Gemini `parts` array. `functionCall`,
/// `functionResponse` and `inlineData` parts are never touched (tool I/O and
/// binary), so only natural-language turns are squeezed. Returns segments
/// rewritten.
pub fn compress_gemini_text_parts(parts: &mut [Value], aggressiveness: f64) -> u32 {
    let mut count = 0;
    for part in parts.iter_mut() {
        if part.get("functionCall").is_some()
            || part.get("functionResponse").is_some()
            || part.get("inlineData").is_some()
        {
            continue;
        }
        let Some(text) = part.get("text").and_then(Value::as_str) else {
            continue;
        };
        if let Some(compressed) = compress_prose(text, aggressiveness) {
            part["text"] = Value::String(compressed);
            count += 1;
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;

    fn long_prose() -> String {
        // Natural-language paragraphs with a repeated sentence the squeeze can
        // dedup, comfortably over the prose floor.
        let p = "You are a meticulous senior engineer who values clarity and \
                 correctness above all. Always explain your reasoning before \
                 acting, and prefer small, reviewable changes over large ones. ";
        format!("{p}\n\n{p}\n\n{p}")
    }

    #[test]
    fn compresses_long_prose_deterministically() {
        let text = long_prose();
        let a = compress_prose(&text, 0.5);
        let b = compress_prose(&text, 0.5);
        assert_eq!(a, b, "compress_prose must be a pure function of its inputs");
        assert!(a.is_some(), "long, duplicate-rich prose must compress");
        assert!(count_tokens(&a.unwrap()) < count_tokens(&text));
    }

    #[test]
    fn anti_inflation_leaves_short_text() {
        // Below the prose floor → never touched.
        assert_eq!(compress_prose("Be concise.", 1.0), None);
    }

    #[test]
    fn anti_inflation_when_no_saving_possible() {
        // Long but already-unique, high-entropy prose at a=0.0 (keep everything)
        // cannot get smaller → None, never a same-or-bigger rewrite.
        let unique = (0..40)
            .map(|i| {
                format!("Distinct instruction number {i} about handling edge case {i} carefully.")
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(compress_prose(&unique, 0.0), None);
    }

    #[test]
    fn rejects_code_like_input() {
        let code = (0..40)
            .map(|i| format!("    let value_{i} = compute_{i}(ctx, opts);"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!looks_like_prose(&code));
        assert_eq!(compress_prose(&code, 1.0), None);
    }

    #[test]
    fn rejects_json_like_input() {
        let json = r#"{"a": 1, "b": {"c": [1,2,3], "d": "x"}, "e": true, "f": null}"#.repeat(20);
        assert!(!looks_like_prose(&json));
    }

    #[test]
    fn text_blocks_skip_cache_control() {
        let big = long_prose();
        let mut blocks = vec![
            serde_json::json!({"type": "text", "text": big, "cache_control": {"type": "ephemeral"}}),
            serde_json::json!({"type": "text", "text": big}),
        ];
        let n = compress_text_blocks(&mut blocks, 0.5);
        assert_eq!(n, 1, "only the non-cache_control block may be rewritten");
        // The cached block is byte-identical to its original.
        assert!(
            blocks[0]["text"]
                .as_str()
                .unwrap()
                .contains("meticulous senior engineer"),
            "cache_control block must survive verbatim"
        );
    }

    #[test]
    fn system_value_handles_string_and_array() {
        let big = long_prose();
        let mut as_string = Value::String(big.clone());
        assert_eq!(compress_system_value(&mut as_string, 0.5), 1);

        let mut as_array = serde_json::json!([{"type": "text", "text": big}]);
        let arr = as_array.as_array_mut().unwrap();
        assert_eq!(compress_text_blocks(arr, 0.5), 1);
    }
}
