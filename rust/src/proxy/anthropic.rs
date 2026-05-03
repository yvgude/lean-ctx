use axum::{
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    response::Response,
};
use serde_json::Value;

use super::compress::compress_tool_result;
use super::forward;
use super::ProxyState;

const UPSTREAM: &str = "https://api.anthropic.com";

pub async fn handler(state: State<ProxyState>, req: Request<Body>) -> Result<Response, StatusCode> {
    forward::forward_request(
        state,
        req,
        UPSTREAM,
        "/v1/messages",
        compress_request_body,
        "Anthropic",
        &[],
    )
    .await
}

fn compress_request_body(body: &[u8]) -> (Vec<u8>, usize, usize) {
    let original_size = body.len();

    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (body.to_vec(), original_size, original_size),
    };

    let mut doc = parsed;
    let mut modified = false;

    if let Some(messages) = doc.get_mut("messages").and_then(|m| m.as_array_mut()) {
        for msg in messages.iter_mut() {
            let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");
            if role != "user" {
                continue;
            }

            if let Some(content) = msg.get_mut("content").and_then(|c| c.as_array_mut()) {
                for block in content.iter_mut() {
                    if block.get("type").and_then(|t| t.as_str()) != Some("tool_result") {
                        continue;
                    }

                    if let Some(inner_content) = block.get_mut("content") {
                        modified |= compress_content_field(inner_content, None);
                    }
                }
            }
        }
    }

    if !modified {
        return (body.to_vec(), original_size, original_size);
    }

    match serde_json::to_vec(&doc) {
        Ok(compressed) => {
            let compressed_size = compressed.len();
            (compressed, original_size, compressed_size)
        }
        Err(_) => (body.to_vec(), original_size, original_size),
    }
}

fn compress_content_field(content: &mut Value, tool_name: Option<&str>) -> bool {
    match content {
        Value::String(s) => {
            let compressed = compress_tool_result(s, tool_name);
            if compressed.len() < s.len() {
                *s = compressed;
                return true;
            }
            false
        }
        Value::Array(arr) => {
            let mut modified = false;
            for item in arr.iter_mut() {
                if item.get("type").and_then(|t| t.as_str()) == Some("text") {
                    if let Some(text) = item
                        .get_mut("text")
                        .and_then(|t| t.as_str().map(String::from))
                    {
                        let compressed = compress_tool_result(&text, tool_name);
                        if compressed.len() < text.len() {
                            item["text"] = Value::String(compressed);
                            modified = true;
                        }
                    }
                }
            }
            modified
        }
        _ => false,
    }
}
