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

/// Proxy handler for OpenAI's Responses API (`POST /v1/responses`).
///
/// The Responses API superseded Chat Completions for clients such as opencode
/// and the OpenAI Agents SDK. Its conversation turns live in `input` rather than
/// `messages`, so the Chat Completions handler never saw — and never compressed —
/// them. This handler reuses the same upstream, auth and streaming path but
/// understands the Responses-API request shape.
///
/// Retrieve / cancel / delete / input_items sub-paths
/// (`/v1/responses/{id}/...`) are routed here as well and pass through untouched:
/// they carry no `input` array, so `compress_request_body` is a no-op for them.
///
/// Handles `POST /v1/responses` (and the bare `/responses`) over HTTP/SSE.
pub async fn handler(
    State(state): State<ProxyState>,
    req: Request<Body>,
) -> Result<Response, StatusCode> {
    let upstream = state.openai_upstream.clone();
    forward::forward_request(
        State(state),
        req,
        &upstream,
        "/v1/responses",
        compress_request_body,
        "OpenAI",
        &[],
    )
    .await
}

/// Handles the WebSocket Responses transport on `GET /v1/responses`.
///
/// Codex (and the OpenAI SDK) default to `ws://…/responses` with one
/// `response.create` event per turn. Bridging the upgrade here lets the proxy be
/// a drop-in for Codex without forcing `supports_websockets = false` (#440); the
/// actual WS↔HTTP/SSE bridging lives in `openai_responses_ws`.
pub async fn ws_handler(
    State(state): State<ProxyState>,
    headers: axum::http::HeaderMap,
    ws: axum::extract::ws::WebSocketUpgrade,
) -> Response {
    super::openai_responses_ws::upgrade(state, ws, &headers)
}

fn compress_request_body(parsed: Value, original_size: usize) -> (Vec<u8>, usize, usize) {
    let mut doc = parsed;
    let modified = compress_responses_input(&mut doc);
    let out = serde_json::to_vec(&doc).unwrap_or_default();
    let compressed_size = if modified { out.len() } else { original_size };
    (out, original_size, compressed_size)
}

/// Compresses the `function_call_output.output` entries of a Responses-API body
/// in place, returning whether anything changed. Shared by the HTTP handler and
/// the WebSocket bridge (#440) so both paths get identical, safe savings.
///
/// The only token sink we shrink is each `function_call_output.output` — the
/// Responses-API analogue of a Chat Completions `role:"tool"` message. We
/// deliberately do NOT prune or reorder the `input` array: the Responses API
/// rejects a `function_call` whose matching `function_call_output` is absent
/// (and reasoning items must keep their originating call), so structural
/// history-pruning would risk 400s here. Compressing only the tool outputs
/// captures the bulk of the savings without touching the conversation structure.
pub(super) fn compress_responses_input(doc: &mut Value) -> bool {
    let mut modified = false;
    if let Some(input) = doc.get_mut("input").and_then(|i| i.as_array_mut()) {
        let tool_names = tool_kind::responses_tool_names(input);
        for item in input.iter_mut() {
            if item.get("type").and_then(|t| t.as_str()) != Some("function_call_output") {
                continue;
            }
            let name = item
                .get("call_id")
                .and_then(|v| v.as_str())
                .and_then(|id| tool_names.get(id))
                .map(String::as_str);
            let kind = name.map_or(ToolResultKind::Other, tool_kind::classify_tool_name);
            if let Some(output) = item.get_mut("output") {
                modified |= compress_output_field(output, name, kind);
            }
        }
    }
    modified
}

