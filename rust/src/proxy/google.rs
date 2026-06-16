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
    let upstream = state.gemini_upstream.clone();
    forward::forward_request(
        State(state),
        req,
        &upstream,
        "/",
        compress_request_body,
        "Gemini",
        &["application/x-ndjson"],
    )
    .await
}

fn compress_request_body(parsed: Value, original_size: usize) -> (Vec<u8>, usize, usize) {
    let mut doc = parsed;
    let mut modified = false;

    if let Some(contents) = doc.get_mut("contents").and_then(|c| c.as_array_mut()) {
        for content in contents.iter_mut() {
            if let Some(parts) = content.get_mut("parts").and_then(|p| p.as_array_mut()) {
                for part in parts.iter_mut() {
                    if let Some(func_resp) = part.get_mut("functionResponse") {
                        // Gemini carries the originating function name inline.
                        let kind = func_resp
                            .get("name")
                            .and_then(|v| v.as_str())
                            .map_or(ToolResultKind::Other, tool_kind::classify_tool_name);
                        if let Some(response) = func_resp.get_mut("response") {
                            modified |= compress_string_field(response, "result", kind);
                            modified |= compress_string_field(response, "content", kind);
                        }
                    }
                }
            }
        }
    }

    let out = serde_json::to_vec(&doc).unwrap_or_default();
    let compressed_size = if modified { out.len() } else { original_size };
    (out, original_size, compressed_size)
}

fn compress_string_field(obj: &mut Value, field: &str, kind: ToolResultKind) -> bool {
    if let Some(val) = obj
        .get_mut(field)
        .and_then(|v| v.as_str().map(String::from))
    {
        if should_protect(kind, &val) {
            return false;
        }
        let compressed = compress_tool_result(&val, None);
        if compressed.len() < val.len() {
            obj[field] = Value::String(compressed);
            return true;
        }
    }
    false
}
