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
    let upstream = state.openai_upstream();
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
        &super::holdout::openai_chat_key(&doc),
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
    // #834: cache-safe cross-provider effort control. Default off → no-op. The
    // value is a constant, so it never perturbs the prompt-cache prefix; it sets
    // `reasoning_effort` only on reasoning models and never overrides a
    // client-set value.
    if arm == super::holdout::Arm::Treatment {
        if let Some(effort) = cfg.proxy.resolved_effort() {
            modified |= super::effort::apply_openai_chat(&mut doc, effort);
        }
        // #895: cache-safe wire verbosity steer; control arm skips it (measured).
        if cfg.proxy.verbosity_steer_enabled() {
            modified |= super::verbosity::apply_openai_chat(&mut doc);
        }
    }
    // Meter-only (#481): nothing rewrites the body, so skip all work and let
    // forward + usage metering run against the byte-unchanged request. A pending
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

    if let Some(messages) = doc.get_mut("messages").and_then(|m| m.as_array_mut()) {
        let tool_names = tool_kind::openai_tool_names(messages);

        // OpenAI's automatic prompt caching is prefix-based like Anthropic's,
        // so history is pruned at the same frozen, cache-aware boundary. `mode`
        // resolved above.
        let boundary = super::history_prune::prune_boundary(mode, messages.len());
        // Mirror the Anthropic guard: never rewrite content behind a client
        // `cache_control` breakpoint (#448). OpenAI requests carry none, so this
        // resolves to 0 and pruning is byte-for-byte unchanged — but the code
        // path stays uniform across providers.
        let cached = super::history_prune::cached_prefix_len(messages);
        modified |=
            super::history_prune::prune_history_range(messages, cached, boundary, &tool_names);

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

            // #481: skip live compression when globally off or tool excluded.
            if !live_compress || name.is_some_and(|n| cfg.proxy.is_tool_live_compress_excluded(n)) {
                continue;
            }
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

        // Frozen-region prose: system anchors (deterministic rewrite keeps the
        // auto-cached prefix byte-stable, so safe at any position outside the
        // client-cached prefix) and user turns in `[cached, boundary)`. The
        // `assistant` and `tool` roles are never touched (passthrough). Both
        // knobs default off, so this is inert unless an operator opts in.
        if system_aggr.is_some() || user_aggr.is_some() {
            for (i, msg) in messages.iter_mut().enumerate() {
                if i < cached {
                    continue;
                }
                let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");
                let aggr = match role {
                    "system" | "developer" => system_aggr,
                    "user" if i < boundary => user_aggr,
                    _ => None,
                };
                if let Some(a) = aggr {
                    prose_segments += u64::from(prose::compress_message_content(msg, a));
                }
            }
        }
    }

    if prose_segments > 0 {
        modified = true;
    }
    cache_safety::record(prose_segments, true);

    // Ask OpenAI to append a final usage chunk so the proxy can meter real
    // spend. Not counted as compression (it slightly grows the body), so it
    // never inflates the savings figure.
    maybe_inject_usage_reporting(&mut doc);

    let out = serde_json::to_vec(&doc).unwrap_or_default();
    let compressed_size = if modified { out.len() } else { original_size };
    (out, original_size, compressed_size)
}

/// Config-gated wrapper around [`inject_usage_reporting`].
fn maybe_inject_usage_reporting(doc: &mut Value) {
    if crate::core::config::Config::load()
        .proxy
        .meters_openai_usage()
    {
        inject_usage_reporting(doc);
    }
}

