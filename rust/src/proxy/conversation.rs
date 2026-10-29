#![allow(clippy::cast_precision_loss)]
//! Conversation history compression (#1123): reduces token cost of long
//! multi-turn sessions by scoring and tiering messages.
//!
//! In 80+ turn sessions, accumulated conversation history becomes the dominant
//! token cost. This module scores each message on multiple dimensions and
//! applies tiered compression: preserve (verbatim), summarize (1-line), or
//! drop (one-line stub). Neither keeps a recoverable copy.
//!
//! The bounds come from `[conversation]` (`ConversationConfig`).
//!
//! Determinism (#498): same messages array and settings → same compression
//! decisions. Scoring is a pure function of (message_content, position,
//! total_messages).

use serde_json::Value;

use crate::core::config::ConversationConfig;

const CHARS_PER_TOKEN: usize = 4;
/// Messages whose file mentions count as "recent" when scoring relevance.
const RECENT_MENTION_WINDOW: usize = 10;

/// Result of conversation compression.
pub(super) struct CompressionResult {
    pub messages: Vec<Value>,
    pub tokens_saved: usize,
    pub messages_summarized: usize,
    pub messages_dropped: usize,
}

/// Compress a conversation's messages array. Returns `None` if compression
/// is not beneficial (below threshold or too few messages).
pub(super) fn compress_messages(
    messages: &[Value],
    settings: &ConversationConfig,
) -> Option<CompressionResult> {
    if messages.len() <= settings.preserve_last_n_turns + 2 {
        return None;
    }

    let total_tokens: usize = messages.iter().map(message_token_estimate).sum();

    if total_tokens < settings.compression_threshold_tokens {
        return None;
    }

    let system_count = messages
        .iter()
        .take_while(|m| m.get("role").and_then(|r| r.as_str()) == Some("system"))
        .count();

    let compressible_end = messages
        .len()
        .saturating_sub(settings.preserve_last_n_turns);
    if compressible_end <= system_count {
        return None;
    }

    let scores: Vec<f64> = (0..messages.len())
        .map(|i| score_message(&messages[i], i, messages.len(), messages))
        .collect();

    let mut result_messages = Vec::with_capacity(messages.len());
    let mut tokens_saved = 0usize;
    let mut messages_summarized = 0usize;
    let mut messages_dropped = 0usize;

    for (i, msg) in messages.iter().enumerate() {
        // System prompts always preserved
        if i < system_count {
            result_messages.push(msg.clone());
            continue;
        }
        // Last N turns always preserved
        if i >= compressible_end {
            result_messages.push(msg.clone());
            continue;
        }

        let score = scores[i];
        let msg_tokens = message_token_estimate(msg);

        if score >= settings.min_score_to_preserve {
            result_messages.push(msg.clone());
        } else if score >= settings.drop_score_below {
            let summary = summarize_message(msg, i);
            let summary_tokens = summary.len() / CHARS_PER_TOKEN;
            tokens_saved += msg_tokens.saturating_sub(summary_tokens);
            messages_summarized += 1;
            result_messages.push(build_summary_message(msg, &summary));
        } else {
            tokens_saved += msg_tokens;
            messages_dropped += 1;
            // Dropped messages get a minimal stub (no copy is kept).
            let stub = format!(
                "[Turn {}: {} message omitted by conversation compression]",
                i + 1,
                msg.get("role")
                    .and_then(|r| r.as_str())
                    .unwrap_or("unknown")
            );
            result_messages.push(build_summary_message(msg, &stub));
        }
    }

    if tokens_saved == 0 {
        return None;
    }

    Some(CompressionResult {
        messages: result_messages,
        tokens_saved,
        messages_summarized,
        messages_dropped,
    })
}

// --- Message Scoring ---

fn score_message(msg: &Value, index: usize, total: usize, all_messages: &[Value]) -> f64 {
    let recency = score_recency(index, total);
    let error_signal = score_error_signal(msg);
    let decision_weight = score_decision_weight(msg);
    let file_relevance = score_file_relevance(msg, all_messages);
    let action_density = score_action_density(msg);

    recency * 0.30
        + error_signal * 0.25
        + decision_weight * 0.20
        + file_relevance * 0.15
        + action_density * 0.10
}

fn score_recency(index: usize, total: usize) -> f64 {
    if total == 0 {
        return 1.0;
    }
    let position = index as f64 / total as f64;
    // Exponential: recent messages score high
    position * position
}

