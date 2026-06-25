//! Cache-safe wire verbosity steer (#895 Track B).
//!
//! Opt-in (`proxy.verbosity_steer`). When on, the proxy appends a single
//! **constant** "be concise" instruction to the *last user turn* of each
//! request. Output-shaping for non-rules-aware clients: lean-ctx already steers
//! verbosity through rules injection for editors that load rules, but raw API
//! clients (and the holdout's treatment arm) need it on the wire.
//!
//! Cache-safety, by construction:
//! - The steer is **constant** (identical bytes every turn), so it never adds
//!   per-turn entropy.
//! - It is appended **after** all existing content of the last user turn — i.e.
//!   strictly after the last `cache_control` breakpoint — and never modifies a
//!   `cache_control`-anchored block. The provider's cached prefix (everything up
//!   to the last breakpoint) stays byte-identical across turns (#448/#498); only
//!   the always-reprocessed tail grows by the constant suffix.
//! - For array content the suffix is a **new** trailing text element; existing
//!   blocks (including cache anchors) are left untouched. For plain-string
//!   content (OpenAI prefix-cached; Anthropic strings carry no `cache_control`)
//!   the suffix is concatenated.
//! - Idempotent: if the steer is already present it is not appended again.

use serde_json::{Map, Value};

/// The constant verbosity instruction. Kept short and directive; its byte
/// stability is what makes the steer prompt-cache-safe.
pub const STEER: &str =
    "Be concise: answer directly and do not restate the question or surrounding context.";

/// Suffix form for plain-string content (leading separation from prior text).
fn string_suffix() -> String {
    format!("\n\n{STEER}")
}

/// True when `text` already ends with the steer, so we never double-append
/// (keeps the rewrite idempotent and deterministic).
fn already_steered(text: &str) -> bool {
    text.trim_end().ends_with(STEER)
}

/// A `{ "type": "text", "text": STEER }` block for array content.
fn steer_text_block() -> Value {
    let mut m = Map::new();
    m.insert("type".to_string(), Value::String("text".to_string()));
    m.insert("text".to_string(), Value::String(STEER.to_string()));
    Value::Object(m)
}

/// A `{ "text": STEER }` part for Gemini `parts` arrays.
fn steer_gemini_part() -> Value {
    let mut m = Map::new();
    m.insert("text".to_string(), Value::String(STEER.to_string()));
    Value::Object(m)
}

/// Append the steer to a content value that is either a plain string or an array
/// of text blocks. `block` builds the trailing element for the array case.
/// Returns whether the value changed.
fn append_to_content(content: &mut Value, block: impl FnOnce() -> Value) -> bool {
    match content {
        Value::String(s) => {
            if already_steered(s) {
                return false;
            }
            s.push_str(&string_suffix());
            true
        }
        Value::Array(parts) => {
            // Already steered if the last text element is the steer.
            let last_text = parts
                .iter()
                .rev()
                .find_map(|p| p.get("text").and_then(Value::as_str));
            if last_text.is_some_and(already_steered) {
                return false;
            }
            parts.push(block());
            true
        }
        _ => false,
    }
}

/// Index of the last message in `messages` whose `role` matches.
fn last_role_index(messages: &[Value], role: &str) -> Option<usize> {
    messages
        .iter()
        .rposition(|m| m.get("role").and_then(Value::as_str) == Some(role))
}

/// Append the steer to the last user message of an Anthropic `/v1/messages`
/// body. No-op (returns false) when there is no user turn.
pub fn apply_anthropic(doc: &mut Value) -> bool {
    let Some(messages) = doc.get_mut("messages").and_then(Value::as_array_mut) else {
        return false;
    };
    let Some(idx) = last_role_index(messages, "user") else {
        return false;
    };
    let Some(content) = messages[idx].get_mut("content") else {
        return false;
    };
    let changed = append_to_content(content, steer_text_block);
    if changed {
        record();
    }
    changed
}

/// Append the steer to the last user message of an OpenAI Chat Completions body.
/// OpenAI prefix-caches automatically, so appending to the newest turn never
/// disturbs a previously cached prefix.
pub fn apply_openai_chat(doc: &mut Value) -> bool {
    let Some(messages) = doc.get_mut("messages").and_then(Value::as_array_mut) else {
        return false;
    };
    let Some(idx) = last_role_index(messages, "user") else {
        return false;
    };
    let Some(content) = messages[idx].get_mut("content") else {
        return false;
    };
    let changed = append_to_content(content, steer_text_block);
    if changed {
        record();
    }
    changed
}

/// Append the steer to the last user item of an OpenAI Responses body. `input`
/// is either a plain string or an array of role/content items.
pub fn apply_openai_responses(doc: &mut Value) -> bool {
    let changed = match doc.get_mut("input") {
        Some(Value::String(s)) => {
            if already_steered(s) {
                false
            } else {
                s.push_str(&string_suffix());
                true
            }
        }
        Some(Value::Array(items)) => {
            let Some(idx) = last_role_index(items, "user") else {
                return false;
            };
            match items[idx].get_mut("content") {
                Some(content) => append_to_content(content, steer_text_block),
                None => false,
            }
        }
        _ => false,
    };
    if changed {
        record();
    }
    changed
}

