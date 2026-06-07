//! Flatten an MCP tool result into plain text.
//!
//! Mirrors `cookbook/sdk/src/toolText.ts` so the Rust and TypeScript clients
//! extract text identically across MCP content shapes.

use serde_json::Value;

/// Extract the human-readable text from an MCP tool result.
///
/// Handles the common content shapes (`{ text: "…" }`, `{ text: { text: "…" } }`,
/// `{ type: "text", value: "…" }`) and falls back to `structuredContent`
/// (pretty-printed when it is not already a string). Returns an empty string
/// when there is nothing textual to show.
#[must_use]
pub fn tool_result_to_text(result: &Value) -> String {
    let Some(obj) = result.as_object() else {
        return String::new();
    };

    let mut out = String::new();
    if let Some(content) = obj.get("content").and_then(Value::as_array) {
        for item in content {
            let Some(c) = item.as_object() else { continue };

            if let Some(direct) = c.get("text").and_then(Value::as_str) {
                out.push_str(direct);
                continue;
            }
            if let Some(nested) = c
                .get("text")
                .and_then(Value::as_object)
                .and_then(|t| t.get("text"))
                .and_then(Value::as_str)
            {
                out.push_str(nested);
                continue;
            }
            if c.get("type").and_then(Value::as_str) == Some("text") {
                if let Some(value) = c.get("value").and_then(Value::as_str) {
                    out.push_str(value);
                }
            }
        }
    }

    if !out.is_empty() {
        return out;
    }

    let structured = obj
        .get("structuredContent")
        .or_else(|| obj.get("structured_content"));
    match structured {
        None => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(v) => serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extracts_direct_text_blocks() {
        let r = json!({ "content": [{ "type": "text", "text": "hello " }, { "text": "world" }] });
        assert_eq!(tool_result_to_text(&r), "hello world");
    }

    #[test]
    fn extracts_nested_and_value_shapes() {
        let r = json!({ "content": [{ "text": { "text": "nested" } }, { "type": "text", "value": "+v" }] });
        assert_eq!(tool_result_to_text(&r), "nested+v");
    }

    #[test]
    fn falls_back_to_structured_content() {
        let r = json!({ "content": [], "structuredContent": { "a": 1 } });
        assert_eq!(tool_result_to_text(&r), "{\n  \"a\": 1\n}");
    }

    #[test]
    fn structured_string_passthrough_and_empty() {
        assert_eq!(
            tool_result_to_text(&json!({ "structured_content": "raw" })),
            "raw"
        );
        assert_eq!(tool_result_to_text(&json!({ "content": [] })), "");
        assert_eq!(tool_result_to_text(&json!(42)), "");
    }
}