fn score_error_signal(msg: &Value) -> f64 {
    let content = extract_text_content(msg);
    let error_indicators = [
        "error",
        "Error",
        "ERROR",
        "FAILED",
        "panic",
        "exception",
        "traceback",
        "stack trace",
        "segfault",
        "WARN",
        "fatal",
    ];
    let matches = error_indicators
        .iter()
        .filter(|ind| content.contains(*ind))
        .count();
    (matches as f64 / 3.0).min(1.0)
}

fn score_decision_weight(msg: &Value) -> f64 {
    let content = extract_text_content(msg);
    let decision_indicators = [
        "I decided",
        "I chose",
        "the approach",
        "architecture",
        "trade-off",
        "instead of",
        "because",
        "the reason",
        "design decision",
        "we should",
        "the plan is",
    ];
    let matches = decision_indicators
        .iter()
        .filter(|ind| content.to_lowercase().contains(&ind.to_lowercase()))
        .count();
    (matches as f64 / 2.0).min(1.0)
}

fn score_file_relevance(msg: &Value, all_messages: &[Value]) -> f64 {
    let content = extract_text_content(msg);
    // Check if files mentioned in this message appear in recent messages
    let recent_files = extract_recent_file_mentions(all_messages);
    if recent_files.is_empty() {
        return 0.3; // neutral
    }
    let mentions = recent_files
        .iter()
        .filter(|f| content.contains(f.as_str()))
        .count();
    (mentions as f64 / recent_files.len().max(1) as f64).min(1.0)
}

fn score_action_density(msg: &Value) -> f64 {
    let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");
    if role == "tool" {
        return 0.6; // tool results are moderately important
    }
    let content = extract_text_content(msg);
    if content.is_empty() {
        return 0.0;
    }
    // Code blocks and file paths indicate actionable content
    let code_blocks = content.matches("```").count() / 2;
    let has_paths = content.contains('/')
        && (content.contains(".rs") || content.contains(".ts") || content.contains(".py"));
    (code_blocks as f64 * 0.3 + if has_paths { 0.3 } else { 0.0 }).min(1.0)
}

// --- Helpers ---

fn extract_text_content(msg: &Value) -> String {
    if let Some(content) = msg.get("content") {
        if let Some(text) = content.as_str() {
            return text.to_string();
        }
        if let Some(arr) = content.as_array() {
            return arr
                .iter()
                .filter_map(|block| {
                    if block.get("type").and_then(|t| t.as_str()) == Some("text") {
                        block.get("text").and_then(|t| t.as_str())
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
                .join(" ");
        }
    }
    String::new()
}

fn extract_recent_file_mentions(messages: &[Value]) -> Vec<String> {
    let recent = &messages[messages.len().saturating_sub(RECENT_MENTION_WINDOW)..];
    let mut files = Vec::new();
    for msg in recent {
        let content = extract_text_content(msg);
        for word in content.split_whitespace() {
            if (word.contains('/') || word.contains('\\'))
                && (std::path::Path::new(word)
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("rs"))
                    || std::path::Path::new(word)
                        .extension()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("ts"))
                    || std::path::Path::new(word)
                        .extension()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("py"))
                    || std::path::Path::new(word)
                        .extension()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("js"))
                    || std::path::Path::new(word)
                        .extension()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("go"))
                    || std::path::Path::new(word)
                        .extension()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("toml")))
            {
                let cleaned = word.trim_matches(|c: char| {
                    !c.is_alphanumeric() && c != '/' && c != '.' && c != '_' && c != '-'
                });
                if cleaned.len() > 3 {
                    files.push(cleaned.to_string());
                }
            }
        }
    }
    files.sort();
    files.dedup();
    files
}

fn message_token_estimate(msg: &Value) -> usize {
    let content = extract_text_content(msg);
    content.len() / CHARS_PER_TOKEN + 4 // +4 for role/structure overhead
}

fn summarize_message(msg: &Value, index: usize) -> String {
    let role = msg
        .get("role")
        .and_then(|r| r.as_str())
        .unwrap_or("unknown");
    let content = extract_text_content(msg);

    if role == "tool" {
        let name = msg
            .get("name")
            .and_then(|n| n.as_str())
            .or_else(|| msg.get("tool_call_id").and_then(|t| t.as_str()))
            .unwrap_or("tool");
        let preview = &content[..content.len().min(80)];
        return format!(
            "[Turn {}: {} result — {}…]",
            index + 1,
            name,
            preview.trim()
        );
    }

    if role == "assistant" {
        // Extract first meaningful sentence
        let first_sentence = content
            .split(['.', '\n'])
            .find(|s| s.trim().len() > 10)
            .unwrap_or(&content[..content.len().min(60)]);
        return format!(
            "[Turn {}: assistant — {}]",
            index + 1,
            first_sentence.trim()
        );
    }

    // User messages — brief
    let preview = &content[..content.len().min(60)];
    format!("[Turn {}: {} — {}…]", index + 1, role, preview.trim())
}

