//! `ctx_transcript_compact` business logic (#hermes-engine).
//!
//! Deterministic, prompt-cache-friendly compaction of an OpenAI-format message
//! array. Used by the Hermes context-engine plugin so the compaction lives in
//! the daemon (Single Source of Truth) instead of being re-implemented per
//! client (AGENTS.md #498).
//!
//! Invariants (mirrored by the Python plugin's `compaction.py`):
//! * an `assistant` message with `tool_calls` is never separated from its
//!   following `tool` results (atomic blocks);
//! * leading / inline `system`/`developer` messages are preserved verbatim;
//! * output is a deterministic function of the input (no time/random).

use serde_json::{Map, Value, json};

use crate::core::tokens::count_tokens;
use crate::core::transcript_compact::summarize_content;

const PROTECTED_ROLES: [&str; 2] = ["system", "developer"];
const SUMMARY_MARKER: &str = "[lean-ctx] compacted-context";
const MAX_USER_SNIPPETS: usize = 24;
const OFFLOAD_MAX_CHARS: usize = 8_000;

/// Outcome of a compaction pass.
pub struct CompactResult {
    /// The compacted message array (head + lifted + summary + tail).
    pub messages: Vec<Value>,
    /// The non-protected older messages that were summarized (for offload).
    pub summarized: Vec<Value>,
    pub original_tokens: usize,
    pub compacted_tokens: usize,
    pub did_compact: bool,
}

fn role(m: &Value) -> &str {
    m.get("role").and_then(Value::as_str).unwrap_or("")
}

fn is_protected(m: &Value) -> bool {
    PROTECTED_ROLES.contains(&role(m))
}

fn has_tool_calls(m: &Value) -> bool {
    m.get("tool_calls")
        .and_then(Value::as_array)
        .is_some_and(|a| !a.is_empty())
}

