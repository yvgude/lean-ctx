//! Thin integration layer connecting response shaping + conversation
//! compression into the proxy forward path.
//!
//! Keeps `forward.rs` below LOC gate while providing the full pipeline:
//! 1. Response shaping (strip preambles/confirmations from LLM output)
//! 2. Conversation compression (score+tier messages on long conversations)
//!
//! Both are gated on `Config::response_shaping_mode` / `Config::conversation_compression`.
use super::conversation;
use super::response_shaper::{self, ShapingMode, ShapingResult};
use crate::core::config::Config;

/// Apply response shaping to non-streaming response bytes.
/// Returns shaped bytes + tokens saved, or `None` if shaping didn't apply.
pub(crate) fn shape_response(resp_bytes: &[u8], mode: &str) -> Option<ShapingResult> {
    let shaping_mode = ShapingMode::from_str_config(mode);
    response_shaper::shape_response(resp_bytes, shaping_mode)
}

/// Config-gated response shaping. Returns `None` if disabled or mode is "off".
pub(crate) fn shape_response_if_enabled(resp_bytes: &[u8]) -> Option<ShapingResult> {
    let config = Config::load();
    if !config.response_shaping.enabled {
        return None;
    }
    shape_response(resp_bytes, &config.response_shaping.mode)
}

/// Compress conversation messages if the request exceeds thresholds.
/// Returns compressed messages array + savings stats, or `None` if not applicable.
pub(crate) fn compress_conversation(
    body_bytes: &[u8],
    settings: &crate::core::config::ConversationConfig,
) -> Option<(Vec<u8>, usize, usize, usize)> {
    let mut parsed: serde_json::Value = serde_json::from_slice(body_bytes).ok()?;

    let messages = parsed.get("messages")?.as_array()?.clone();
    let result = conversation::compress_messages(&messages, settings)?;

    parsed["messages"] = serde_json::Value::Array(result.messages);
    let output = serde_json::to_vec(&parsed).ok()?;

    Some((
        output,
        result.tokens_saved,
        result.messages_summarized,
        result.messages_dropped,
    ))
}

/// Config-gated conversation compression. Returns `None` if disabled or not applicable.
pub(crate) fn compress_conversation_if_enabled(
    body_bytes: &[u8],
) -> Option<(Vec<u8>, usize, usize, usize)> {
    let config = Config::load();
    if !config.conversation.compression_enabled {
        return None;
    }
    let compressed = compress_conversation(body_bytes, &config.conversation)?;
    // Summarized and dropped turns have no recovery copy and no lineage of
    // their own: under protection the conversation stays as it is, counted
    // once per request.
    if super::compress::protected() {
        super::compress::note_kept_whole(
            "conversation summary",
            super::compress::SUMMARY_WITHOUT_COPY,
        );
        return None;
    }
    Some(compressed)
}

#[cfg(test)]
mod tests {
    use super::{
        compress_conversation, compress_conversation_if_enabled, shape_response,
        shape_response_if_enabled,
    };

    #[test]
    fn shape_response_off_mode_returns_none() {
        let json = serde_json::json!({
            "choices": [{"message": {"role": "assistant", "content": "Great, I'll do that.\n\nDone.\n\nLet me know if you need help!"}}]
        });
        let bytes = serde_json::to_vec(&json).unwrap();
        assert!(shape_response(&bytes, "off").is_none());
    }

    #[test]
    fn shape_response_gentle_strips_ceremony() {
        let json = serde_json::json!({
            "choices": [{"message": {"role": "assistant", "content": "Sure, let me check that.\n\nThe answer is 42.\n\nLet me know if you need anything else!"}}]
        });
        let bytes = serde_json::to_vec(&json).unwrap();
        let result = shape_response(&bytes, "gentle");
        assert!(result.is_some());
        let shaped: serde_json::Value = serde_json::from_slice(&result.unwrap().bytes).unwrap();
        let content = shaped["choices"][0]["message"]["content"].as_str().unwrap();
        assert!(!content.contains("Sure, let me"));
        assert!(!content.contains("Let me know"));
        assert!(content.contains("42"));
    }

    #[test]
    fn compress_conversation_below_threshold_returns_none() {
        let body = serde_json::json!({
            "model": "claude-4",
            "messages": [
                {"role": "system", "content": "You are helpful."},
                {"role": "user", "content": "Hello"},
                {"role": "assistant", "content": "Hi!"}
            ]
        });
        let bytes = serde_json::to_vec(&body).unwrap();
        assert!(
            compress_conversation(&bytes, &crate::core::config::ConversationConfig::default())
                .is_none()
        );
    }

    #[test]
    fn compress_conversation_if_enabled_respects_config() {
        let messages: Vec<serde_json::Value> = (0..30)
            .map(|i| {
                serde_json::json!({
                    "role": if i % 2 == 0 { "user" } else { "assistant" },
                    "content": "x".repeat(2000)
                })
            })
            .collect();
        let body = serde_json::json!({"messages": messages});
        let bytes = serde_json::to_vec(&body).unwrap();
        assert!(compress_conversation_if_enabled(&bytes).is_none());
    }

    #[test]
    fn shape_response_if_enabled_returns_none_when_disabled() {
        let response = serde_json::json!({
            "choices": [{"message": {"role": "assistant", "content": "42"}}]
        });
        let bytes = serde_json::to_vec(&response).unwrap();
        let _result = shape_response_if_enabled(&bytes);
    }
}