fn build_summary_message(original: &Value, summary: &str) -> Value {
    let mut msg = serde_json::Map::new();
    msg.insert(
        "role".into(),
        original
            .get("role")
            .cloned()
            .unwrap_or(Value::String("assistant".into())),
    );
    msg.insert("content".into(), Value::String(summary.to_string()));
    Value::Object(msg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_message(role: &str, content: &str) -> Value {
        json!({"role": role, "content": content})
    }

    fn make_long_conversation(turns: usize) -> Vec<Value> {
        let mut messages = vec![make_message("system", "You are a helpful assistant.")];
        for i in 0..turns {
            messages.push(make_message(
                "user",
                &format!("Question {} about the codebase: {}", i, "x".repeat(2000)),
            ));
            messages.push(make_message(
                "assistant",
                &format!("Answer {}: Here's what I found. {}", i, "y".repeat(3000)),
            ));
        }
        messages
    }

    /// `[conversation]` bounds were documented but hard-coded until 2026-10-16.
    #[test]
    fn configured_bounds_are_applied() {
        let messages = make_long_conversation(60);
        let defaults = ConversationConfig::default();
        assert!(compress_messages(&messages, &defaults).is_some());

        let high_threshold = ConversationConfig {
            compression_threshold_tokens: usize::MAX,
            ..defaults.clone()
        };
        assert!(compress_messages(&messages, &high_threshold).is_none());

        let keep_everything = ConversationConfig {
            min_score_to_preserve: 0.0,
            ..defaults.clone()
        };
        assert!(compress_messages(&messages, &keep_everything).is_none());

        let keep_last_100 = ConversationConfig {
            preserve_last_n_turns: 100,
            ..defaults
        };
        let result = compress_messages(&messages, &keep_last_100).expect("compresses the rest");
        let tail = messages.len() - 100;
        assert_eq!(result.messages[tail..], messages[tail..]);
    }

    #[test]
    fn no_compression_below_threshold() {
        let messages = make_long_conversation(5);
        assert!(compress_messages(&messages, &ConversationConfig::default()).is_none());
    }

    #[test]
    fn compresses_long_conversations() {
        let messages = make_long_conversation(60);
        let result =
            compress_messages(&messages, &ConversationConfig::default()).expect("should compress");
        assert!(result.tokens_saved > 0);
        assert!(result.messages.len() <= messages.len());
    }

    #[test]
    fn preserves_system_prompt() {
        let messages = make_long_conversation(60);
        let result = compress_messages(&messages, &ConversationConfig::default()).unwrap();
        let first = &result.messages[0];
        assert_eq!(first["role"].as_str(), Some("system"));
        assert_eq!(
            first["content"].as_str(),
            Some("You are a helpful assistant.")
        );
    }

    #[test]
    fn preserves_last_n_turns() {
        let messages = make_long_conversation(60);
        let original_last = messages[messages.len() - 1].clone();
        let result = compress_messages(&messages, &ConversationConfig::default()).unwrap();
        let result_last = result.messages.last().unwrap();
        assert_eq!(original_last["content"], result_last["content"]);
    }

    #[test]
    fn error_messages_score_high() {
        let msg = make_message(
            "assistant",
            "error[E0308]: mismatched types\n  --> src/main.rs:5:5",
        );
        let score = score_message(&msg, 80, 100, &[]);
        assert!(
            score > ConversationConfig::default().drop_score_below,
            "Error message should score above summarize threshold"
        );
    }

    #[test]
    fn scoring_is_deterministic() {
        let messages = make_long_conversation(40);
        let scores1: Vec<f64> = (0..messages.len())
            .map(|i| score_message(&messages[i], i, messages.len(), &messages))
            .collect();
        let scores2: Vec<f64> = (0..messages.len())
            .map(|i| score_message(&messages[i], i, messages.len(), &messages))
            .collect();
        assert_eq!(scores1, scores2);
    }

    #[test]
    fn compression_is_deterministic() {
        let messages = make_long_conversation(60);
        let r1 = compress_messages(&messages, &ConversationConfig::default()).unwrap();
        let r2 = compress_messages(&messages, &ConversationConfig::default()).unwrap();
        assert_eq!(r1.tokens_saved, r2.tokens_saved);
        assert_eq!(r1.messages_summarized, r2.messages_summarized);
        assert_eq!(r1.messages_dropped, r2.messages_dropped);
    }
}
