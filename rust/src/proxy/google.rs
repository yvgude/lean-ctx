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
    let upstream = state.gemini_upstream();
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
        // Gemini's implicit prompt cache is prefix-based, so the frozen OLD
        // region is pruned at the same monotone staircase boundary as every
        // other rail. We never remove a `contents` entry — only rewrite the
        // `functionResponse` text in place — so the conversation structure
        // (and any `functionCall` ↔ `functionResponse` correspondence) is intact.
        let mode = crate::core::config::Config::load()
            .proxy
            .resolved_history_mode();
        let boundary = super::history_prune::prune_boundary(mode, contents.len());

        for (idx, content) in contents.iter_mut().enumerate() {
            let in_old_region = idx < boundary;
            let Some(parts) = content.get_mut("parts").and_then(|p| p.as_array_mut()) else {
                continue;
            };
            for part in parts.iter_mut() {
                let Some(func_resp) = part.get_mut("functionResponse") else {
                    continue;
                };
                // Gemini carries the originating function name inline — route it
                // to the compressor (not `None`) so tool-specific patterns apply.
                let name = func_resp
                    .get("name")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                let kind = name
                    .as_deref()
                    .map_or(ToolResultKind::Other, tool_kind::classify_tool_name);
                let Some(response) = func_resp.get_mut("response") else {
                    continue;
                };
                for field in ["result", "content"] {
                    modified |= if in_old_region {
                        prune_string_field(response, field, kind)
                    } else {
                        compress_string_field(response, field, name.as_deref(), kind)
                    };
                }
            }
        }
    }

    let out = serde_json::to_vec(&doc).unwrap_or_default();
    let compressed_size = if modified { out.len() } else { original_size };
    (out, original_size, compressed_size)
}

/// Compress a recent `functionResponse.response.<field>` string. `tool_name` is
/// routed to the compressor so tool-specific patterns (git status, ls, …) apply;
/// protected file/source reads in the recent region are left intact.
fn compress_string_field(
    obj: &mut Value,
    field: &str,
    tool_name: Option<&str>,
    kind: ToolResultKind,
) -> bool {
    if let Some(val) = obj
        .get_mut(field)
        .and_then(|v| v.as_str().map(String::from))
    {
        if should_protect(kind, &val) {
            return false;
        }
        let compressed = compress_tool_result(&val, tool_name);
        if compressed.len() < val.len() {
            obj[field] = Value::String(compressed);
            return true;
        }
    }
    false
}

