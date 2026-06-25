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
#[must_use]
pub fn prune_boundary(mode: HistoryMode, len: usize) -> usize {
    match mode {
        HistoryMode::Off => 0,
        HistoryMode::Rolling => len.saturating_sub(ROLLING_KEEP_RECENT),
        HistoryMode::CacheAware => ((len.saturating_sub(KEEP_MIN)) / STRIDE) * STRIDE,
    }
}

/// Number of leading messages that belong to the client's provider-cached
/// prefix: everything up to and including the last message that carries a
/// `cache_control` breakpoint. On cache-metered rails (Anthropic) this content
/// must never be rewritten, or the prompt cache is invalidated from the first
/// changed message — re-billing cheap reads (0.1x) as writes (1.25x) every time
/// the prune boundary advances a stride (#448). Returns `0` when no
/// `cache_control` marker is present (e.g. every `OpenAI` request), so pruning is
/// unchanged there.
#[must_use]
pub fn cached_prefix_len(messages: &[Value]) -> usize {
    let mut cached = 0;
    for (i, msg) in messages.iter().enumerate() {
        if message_has_cache_control(msg) {
            cached = i + 1;
        }
    }
    cached
}

/// `true` if `msg` carries a `cache_control` marker at the message level, on any
/// of its content blocks, or on a nested text item inside a block — the three
/// shapes Anthropic clients use to set prompt-cache breakpoints.
fn message_has_cache_control(msg: &Value) -> bool {
    if msg.get("cache_control").is_some() {
        return true;
    }
    let Some(blocks) = msg.get("content").and_then(|c| c.as_array()) else {
        return false;
    };
    blocks.iter().any(|block| {
        block.get("cache_control").is_some()
            || block
                .get("content")
                .and_then(|c| c.as_array())
                .is_some_and(|items| items.iter().any(|it| it.get("cache_control").is_some()))
    })
}

/// Summarize `tool_result` blocks in `messages[..prune_end]` to reduce token
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
    prune_history_range(messages, 0, prune_end, tool_names)
}

