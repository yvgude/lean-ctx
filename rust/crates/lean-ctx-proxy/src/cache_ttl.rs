// SPDX-License-Identifier: Apache-2.0
//! Agent-loop cache lifetime for Anthropic Messages requests.
//!
//! Claude Code on a subscription marks every cache breakpoint with
//! `"ttl": "1h"`. A 1 h cache write costs 2x the input price, a 5 min write
//! 1.25x, and on short agent tasks those writes are close to half of the
//! cost. Inside an agent loop — the request ends in tool results — the next
//! request reads the cache seconds later, so the conversation does not need
//! an hour:
//!
//! - the person's last prompt gets (or keeps) a 1 h marker of its own, so a
//!   long pause before their next prompt loses at most this turn's tool steps;
//! - every other 1 h marker in `messages` after it becomes 5 min;
//! - `system` and `tools` markers are left as they are. Anthropic requires
//!   longer lifetimes before shorter ones; system -> tools -> the prompt (1 h)
//!   -> the loop (5 min) keeps that order.
//!
//! A request whose last message is the person's prompt is left unchanged:
//! nobody knows when the next request comes. A headless client (Claude Code
//! `-p`, the SDKs, CI) has no person who comes back, so there every message
//! marker is written for 5 minutes from the first request on, and so is the
//! last of two or more `system` markers: Claude Code puts the session's own
//! part (working directory, date, memory path) behind its second marker,
//! while the first covers the static part every session shares and keeps its
//! hour (measured on Haiku 5.5, Context Mode short tasks: -22.6 % against
//! native with it, -16.8 % with the message markers alone).
//!
//! Only `cache_control` values are touched, never content, and the result is
//! a pure function of the request and the client kind, so every turn carries
//! the same bytes (#498). Anthropic allows four markers; the prompt marker is
//! only added when one is free.

use serde_json::Value;

/// Whether the `User-Agent` names a client that runs without a person at the
/// keyboard. Claude Code states its entrypoint there:
/// `claude-cli/2.1.316 (external, sdk-cli)` for `-p`, `(external, cli)` when
/// interactive; the SDK entrypoints all start with `sdk-`.
pub(crate) fn is_headless_client(user_agent: Option<&str>) -> bool {
    user_agent
        .and_then(|ua| ua.split_once('('))
        .and_then(|(_, rest)| rest.split_once(')'))
        .is_some_and(|(inside, _)| {
            inside
                .split(',')
                .any(|part| part.trim().starts_with("sdk-"))
        })
}

/// Every 1 h marker in `messages[from..]` becomes 5 min; returns how many.
fn shorten_from(messages: &mut [Value], from: usize) -> usize {
    let mut shortened = 0;
    for message in &mut messages[from..] {
        let Some(blocks) = message.get_mut("content").and_then(Value::as_array_mut) else {
            continue;
        };
        for block in blocks {
            if let Some(ttl) = block.pointer_mut("/cache_control/ttl")
                && ttl.as_str() == Some("1h")
            {
                *ttl = Value::String("5m".into());
                shortened += 1;
            }
        }
    }
    shortened
}

const MAX_MARKERS: usize = 4;