/// Append the steer to the last user turn of a Gemini `generateContent` body
/// (`contents[].parts[]`).
pub fn apply_google(doc: &mut Value) -> bool {
    let Some(contents) = doc.get_mut("contents").and_then(Value::as_array_mut) else {
        return false;
    };
    let Some(idx) = last_role_index(contents, "user") else {
        return false;
    };
    let Some(parts) = contents[idx].get_mut("parts").and_then(Value::as_array_mut) else {
        return false;
    };
    let last_text = parts
        .iter()
        .rev()
        .find_map(|p| p.get("text").and_then(Value::as_str));
    if last_text.is_some_and(already_steered) {
        return false;
    }
    parts.push(steer_gemini_part());
    record();
    true
}

use std::sync::atomic::{AtomicU64, Ordering};

/// Count of requests steered, surfaced via [`steered_count`] for `/status`.
static STEERED: AtomicU64 = AtomicU64::new(0);

fn record() {
    STEERED.fetch_add(1, Ordering::Relaxed);
}

/// Total requests that received a wire verbosity steer (telemetry).
#[must_use]
pub fn steered_count() -> u64 {
    STEERED.load(Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn anthropic_appends_block_to_last_user() {
        let mut doc = json!({
            "messages": [
                {"role": "user", "content": "first"},
                {"role": "assistant", "content": "ok"},
                {"role": "user", "content": [{"type": "text", "text": "second"}]}
            ]
        });
        assert!(apply_anthropic(&mut doc));
        let last = &doc["messages"][2]["content"];
        let arr = last.as_array().unwrap();
        assert_eq!(arr.len(), 2, "new steer block appended");
        assert_eq!(arr[1]["text"], STEER);
        // The earlier user turn is untouched.
        assert_eq!(doc["messages"][0]["content"], "first");
    }

    #[test]
    fn anthropic_string_content_concatenates() {
        let mut doc = json!({"messages": [{"role": "user", "content": "hello"}]});
        assert!(apply_anthropic(&mut doc));
        let s = doc["messages"][0]["content"].as_str().unwrap();
        assert!(s.starts_with("hello"));
        assert!(s.ends_with(STEER));
    }

    #[test]
    fn never_modifies_cache_control_blocks() {
        let mut doc = json!({
            "messages": [{
                "role": "user",
                "content": [
                    {"type": "text", "text": "cached", "cache_control": {"type": "ephemeral"}}
                ]
            }]
        });
        assert!(apply_anthropic(&mut doc));
        let arr = doc["messages"][0]["content"].as_array().unwrap();
        // The cache-anchored block survives verbatim; the steer is a NEW block
        // appended strictly after it.
        assert_eq!(arr.len(), 2);
        assert!(arr[0].get("cache_control").is_some());
        assert_eq!(arr[0]["text"], "cached");
        assert_eq!(arr[1]["text"], STEER);
        assert!(arr[1].get("cache_control").is_none());
    }

    #[test]
    fn idempotent_no_double_append() {
        let mut doc = json!({"messages": [{"role": "user", "content": "hi"}]});
        assert!(apply_anthropic(&mut doc));
        let once = doc.clone();
        assert!(!apply_anthropic(&mut doc), "second pass is a no-op");
        assert_eq!(doc, once, "byte-identical after a redundant pass");
    }

    #[test]
    fn deterministic_constant_suffix() {
        let mk = || json!({"messages": [{"role": "user", "content": "ask"}]});
        let mut a = mk();
        let mut b = mk();
        apply_anthropic(&mut a);
        apply_anthropic(&mut b);
        assert_eq!(a, b, "constant steer ⇒ byte-identical rewrite");
    }

    #[test]
    fn openai_chat_appends_to_last_user() {
        let mut doc = json!({
            "messages": [
                {"role": "system", "content": "sys"},
                {"role": "user", "content": "q"}
            ]
        });
        assert!(apply_openai_chat(&mut doc));
        let s = doc["messages"][1]["content"].as_str().unwrap();
        assert!(s.ends_with(STEER));
        assert_eq!(doc["messages"][0]["content"], "sys", "system untouched");
    }

    #[test]
    fn responses_string_and_array_input() {
        let mut s = json!({"input": "plain"});
        assert!(apply_openai_responses(&mut s));
        assert!(s["input"].as_str().unwrap().ends_with(STEER));

        let mut a =
            json!({"input": [{"role": "user", "content": [{"type": "text", "text": "q"}]}]});
        assert!(apply_openai_responses(&mut a));
        let parts = a["input"][0]["content"].as_array().unwrap();
        assert_eq!(parts.last().unwrap()["text"], STEER);
    }

    #[test]
    fn google_appends_part_to_last_user() {
        let mut doc = json!({
            "contents": [
                {"role": "user", "parts": [{"text": "q1"}]},
                {"role": "model", "parts": [{"text": "a1"}]},
                {"role": "user", "parts": [{"text": "q2"}]}
            ]
        });
        assert!(apply_google(&mut doc));
        let parts = doc["contents"][2]["parts"].as_array().unwrap();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[1]["text"], STEER);
    }

    #[test]
    fn no_user_turn_is_noop() {
        let mut doc = json!({"messages": [{"role": "assistant", "content": "x"}]});
        assert!(!apply_anthropic(&mut doc));
    }
}