/// Like [`prune_history`] but only rewrites `messages[prune_start..prune_end]`.
/// `prune_start` skips the client's provider-cached prefix (see
/// [`cached_prefix_len`]) so cache-aware pruning never invalidates an
/// already-cached prompt prefix on metered rails (#448). With `prune_start = 0`
/// this is identical to the historical `prune_history` behaviour.
pub fn prune_history_range(
    messages: &mut [Value],
    prune_start: usize,
    prune_end: usize,
    tool_names: &HashMap<String, String>,
) -> bool {
    let prune_end = prune_end.min(messages.len());
    let prune_start = prune_start.min(prune_end);
    let mut modified = false;

    for msg in &mut messages[prune_start..prune_end] {
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

/// Cache-aware prune of a single tool-output *string* for the frozen OLD region.
///
/// Returns `Some(pruned)` only when the text is long enough to be worth pruning
/// AND the pruned form is actually shorter; otherwise `None` (leave it intact).
/// The result is content-deterministic — it depends only on `text` and `kind`,
/// never on position — so a pruned output is byte-identical on every later turn.
///
/// This is the Responses-API analogue of [`prune_history_range`]: that path can
/// drop nothing because the API rejects a `function_call` whose matching
/// `function_call_output` is absent, so we prune *in place* (file/source reads
/// collapse to an honest re-read stub, everything else head/tail summarizes)
/// without ever touching the conversation structure.
#[must_use]
pub fn prune_output_text(text: &str, kind: ToolResultKind) -> Option<String> {
    if text.len() <= 200 {
        return None;
    }
    let pruned = summarize_or_stub(text, kind);
    (pruned.len() < text.len()).then_some(pruned)
}

/// For a *protected* (file/source) result, emit an honest, recoverable stub. For
/// everything else, head/tail summarize so diagnostics stay readable.
///
/// CCR (#482): both paths persist the verbatim original to the content-addressed
/// tee store and embed a retrieval handle, so the model can recover the exact
/// *historical* bytes instead of re-reading a file that may have changed (or
/// vanished) since. The handle is a pure function of the content hash, so the
/// stub stays byte-identical across turns — cache-safe by construction (#448).
fn summarize_or_stub(text: &str, kind: ToolResultKind) -> String {
    if should_protect(kind, text) {
        let lines = text.lines().count();
        return match super::ccr::persist(text) {
            Some(handle) => match super::ccr::inband_locator(&handle) {
                // In-band (#493): the remote agent can't read the tee path, so
                // offer the echo-able marker to splice the historical bytes back.
                Some(marker) => format!(
                    "[lean-ctx: an earlier file read ({lines} lines) was pruned from older context to save tokens. This is the version shown that turn — the file may have changed since. Echo {marker} to splice the verbatim original back inline, or re-read the file for its current contents.]"
                ),
                None => format!(
                    "[lean-ctx: an earlier file read ({lines} lines) was pruned from older context to save tokens. This is the version shown that turn — the file may have changed since. Full original at {handle}. Re-read the file for its current contents.]"
                ),
            },
            None => format!(
                "[lean-ctx: an earlier file read ({lines} lines) was pruned from older context to save tokens. This was an older version — the file may have changed since. Re-read the file for its current contents.]"
            ),
        };
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

    // CCR (#482): a head/tail summary drops the middle; offer a recovery handle
    // to the verbatim original (content-addressed → cache-safe, MCP-independent).
    // In-band (#493): advertise the echo-able marker when there is no shared FS.
    let recover = super::ccr::persist(text)
        .map(|h| match super::ccr::inband_locator(&h) {
            Some(marker) => format!(" · echo {marker} for the full original"),
            None => format!(" · full at {h}"),
        })
        .unwrap_or_default();

    format!(
        "{}\n[...{} lines pruned by lean-ctx{}...]\n{}",
        first_3.join("\n"),
        lines.len() - 5,
        recover,
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
        // CCR handles embed the content-addressed tee path (`state_dir()`-derived);
        // serialize against tests that repoint LEAN_CTX_DATA_DIR so the data dir
        // stays fixed across turns — exactly the stable-env reality in production.
        let _lock = crate::core::data_dir::test_env_lock();
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
        // See `cache_aware_prefix_is_byte_stable_across_turns`: the CCR handle is
        // `state_dir()`-derived, so hold the env lock for a fixed data dir.
        let _lock = crate::core::data_dir::test_env_lock();
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

    /// CCR (#482): a pruned file read must (a) be honest that the bytes are a
    /// historical version, (b) carry a recovery handle, and (c) let the verbatim
    /// original be read back from that handle — so the model recovers the exact
    /// historical content instead of a stale re-read.
    #[test]
    fn pruned_file_read_is_recoverable_and_honest_about_staleness() {
        let _lock = crate::core::data_dir::test_env_lock();
        let code = (0..60)
            .map(|i| format!("    let handle_marker_{i} = compute_{i}(input);"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut names = HashMap::new();
        names.insert("call_1".to_string(), "read_file".to_string());
        let mut messages = vec![
            serde_json::json!({"role": "tool", "tool_call_id": "call_1", "content": code.clone()}),
            serde_json::json!({"role": "assistant", "content": "ok"}),
            serde_json::json!({"role": "user", "content": "next"}),
        ];
        prune_history(&mut messages, 1, &names);
        let stub = messages[0]["content"].as_str().unwrap();

        assert!(
            stub.contains("may have changed since"),
            "stub must be honest about staleness, got: {stub}"
        );
        // Extract the handle path from the stub and read the verbatim original back.
        let handle = stub
            .split("Full original at ")
            .nth(1)
            .and_then(|rest| rest.split(". Re-read").next())
            .expect("stub carries a recovery handle");
        let recovered = std::fs::read_to_string(handle.trim()).expect("handle is readable");
        assert!(
            recovered.contains("handle_marker_42"),
            "the exact historical bytes must be recoverable via the handle"
        );
        assert!(
            !stub.contains("handle_marker_42"),
            "the stub itself must not leak the source body"
        );
    }

    /// The recovery handle is content-addressed, so re-pruning the same message
    /// yields a byte-identical stub — the cache-safety invariant CCR must keep.
    #[test]
    fn ccr_stub_is_byte_stable_across_turns() {
        let _lock = crate::core::data_dir::test_env_lock();
        let code = (0..60)
            .map(|i| format!("    let stable_{i} = f_{i}();"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut names = HashMap::new();
        names.insert("c".to_string(), "read_file".to_string());
        let stub_for = || {
            let mut m = vec![
                serde_json::json!({"role": "tool", "tool_call_id": "c", "content": code.clone()}),
                serde_json::json!({"role": "assistant", "content": "ok"}),
            ];
            prune_history(&mut m, 1, &names);
            m[0]["content"].as_str().unwrap().to_string()
        };
        assert_eq!(stub_for(), stub_for(), "CCR stub must be deterministic");
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

    /// Build `pairs` Anthropic-shaped turns (assistant `tool_use` + user
    /// `tool_result`). When `cache_last`, the latest `tool_result` carries a
    /// `cache_control` breakpoint — i.e. the whole history is client-cached.
    fn anthropic_turns(pairs: usize, cache_last: bool) -> Vec<Value> {
        let long = (0..40)
            .map(|i| format!("INFO line {i}: long enough diagnostic output to exceed threshold"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut messages = Vec::new();
        for t in 0..pairs {
            messages.push(serde_json::json!({
                "role": "assistant",
                "content": [{"type": "tool_use", "id": format!("t{t}"), "name": "Bash", "input": {}}],
            }));
            let mut block = serde_json::json!({
                "type": "tool_result",
                "tool_use_id": format!("t{t}"),
                "content": format!("{long}\nturn {t}"),
            });
            if cache_last && t == pairs - 1 {
                block["cache_control"] = serde_json::json!({"type": "ephemeral"});
            }
            messages.push(serde_json::json!({"role": "user", "content": [block]}));
        }
        messages
    }

    #[test]
    fn cached_prefix_len_detects_markers_at_every_level() {
        // message-level marker
        let msgs = vec![
            serde_json::json!({"role": "user", "content": "hi"}),
            serde_json::json!({"role": "assistant", "cache_control": {"type": "ephemeral"}, "content": "ok"}),
            serde_json::json!({"role": "user", "content": "next"}),
        ];
        assert_eq!(cached_prefix_len(&msgs), 2);

        // content-block-level marker
        let msgs = vec![
            serde_json::json!({"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "t1", "cache_control": {"type": "ephemeral"}, "content": "x"}
            ]}),
            serde_json::json!({"role": "assistant", "content": "ok"}),
        ];
        assert_eq!(cached_prefix_len(&msgs), 1);

        // nested text-item-level marker
        let msgs = vec![serde_json::json!({"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": "t1", "content": [
                {"type": "text", "text": "x", "cache_control": {"type": "ephemeral"}}
            ]}
        ]})];
        assert_eq!(cached_prefix_len(&msgs), 1);
    }

    #[test]
    fn cached_prefix_len_is_zero_without_markers() {
        let msgs = anthropic_turns(3, false);
        assert_eq!(cached_prefix_len(&msgs), 0);
    }

    /// Inverted form of the #448 reporter's churn test: with a client
    /// `cache_control` breakpoint on the latest `tool_result` (the whole history
    /// is cached), advancing the prune boundary across a `STRIDE` multiple must
    /// NOT rewrite any cached message — otherwise Anthropic's prompt cache is
    /// invalidated from the first changed message.
    #[test]
    fn cached_prefix_is_never_rewritten_across_stride_jump() {
        // Production guard: prune only `[cached_prefix_len .. boundary)`.
        let stubs = |pairs: usize| -> usize {
            let mut messages = anthropic_turns(pairs, true);
            let boundary = prune_boundary(HistoryMode::CacheAware, messages.len());
            let cached = cached_prefix_len(&messages);
            prune_history_range(&mut messages, cached, boundary, &no_names());
            messages
                .iter()
                .filter(|m| m.to_string().contains("pruned"))
                .count()
        };
        // 11 pairs (22 msgs) -> boundary 0; 12 pairs (24 msgs) -> boundary 16.
        // The breakpoint sits on the last message so `cached == len >= boundary`:
        // the prune window is empty on both turns and nothing is stubbed.
        assert_eq!(stubs(11), 0, "no pruning below the first stride jump");
        assert_eq!(
            stubs(12),
            0,
            "cached prefix not rewritten after the jump (#448)"
        );
        assert_eq!(stubs(20), 0, "still zero deep into the session");

        // Contrast: the unguarded boundary (`prune_start = 0`) DOES stub the
        // cached prefix at the same length — the exact churn #448 fixes.
        let mut unguarded = anthropic_turns(12, true);
        let boundary = prune_boundary(HistoryMode::CacheAware, unguarded.len());
        prune_history(&mut unguarded, boundary, &no_names());
        assert!(
            unguarded.iter().any(|m| m.to_string().contains("pruned")),
            "guardless pruning rewrites cached content — the bug #448 fixes"
        );
    }
}