fn is_tool_result_turn(message: &Value) -> bool {
    message.get("role").and_then(Value::as_str) == Some("user")
        && message
            .get("content")
            .and_then(Value::as_array)
            .is_some_and(|blocks| {
                blocks
                    .iter()
                    .any(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"))
            })
}

fn marker_count(value: Option<&Value>) -> usize {
    value.and_then(Value::as_array).map_or(0, |blocks| {
        blocks
            .iter()
            .filter(|b| b.get("cache_control").is_some())
            .count()
    })
}

fn hour_marker() -> Value {
    serde_json::json!({ "type": "ephemeral", "ttl": "1h" })
}

/// Apply the agent-loop lifetime; returns how many markers were shortened.
pub(crate) fn apply_loop_ttl(doc: &mut Value, headless: bool) -> usize {
    let outside = marker_count(doc.get("system")) + marker_count(doc.get("tools"));
    let mut system_shortened = 0;
    if headless
        && marker_count(doc.get("system")) >= 2
        && let Some(ttl) = doc
            .get_mut("system")
            .and_then(Value::as_array_mut)
            .and_then(|blocks| {
                blocks
                    .iter_mut()
                    .rev()
                    .find(|b| b.get("cache_control").is_some())
            })
            .and_then(|b| b.pointer_mut("/cache_control/ttl"))
        && ttl.as_str() == Some("1h")
    {
        *ttl = Value::String("5m".into());
        system_shortened = 1;
    }
    let Some(messages) = doc.get_mut("messages").and_then(Value::as_array_mut) else {
        return system_shortened;
    };
    if headless {
        return system_shortened + shorten_from(messages, 0);
    }
    if !messages.last().is_some_and(is_tool_result_turn) {
        return 0;
    }
    let Some(prompt) = messages.iter().rposition(|m| {
        m.get("role").and_then(Value::as_str) == Some("user") && !is_tool_result_turn(m)
    }) else {
        return 0;
    };
    let inside: usize = messages
        .iter()
        .map(|m| marker_count(m.get("content")))
        .sum();
    let loop_has_hour_marker = messages[prompt + 1..].iter().any(|m| {
        m.get("content")
            .and_then(Value::as_array)
            .is_some_and(|blocks| {
                blocks
                    .iter()
                    .any(|b| b.pointer("/cache_control/ttl").and_then(Value::as_str) == Some("1h"))
            })
    });
    if !loop_has_hour_marker {
        return 0;
    }

    // The prompt's own 1 h marker, on its last block. A plain-string prompt is
    // left alone: wrapping it into a block would change its bytes and miss the
    // cache the turn's first request wrote.
    let Some(last) = messages[prompt]
        .get_mut("content")
        .and_then(Value::as_array_mut)
        .and_then(|blocks| blocks.last_mut())
        .and_then(Value::as_object_mut)
    else {
        return 0;
    };
    match last.get_mut("cache_control") {
        Some(cc) => *cc = hour_marker(),
        None if outside + inside < MAX_MARKERS => {
            last.insert("cache_control".into(), hour_marker());
        }
        // No free marker: the loop keeps 1 h rather than risk the whole turn.
        None => return 0,
    }
    shorten_from(messages, prompt + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loop_request(prompt: &Value) -> Value {
        serde_json::json!({
            "system": [
                { "type": "text", "text": "a", "cache_control": { "type": "ephemeral", "ttl": "1h" } },
                { "type": "text", "text": "b", "cache_control": { "type": "ephemeral", "ttl": "1h" } }
            ],
            "messages": [
                { "role": "user", "content": prompt },
                { "role": "assistant", "content": [ { "type": "tool_use", "id": "t1", "name": "Bash", "input": {} } ] },
                { "role": "user", "content": [ { "type": "tool_result", "tool_use_id": "t1", "content": "ok",
                  "cache_control": { "type": "ephemeral", "ttl": "1h" } } ] }
            ]
        })
    }

    fn prompt_blocks() -> Value {
        serde_json::json!([ { "type": "text", "text": "fix the test" } ])
    }

    #[test]
    fn the_loop_is_written_for_five_minutes_and_the_prompt_for_an_hour() {
        let mut doc = loop_request(&prompt_blocks());
        assert_eq!(apply_loop_ttl(&mut doc, false), 1);
        assert_eq!(
            doc["messages"][2]["content"][0]["cache_control"]["ttl"],
            "5m"
        );
        assert_eq!(
            doc["messages"][0]["content"][0]["cache_control"]["ttl"],
            "1h"
        );
        assert_eq!(doc["messages"][0]["content"][0]["text"], "fix the test");
        assert_eq!(
            doc["system"][1]["cache_control"]["ttl"], "1h",
            "system keeps its lifetime"
        );

        let mut again = loop_request(&prompt_blocks());
        apply_loop_ttl(&mut again, false);
        assert_eq!(doc, again, "same request, same bytes (#498)");

        let mut plain = loop_request(&serde_json::json!("fix the test"));
        let before = plain.clone();
        assert_eq!(
            apply_loop_ttl(&mut plain, false),
            0,
            "a string prompt is never rewrapped"
        );
        assert_eq!(plain, before);
    }

    #[test]
    fn a_turn_that_starts_with_the_prompt_is_left_alone() {
        let mut doc = serde_json::json!({ "messages": [ { "role": "user", "content": [
            { "type": "text", "text": "hi", "cache_control": { "type": "ephemeral", "ttl": "1h" } } ] } ] });
        let before = doc.clone();
        assert_eq!(apply_loop_ttl(&mut doc, false), 0);
        assert_eq!(doc, before);
    }

    #[test]
    fn without_a_free_marker_the_loop_keeps_an_hour() {
        let mut doc = loop_request(&prompt_blocks());
        doc["tools"] = serde_json::json!([ { "name": "x", "cache_control": { "type": "ephemeral", "ttl": "1h" } } ]);
        let before = doc.clone();
        assert_eq!(
            apply_loop_ttl(&mut doc, false),
            0,
            "four markers in use: nothing changes"
        );
        assert_eq!(doc, before);
    }

    #[test]
    fn a_headless_client_writes_every_message_marker_for_five_minutes() {
        assert!(is_headless_client(Some(
            "claude-cli/2.1.316 (external, sdk-cli)"
        )));
        assert!(!is_headless_client(Some(
            "claude-cli/2.1.316 (external, cli)"
        )));
        assert!(!is_headless_client(None));

        // The turn's first request: interactive leaves it alone, headless shortens it.
        let first = || {
            serde_json::json!({ "messages": [ { "role": "user", "content": [
            { "type": "text", "text": "hi", "cache_control": { "type": "ephemeral", "ttl": "1h" } } ] } ] })
        };
        let mut doc = first();
        assert_eq!(apply_loop_ttl(&mut doc, true), 1);
        assert_eq!(
            doc["messages"][0]["content"][0]["cache_control"]["ttl"],
            "5m"
        );

        let mut loop_doc = loop_request(&prompt_blocks());
        assert_eq!(
            apply_loop_ttl(&mut loop_doc, true),
            2,
            "message + session system marker"
        );
        assert!(
            loop_doc["messages"][0]["content"][0]
                .get("cache_control")
                .is_none(),
            "no prompt marker without a person to come back"
        );
        assert_eq!(
            loop_doc["system"][0]["cache_control"]["ttl"], "1h",
            "the shared static part keeps its hour"
        );
        assert_eq!(
            loop_doc["system"][1]["cache_control"]["ttl"], "5m",
            "the session's own part does not"
        );
    }
}
