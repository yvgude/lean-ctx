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

const UPSTREAM: &str = "https://generativelanguage.googleapis.com";

pub async fn handler(state: State<ProxyState>, req: Request<Body>) -> Result<Response, StatusCode> {
    forward::forward_request(
        state,
        req,
        UPSTREAM,
        "/",
        compress_request_body,
        "Gemini",
        &["application/x-ndjson"],
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

    if let Some(contents) = doc.get_mut("contents").and_then(|c| c.as_array_mut()) {
        for content in contents.iter_mut() {
            if let Some(parts) = content.get_mut("parts").and_then(|p| p.as_array_mut()) {
                for part in parts.iter_mut() {
                    if let Some(func_resp) = part.get_mut("functionResponse") {
                        if let Some(response) = func_resp.get_mut("response") {
                            modified |= compress_string_field(response, "result");
                            modified |= compress_string_field(response, "content");
                        }
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

fn compress_string_field(obj: &mut Value, field: &str) -> bool {
    if let Some(val) = obj
        .get_mut(field)
        .and_then(|v| v.as_str().map(String::from))
    {
        let compressed = compress_tool_result(&val, None);
        if compressed.len() < val.len() {
            obj[field] = Value::String(compressed);
            return true;
        }
    }
    false
}
