//! Live-zone-aware request compression preserves provider-cached prefixes.
//!
//! The detector deliberately works on message values rather than serialized request bytes:
//! frozen messages are moved through the pipeline unchanged, while only the live suffix may
//! be rewritten.

use serde_json::{Value, json};
use std::sync::{Mutex, OnceLock};

use crate::core::tokens::{TokenizerFamily, count_tokens_for};

use super::adaptive_policy::CompressionPolicy;
use super::compress::compress_tool_result_gateway_with_policy;

/// A conversation split at the first message safe to rewrite.
#[derive(Debug, Clone, PartialEq)]
pub struct LiveZoneSplit {
    pub frozen_messages: Vec<Value>,
    pub live_messages: Vec<Value>,
    pub boundary_turn: usize,
    pub frozen_tokens_estimate: u64,
}

/// Compression accounting for a single live-zone pass.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct LiveZoneStats {
    pub frozen_tokens: u64,
    pub live_tokens_before: u64,
    pub live_tokens_after: u64,
    pub savings_pct: f64,
}

/// An agent-selected provider-cache prefix. This state is intentionally kept
/// in memory only: cache markers are valid for the current proxy process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExplicitFreeze {
    pub frozen_at_turn: usize,
    pub snapshot_hash: String,
    pub frozen_tokens_estimate: u64,
}

static EXPLICIT_FREEZE: OnceLock<Mutex<Option<ExplicitFreeze>>> = OnceLock::new();
static LATEST_CONTEXT: OnceLock<Mutex<Vec<Value>>> = OnceLock::new();

fn explicit_freeze_state() -> &'static Mutex<Option<ExplicitFreeze>> {
    EXPLICIT_FREEZE.get_or_init(|| Mutex::new(None))
}

fn latest_context_state() -> &'static Mutex<Vec<Value>> {
    LATEST_CONTEXT.get_or_init(|| Mutex::new(Vec::new()))
}

/// Records the latest provider message prefix for the agent-facing live-zone
/// tool. The data disappears with the proxy process.
pub fn record_context(messages: &[Value]) {
    *latest_context_state()
        .lock()
        .expect("live-zone context lock poisoned") = messages.to_vec();
}

pub fn latest_context() -> Vec<Value> {
    latest_context_state()
        .lock()
        .expect("live-zone context lock poisoned")
        .clone()
}

pub fn explicit_freeze() -> Option<ExplicitFreeze> {
    explicit_freeze_state()
        .lock()
        .expect("live-zone freeze lock poisoned")
        .clone()
}

pub fn set_explicit_freeze(freeze: ExplicitFreeze) {
    *explicit_freeze_state()
        .lock()
        .expect("live-zone freeze lock poisoned") = Some(freeze);
}

pub fn clear_explicit_freeze() -> Option<ExplicitFreeze> {
    explicit_freeze_state()
        .lock()
        .expect("live-zone freeze lock poisoned")
        .take()
}

pub fn snapshot_hash(messages: &[Value], turns: usize) -> String {
    let prefix = &messages[..turns.min(messages.len())];
    let encoded = serde_json::to_vec(prefix).expect("messages must serialize");
    blake3::hash(&encoded).to_hex().to_string()
}

pub fn estimate_context_tokens(messages: &[Value]) -> u64 {
    serde_json::to_string(messages)
        .map(|serialized| (serialized.len() as u64).div_ceil(4))
        .unwrap_or_default()
}