/// Injects `stream_options.include_usage = true` into a streamed Chat
/// Completions request so the final SSE chunk carries `usage` (`OpenAI` omits it
/// otherwise). No-op for non-streamed requests or when the client already
/// configured `stream_options.include_usage`.
fn inject_usage_reporting(doc: &mut Value) {
    if doc.get("stream").and_then(Value::as_bool) != Some(true) {
        return;
    }
    let Some(obj) = doc.as_object_mut() else {
        return;
    };
    let opts = obj
        .entry("stream_options")
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    if let Some(opts_obj) = opts.as_object_mut() {
        opts_obj.entry("include_usage").or_insert(Value::Bool(true));
    }
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

    #[test]
    fn injects_include_usage_for_streaming() {
        let mut doc = serde_json::json!({"model": "gpt-5.4", "stream": true, "messages": []});
        inject_usage_reporting(&mut doc);
        assert_eq!(doc["stream_options"]["include_usage"], Value::Bool(true));
    }

    #[test]
    fn no_injection_for_non_streaming() {
        let mut doc = serde_json::json!({"model": "gpt-5.4", "messages": []});
        inject_usage_reporting(&mut doc);
        assert!(
            doc.get("stream_options").is_none(),
            "non-streamed requests get usage in the body, no injection needed"
        );
    }

    #[test]
    fn respects_client_set_include_usage() {
        let mut doc = serde_json::json!({
            "model": "gpt-5.4",
            "stream": true,
            "stream_options": {"include_usage": false},
            "messages": []
        });
        inject_usage_reporting(&mut doc);
        assert_eq!(
            doc["stream_options"]["include_usage"],
            Value::Bool(false),
            "an explicit client value must be preserved"
        );
    }

    fn big_prose() -> String {
        let p = "You are a careful, senior software engineer. You always explain your \
                 reasoning before making changes, you prefer small reviewable diffs, and \
                 you never introduce mock data or placeholders into production code. ";
        [p; 6].join("\n")
    }

    #[test]
    fn system_message_compressed_and_assistant_untouched() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        crate::core::config::Config::update_global(|c| {
            c.proxy.role_aggressiveness.system = Some(0.6);
        })
        .unwrap();

        let prose = big_prose();
        let body = serde_json::json!({
            "model": "gpt-5",
            "messages": [
                {"role": "system", "content": prose},
                {"role": "user", "content": "hi"},
                {"role": "assistant", "content": prose},
            ]
        });
        let bytes = serde_json::to_vec(&body).unwrap();
        let (out, _o, _c) = compress_request_body(body, bytes.len());
        let parsed: Value = serde_json::from_slice(&out).unwrap();

        assert!(
            parsed["messages"][0]["content"].as_str().unwrap().len() < prose.len(),
            "system message prose must be compressed when enabled"
        );
        assert_eq!(
            parsed["messages"][2]["content"].as_str().unwrap(),
            prose,
            "assistant turns must pass through verbatim (#710)"
        );
    }

    #[test]
    fn openai_prose_compression_is_deterministic() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        crate::core::config::Config::update_global(|c| {
            c.proxy.role_aggressiveness.system = Some(0.6);
        })
        .unwrap();
        let prose = big_prose();
        let mk = || {
            serde_json::json!({
                "model": "gpt-5",
                "messages": [{"role": "system", "content": prose}, {"role": "user", "content": "hi"}]
            })
        };
        let (a, b) = (mk(), mk());
        let la = serde_json::to_vec(&a).unwrap().len();
        let lb = serde_json::to_vec(&b).unwrap().len();
        assert_eq!(
            compress_request_body(a, la).0,
            compress_request_body(b, lb).0,
            "identical input must yield byte-identical output (#498)"
        );
    }

    #[test]
    fn effort_control_sets_reasoning_effort_and_off_is_noop() {
        // #834 end-to-end through the Chat Completions request path.
        let _iso = crate::core::data_dir::isolated_data_dir();
        crate::test_env::remove_var("LEAN_CTX_PROXY_EFFORT");
        let body = serde_json::json!({
            "model": "gpt-5.5", "messages": [{"role": "user", "content": "hi"}]
        });
        let bytes = serde_json::to_vec(&body).unwrap();

        // Off by default: a cache-safe no-op (size unchanged, no param added).
        let (off, o, c) = compress_request_body(body.clone(), bytes.len());
        assert_eq!(c, o, "effort off must be a passthrough");
        assert!(
            serde_json::from_slice::<Value>(&off)
                .unwrap()
                .get("reasoning_effort")
                .is_none()
        );

        // Enabled: reasoning_effort is filled on the reasoning model.
        crate::core::config::Config::update_global(|cfg| {
            cfg.proxy.effort = Some("low".into());
        })
        .unwrap();
        let (on, _o, _c) = compress_request_body(body, bytes.len());
        assert_eq!(
            serde_json::from_slice::<Value>(&on).unwrap()["reasoning_effort"],
            "low"
        );
    }
}
