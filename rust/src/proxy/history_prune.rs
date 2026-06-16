use std::collections::HashMap;

use serde_json::Value;

use crate::core::config::HistoryMode;

use super::tool_kind::{ToolResultKind, classify_tool_name, should_protect};

/// Minimum number of messages at the tail that are never pruned in
/// cache-aware mode. The boundary staircase keeps between `KEEP_MIN` and
/// `KEEP_MIN + STRIDE - 1` recent messages intact.
const KEEP_MIN: usize = 8;

/// Step size of the frozen compaction boundary in cache-aware mode. The
/// boundary only ever advances in whole strides, so the request prefix stays
/// byte-identical for up to `STRIDE` consecutive turns — exactly what provider
/// prompt caches need to keep hitting. Larger stride = fewer cache
/// invalidations but more un-pruned history between jumps.
const STRIDE: usize = 16;

/// Tail window of the legacy rolling mode (pre-cache-aware behaviour).
const ROLLING_KEEP_RECENT: usize = 6;

/// How many messages from the front of `messages` may be pruned for the given
/// history mode.
///
/// Cache-aware mode returns a *staircase* boundary: it is a deterministic,
/// monotonically non-decreasing function of the conversation length that only
/// advances in `STRIDE`-sized jumps. Because [`prune_history`] rewrites each
/// message purely from its own content, every message before an
/// already-passed boundary is frozen — re-pruning it on the next turn yields
/// byte-identical output, so the provider prompt-cache prefix stays valid
/// between jumps. A rolling `len - keep_recent` boundary (legacy mode) moves
/// every turn and invalidates the cache from the moved position on.
pub fn prune_boundary(mode: HistoryMode, len: usize) -> usize {
    match mode {
        HistoryMode::Off => 0,
        HistoryMode::Rolling => len.saturating_sub(ROLLING_KEEP_RECENT),
        HistoryMode::CacheAware => ((len.saturating_sub(KEEP_MIN)) / STRIDE) * STRIDE,
    }
}

/// Summarize tool_result blocks in `messages[..prune_end]` to reduce token
/// count. Returns `true` if at least one message was actually rewritten.
///
/// The rewrite is *content-deterministic*: a message's pruned form depends
/// only on that message, never on its position or the conversation length.
/// This is what makes the cache-aware boundary of [`prune_boundary`] safe —
/// once a message has been pruned it looks the same on every later turn.
///
/// `tool_names` maps the originating tool-call id → tool name so a pruned *file
/// read* is replaced with an honest, actionable stub ("re-read the file") rather
/// than a misleading first-3/last-2 excerpt of source code. Command/log output
/// keeps the head/tail summary, which stays readable for diagnostics.
pub fn prune_history(
    messages: &mut [Value],
    prune_end: usize,
    tool_names: &HashMap<String, String>,
) -> bool {
    let prune_end = prune_end.min(messages.len());
    let mut modified = false;

    for msg in &mut messages[..prune_end] {
        let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");

        match role {
            // Anthropic: user messages with tool_result content blocks
            "user" => {
                if let Some(content) = msg.get_mut("content").and_then(|c| c.as_array_mut()) {
                    for block in content.iter_mut() {
                        if block.get("type").and_then(|t| t.as_str()) == Some("tool_result") {
                            let kind = block
                                .get("tool_use_id")
                                .and_then(|v| v.as_str())
                                .and_then(|id| tool_names.get(id))
                                .map_or(ToolResultKind::Other, |n| classify_tool_name(n));
                            modified |= summarize_anthropic_tool_result(block, kind);
                        }
                    }
                }
            }
            // OpenAI: tool role messages
            "tool" => {
                let kind = msg
                    .get("tool_call_id")
                    .and_then(|v| v.as_str())
                    .and_then(|id| tool_names.get(id))
                    .map_or(ToolResultKind::Other, |n| classify_tool_name(n));
                if let Some(content) = msg.get("content").and_then(|c| c.as_str())
                    && content.len() > 200
                {
                    let summary = summarize_or_stub(content, kind);
                    msg["content"] = Value::String(summary);
                    modified = true;
                }
            }
            _ => {}
        }
    }
    modified
}