/// Locate the prefix already eligible for provider cache reuse.
///
/// An Anthropic cache marker freezes every preceding turn.  Absent a marker, retain the
/// preceding conversation and expose only the final three messages for mutation. System
/// messages are always frozen, so a system message that appears later advances the boundary.
pub fn detect_live_zone(messages: &[Value]) -> LiveZoneSplit {
    record_context(messages);
    if let Some(freeze) = explicit_freeze() {
        let boundary = freeze.frozen_at_turn.min(messages.len());
        if snapshot_hash(messages, boundary) == freeze.snapshot_hash {
            let frozen_messages = messages[..boundary].to_vec();
            let live_messages = messages[boundary..].to_vec();
            return LiveZoneSplit {
                frozen_messages,
                live_messages,
                boundary_turn: boundary,
                frozen_tokens_estimate: freeze.frozen_tokens_estimate,
            };
        }
    }
    let marked_boundary = messages
        .iter()
        .enumerate()
        .filter(|(_, message)| has_cache_control(message))
        .map(|(index, _)| index + 1)
        .max();
    let fallback_boundary = messages.len().saturating_sub(3);
    let mut boundary_turn = marked_boundary.unwrap_or(fallback_boundary);

    // System prompts must never become a live rewrite target.
    if let Some(last_system) = messages
        .iter()
        .enumerate()
        .filter(|(_, message)| role(message) == Some("system"))
        .map(|(index, _)| index)
        .max()
    {
        boundary_turn = boundary_turn.max(last_system + 1);
    }
    boundary_turn = boundary_turn.min(messages.len());

    let frozen_messages = messages[..boundary_turn].to_vec();
    let live_messages = messages[boundary_turn..].to_vec();
    let frozen_tokens_estimate = messages_tokens(&frozen_messages, TokenizerFamily::default());
    LiveZoneSplit {
        frozen_messages,
        live_messages,
        boundary_turn,
        frozen_tokens_estimate,
    }
}

/// Compress only the non-cached suffix in place.
///
/// This function reconstructs the original slice order without assigning to the frozen prefix.
pub fn compress_live_only(
    messages: &mut [Value],
    family: TokenizerFamily,
    policy: CompressionPolicy,
) -> LiveZoneStats {
    let split = detect_live_zone(messages);
    let mut stats = LiveZoneStats {
        frozen_tokens: messages_tokens(&split.frozen_messages, family),
        live_tokens_before: messages_tokens(&split.live_messages, family),
        ..LiveZoneStats::default()
    };

    for message in &mut messages[split.boundary_turn..] {
        compress_message(message, family, policy);
    }

    stats.live_tokens_after = messages_tokens(&messages[split.boundary_turn..], family);
    stats.savings_pct = percentage_saved(stats.live_tokens_before, stats.live_tokens_after);
    stats
}

/// Add one Anthropic cache breakpoint at the frozen/live boundary.
///
/// Existing client cache markers are retained. The marker is attached to an existing text
/// block so it does not alter provider message ordering.
pub fn add_cache_markers(messages: &mut [Value]) {
    let boundary = detect_live_zone(messages).boundary_turn;
    let idx = if boundary > 0 && boundary <= messages.len() {
        boundary - 1
    } else {
        messages.len().saturating_sub(1)
    };
    let Some(message) = messages.get_mut(idx) else {
        return;
    };

    if has_cache_control(message) {
        return;
    }

    let Some(object) = message.as_object_mut() else {
        return;
    };
    match object.get_mut("content") {
        Some(Value::Array(blocks)) => {
            if let Some(block) = blocks.last_mut().and_then(Value::as_object_mut) {
                block.insert("cache_control".into(), ephemeral());
            }
        }
        Some(Value::String(text)) => {
            let text = std::mem::take(text);
            object.insert(
                "content".into(),
                Value::Array(vec![json!({
                    "type": "text",
                    "text": text,
                    "cache_control": ephemeral(),
                })]),
            );
        }
        _ => {}
    }
}

fn compress_message(message: &mut Value, family: TokenizerFamily, policy: CompressionPolicy) {
    let tool_name = message
        .get("name")
        .and_then(Value::as_str)
        .map(str::to_owned);
    if let Some(content) = message.get_mut("content") {
        compress_content(content, tool_name.as_deref(), family, policy);
    }
}