/// Cache-aware prune of an OLD `functionResponse.response.<field>`: file/source
/// reads collapse to a re-read stub, everything else head/tail summarizes.
/// Content-deterministic, so the cached prefix stays byte-stable across turns.
fn prune_string_field(obj: &mut Value, field: &str, kind: ToolResultKind) -> bool {
    if let Some(val) = obj.get(field).and_then(|v| v.as_str())
        && let Some(pruned) = super::history_prune::prune_output_text(val, kind)
    {
        obj[field] = Value::String(pruned);
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `pairs` Gemini turns: a `model` `functionCall` then the `user`
    /// `functionResponse` carrying a long file read.
    fn gemini_read_turns(pairs: usize) -> Vec<Value> {
        let code = (0..40)
            .map(|i| format!("    let v{i} = compute_{i}(ctx, opts);"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut contents = Vec::new();
        for t in 0..pairs {
            contents.push(serde_json::json!({
                "role": "model",
                "parts": [{"functionCall": {"name": "read_file", "args": {}}}]
            }));
            contents.push(serde_json::json!({
                "role": "user",
                "parts": [{"functionResponse": {
                    "name": "read_file",
                    "response": {"result": format!("{code}\n// turn {t}")}
                }}]
            }));
        }
        contents
    }

    #[test]
    fn recent_response_routes_tool_name_to_compressor() {
        // Default (isolated) config; single content → boundary 0 → recent path.
        let _iso = crate::core::data_dir::isolated_data_dir();
        // A compressible search result. The proxy must route the inline tool name
        // to the shared engine, so its output matches the name-routed engine
        // byte-for-byte (the contract that distinguishes this from `None`).
        // `infer_command`'s use of the name is unit-tested in `compress.rs`.
        let raw = (0..60)
            .map(|i| format!("src/file_{i}.rs:{i}:    let matched = find(foo, bar, baz);"))
            .collect::<Vec<_>>()
            .join("\n");
        let routed = compress_tool_result(&raw, Some("search_files"));
        assert!(routed.len() < raw.len(), "fixture must be compressible");

        let body = serde_json::json!({
            "contents": [
                {"role": "user", "parts": [{"functionResponse": {
                    "name": "search_files", "response": {"result": raw}
                }}]}
            ]
        });
        let bytes = serde_json::to_vec(&body).unwrap();
        let (out, orig, comp) = compress_request_body(body, bytes.len());
        assert!(comp < orig, "recent response must be compressed");
        let parsed: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(
            parsed["contents"][0]["parts"][0]["functionResponse"]["response"]["result"]
                .as_str()
                .unwrap(),
            routed,
            "Gemini path must route the inline tool name to the shared compressor"
        );
    }

    #[test]
    fn cache_aware_prune_stubs_old_reads_keeps_recent() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        // 13 pairs = 26 contents → staircase boundary 16.
        let contents = gemini_read_turns(13);
        let n = contents.len();
        let body = serde_json::json!({ "contents": contents });
        let bytes = serde_json::to_vec(&body).unwrap();
        let (out, orig, comp) = compress_request_body(body, bytes.len());
        assert!(comp < orig, "old reads must be pruned for savings");

        let parsed: Value = serde_json::from_slice(&out).unwrap();
        let got = parsed["contents"].as_array().unwrap();
        assert_eq!(got.len(), n, "no contents may be removed");
        // OLD file read (content index 1, before boundary 16) is stubbed.
        let old = got[1]["parts"][0]["functionResponse"]["response"]["result"]
            .as_str()
            .unwrap();
        assert!(
            old.contains("Re-read the file"),
            "old read should be stubbed, got: {old}"
        );
        // RECENT file read (content index 25, after the boundary) keeps its body.
        let recent = got[25]["parts"][0]["functionResponse"]["response"]["result"]
            .as_str()
            .unwrap();
        assert!(
            recent.contains("v39"),
            "recent read must be protected, got: {recent}"
        );
    }

    #[test]
    fn short_history_is_passthrough() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        let body = serde_json::json!({
            "contents": [
                {"role": "user", "parts": [{"functionResponse": {
                    "name": "read_file", "response": {"result": "ok"}
                }}]}
            ]
        });
        let bytes = serde_json::to_vec(&body).unwrap();
        let (out, orig, comp) = compress_request_body(body.clone(), bytes.len());
        assert_eq!(comp, orig);
        let reparsed: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(reparsed, body);
    }

    #[test]
    fn gemini_compression_is_deterministic() {
        // #498: identical request → identical bytes.
        let _iso = crate::core::data_dir::isolated_data_dir();
        let mk = || serde_json::json!({ "contents": gemini_read_turns(13) });
        let (a, b) = (mk(), mk());
        let (la, lb) = (
            serde_json::to_vec(&a).unwrap().len(),
            serde_json::to_vec(&b).unwrap().len(),
        );
        let (out_a, _, _) = compress_request_body(a, la);
        let (out_b, _, _) = compress_request_body(b, lb);
        assert_eq!(out_a, out_b, "identical input must yield identical bytes");
    }

    #[test]
    fn cache_aware_gemini_prefix_is_byte_stable_across_turns() {
        // THE cache invariant for the Gemini rail: every `contents` entry before
        // an already-passed boundary stays byte-identical as the chat grows.
        let _iso = crate::core::data_dir::isolated_data_dir();
        let mut prev: Vec<String> = Vec::new();
        let mut prev_boundary = 0;
        for pairs in 1..=20 {
            let contents = gemini_read_turns(pairs);
            let len = contents.len();
            let body = serde_json::json!({ "contents": contents });
            let bytes = serde_json::to_vec(&body).unwrap();
            let (out, _, _) = compress_request_body(body, bytes.len());
            let parsed: Value = serde_json::from_slice(&out).unwrap();
            let items: Vec<String> = parsed["contents"]
                .as_array()
                .unwrap()
                .iter()
                .map(Value::to_string)
                .collect();
            for i in 0..prev_boundary {
                assert_eq!(
                    prev[i], items[i],
                    "Gemini content {i} changed at turn {pairs} — prompt cache prefix broken"
                );
            }
            prev = items;
            prev_boundary = crate::proxy::history_prune::prune_boundary(
                crate::core::config::HistoryMode::CacheAware,
                len,
            );
        }
    }
}
