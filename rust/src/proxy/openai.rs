use axum::{
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    response::Response,
};
use serde_json::Value;

use super::ProxyState;
use super::compress::compress_tool_result;
use super::forward;
use super::tool_kind::{self, ToolResultKind, should_protect};

pub async fn handler(
    State(state): State<ProxyState>,
    req: Request<Body>,
) -> Result<Response, StatusCode> {
    let upstream = state.openai_upstream.clone();
    forward::forward_request(
        State(state),
        req,
        &upstream,
        "/v1/chat/completions",
        compress_request_body,
        "OpenAI",
        &[],
    )
    .await
}

fn compress_request_body(parsed: Value, original_size: usize) -> (Vec<u8>, usize, usize) {
    let mut doc = parsed;
    let mut modified = false;

    if let Some(messages) = doc.get_mut("messages").and_then(|m| m.as_array_mut()) {
        let tool_names = tool_kind::openai_tool_names(messages);

        // OpenAI's automatic prompt caching is prefix-based like Anthropic's,
        // so history is pruned at the same frozen, cache-aware boundary.
        let mode = crate::core::config::Config::load()
            .proxy
            .resolved_history_mode();
        let boundary = super::history_prune::prune_boundary(mode, messages.len());
        modified |= super::history_prune::prune_history(messages, boundary, &tool_names);

        for msg in messages.iter_mut() {
            let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");
            if role != "tool" {
                continue;
            }

            let name = msg
                .get("tool_call_id")
                .and_then(|v| v.as_str())
                .and_then(|id| tool_names.get(id))
                .map(String::as_str);
            let kind = name.map_or(ToolResultKind::Other, tool_kind::classify_tool_name);

            if let Some(content) = msg
                .get_mut("content")
                .and_then(|c| c.as_str().map(String::from))
            {
                if should_protect(kind, &content) {
                    continue;
                }
                let compressed = compress_tool_result(&content, name);
                if compressed.len() < content.len() {
                    msg["content"] = Value::String(compressed);
                    modified = true;
                }
            }
        }
    }

    let out = serde_json::to_vec(&doc).unwrap_or_default();
    let compressed_size = if modified { out.len() } else { original_size };
    (out, original_size, compressed_size)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_file_tool_result_protected() {
        let code = (0..60)
            .map(|i| format!("    const value{i} = computeValue{i}(ctx, opts);"))
            .collect::<Vec<_>>()
            .join("\n");
        let body = serde_json::json!({
            "model": "gpt-5",
            "messages": [
                {"role": "assistant", "tool_calls": [{"id": "call_1", "type": "function", "function": {"name": "read_file"}}]},
                {"role": "tool", "tool_call_id": "call_1", "content": code}
            ]
        });
        let bytes = serde_json::to_vec(&body).unwrap();
        let (out, _orig, _comp) = compress_request_body(body, bytes.len());
        let parsed: Value = serde_json::from_slice(&out).unwrap();
        assert!(
            parsed["messages"][1]["content"]
                .as_str()
                .unwrap()
                .contains("value59")
        );
    }
}