fn compress_content(
    content: &mut Value,
    tool_name: Option<&str>,
    family: TokenizerFamily,
    policy: CompressionPolicy,
) {
    match content {
        Value::String(text) => compress_text(text, tool_name, family, policy),
        Value::Array(blocks) => {
            for block in blocks {
                let Some(object) = block.as_object_mut() else {
                    continue;
                };
                let name = object
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let block_type = object
                    .get("type")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                match block_type.as_deref() {
                    Some("text") => {
                        if let Some(Value::String(text)) = object.get_mut("text") {
                            compress_text(text, name.as_deref(), family, policy);
                        }
                    }
                    Some("tool_result") => {
                        if let Some(inner) = object.get_mut("content") {
                            compress_content(inner, name.as_deref(), family, policy);
                        }
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
}

fn compress_text(
    text: &mut String,
    tool_name: Option<&str>,
    family: TokenizerFamily,
    policy: CompressionPolicy,
) {
    let compressed = compress_tool_result_gateway_with_policy(text, tool_name, family, policy);
    if compressed.len() < text.len() {
        *text = compressed;
    }
}

fn messages_tokens(messages: &[Value], family: TokenizerFamily) -> u64 {
    messages
        .iter()
        .map(|message| count_tokens_for(&message.to_string(), family) as u64)
        .sum()
}

fn percentage_saved(before: u64, after: u64) -> f64 {
    if before == 0 {
        0.0
    } else {
        ((before.saturating_sub(after)) as f64 / before as f64) * 100.0
    }
}

fn role(message: &Value) -> Option<&str> {
    message.get("role").and_then(Value::as_str)
}

fn has_cache_control(message: &Value) -> bool {
    message.get("cache_control").is_some()
        || message
            .get("content")
            .and_then(Value::as_array)
            .is_some_and(|blocks| {
                blocks
                    .iter()
                    .any(|block| block.get("cache_control").is_some())
            })
}

fn ephemeral() -> Value {
    json!({"type": "ephemeral"})
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn policy() -> CompressionPolicy {
        super::super::adaptive_policy::select_policy("chat")
    }

    fn verbose_text() -> String {
        (0..80)
            .map(|index| format!("log line {index}: repeated operational detail for compression"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn system_messages_always_frozen() {
        let messages = vec![
            json!({"role": "user", "content": "old"}),
            json!({"role": "system", "content": "do not alter"}),
            json!({"role": "user", "content": "new"}),
        ];
        let split = detect_live_zone(&messages);
        assert_eq!(split.boundary_turn, 2);
        assert_eq!(split.frozen_messages[1], messages[1]);
    }

    #[test]
    fn cache_control_freezes_preceding_messages() {
        let messages = vec![
            json!({"role": "user", "content": "old"}),
            json!({"role": "assistant", "content": [{"type":"text", "text":"cached", "cache_control":{"type":"ephemeral"}}]}),
            json!({"role": "user", "content": "new"}),
        ];
        let split = detect_live_zone(&messages);
        assert_eq!(split.boundary_turn, 2);
        assert_eq!(split.frozen_messages, messages[..2]);
        assert_eq!(split.live_messages, messages[2..]);
    }

    #[test]
    fn live_messages_are_compressed() {
        let mut messages = vec![
            json!({"role": "user", "content": "old"}),
            json!({"role": "assistant", "content": "cached"}),
            json!({"role": "user", "content": "old too"}),
            json!({"role": "tool", "name": "bash", "content": verbose_text()}),
        ];
        let before = messages[3].clone();
        let stats = compress_live_only(&mut messages, TokenizerFamily::default(), policy());
        assert!(stats.live_tokens_after < stats.live_tokens_before);
        assert_ne!(messages[3], before);
    }

    #[test]
    fn frozen_messages_remain_byte_identical() {
        let mut messages = vec![
            json!({"role": "system", "content": "stable system"}),
            json!({"role": "user", "content": [{"type":"text", "text":"cached", "cache_control":{"type":"ephemeral"}}]}),
            json!({"role": "assistant", "content": "answer"}),
            json!({"role": "tool", "name": "bash", "content": verbose_text()}),
        ];
        let frozen = messages[..2]
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>();
        compress_live_only(&mut messages, TokenizerFamily::default(), policy());
        assert_eq!(
            frozen,
            messages[..2]
                .iter()
                .map(Value::to_string)
                .collect::<Vec<_>>(),
        );
    }

    #[test]
    fn stats_report_frozen_and_live_token_counts() {
        let mut messages = vec![
            json!({"role": "system", "content": "stable"}),
            json!({"role": "user", "content": "old"}),
            json!({"role": "assistant", "content": "answer"}),
            json!({"role": "tool", "name": "bash", "content": verbose_text()}),
        ];
        let stats = compress_live_only(&mut messages, TokenizerFamily::default(), policy());
        assert!(stats.frozen_tokens > 0);
        assert!(stats.live_tokens_before >= stats.live_tokens_after);
        assert!(stats.savings_pct >= 0.0);
    }

    #[test]
    fn cache_marker_is_added_at_boundary() {
        let mut messages = vec![
            json!({"role": "user", "content": "old"}),
            json!({"role": "assistant", "content": "answer"}),
            json!({"role": "user", "content": "new"}),
            json!({"role": "user", "content": "latest"}),
        ];
        add_cache_markers(&mut messages);
        assert!(has_cache_control(&messages[0]));
    }
}