/// Returns `true` if any text inside the block was rewritten. Only the
/// `content` payload is touched — sibling block properties (`tool_use_id`,
/// `is_error`, `cache_control`, ...) are preserved so client-set prompt-cache
/// breakpoints survive pruning.
fn summarize_anthropic_tool_result(block: &mut Value, kind: ToolResultKind) -> bool {
    let mut modified = false;
    if let Some(inner) = block.get_mut("content") {
        match inner {
            Value::String(s) if s.len() > 200 => {
                *s = summarize_or_stub(s, kind);
                modified = true;
            }
            Value::Array(arr) => {
                for item in arr.iter_mut() {
                    if item.get("type").and_then(|t| t.as_str()) == Some("text")
                        && let Some(Value::String(s)) = item.get_mut("text")
                        && s.len() > 200
                    {
                        *s = summarize_or_stub(s, kind);
                        modified = true;
                    }
                }
            }
            _ => {}
        }
    }
    modified
}

/// For a *protected* (file/source) result, emit an honest re-read stub. For
/// everything else, head/tail summarize so diagnostics stay readable.
fn summarize_or_stub(text: &str, kind: ToolResultKind) -> String {
    if should_protect(kind, text) {
        let lines = text.lines().count();
        return format!(
            "[lean-ctx: an earlier file read ({lines} lines) was pruned from older context to save tokens. Re-read the file if you need its full contents again.]"
        );
    }
    summarize_text(text)
}

