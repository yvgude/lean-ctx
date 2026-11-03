// SPDX-License-Identifier: Apache-2.0
//! Active prompt-cache breakpoint injection (#939, Headroom "cache aligner"
//! adjacent).
//!
//! Anthropic's prompt cache is opt-in per request: the client marks a
//! `cache_control: {type:"ephemeral"}` breakpoint and Anthropic caches the
//! prefix up to it (billing later turns at the cached rate). A raw API client
//! that does *not* set one pays full price for its (large, stable) system prompt
//! on every single turn. This module injects exactly one breakpoint on the
//! `system` field for those clients, so the proxy delivers the cache win the
//! client left on the table.
//!
//! ## Anthropic-only by construction
//! `cache_control` is an Anthropic concept. OpenAI Chat Completions and the
//! Responses API cache prefixes **automatically** (no per-request markers; an
//! injected `cache_control` would be ignored at best), so there is nothing to
//! inject there — those paths rely on OpenAI's implicit caching and are left
//! byte-unchanged. This module therefore wires into the Anthropic path only.
//!
//! ## Safety
//! - **Only when the client set none.** The caller gates on
//!   `cached_prefix_len(messages) == 0` and `!prose::value_has_cache_control(system)`,
//!   so we never add a *second* breakpoint (Anthropic caps them at 4) or move a
//!   client anchor.
//! - **Exactly one**, on `system` — the largest, most stable prefix, fixed at
//!   the very start, so it never churns with the prune boundary.
//! - **Deterministic** (#498): a pure function of the body, so the rewritten
//!   request is byte-identical across turns and the cache prefix it creates is
//!   itself stable.
//! - **Min size.** Below Anthropic's minimum cacheable prefix the marker is
//!   ignored, so we skip tiny system prompts to avoid pointless churn.

use serde_json::{Map, Value};

use lean_ctx::core::tokens::count_tokens;

/// Anthropic ignores a cache breakpoint whose prefix is under its minimum
/// cacheable size (1024 tokens for Sonnet/Opus; Haiku is higher). Injecting
/// below this just churns bytes for no cache, so gate on it.
const MIN_CACHEABLE_TOKENS: usize = 1024;

/// The ephemeral cache-control marker Anthropic honours.
fn ephemeral() -> Value {
    serde_json::json!({ "type": "ephemeral" })
}

/// Inject one `cache_control: {type:"ephemeral"}` breakpoint on the Anthropic
/// `system` field, returning `true` iff one was added.
///
/// `system` may be a plain string or an array of text blocks; both are valid
/// Anthropic shapes. A string is wrapped into a single cache-marked text block
/// (the documented way to make a string system prompt cacheable); an array gets
/// the marker on its last block. Returns `false` when there is no `system`, it
/// is too small to be cached, or it already carries a breakpoint (defensive —
/// the caller already guards this).
pub(crate) fn inject_anthropic_system(doc: &mut Value) -> bool {
    let Some(system) = doc.get_mut("system") else {
        return false;
    };
    match system {
        Value::String(s) => {
            if count_tokens(s) < MIN_CACHEABLE_TOKENS {
                return false;
            }
            let text = std::mem::take(s);
            let mut block = Map::new();
            block.insert("type".into(), Value::String("text".into()));
            block.insert("text".into(), Value::String(text));
            block.insert("cache_control".into(), ephemeral());
            *system = Value::Array(vec![Value::Object(block)]);
            true
        }
        Value::Array(blocks) => {
            if blocks.iter().any(|b| b.get("cache_control").is_some()) {
                return false;
            }
            let total: usize = blocks
                .iter()
                .filter_map(|b| b.get("text").and_then(Value::as_str))
                .map(count_tokens)
                .sum();
            if total < MIN_CACHEABLE_TOKENS {
                return false;
            }
            let Some(last) = blocks.last_mut().and_then(Value::as_object_mut) else {
                return false;
            };
            last.insert("cache_control".into(), ephemeral());
            true
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn big_system() -> String {
        // Comfortably over MIN_CACHEABLE_TOKENS so the gate fires.
        "You are a meticulous senior engineer. ".repeat(400)
    }

    #[test]
    fn wraps_string_system_into_cache_marked_block() {
        let mut doc = serde_json::json!({ "system": big_system(), "messages": [] });
        assert!(inject_anthropic_system(&mut doc));
        let block = &doc["system"][0];
        assert_eq!(block["type"], "text");
        assert_eq!(block["cache_control"]["type"], "ephemeral");
        assert!(
            block["text"].as_str().unwrap().contains("senior engineer"),
            "the original system text must be preserved verbatim in the block"
        );
    }

    #[test]
    fn marks_last_block_of_array_system() {
        let mut doc = serde_json::json!({
            "system": [
                { "type": "text", "text": big_system() },
                { "type": "text", "text": big_system() }
            ],
            "messages": []
        });
        assert!(inject_anthropic_system(&mut doc));
        assert!(
            doc["system"][0].get("cache_control").is_none(),
            "only the last block is marked"
        );
        assert_eq!(doc["system"][1]["cache_control"]["type"], "ephemeral");
    }

    #[test]
    fn skips_small_system_and_missing_system() {
        let mut small = serde_json::json!({ "system": "be terse", "messages": [] });
        assert!(
            !inject_anthropic_system(&mut small),
            "below the cacheable floor → no churn"
        );
        let mut none = serde_json::json!({ "messages": [] });
        assert!(!inject_anthropic_system(&mut none));
    }

    #[test]
    fn never_adds_a_second_breakpoint() {
        let mut doc = serde_json::json!({
            "system": [
                { "type": "text", "text": big_system(), "cache_control": { "type": "ephemeral" } }
            ],
            "messages": []
        });
        assert!(
            !inject_anthropic_system(&mut doc),
            "a client breakpoint must be left as the sole anchor"
        );
    }

    #[test]
    fn injection_is_deterministic() {
        let mk = || serde_json::json!({ "system": big_system(), "messages": [] });
        let mut a = mk();
        let mut b = mk();
        assert!(inject_anthropic_system(&mut a));
        assert!(inject_anthropic_system(&mut b));
        assert_eq!(
            a, b,
            "identical input must yield byte-identical output (#498)"
        );
    }
}
