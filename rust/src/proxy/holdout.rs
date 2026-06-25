//! Deterministic output-savings holdout (#895 Track B).
//!
//! To honestly measure how much our **output-shaping** (cache-safe effort
//! control #834 + the optional verbosity steer) reduces *output* tokens, we A/B
//! it against a control arm. The cohort is a pure function of conversation
//! identity (system prompt + first user message), so:
//! * the SAME conversation is always in the SAME arm — every turn — which keeps
//!   the decision (and therefore the request body) stable across turns, and
//! * a fraction `f` of conversations land in the control arm, which is metered
//!   but NOT output-shaped, giving a clean paired comparison of average output
//!   tokens per arm.
//!
//! `output_holdout = 0` (default) puts everyone in `Treatment` → no behaviour
//! change. The hash is content-addressed (blake3), not random, so it is stable
//! across processes and machines.

use serde_json::Value;

/// Number of cohort buckets; a conversation maps to one bucket in `0..BUCKETS`.
const BUCKETS: u64 = 10_000;

/// Separator between key fields, chosen so it cannot occur in normal prose.
const FIELD_SEP: char = '\u{1}';

/// Which experiment arm a conversation is assigned to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arm {
    /// Output-shaping skipped (the measurement baseline); still metered.
    Control,
    /// Output-shaping applied (effort control + verbosity steer).
    Treatment,
}

impl Arm {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Arm::Control => "control",
            Arm::Treatment => "treatment",
        }
    }
}

/// Deterministic bucket `0..BUCKETS` from conversation key material.
#[must_use]
pub fn bucket(key: &str) -> u64 {
    let hash = blake3::hash(key.as_bytes());
    let b = hash.as_bytes();
    let n = u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]);
    n % BUCKETS
}

/// Assign an [`Arm`] for `key` given the control fraction `holdout` in `[0,1]`.
/// `holdout <= 0` ⇒ everyone is [`Arm::Treatment`] (no control arm).
#[must_use]
pub fn assign(key: &str, holdout: f64) -> Arm {
    let h = holdout.clamp(0.0, 1.0);
    if h <= 0.0 {
        return Arm::Treatment;
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let threshold = (h * BUCKETS as f64).round() as u64;
    if bucket(key) < threshold {
        Arm::Control
    } else {
        Arm::Treatment
    }
}

/// Flatten a JSON content value (string / array of text blocks / `{text}` /
/// `{content}`) into plain text for cohort keying. Order-preserving and pure.
fn flatten_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(items) => items.iter().map(flatten_text).collect::<Vec<_>>().join(" "),
        Value::Object(map) => {
            if let Some(Value::String(t)) = map.get("text") {
                t.clone()
            } else if let Some(inner) = map.get("content") {
                flatten_text(inner)
            } else if let Some(parts) = map.get("parts") {
                flatten_text(parts)
            } else {
                String::new()
            }
        }
        _ => String::new(),
    }
}

/// Text of the first message in `messages` whose `role` matches, flattening the
/// `content_field` (e.g. `"content"` for OpenAI/Anthropic, `"parts"` for
/// Gemini). Empty when absent.
fn first_message_text(messages: Option<&Value>, role: &str, content_field: &str) -> String {
    messages
        .and_then(Value::as_array)
        .and_then(|arr| {
            arr.iter()
                .find(|m| m.get("role").and_then(Value::as_str) == Some(role))
        })
        .and_then(|m| m.get(content_field))
        .map(flatten_text)
        .unwrap_or_default()
}

/// Cohort key for an Anthropic `/v1/messages` body: `system` + first user turn.
#[must_use]
pub fn anthropic_key(doc: &Value) -> String {
    let system = doc.get("system").map(flatten_text).unwrap_or_default();
    let first_user = first_message_text(doc.get("messages"), "user", "content");
    format!("{system}{FIELD_SEP}{first_user}")
}

/// Cohort key for an OpenAI Chat Completions body: first `system`/`developer`
/// message + first user message.
#[must_use]
pub fn openai_chat_key(doc: &Value) -> String {
    let messages = doc.get("messages");
    let mut system = first_message_text(messages, "system", "content");
    if system.is_empty() {
        system = first_message_text(messages, "developer", "content");
    }
    let first_user = first_message_text(messages, "user", "content");
    format!("{system}{FIELD_SEP}{first_user}")
}