/// Compress a `function_call_output.output`. OpenAI sends this as a JSON string,
/// but the API also accepts an array of content parts (`input_text` blocks) for
/// tools returning richer data, so both shapes are handled.
///
/// A protected file/source read (resolved from the matching `function_call`
/// name) is left intact so a mid-refactor model never loses the body it edits.
fn compress_output_field(
    output: &mut Value,
    tool_name: Option<&str>,
    kind: ToolResultKind,
) -> bool {
    match output {
        Value::String(s) => {
            if should_protect(kind, s) {
                return false;
            }
            let compressed = compress_tool_result(s, tool_name);
            if compressed.len() < s.len() {
                *s = compressed;
                return true;
            }
            false
        }
        Value::Array(parts) => {
            let mut changed = false;
            for part in parts.iter_mut() {
                if let Some(Value::String(text)) = part.get_mut("text") {
                    if should_protect(kind, text) {
                        continue;
                    }
                    let compressed = compress_tool_result(text, tool_name);
                    if compressed.len() < text.len() {
                        *text = compressed;
                        changed = true;
                    }
                }
            }
            changed
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A long `git status` is a known-compressible fixture: `has_structural_output`
    /// is false for it, so it flows through the git-status pattern compressor.
    fn long_git_status() -> String {
        let mut s = String::from(
            "$ git status\nOn branch main\nYour branch is up to date with 'origin/main'.\n\nChanges not staged for commit:\n  (use \"git add <file>...\" to update what will be committed)\n",
        );
        for i in 0..80 {
            s.push_str(&format!("\tmodified:   src/module_{i}/file_{i}.rs\n"));
        }
        s.push_str("\nno changes added to commit (use \"git add\" and/or \"git commit -a\")\n");
        s
    }

    #[test]
    fn string_output_mirrors_engine_and_shrinks() {
        let raw = long_git_status();
        let expected = compress_tool_result(&raw, None);
        assert!(
            expected.len() < raw.len(),
            "fixture must be compressible by the shared engine"
        );

        let body = serde_json::json!({
            "model": "gpt-5",
            "input": [
                {"type": "function_call_output", "call_id": "call_1", "output": raw}
            ]
        });
        let bytes = serde_json::to_vec(&body).unwrap();
        let (out, orig, comp) = compress_request_body(body, bytes.len());

        assert!(comp < orig, "compressed body must be smaller");
        let parsed: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(
            parsed["input"][0]["output"].as_str().unwrap(),
            expected,
            "output must be exactly what the shared compressor produces"
        );
    }

    #[test]
    fn array_output_text_is_compressed() {
        let raw = long_git_status();
        let expected = compress_tool_result(&raw, None);

        let body = serde_json::json!({
            "input": [
                {
                    "type": "function_call_output",
                    "call_id": "call_1",
                    "output": [{"type": "input_text", "text": raw}]
                }
            ]
        });
        let bytes = serde_json::to_vec(&body).unwrap();
        let (out, orig, comp) = compress_request_body(body, bytes.len());

        assert!(comp < orig);
        let parsed: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(
            parsed["input"][0]["output"][0]["text"].as_str().unwrap(),
            expected
        );
    }

    #[test]
    fn non_tool_output_items_are_untouched() {
        let body = serde_json::json!({
            "input": [
                {"type": "message", "role": "user", "content": long_git_status()},
                {"type": "function_call", "call_id": "c", "name": "x", "arguments": "{}"}
            ]
        });
        let bytes = serde_json::to_vec(&body).unwrap();
        let (out, orig, comp) = compress_request_body(body.clone(), bytes.len());

        assert_eq!(comp, orig, "no function_call_output → passthrough");
        let reparsed: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(reparsed, body);
    }

    #[test]
    fn plain_string_input_passthrough() {
        let body = serde_json::json!({"model": "gpt-5", "input": "hello world"});
        let bytes = serde_json::to_vec(&body).unwrap();
        let (out, orig, comp) = compress_request_body(body.clone(), bytes.len());
        assert_eq!(comp, orig);
        let reparsed: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(reparsed, body);
    }

    #[test]
    fn no_input_field_passthrough() {
        let body = serde_json::json!({"model": "gpt-5", "previous_response_id": "resp_abc"});
        let bytes = serde_json::to_vec(&body).unwrap();
        let (out, orig, comp) = compress_request_body(body.clone(), bytes.len());
        assert_eq!(comp, orig);
        let reparsed: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(reparsed, body);
    }

    #[test]
    fn short_output_unchanged() {
        let body = serde_json::json!({
            "input": [
                {"type": "function_call_output", "call_id": "c", "output": "ok"}
            ]
        });
        let bytes = serde_json::to_vec(&body).unwrap();
        let (out, orig, comp) = compress_request_body(body.clone(), bytes.len());
        assert_eq!(comp, orig);
        let reparsed: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(reparsed, body);
    }
}
