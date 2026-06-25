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
use super::{cache_safety, prose};
use crate::core::config::{HistoryMode, ProseRole};

pub async fn handler(
    State(state): State<ProxyState>,
    req: Request<Body>,
) -> Result<Response, StatusCode> {
    let upstream = state.gemini_upstream();
    // Gemini carries the model in the URL path, not the body — capture it here so
    // the effort applier can pick the right thinking control (#840).
    let model = super::usage::gemini_model_from_path(req.uri().path());
    forward::forward_request(
        State(state),
        req,
        &upstream,
        "/",
        move |body, size| compress_request_body(body, size, model.as_deref()),
        "Gemini",
        &["application/x-ndjson"],
    )
    .await
}

fn compress_request_body(
    parsed: Value,
    original_size: usize,
    model: Option<&str>,
) -> (Vec<u8>, usize, usize) {
    let mut doc = parsed;
    let mut modified = false;

    // Opt-in per-role prose aggressiveness (#710); both default `None` → no-op.
    let cfg = crate::core::config::Config::load();
    let system_aggr = cfg.proxy.resolved_role_aggressiveness(ProseRole::System);
    let user_aggr = cfg.proxy.resolved_role_aggressiveness(ProseRole::User);
    let live_compress = cfg.proxy.live_compresses();
    let mode = cfg.proxy.resolved_history_mode();
    // #895 Track B: output-savings holdout arm, from the pristine body (before any
    // mutation below) so it matches the arm the response meter records. Control
    // conversations skip output-shaping but are still metered. Default 0 → Treatment.
    let arm = super::holdout::assign(
        &super::holdout::google_key(&doc),
        cfg.proxy.output_holdout_fraction(),
    );
    // #493: in-band CCR expansion (opt-in). Splice any <lc_expand:HASH> the model
    // echoed back into the verbatim original from the local tee store. A strict
    // no-op when no marker is present (byte-identical body → cache-safe). Runs
    // before the meter-only short-circuit so an explicit expand request is
    // honored even when the proxy is otherwise byte-passthrough.
    if cfg.proxy.ccr_inband_enabled() {
        modified |= super::ccr::splice_inband_in_place(&mut doc);
    }
    // #834/#840: cache-safe cross-provider effort control. Default off → no-op.
    // The level is a constant, so it never perturbs the prompt-cache prefix; it
    // sets generationConfig.thinkingConfig (thinkingLevel on 3.x, thinkingBudget
    // on 2.5 pro/flash) only for models that accept it and only when the client
    // didn't pin its own thinking field. `model` is read from the URL path.
    if arm == super::holdout::Arm::Treatment {
        if let Some(effort) = cfg.proxy.resolved_effort() {
            modified |= super::effort::apply_google(&mut doc, effort, model);
        }
        // #895: cache-safe wire verbosity steer; control arm skips it (measured).
        if cfg.proxy.verbosity_steer_enabled() {
            modified |= super::verbosity::apply_google(&mut doc);
        }
    }
    // Meter-only (#481): no live compression, no history pruning, no prose → the
    // body is forwarded unchanged while usage metering still runs. A pending
    // in-band splice (`modified`) opts out: the body did change this turn.
    if !live_compress
        && mode == HistoryMode::Off
        && system_aggr.is_none()
        && user_aggr.is_none()
        && !modified
    {
        let out = serde_json::to_vec(&doc).unwrap_or_default();
        return (out, original_size, original_size);
    }
    let mut prose_segments: u64 = 0;

    // System prose: the top-level `systemInstruction` anchor. Gemini has no
    // client `cache_control`, and the rewrite is deterministic, so the implicit
    // prefix cache stays byte-stable across turns — cache-safe by construction.
    if let Some(a) = system_aggr {
        for key in ["systemInstruction", "system_instruction"] {
            if let Some(parts) = doc
                .get_mut(key)
                .and_then(|si| si.get_mut("parts"))
                .and_then(|p| p.as_array_mut())
            {
                prose_segments += u64::from(prose::compress_gemini_text_parts(parts, a));
            }
        }
    }

    if let Some(contents) = doc.get_mut("contents").and_then(|c| c.as_array_mut()) {
        // Gemini's implicit prompt cache is prefix-based, so the frozen OLD
        // region is pruned at the same monotone staircase boundary as every
        // other rail. We never remove a `contents` entry — only rewrite the
        // `functionResponse` text in place — so the conversation structure
        // (and any `functionCall` ↔ `functionResponse` correspondence) is intact.
        // `mode` resolved above.
        let boundary = super::history_prune::prune_boundary(mode, contents.len());

        for (idx, content) in contents.iter_mut().enumerate() {
            let in_old_region = idx < boundary;
            // Own the role before the mutable `parts` borrow below.
            let role = content
                .get("role")
                .and_then(|r| r.as_str())
                .map(String::from);
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
                // #481: recent-region live compression respects the global toggle
                // and the per-tool exclusion list (Serena default). Old-region
                // pruning stays governed by `history_mode`.
                let live = live_compress
                    && !name
                        .as_deref()
                        .is_some_and(|n| cfg.proxy.is_tool_live_compress_excluded(n));
                let Some(response) = func_resp.get_mut("response") else {
                    continue;
                };
                for field in ["result", "content"] {
                    modified |= if in_old_region {
                        prune_string_field(response, field, kind)
                    } else if live {
                        compress_string_field(response, field, name.as_deref(), kind)
                    } else {
                        false
                    };
                }
            }

            // Frozen-region user prose: free-text `text` parts of user turns in
            // the old region `[0, boundary)`. Model turns (assistant) and tool
            // I/O parts are never touched.
            if in_old_region
                && role.as_deref() == Some("user")
                && let Some(a) = user_aggr
            {
                prose_segments += u64::from(prose::compress_gemini_text_parts(parts, a));
            }
        }
    }

    if prose_segments > 0 {
        modified = true;
    }
    cache_safety::record(prose_segments, true);

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
        let (out, orig, comp) = compress_request_body(body, bytes.len(), None);
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
        let (out, orig, comp) = compress_request_body(body, bytes.len(), None);
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
        let (out, orig, comp) = compress_request_body(body.clone(), bytes.len(), None);
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
        let (out_a, _, _) = compress_request_body(a, la, None);
        let (out_b, _, _) = compress_request_body(b, lb, None);
        assert_eq!(out_a, out_b, "identical input must yield identical bytes");
    }

    fn big_prose() -> String {
        let p = "You are a careful, senior software engineer. You always explain your \
                 reasoning before making changes, you prefer small reviewable diffs, and \
                 you never introduce mock data or placeholders into production code. ";
        [p; 6].join("\n")
    }

    #[test]
    fn system_instruction_compressed_and_model_untouched() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        crate::core::config::Config::update_global(|c| {
            c.proxy.role_aggressiveness.system = Some(0.6);
        })
        .unwrap();

        let prose = big_prose();
        let body = serde_json::json!({
            "systemInstruction": {"parts": [{"text": prose}]},
            "contents": [
                {"role": "user", "parts": [{"text": "hi"}]},
                {"role": "model", "parts": [{"text": prose}]},
            ]
        });
        let bytes = serde_json::to_vec(&body).unwrap();
        let (out, _o, _c) = compress_request_body(body, bytes.len(), None);
        let parsed: Value = serde_json::from_slice(&out).unwrap();

        assert!(
            parsed["systemInstruction"]["parts"][0]["text"]
                .as_str()
                .unwrap()
                .len()
                < prose.len(),
            "systemInstruction prose must be compressed when enabled"
        );
        assert_eq!(
            parsed["contents"][1]["parts"][0]["text"].as_str().unwrap(),
            prose,
            "model (assistant) turns must pass through verbatim (#710)"
        );
    }

    #[test]
    fn gemini_prose_compression_is_deterministic() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        crate::core::config::Config::update_global(|c| {
            c.proxy.role_aggressiveness.system = Some(0.6);
        })
        .unwrap();
        let prose = big_prose();
        let mk = || {
            serde_json::json!({
                "systemInstruction": {"parts": [{"text": prose}]},
                "contents": [{"role": "user", "parts": [{"text": "hi"}]}]
            })
        };
        let (a, b) = (mk(), mk());
        let la = serde_json::to_vec(&a).unwrap().len();
        let lb = serde_json::to_vec(&b).unwrap().len();
        assert_eq!(
            compress_request_body(a, la, None).0,
            compress_request_body(b, lb, None).0,
            "identical input must yield byte-identical output (#498)"
        );
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
            let (out, _, _) = compress_request_body(body, bytes.len(), None);
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

    #[test]
    fn effort_control_sets_thinking_config_by_generation() {
        // #840 end-to-end: the model is taken from the URL path, so the handler
        // threads it in. 3.x → thinkingLevel; off / unknown model → byte no-op.
        let _iso = crate::core::data_dir::isolated_data_dir();
        crate::test_env::remove_var("LEAN_CTX_PROXY_EFFORT");
        crate::core::config::Config::update_global(|c| {
            c.proxy.effort = Some("low".into());
        })
        .unwrap();

        let body = serde_json::json!({
            "contents": [{"role": "user", "parts": [{"text": "hi"}]}]
        });
        let bytes = serde_json::to_vec(&body).unwrap();
        let (out, _o, _c) = compress_request_body(body.clone(), bytes.len(), Some("gemini-3-pro"));
        assert_eq!(
            serde_json::from_slice::<Value>(&out).unwrap()["generationConfig"]["thinkingConfig"]["thinkingLevel"],
            "low"
        );

        // No model (path didn't resolve) → strict no-op, body byte-unchanged.
        let bytes = serde_json::to_vec(&body).unwrap();
        let (out, _o, _c) = compress_request_body(body.clone(), bytes.len(), None);
        assert_eq!(serde_json::from_slice::<Value>(&out).unwrap(), body);
    }
}