fn summarize_text(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() <= 5 {
        return text.to_string();
    }

    let first_3: Vec<&str> = lines.iter().take(3).copied().collect();
    let last_2: Vec<&str> = lines.iter().rev().take(2).rev().copied().collect();

    format!(
        "{}\n[...{} lines pruned by lean-ctx...]\n{}",
        first_3.join("\n"),
        lines.len() - 5,
        last_2.join("\n")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_names() -> HashMap<String, String> {
        HashMap::new()
    }

    #[test]
    fn prune_skips_recent_messages() {
        let long_content = (0..40).map(|i| format!("line {i}: this is a longer line to ensure content exceeds the 200 character threshold for pruning")).collect::<Vec<_>>().join("\n");
        let mut messages = vec![
            serde_json::json!({"role": "tool", "content": long_content}),
            serde_json::json!({"role": "assistant", "content": "ok"}),
            serde_json::json!({"role": "tool", "content": long_content}),
        ];
        assert!(prune_history(&mut messages, 1, &no_names()));
        let first = messages[0]["content"].as_str().unwrap();
        assert!(first.contains("pruned"), "old message should be pruned");
        let last = messages[2]["content"].as_str().unwrap();
        assert!(!last.contains("pruned"), "recent message should be kept");
    }

    #[test]
    fn prune_handles_short_content() {
        let mut messages = vec![serde_json::json!({"role": "tool", "content": "short"})];
        assert!(!prune_history(&mut messages, 1, &no_names()));
        assert_eq!(messages[0]["content"].as_str().unwrap(), "short");
    }

    #[test]
    fn boundary_is_a_monotone_staircase() {
        let mut last = 0;
        for len in 0..200 {
            let b = prune_boundary(HistoryMode::CacheAware, len);
            assert!(b >= last, "boundary must never move backwards");
            assert!(
                b == last || b == last + STRIDE,
                "boundary advances in whole strides (len={len}: {last} -> {b})"
            );
            assert!(
                len - b >= KEEP_MIN || b == 0,
                "at least KEEP_MIN recent messages stay intact (len={len}, b={b})"
            );
            last = b;
        }
    }

    #[test]
    fn boundary_modes() {
        assert_eq!(prune_boundary(HistoryMode::Off, 100), 0);
        assert_eq!(
            prune_boundary(HistoryMode::Rolling, 100),
            100 - ROLLING_KEEP_RECENT
        );
        // Below KEEP_MIN + STRIDE nothing is pruned in cache-aware mode.
        assert_eq!(
            prune_boundary(HistoryMode::CacheAware, KEEP_MIN + STRIDE - 1),
            0
        );
        assert_eq!(
            prune_boundary(HistoryMode::CacheAware, KEEP_MIN + STRIDE),
            STRIDE
        );
    }

    /// THE cache invariant: as the conversation grows turn by turn, the pruned
    /// form of every message before an already-passed boundary must stay
    /// byte-identical. If this holds, provider prompt caches keep matching the
    /// request prefix between boundary jumps.
    #[test]
    fn cache_aware_prefix_is_byte_stable_across_turns() {
        let long = (0..30)
            .map(|i| {
                format!("INFO line {i}: a sufficiently long log line for the pruning threshold")
            })
            .collect::<Vec<_>>()
            .join("\n");
        let make_msg = |i: usize| {
            if i.is_multiple_of(2) {
                serde_json::json!({"role": "tool", "tool_call_id": format!("c{i}"), "content": format!("{long}\nmsg {i}")})
            } else {
                serde_json::json!({"role": "assistant", "content": format!("ack {i}")})
            }
        };

        let mut prev_pruned: Vec<String> = Vec::new();
        let mut prev_boundary = 0;
        for len in 1..=80 {
            let mut messages: Vec<Value> = (0..len).map(make_msg).collect();
            let boundary = prune_boundary(HistoryMode::CacheAware, len);
            prune_history(&mut messages, boundary, &no_names());

            let pruned: Vec<String> = messages.iter().map(Value::to_string).collect();
            // Everything before the *previous* boundary must be unchanged
            // relative to the previous turn — that is the cached prefix.
            for i in 0..prev_boundary {
                assert_eq!(
                    prev_pruned[i],
                    pruned[i],
                    "message {i} changed between turn {} and {len} — prompt cache prefix broken",
                    len - 1
                );
            }
            prev_pruned = pruned;
            prev_boundary = boundary;
        }
    }

    #[test]
    fn pruning_is_deterministic() {
        let long = (0..30)
            .map(|i| format!("line {i}: deterministic content that exceeds the length threshold"))
            .collect::<Vec<_>>()
            .join("\n");
        let mk = || {
            vec![
                serde_json::json!({"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "t1", "content": long.clone()}
                ]}),
                serde_json::json!({"role": "assistant", "content": "ok"}),
            ]
        };
        let mut a = mk();
        let mut b = mk();
        prune_history(&mut a, 1, &no_names());
        prune_history(&mut b, 1, &no_names());
        assert_eq!(
            serde_json::to_string(&a).unwrap(),
            serde_json::to_string(&b).unwrap()
        );
    }

    #[test]
    fn cache_control_breakpoints_survive_pruning() {
        let long = (0..20)
            .map(|i| format!("log line {i}: some sufficiently long diagnostic output here"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut messages = vec![
            serde_json::json!({"role": "user", "content": [
                {
                    "type": "tool_result",
                    "tool_use_id": "t1",
                    "cache_control": {"type": "ephemeral"},
                    "content": [{"type": "text", "text": long, "cache_control": {"type": "ephemeral"}}]
                }
            ]}),
            serde_json::json!({"role": "assistant", "content": "ok"}),
        ];
        assert!(prune_history(&mut messages, 1, &no_names()));
        let block = &messages[0]["content"][0];
        assert_eq!(block["cache_control"]["type"], "ephemeral");
        assert_eq!(block["content"][0]["cache_control"]["type"], "ephemeral");
        assert!(
            block["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("pruned by lean-ctx")
        );
    }

    #[test]
    fn old_file_read_gets_honest_reread_stub() {
        let code = (0..40)
            .map(|i| format!("    let value_{i} = compute_{i}(input);"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut names = HashMap::new();
        names.insert("call_1".to_string(), "read_file".to_string());
        let mut messages = vec![
            serde_json::json!({"role": "tool", "tool_call_id": "call_1", "content": code}),
            serde_json::json!({"role": "assistant", "content": "ok"}),
            serde_json::json!({"role": "user", "content": "next"}),
        ];
        prune_history(&mut messages, 1, &names);
        let stub = messages[0]["content"].as_str().unwrap();
        assert!(
            stub.contains("Re-read the file"),
            "code read should get re-read stub, got: {stub}"
        );
        assert!(
            !stub.contains("value_5"),
            "source body must not be partially leaked"
        );
    }

    #[test]
    fn old_log_output_keeps_head_tail_summary() {
        let log = (0..40)
            .map(|i| format!("INFO line {i}: processing item number {i} in the batch run"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut names = HashMap::new();
        names.insert("call_1".to_string(), "Bash".to_string());
        let mut messages = vec![
            serde_json::json!({"role": "tool", "tool_call_id": "call_1", "content": log}),
            serde_json::json!({"role": "assistant", "content": "ok"}),
            serde_json::json!({"role": "user", "content": "next"}),
        ];
        prune_history(&mut messages, 1, &names);
        let summary = messages[0]["content"].as_str().unwrap();
        assert!(
            summary.contains("lines pruned by lean-ctx"),
            "logs keep head/tail summary"
        );
    }
}
