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

const UPSTREAM: &str = "https://api.openai.com";

pub async fn handler(state: State<ProxyState>, req: Request<Body>) -> Result<Response, StatusCode> {
    forward::forward_request(
        state,
        req,
        UPSTREAM,
        "/v1/chat/completions",
        compress_request_body,
        "OpenAI",
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
            if role != "tool" {
                continue;
            }

            if let Some(content) = msg
                .get_mut("content")
                .and_then(|c| c.as_str().map(String::from))
            {
                let compressed = compress_tool_result(&content, None);
                if compressed.len() < content.len() {
                    msg["content"] = Value::String(compressed);
                    modified = true;
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
