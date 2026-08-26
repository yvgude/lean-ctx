//! Applies the agent's compaction directive to a request (#1570 P1).
//!
//! Cache-safe by construction — the exact properties DCP lacks:
//! - never touches the client's `cache_control`'d prefix,
//! - the cut boundary is fingerprint-pinned on first apply, so the replaced
//!   prefix stays byte-identical while the conversation grows,
//! - the span is replaced only at tool-pair-safe message boundaries,
//! - the verbatim span is CCR-persisted before replacement (defense in
//!   depth; restore = clearing the directive, the client resends history).

use serde_json::{Value, json};

use crate::core::compact_directive;

/// Minimum messages in the span — below this the stub saves nothing.
const MIN_SPAN_MESSAGES: usize = 3;

fn fingerprint(msg: &Value) -> String {
    blake3::hash(msg.to_string().as_bytes()).to_hex()[..16].to_string()
}

fn has_tool_result(msg: &Value) -> bool {
    msg.get("content")
        .and_then(|c| c.as_array())
        .is_some_and(|blocks| {
            blocks
                .iter()
                .any(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_result"))
        })
}

/// A "plain" user message (no tool_result blocks) is a tool-pair-safe cut
/// boundary: everything before it closes a complete assistant/tool cycle.
fn is_plain_user(msg: &Value) -> bool {
    msg.get("role").and_then(|r| r.as_str()) == Some("user") && !has_tool_result(msg)
}

/// Apply the active directive to `parsed`. Returns tokens saved (0 when no
/// directive, wrong shape, or any safety guard declines — never an error).
pub(crate) fn apply(parsed: &mut Value) -> usize {
    let Some(mut directive) = compact_directive::load_active() else {
        return 0;
    };
    let Some(messages) = parsed.get_mut("messages").and_then(|m| m.as_array_mut()) else {
        return 0;
    };
    // Anthropic shape only in v1: the OpenAI `tool` role has different
    // pairing rules; decline rather than risk an invalid body.
    if messages
        .iter()
        .any(|m| m.get("role").and_then(|r| r.as_str()) == Some("tool"))
    {
        return 0;
    }

    let prefix = super::history_prune::cached_prefix_len(messages);
    // The first replaced message must not carry tool_results whose tool_use
    // lives inside the cached prefix — that would tear a pair apart.
    if messages.get(prefix).is_some_and(has_tool_result) {
        return 0;
    }

    let cut = if let Some(pinned) = &directive.boundary_fingerprint {
        // Pinned boundary: find it; if the client's history no longer
        // contains it (compaction on their side, edited history), decline.
        match messages.iter().position(|m| &fingerprint(m) == pinned) {
            Some(idx) => idx,
            None => return 0,
        }
    } else {
        // First apply: keep the trailing `keep_recent_turns` plain-user
        // turns; the cut lands on the earliest of them.
        let mut seen = 0usize;
        let mut cut = None;
        for idx in (prefix..messages.len()).rev() {
            if is_plain_user(&messages[idx]) {
                seen += 1;
                if seen >= directive.keep_recent_turns {
                    cut = Some(idx);
                    break;
                }
            }
        }
        match cut {
            Some(idx) => idx,
            None => return 0,
        }
    };

    if cut <= prefix || cut - prefix < MIN_SPAN_MESSAGES {
        return 0;
    }

    let span_serialized = messages[prefix..cut]
        .iter()
        .map(std::string::ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    let span_tokens = crate::core::tokens::count_tokens(&span_serialized);

    if directive.boundary_fingerprint.is_none() {
        directive.boundary_fingerprint = Some(fingerprint(&messages[cut]));
        directive.original_handle = super::ccr::persist_conversation(&span_serialized);
        // Best-effort: a failed save only costs boundary stability on the
        // next request, never correctness.
        let _ = compact_directive::save(&directive);
    }

    let restore_hint = directive
        .original_handle
        .as_deref()
        .map(|handle| format!(" Verbatim transcript archived at {handle}."))
        .unwrap_or_default();
    let stub = json!({
        "role": "user",
        "content": [{
            "type": "text",
            "text": format!(
                "[lean-ctx compact] Earlier conversation compacted at the agent's request. \
                 Authoritative summary:\n{}\n[restore losslessly any time: ctx_session action=\"restore\".{}]",
                directive.summary, restore_hint
            )
        }]
    });
    messages.splice(prefix..cut, [stub]);

    let stub_tokens = crate::core::tokens::count_tokens(
        &messages
            .get(prefix)
            .map(std::string::ToString::to_string)
            .unwrap_or_default(),
    );
    span_tokens.saturating_sub(stub_tokens)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain_user(text: &str) -> Value {
        json!({"role": "user", "content": text})
    }

    fn assistant_with_tool(id: &str) -> Value {
        json!({"role": "assistant", "content": [
            {"type": "tool_use", "id": id, "name": "ctx_shell", "input": {"command": "ls"}}
        ]})
    }

    fn user_tool_result(id: &str) -> Value {
        json!({"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": id, "content": "x".repeat(400)}
        ]})
    }

    fn body(messages: Vec<Value>) -> Value {
        json!({"model": "claude-x", "messages": messages})
    }

    fn seeded_directive(keep: usize) {
        crate::core::compact_directive::clear();
        crate::core::compact_directive::create(keep, "SUMMARY: all findings kept.".into())
            .expect("directive");
    }

    #[test]
    fn compaction_replaces_old_span_pins_boundary_and_stays_byte_stable() {
        let _lock = crate::core::data_dir::test_env_lock();
        let _dir = crate::core::data_dir::isolated_data_dir();
        seeded_directive(2);

        let mut msgs = vec![plain_user("turn 1 with plenty of text to compact away")];
        for i in 0..4 {
            msgs.push(assistant_with_tool(&format!("t{i}")));
            msgs.push(user_tool_result(&format!("t{i}")));
        }
        msgs.push(plain_user("turn 2"));
        msgs.push(plain_user("turn 3 (latest)"));

        let mut first = body(msgs.clone());
        let saved = apply(&mut first);
        assert!(saved > 0, "compaction must save tokens");
        let compacted = first["messages"].as_array().unwrap();
        assert!(
            compacted[0]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("Authoritative summary"),
            "span replaced by the summary stub"
        );
        assert_eq!(
            compacted.last().unwrap()["content"],
            json!("turn 3 (latest)"),
            "recent turns stay verbatim"
        );

        // Client continues the conversation — the replaced prefix must stay
        // byte-identical (fingerprint-pinned boundary).
        let mut grown = msgs.clone();
        grown.push(plain_user("turn 4 (new)"));
        let mut second = body(grown);
        apply(&mut second);
        assert_eq!(
            first["messages"][0], second["messages"][0],
            "stub byte-stable across requests"
        );

        // Restore = clear; the untouched original history flows again.
        assert!(crate::core::compact_directive::clear());
        let mut third = body(msgs);
        assert_eq!(apply(&mut third), 0);
        assert!(third["messages"][0]["content"].is_string());
    }

    #[test]
    fn guards_decline_unsafe_or_pointless_compaction() {
        let _lock = crate::core::data_dir::test_env_lock();
        let _dir = crate::core::data_dir::isolated_data_dir();

        // Span smaller than the minimum → no-op.
        seeded_directive(1);
        let mut tiny = body(vec![plain_user("a"), plain_user("b")]);
        assert_eq!(apply(&mut tiny), 0);

        // OpenAI shape (tool role) → no-op.
        seeded_directive(1);
        let mut openai = body(vec![
            plain_user("q"),
            json!({"role": "tool", "tool_call_id": "1", "content": "r"}),
            plain_user("next"),
            plain_user("latest"),
        ]);
        assert_eq!(apply(&mut openai), 0);

        // First live message carries a tool_result (pair reaches into the
        // cached prefix) → no-op.
        seeded_directive(1);
        let mut torn = body(vec![
            json!({"role": "user", "cache_control": {"type": "ephemeral"}, "content": "cached"}),
            user_tool_result("t9"),
            plain_user("a"),
            plain_user("b"),
            plain_user("c"),
        ]);
        // cached prefix ends at index 1; messages[1] is a tool_result.
        assert_eq!(apply(&mut torn), 0);
        crate::core::compact_directive::clear();
    }
}