/// Cohort key for an OpenAI Responses body: `instructions` + first user `input`.
#[must_use]
pub fn openai_responses_key(doc: &Value) -> String {
    let system = doc
        .get("instructions")
        .map(flatten_text)
        .unwrap_or_default();
    // `input` is either a plain string or an array of role/content items.
    let first_user = match doc.get("input") {
        Some(Value::String(s)) => s.clone(),
        other => first_message_text(other, "user", "content"),
    };
    format!("{system}{FIELD_SEP}{first_user}")
}

/// Cohort key for a Gemini `generateContent` body: `systemInstruction` + first
/// user `contents` turn.
#[must_use]
pub fn google_key(doc: &Value) -> String {
    let system = doc
        .get("systemInstruction")
        .map(flatten_text)
        .unwrap_or_default();
    let first_user = first_message_text(doc.get("contents"), "user", "parts");
    format!("{system}{FIELD_SEP}{first_user}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn holdout_zero_is_all_treatment() {
        assert_eq!(assign("any key", 0.0), Arm::Treatment);
        assert_eq!(assign("any key", -1.0), Arm::Treatment);
    }

    #[test]
    fn holdout_one_is_all_control() {
        assert_eq!(assign("any key", 1.0), Arm::Control);
        assert_eq!(assign("another", 2.0), Arm::Control);
    }

    #[test]
    fn assignment_is_deterministic_and_stable() {
        // Same key → same arm, every call (cross-turn stability).
        let k = "system\u{1}first user message";
        let a = assign(k, 0.5);
        for _ in 0..20 {
            assert_eq!(assign(k, 0.5), a);
        }
    }

    #[test]
    fn fraction_is_approximately_honoured() {
        // ~30% of distinct conversations land in control.
        let control = (0..5000)
            .filter(|i| assign(&format!("conv-{i}"), 0.3) == Arm::Control)
            .count();
        let frac = control as f64 / 5000.0;
        assert!((0.27..0.33).contains(&frac), "got {frac}");
    }

    #[test]
    fn anthropic_key_uses_system_and_first_user() {
        let doc = json!({
            "system": "You are helpful.",
            "messages": [
                {"role": "user", "content": "Hello there"},
                {"role": "assistant", "content": "Hi"},
                {"role": "user", "content": "later turn"}
            ]
        });
        assert_eq!(anthropic_key(&doc), "You are helpful.\u{1}Hello there");
    }

    #[test]
    fn anthropic_key_flattens_block_arrays() {
        let doc = json!({
            "system": [{"type": "text", "text": "Sys A"}, {"type": "text", "text": "Sys B"}],
            "messages": [{"role": "user", "content": [{"type": "text", "text": "U1"}]}]
        });
        assert_eq!(anthropic_key(&doc), "Sys A Sys B\u{1}U1");
    }

    #[test]
    fn openai_chat_key_prefers_system_then_developer() {
        let doc = json!({
            "messages": [
                {"role": "developer", "content": "Dev rules"},
                {"role": "user", "content": "Q"}
            ]
        });
        assert_eq!(openai_chat_key(&doc), "Dev rules\u{1}Q");
    }

    #[test]
    fn google_key_uses_system_instruction_and_contents() {
        let doc = json!({
            "systemInstruction": {"parts": [{"text": "Be terse"}]},
            "contents": [{"role": "user", "parts": [{"text": "First Q"}]}]
        });
        assert_eq!(google_key(&doc), "Be terse\u{1}First Q");
    }

    #[test]
    fn responses_key_handles_string_and_array_input() {
        let s = json!({"instructions": "Sys", "input": "just a string"});
        assert_eq!(openai_responses_key(&s), "Sys\u{1}just a string");
        let a = json!({
            "instructions": "Sys",
            "input": [{"role": "user", "content": "arr q"}]
        });
        assert_eq!(openai_responses_key(&a), "Sys\u{1}arr q");
    }
}
