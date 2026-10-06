// SPDX-License-Identifier: Apache-2.0

//! Pure canonical request helpers shared by Local and Via Edge.

use std::collections::BTreeMap;

use serde_json::Value;

/// Canonical JSON: sorted object keys, stable array order and standard scalars.
pub fn canonical_json_bytes(value: &Value) -> Vec<u8> {
    let mut bytes = Vec::new();
    write_canonical_json(value, &mut bytes);
    bytes
}

/// Canonical bytes for an ordered prefix of provider messages.
pub fn canonical_message_prefix(messages: &[Value], count: usize) -> Vec<u8> {
    canonical_json_bytes(&Value::Array(
        messages.iter().take(count).cloned().collect(),
    ))
}

/// Byte length of the open canonical array prefix used by Local's cache guard.
pub fn canonical_message_prefix_len(messages: &[Value], count: usize) -> usize {
    if count == 0 {
        return 0;
    }
    let mut bytes = Vec::from(b"[".as_slice());
    for (index, message) in messages.iter().take(count).enumerate() {
        if index > 0 {
            bytes.push(b',');
        }
        write_canonical_json(message, &mut bytes);
    }
    bytes.len()
}

/// Extract context regions whose exact bytes affect provider prompt caching.
pub fn cache_relevant_messages(request: &Value) -> Vec<Value> {
    let mut messages = Vec::new();
    if let Some(system) = request.get("system") {
        messages.push(serde_json::json!({"role": "system", "content": system}));
    }
    if let Some(items) = request
        .get("messages")
        .or_else(|| request.get("input"))
        .and_then(Value::as_array)
    {
        messages.extend(items.iter().cloned());
    }
    messages
}

/// Number of leading messages protected by the final explicit cache marker.
pub fn cache_breakpoint_len(messages: &[Value]) -> usize {
    messages
        .iter()
        .enumerate()
        .filter_map(|(index, message)| message_has_cache_control(message).then_some(index + 1))
        .next_back()
        .unwrap_or_default()
}

/// Whether a message contains an explicit cache-control marker at a known level.
pub fn message_has_cache_control(message: &Value) -> bool {
    if message.get("cache_control").is_some() {
        return true;
    }
    message
        .get("content")
        .and_then(Value::as_array)
        .is_some_and(|blocks| {
            blocks.iter().any(|block| {
                block.get("cache_control").is_some()
                    || block
                        .get("content")
                        .and_then(Value::as_array)
                        .is_some_and(|items| {
                            items.iter().any(|item| item.get("cache_control").is_some())
                        })
            })
        })
}

fn write_canonical_json(value: &Value, out: &mut Vec<u8>) {
    match value {
        Value::Array(items) => {
            out.push(b'[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(b',');
                }
                write_canonical_json(item, out);
            }
            out.push(b']');
        }
        Value::Object(map) => {
            out.push(b'{');
            for (index, (key, item)) in map
                .iter()
                .collect::<BTreeMap<_, _>>()
                .into_iter()
                .enumerate()
            {
                if index > 0 {
                    out.push(b',');
                }
                out.extend(serde_json::to_vec(key).expect("JSON object keys serialize"));
                out.push(b':');
                write_canonical_json(item, out);
            }
            out.push(b'}');
        }
        _ => out.extend(serde_json::to_vec(value).expect("JSON scalar serializes")),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn canonical_bytes_and_cache_boundary_are_deterministic() {
        let value = json!({"z": 1, "a": [{"b": 2, "a": 1}]});
        assert_eq!(
            canonical_json_bytes(&value),
            br#"{"a":[{"a":1,"b":2}],"z":1}"#
        );
        let messages = vec![
            json!({"role":"user","content":"old"}),
            json!({"role":"assistant","content":[{"type":"text","text":"cached","cache_control":{"type":"ephemeral"}}]}),
            json!({"role":"user","content":"current"}),
        ];
        assert_eq!(cache_breakpoint_len(&messages), 2);
        assert_eq!(
            canonical_message_prefix(&messages, 2),
            canonical_json_bytes(&Value::Array(messages[..2].to_vec()))
        );
    }
}