fn content_text(m: &Value) -> String {
    match m.get("content") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|p| {
                if let Some(s) = p.as_str() {
                    Some(s.to_string())
                } else {
                    p.get("text").and_then(Value::as_str).map(String::from)
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn count_message_tokens(m: &Value) -> usize {
    let mut total = 4 + count_tokens(&content_text(m));
    if let Some(tcs) = m.get("tool_calls").and_then(Value::as_array) {
        for tc in tcs {
            if let Some(f) = tc.get("function") {
                total += count_tokens(f.get("name").and_then(Value::as_str).unwrap_or(""));
                total += count_tokens(f.get("arguments").and_then(Value::as_str).unwrap_or(""));
                total += 3;
            }
        }
    }
    total
}

fn count_messages_tokens(msgs: &[Value]) -> usize {
    msgs.iter().map(count_message_tokens).sum()
}

/// Group `body` into atomic `[start, end)` blocks (`assistant+tool_calls` and its
/// trailing tool results stay together; stray tool results attach backwards).
fn atomic_blocks(body: &[Value]) -> Vec<(usize, usize)> {
    let mut blocks: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    let n = body.len();
    while i < n {
        if role(&body[i]) == "tool" {
            if let Some(last) = blocks.last_mut() {
                last.1 = i + 1;
                i += 1;
                continue;
            }
            // No previous block: treat as its own (degenerate) block.
            blocks.push((i, i + 1));
            i += 1;
            continue;
        }
        if role(&body[i]) == "assistant" && has_tool_calls(&body[i]) {
            let mut j = i + 1;
            while j < n && role(&body[j]) == "tool" {
                j += 1;
            }
            blocks.push((i, j));
            i = j;
        } else {
            blocks.push((i, i + 1));
            i += 1;
        }
    }
    blocks
}

fn snippet(text: &str, limit: usize) -> String {
    let collapsed: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= limit {
        return collapsed;
    }
    let end: String = collapsed.chars().take(limit.saturating_sub(1)).collect();
    format!("{}…", end.trim_end())
}

fn build_summary_text(to_summarize: &[Value], focus_topic: Option<&str>) -> String {
    let mut assistant_turns = 0usize;
    let mut tool_results = 0usize;
    let mut tool_calls = 0usize;
    let mut tool_names: Vec<String> = Vec::new();
    let mut user_snippets: Vec<String> = Vec::new();

    for m in to_summarize {
        match role(m) {
            "assistant" => assistant_turns += 1,
            "tool" => tool_results += 1,
            "user" => {
                let c = content_text(m);
                if !c.trim().is_empty() {
                    user_snippets.push(snippet(&c, 160));
                }
            }
            _ => {}
        }
        if let Some(tcs) = m.get("tool_calls").and_then(Value::as_array) {
            for tc in tcs {
                tool_calls += 1;
                let name = tc
                    .get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if !name.is_empty() && !tool_names.iter().any(|n| n == name) {
                    tool_names.push(name.to_string());
                }
            }
        }
    }
    tool_names.sort();

    let approx_tokens = count_messages_tokens(to_summarize);
    let mut lines: Vec<String> = Vec::new();
    lines.push(format!("## {SUMMARY_MARKER}"));
    lines.push(format!(
        "{} earlier messages (~{} tokens) were offloaded to lean-ctx and replaced by this summary. Full detail is recoverable with the recall tools.",
        to_summarize.len(),
        approx_tokens
    ));
    if let Some(topic) = focus_topic.map(str::trim).filter(|t| !t.is_empty()) {
        lines.push(format!("Focus retained: {topic}."));
    }

    if !user_snippets.is_empty() {
        lines.push(String::new());
        lines.push("User intents (chronological):".to_string());
        for s in user_snippets.iter().take(MAX_USER_SNIPPETS) {
            lines.push(format!("- {s}"));
        }
        let extra = user_snippets.len().saturating_sub(MAX_USER_SNIPPETS);
        if extra > 0 {
            lines.push(format!("- … (+{extra} more user messages)"));
        }
    }

    let mut activity = format!(
        "{assistant_turns} assistant turns, {tool_results} tool results, {tool_calls} tool calls"
    );
    if !tool_names.is_empty() {
        activity.push_str(&format!(" across: {}", tool_names.join(", ")));
    }
    lines.push(String::new());
    lines.push(format!("Activity: {activity}."));

    // Reuse lean-ctx's deterministic transcript summarizer for a compressed
    // head/tail glimpse of the raw turns.
    let serialized = serialize_transcript(to_summarize, OFFLOAD_MAX_CHARS);
    if !serialized.is_empty() {
        lines.push(String::new());
        lines.push(summarize_content(&serialized));
    }

    lines.push(String::new());
    lines.push(
        "Recover detail: ctx_search(), ctx_semantic_search(), ctx_read(), ctx_expand(), ctx_knowledge(), ctx_summary().".to_string(),
    );

    lines.join("\n")
}

/// Render messages to a plain-text transcript (bounded, deterministic) for
/// durable offload into the session/knowledge store.
pub fn serialize_transcript(messages: &[Value], max_chars: usize) -> String {
    let mut lines: Vec<String> = Vec::new();
    for m in messages {
        let r = role(m);
        let c = content_text(m);
        if !c.trim().is_empty() {
            lines.push(format!("{r}: {c}"));
        }
        if let Some(tcs) = m.get("tool_calls").and_then(Value::as_array) {
            for tc in tcs {
                if let Some(f) = tc.get("function") {
                    let name = f.get("name").and_then(Value::as_str).unwrap_or("");
                    let args = f.get("arguments").and_then(Value::as_str).unwrap_or("");
                    lines.push(format!("{r} -> tool_call {name}({args})"));
                }
            }
        }
    }
    let text = lines.join("\n");
    if text.chars().count() <= max_chars {
        return text;
    }
    let half = max_chars / 2;
    let head: String = text.chars().take(half).collect();
    let tail: String = text
        .chars()
        .rev()
        .take(half)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("{head}\n… [truncated] …\n{tail}")
}

/// Compact a message array. `fresh_tail_tokens` and `protect_min_messages`
/// bound the verbatim tail; the split always lands on an atomic-block boundary.
#[must_use]
pub fn compact_messages(
    messages: Vec<Value>,
    fresh_tail_tokens: usize,
    protect_min_messages: usize,
    focus_topic: Option<&str>,
) -> CompactResult {
    let original_tokens = count_messages_tokens(&messages);
    let n = messages.len();
    if n == 0 {
        return CompactResult {
            messages,
            summarized: Vec::new(),
            original_tokens,
            compacted_tokens: original_tokens,
            did_compact: false,
        };
    }

    let mut head_end = 0;
    while head_end < n && is_protected(&messages[head_end]) {
        head_end += 1;
    }
    let head = &messages[..head_end];
    let body = &messages[head_end..];
    if body.is_empty() {
        return CompactResult {
            messages: messages.clone(),
            summarized: Vec::new(),
            original_tokens,
            compacted_tokens: original_tokens,
            did_compact: false,
        };
    }

    let blocks = atomic_blocks(body);
    let mut tail_start_block = blocks.len();
    let mut tail_tokens = 0usize;
    let mut tail_msgs = 0usize;
    for bi in (0..blocks.len()).rev() {
        if tail_start_block != blocks.len()
            && tail_tokens >= fresh_tail_tokens
            && tail_msgs >= protect_min_messages
        {
            break;
        }
        let (s, e) = blocks[bi];
        tail_start_block = bi;
        tail_tokens += count_messages_tokens(&body[s..e]);
        tail_msgs += e - s;
    }
    let tail_idx = if tail_start_block < blocks.len() {
        blocks[tail_start_block].0
    } else {
        body.len()
    };
    let older = &body[..tail_idx];
    let tail = &body[tail_idx..];

    let lifted: Vec<Value> = older.iter().filter(|m| is_protected(m)).cloned().collect();
    let to_summarize: Vec<Value> = older.iter().filter(|m| !is_protected(m)).cloned().collect();

    if to_summarize.is_empty() {
        return CompactResult {
            messages: messages.clone(),
            summarized: Vec::new(),
            original_tokens,
            compacted_tokens: original_tokens,
            did_compact: false,
        };
    }

    let summary = json!({
        "role": "system",
        "content": build_summary_text(&to_summarize, focus_topic),
    });

    let mut out: Vec<Value> = Vec::with_capacity(head.len() + lifted.len() + 1 + tail.len());
    out.extend(head.iter().cloned());
    out.extend(lifted);
    out.push(summary);
    out.extend(tail.iter().cloned());

    let compacted_tokens = count_messages_tokens(&out);
    CompactResult {
        messages: out,
        summarized: to_summarize,
        original_tokens,
        compacted_tokens,
        did_compact: true,
    }
}

/// Detect tool_call/tool_result pairing violations (empty == valid). Used by
/// tests to assert the hard OpenAI-sequence invariant after compaction.
#[cfg(test)]
pub fn tool_pairing_errors(messages: &[Value]) -> Vec<String> {
    let mut errors = Vec::new();
    let mut open_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut expecting = false;
    for (idx, m) in messages.iter().enumerate() {
        match role(m) {
            "assistant" if has_tool_calls(m) => {
                open_ids.clear();
                if let Some(tcs) = m.get("tool_calls").and_then(Value::as_array) {
                    for tc in tcs {
                        if let Some(id) = tc.get("id").and_then(Value::as_str) {
                            open_ids.insert(id.to_string());
                        }
                    }
                }
                expecting = !open_ids.is_empty();
            }
            "tool" => {
                let tcid = m.get("tool_call_id").and_then(Value::as_str);
                if !expecting {
                    errors.push(format!("orphan tool result at index {idx}"));
                } else if let Some(id) = tcid {
                    if !open_ids.is_empty() && !open_ids.contains(id) {
                        errors.push(format!("tool result at index {idx} references unknown id"));
                    } else {
                        open_ids.remove(id);
                        if open_ids.is_empty() {
                            expecting = false;
                        }
                    }
                }
            }
            _ => {
                expecting = false;
                open_ids.clear();
            }
        }
    }
    errors
}

/// Build the JSON payload returned by the MCP tool: the compacted array plus
/// deterministic stats. Separated so the registered wrapper stays thin.
#[must_use]
pub fn render_result(result: &CompactResult) -> String {
    let saved = result
        .original_tokens
        .saturating_sub(result.compacted_tokens);
    let payload = json!({
        "messages": result.messages,
        "stats": {
            "compacted": result.did_compact,
            "summarized_messages": result.summarized.len(),
            "original_tokens": result.original_tokens,
            "compacted_tokens": result.compacted_tokens,
            "saved_tokens": saved,
        },
    });
    let mut map = Map::new();
    if let Value::Object(m) = payload {
        map = m;
    }
    serde_json::to_string(&Value::Object(map)).unwrap_or_else(|_| "{}".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filler(n: usize) -> String {
        "lorem ipsum dolor sit amet ".repeat(n)
    }

    fn make_messages(pairs: usize) -> Vec<Value> {
        let mut v = vec![json!({"role":"system","content":"You are helpful."})];
        for i in 0..pairs {
            v.push(json!({"role":"user","content": format!("q{i}: {}", filler(20))}));
            v.push(json!({"role":"assistant","content": format!("a{i}: {}", filler(20))}));
        }
        v
    }

    fn with_tool_block() -> Vec<Value> {
        vec![
            json!({"role":"system","content":"sys"}),
            json!({"role":"user","content": format!("u0 {}", filler(30))}),
            json!({"role":"assistant","content": format!("a0 {}", filler(30))}),
            json!({"role":"user","content": format!("u1 {}", filler(30))}),
            json!({"role":"assistant","content":null,"tool_calls":[
                {"id":"call_1","type":"function","function":{"name":"ctx_search","arguments":"{}"}},
                {"id":"call_2","type":"function","function":{"name":"ctx_read","arguments":"{}"}}
            ]}),
            json!({"role":"tool","tool_call_id":"call_1","content": format!("r1 {}", filler(30))}),
            json!({"role":"tool","tool_call_id":"call_2","content": format!("r2 {}", filler(30))}),
            json!({"role":"assistant","content": format!("a1 {}", filler(30))}),
            json!({"role":"user","content": format!("u2 {}", filler(30))}),
            json!({"role":"assistant","content": format!("a2 {}", filler(30))}),
        ]
    }

    #[test]
    fn compacts_and_keeps_system_head() {
        let msgs = make_messages(20);
        let r = compact_messages(msgs.clone(), 400, 4, None);
        assert!(r.did_compact);
        assert_eq!(role(&r.messages[0]), "system");
        assert!(r.messages.len() < msgs.len());
        assert!(r.compacted_tokens < r.original_tokens);
    }

    #[test]
    fn output_is_valid_sequence() {
        let r = compact_messages(make_messages(20), 400, 4, None);
        assert_eq!(tool_pairing_errors(&r.messages), Vec::<String>::new());
        let markers: Vec<_> = r
            .messages
            .iter()
            .filter(|m| content_text(m).contains(SUMMARY_MARKER))
            .collect();
        assert_eq!(markers.len(), 1);
    }

    #[test]
    fn never_splits_tool_pairs() {
        let r = compact_messages(with_tool_block(), 1, 1, None);
        assert_eq!(tool_pairing_errors(&r.messages), Vec::<String>::new());
        assert!(!r.messages.iter().any(|m| role(m) == "tool"));
    }

    #[test]
    fn deterministic() {
        let a = compact_messages(make_messages(20), 400, 4, Some("graph"));
        let b = compact_messages(make_messages(20), 400, 4, Some("graph"));
        assert_eq!(render_result(&a), render_result(&b));
    }

    #[test]
    fn inline_system_is_lifted() {
        let mut msgs = make_messages(12);
        msgs.insert(7, json!({"role":"system","content":"MID RULE"}));
        let r = compact_messages(msgs, 200, 2, None);
        // the mid-convo system rule survives verbatim somewhere in the output
        assert!(r.messages.iter().any(|m| content_text(m) == "MID RULE"));
        // and was not part of the summarized set
        assert!(!r.summarized.iter().any(|m| content_text(m) == "MID RULE"));
    }

    #[test]
    fn noop_when_small() {
        let msgs = make_messages(1);
        let r = compact_messages(msgs.clone(), 10_000_000, 2, None);
        assert!(!r.did_compact);
        assert_eq!(r.messages.len(), msgs.len());
    }

    #[test]
    fn serialize_transcript_bounded() {
        let msgs = make_messages(50);
        let text = serialize_transcript(&msgs, 500);
        assert!(text.chars().count() <= 500 + 32);
    }
}
