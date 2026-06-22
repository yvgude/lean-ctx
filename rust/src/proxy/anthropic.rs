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
    let upstream = state.anthropic_upstream();
    forward::forward_request(
        State(state),
        req,
        &upstream,
        "/v1/messages",
        compress_request_body,
        "Anthropic",
        &[],
    )
    .await
}

fn compress_request_body(parsed: Value, original_size: usize) -> (Vec<u8>, usize, usize) {
    let mut doc = parsed;
    let mut modified = false;

    // Opt-in per-role prose aggressiveness (#710). Both default to `None`, in
    // which case nothing below fires and the body is byte-for-byte unchanged.
    let cfg = crate::core::config::Config::load();
    let system_aggr = cfg.proxy.resolved_role_aggressiveness(ProseRole::System);
    let user_aggr = cfg.proxy.resolved_role_aggressiveness(ProseRole::User);
    let live_compress = cfg.proxy.live_compresses();
    let mode = cfg.proxy.resolved_history_mode();
    // Meter-only (#481): live compression off, no history pruning, no prose
    // rewriting → forward + usage metering still run, but the body is left
    // unchanged so the provider prompt-cache prefix stays byte-stable.
    if !live_compress && mode == HistoryMode::Off && system_aggr.is_none() && user_aggr.is_none() {
        let out = serde_json::to_vec(&doc).unwrap_or_default();
        return (out, original_size, original_size);
    }
    let mut prose_segments: u64 = 0;

    // Length of the client's provider-cached message prefix. Needed both for
    // cache-safe pruning below and to gate top-level system prose: if any
    // message is client-cached, `system` (which precedes every message) is part
    // of that cached prefix and must not be rewritten.
    let cached = doc
        .get("messages")
        .and_then(|m| m.as_array())
        .map_or(0, |m| super::history_prune::cached_prefix_len(m));

    // #480: opt-in big-gap cold-prefix repack. When enabled AND the proxy can
    // confidently predict (from idle time vs the provider cache TTL) that the
    // client-cached prefix is already cold, override the normal "never touch the
    // cached prefix" rule for THIS request and prune/compress the prefix too,
    // re-seeding a leaner cache. Default-off; never fires without a measured idle
    // gap past TTL × margin, so warm caches stay byte-stable (#448).
    let repack = cfg.proxy.repacks_cold_prefix()
        && doc
            .get("messages")
            .and_then(|m| m.as_array())
            .is_some_and(|m| super::cold_prefix::repack_decision(m, cached));
    // The prefix length the rewrites below must protect: the full cached prefix
    // normally, or 0 when we are intentionally repacking the cold prefix.
    let protect = if repack { 0 } else { cached };

    // System prose: only when nothing is client-cached and the `system` field
    // carries no `cache_control` of its own — otherwise it anchors the cache.
    // A cold-prefix repack (`protect == 0` with `repack`) deliberately rewrites
    // it to re-seed a leaner cache.
    if let Some(a) = system_aggr
        && protect == 0
        && let Some(system) = doc.get_mut("system")
        && (repack || !prose::value_has_cache_control(system))
    {
        let n = prose::compress_system_value(system, a);
        if n > 0 {
            prose_segments += u64::from(n);
            modified = true;
        }
    }

    if let Some(messages) = doc.get_mut("messages").and_then(|m| m.as_array_mut()) {
        // Resolve tool-call id → tool name so file/source reads can be protected
        // from lossy compression that would force the model to re-read mid-task.
        let tool_names = tool_kind::anthropic_tool_names(messages);

        // Prune at a frozen, cache-aware boundary by default: Anthropic's
        // prompt cache matches exact prefixes, so the boundary must not move
        // every turn (see `history_prune::prune_boundary`). `mode` resolved above.
        let boundary = super::history_prune::prune_boundary(mode, messages.len());
        // Never rewrite content the client has marked with `cache_control`:
        // pruning inside the already-cached prefix invalidates Anthropic's
        // prompt cache from the first changed message (#448). Pruning therefore
        // starts after the last breakpoint; with no breakpoint this is 0, i.e.
        // the previous behaviour.
        modified |=
            super::history_prune::prune_history_range(messages, protect, boundary, &tool_names);

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

                    let name = block
                        .get("tool_use_id")
                        .and_then(|v| v.as_str())
                        .and_then(|id| tool_names.get(id))
                        .map(String::as_str);
                    let kind = name.map_or(ToolResultKind::Other, tool_kind::classify_tool_name);

                    // #481: skip live compression when globally off or when the
                    // originating tool is on the exclusion list (Serena default).
                    let excluded =
                        name.is_some_and(|n| cfg.proxy.is_tool_live_compress_excluded(n));
                    if live_compress
                        && !excluded
                        && let Some(inner_content) = block.get_mut("content")
                    {
                        modified |= compress_content_field(inner_content, name, kind);
                    }
                }
            }
        }

        // Frozen-region user prose: free-text `text` blocks of user turns in
        // `[cached, boundary)`. Cache-safe by construction — the cached prefix
        // and the live tail (`>= boundary`) are both left intact, and the
        // rewrite is content-deterministic so the prefix stays byte-stable.
        if let Some(a) = user_aggr {
            let end = boundary.min(messages.len());
            let start = protect.min(end);
            for msg in &mut messages[start..end] {
                if msg.get("role").and_then(|r| r.as_str()) == Some("user")
                    && let Some(content) = msg.get_mut("content").and_then(|c| c.as_array_mut())
                {
                    prose_segments += u64::from(prose::compress_text_blocks(content, a));
                }
            }
        }
    }

    if prose_segments > 0 {
        modified = true;
    }
    // A deliberate cold-prefix repack (#480) is the one sanctioned exception to
    // the frozen-window rule; count it on its own gauge so it never dilutes the
    // cache-safe ratio (which exists to catch *accidental* #448 regressions).
    // Every other rewrite lands strictly inside the cache-safe frozen window.
    if repack {
        cache_safety::record_cold_repack();
    }
    cache_safety::record(prose_segments, true);

    let out = serde_json::to_vec(&doc).unwrap_or_default();
    let compressed_size = if modified { out.len() } else { original_size };
    (out, original_size, compressed_size)
}

/// Compresses a tool_result `content` field unless it is a protected file/source
/// read, which must reach the model intact (it is what gets edited).
fn compress_content_field(
    content: &mut Value,
    tool_name: Option<&str>,
    kind: ToolResultKind,
) -> bool {
    match content {
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
        Value::Array(arr) => {
            let mut modified = false;
            for item in arr.iter_mut() {
                if item.get("type").and_then(|t| t.as_str()) == Some("text")
                    && let Some(text) = item
                        .get_mut("text")
                        .and_then(|t| t.as_str().map(String::from))
                {
                    if should_protect(kind, &text) {
                        continue;
                    }
                    let compressed = compress_tool_result(&text, tool_name);
                    if compressed.len() < text.len() {
                        item["text"] = Value::String(compressed);
                        modified = true;
                    }
                }
            }
            modified
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source_file_body() -> Vec<u8> {
        let code = (0..60)
            .map(|i| format!("    let binding_{i} = compute_value_{i}(context, options);"))
            .collect::<Vec<_>>()
            .join("\n");
        let body = serde_json::json!({
            "model": "claude-opus-4-8",
            "messages": [
                {
                    "role": "assistant",
                    "content": [{"type": "tool_use", "id": "toolu_1", "name": "Read", "input": {"file_path": "src/app.rs"}}]
                },
                {
                    "role": "user",
                    "content": [{"type": "tool_result", "tool_use_id": "toolu_1", "content": code}]
                }
            ]
        });
        serde_json::to_vec(&body).unwrap()
    }

    #[test]
    fn read_tool_result_is_never_truncated() {
        let bytes = source_file_body();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        let (out, _orig, _comp) = compress_request_body(body, bytes.len());
        let parsed: Value = serde_json::from_slice(&out).unwrap();
        let content = parsed["messages"][1]["content"][0]["content"]
            .as_str()
            .unwrap();
        assert!(
            content.contains("binding_59"),
            "the full source body must survive — refactors need it intact"
        );
        assert!(!content.contains("lines omitted"));
    }

    fn forge_log_body(tool_name: &str) -> Value {
        // Generic, highly-repetitive log with no `$ cmd` hint, so routing falls
        // back to the tool name (exercising the foreign-tool classification)
        // and the generic compressor (not a command-specific pattern).
        let mut log = String::new();
        for i in 0..90 {
            log.push_str(&format!(
                "INFO  processing item {i}: ok, latency={i}ms, queue depth normal, retries 0\n"
            ));
        }
        serde_json::json!({
            "messages": [
                {"role": "assistant", "content": [{"type": "tool_use", "id": "f1", "name": tool_name, "input": {}}]},
                {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "f1", "content": log}]}
            ]
        })
    }

    #[test]
    fn forge_shell_tool_result_compresses() {
        // A vendor-prefixed foreign shell tool reaches the proxy; its log output
        // must still be compressed (rtk/ctx_* never see another server's tools).
        let body = forge_log_body("forge_shell");
        let bytes = serde_json::to_vec(&body).unwrap();
        let (_out, orig, comp) = compress_request_body(body, bytes.len());
        assert!(comp < orig, "foreign shell output must be compressed");
    }

    #[test]
    fn foreign_read_tool_protects_source() {
        // `forge_read` is classified FileRead via the segment fallback, so the
        // source body must reach the model intact (it is what gets edited).
        let code = (0..60)
            .map(|i| format!("    let binding_{i} = compute_value_{i}(context, options);"))
            .collect::<Vec<_>>()
            .join("\n");
        let body = serde_json::json!({
            "messages": [
                {"role": "assistant", "content": [{"type": "tool_use", "id": "r1", "name": "forge_read", "input": {"path": "src/app.rs"}}]},
                {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "r1", "content": code}]}
            ]
        });
        let bytes = serde_json::to_vec(&body).unwrap();
        let (out, _orig, _comp) = compress_request_body(body, bytes.len());
        let parsed: Value = serde_json::from_slice(&out).unwrap();
        let content = parsed["messages"][1]["content"][0]["content"]
            .as_str()
            .unwrap();
        assert!(
            content.contains("binding_59"),
            "source body must survive intact"
        );
    }

    #[test]
    fn compress_request_body_is_deterministic() {
        // tee path depends on the data dir; serialize env access so a parallel
        // test never swaps LEAN_CTX_DATA_DIR between the two compressions.
        let _lock = crate::core::data_dir::test_env_lock();
        // #498: the proxy rewrite must be a pure function of the body so the
        // provider prompt-cache prefix stays byte-identical across turns.
        let bytes = serde_json::to_vec(&forge_log_body("Bash")).unwrap();
        let a = compress_request_body(serde_json::from_slice(&bytes).unwrap(), bytes.len()).0;
        let b = compress_request_body(serde_json::from_slice(&bytes).unwrap(), bytes.len()).0;
        assert_eq!(a, b, "identical input must yield byte-identical output");
    }

    /// Long, duplicate-rich natural-language prose that compresses cleanly.
    fn big_prose() -> String {
        let p = "You are a careful, senior software engineer. You always explain your \
                 reasoning before making changes, you prefer small reviewable diffs, and \
                 you never introduce mock data or placeholders into production code. ";
        [p; 6].join("\n")
    }

    #[test]
    fn system_prose_compressed_and_assistant_untouched() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        crate::core::config::Config::update_global(|c| {
            c.proxy.role_aggressiveness.system = Some(0.6);
            c.proxy.role_aggressiveness.user = Some(0.6);
        })
        .unwrap();

        let prose = big_prose();
        let assistant_text = big_prose();
        let body = serde_json::json!({
            "model": "claude-opus-4-8",
            "system": prose,
            "messages": [
                {"role": "user", "content": [{"type": "text", "text": prose}]},
                {"role": "assistant", "content": assistant_text},
            ]
        });
        let bytes = serde_json::to_vec(&body).unwrap();
        let (out, _orig, _comp) = compress_request_body(body, bytes.len());
        let parsed: Value = serde_json::from_slice(&out).unwrap();

        assert!(
            parsed["system"].as_str().unwrap().len() < prose.len(),
            "system prose must be compressed when enabled"
        );
        assert_eq!(
            parsed["messages"][1]["content"].as_str().unwrap(),
            assistant_text,
            "assistant turns must pass through verbatim (#710)"
        );
    }

    #[test]
    fn user_prose_compressed_only_in_frozen_region() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        crate::core::config::Config::update_global(|c| {
            c.proxy.role_aggressiveness.user = Some(0.7);
        })
        .unwrap();

        let prose = big_prose();
        // 30 messages → cache-aware boundary = ((30-8)/16)*16 = 16.
        let mut messages = Vec::new();
        for i in 0..30 {
            let role = if i % 2 == 0 { "user" } else { "assistant" };
            messages.push(serde_json::json!({
                "role": role,
                "content": [{"type": "text", "text": prose}]
            }));
        }
        let body = serde_json::json!({ "messages": messages });
        let bytes = serde_json::to_vec(&body).unwrap();
        let (out, _o, _c) = compress_request_body(body, bytes.len());
        let parsed: Value = serde_json::from_slice(&out).unwrap();

        let frozen_user = parsed["messages"][0]["content"][0]["text"]
            .as_str()
            .unwrap();
        assert!(
            frozen_user.len() < prose.len(),
            "user prose in the frozen region must be compressed"
        );
        assert_eq!(
            parsed["messages"][1]["content"][0]["text"]
                .as_str()
                .unwrap(),
            prose,
            "assistant prose is never compressed"
        );
        let live_tail_user = parsed["messages"][28]["content"][0]["text"]
            .as_str()
            .unwrap();
        assert_eq!(
            live_tail_user, prose,
            "user prose in the live tail (>= boundary) must be preserved for quality"
        );
    }

    #[test]
    fn client_cached_prefix_disables_system_prose() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        crate::core::config::Config::update_global(|c| {
            c.proxy.role_aggressiveness.system = Some(0.9);
        })
        .unwrap();

        let prose = big_prose();
        let body = serde_json::json!({
            "system": prose,
            "messages": [
                {"role": "user", "content": [
                    {"type": "text", "text": "hi", "cache_control": {"type": "ephemeral"}}
                ]},
                {"role": "assistant", "content": "ok"}
            ]
        });
        let bytes = serde_json::to_vec(&body).unwrap();
        let (out, _o, _c) = compress_request_body(body, bytes.len());
        let parsed: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(
            parsed["system"].as_str().unwrap(),
            prose,
            "system must stay verbatim when the client caches a message prefix (#448)"
        );
    }

    #[test]
    fn prose_compression_is_deterministic() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        crate::core::config::Config::update_global(|c| {
            c.proxy.role_aggressiveness.system = Some(0.6);
        })
        .unwrap();
        let prose = big_prose();
        let mk = || serde_json::json!({"system": prose, "messages": [{"role": "user", "content": "hi"}]});
        let (a, b) = (mk(), mk());
        let la = serde_json::to_vec(&a).unwrap().len();
        let lb = serde_json::to_vec(&b).unwrap().len();
        assert_eq!(
            compress_request_body(a, la).0,
            compress_request_body(b, lb).0,
            "prose compression must be byte-identical for identical input (#498)"
        );
    }

    #[test]
    fn bash_tool_result_still_compresses() {
        let log = {
            let mut s = String::from(
                "$ git status\nOn branch main\nYour branch is up to date with 'origin/main'.\n\nChanges not staged for commit:\n  (use \"git add <file>...\" to update what will be committed)\n",
            );
            for i in 0..90 {
                s.push_str(&format!("\tmodified:   src/module_{i}/file_{i}.rs\n"));
            }
            s.push_str("\nno changes added to commit (use \"git add\" and/or \"git commit -a\")\n");
            s
        };
        let body = serde_json::json!({
            "messages": [
                {"role": "assistant", "content": [{"type": "tool_use", "id": "t1", "name": "Bash", "input": {}}]},
                {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "t1", "content": log}]}
            ]
        });
        let bytes = serde_json::to_vec(&body).unwrap();
        let (_out, orig, comp) = compress_request_body(body, bytes.len());
        assert!(comp < orig, "shell output must still be compressed");
    }

    /// A client-cached message anchors the prefix; `system` precedes it, so the
    /// cached prefix is `cached > 0` and system prose is normally protected.
    /// System-prose verbatim-vs-rewritten is therefore a clean binary signal for
    /// whether the #480 cold-prefix repack fired.
    ///
    /// `first_text` must be UNIQUE per test: it is `messages[0]`, which the
    /// cold-prefix tracker hashes into the conversation key. A shared global
    /// last-touch store has no test-clear hook (that would race with the unit
    /// tests), so distinct keys are how parallel tests stay isolated.
    fn cached_prefix_body(first_text: &str, prose: &str) -> (Vec<Value>, Value) {
        let messages = vec![
            serde_json::json!({"role": "user", "content": [
                {"type": "text", "text": first_text, "cache_control": {"type": "ephemeral"}}
            ]}),
            serde_json::json!({"role": "assistant", "content": "ok"}),
        ];
        let body = serde_json::json!({ "system": prose, "messages": messages.clone() });
        (messages, body)
    }

    #[test]
    fn cold_prefix_repack_rewrites_protected_system_prose_when_enabled() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        crate::test_env::remove_var("LEAN_CTX_PROXY_COLD_PREFIX_REPACK");
        crate::core::config::Config::update_global(|c| {
            c.proxy.role_aggressiveness.system = Some(0.9);
            c.proxy.cold_prefix_repack = Some(true);
        })
        .unwrap();

        let prose = big_prose();
        let (messages, body) = cached_prefix_body("cold-repack-enabled-session", &prose);
        // Predict cold: last touched 3h ago, well past the 5m default TTL × margin.
        super::super::cold_prefix::test_seed_last_touch(&messages, 3 * 60 * 60);

        let bytes = serde_json::to_vec(&body).unwrap();
        let (out, _o, _c) = compress_request_body(body, bytes.len());
        let parsed: Value = serde_json::from_slice(&out).unwrap();
        assert!(
            parsed["system"].as_str().unwrap().len() < prose.len(),
            "a predicted-cold prefix must let the proxy repack the otherwise-protected system prose"
        );
    }

    #[test]
    fn cold_prefix_repack_off_by_default_keeps_prefix_protected() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        crate::test_env::remove_var("LEAN_CTX_PROXY_COLD_PREFIX_REPACK");
        crate::core::config::Config::update_global(|c| {
            c.proxy.role_aggressiveness.system = Some(0.9);
            c.proxy.cold_prefix_repack = Some(false);
        })
        .unwrap();

        let prose = big_prose();
        let (messages, body) = cached_prefix_body("cold-repack-disabled-session", &prose);
        // Even with a huge idle gap, default-off must never touch the prefix.
        super::super::cold_prefix::test_seed_last_touch(&messages, 24 * 60 * 60);

        let bytes = serde_json::to_vec(&body).unwrap();
        let (out, _o, _c) = compress_request_body(body, bytes.len());
        let parsed: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(
            parsed["system"].as_str().unwrap(),
            prose,
            "with repack off the cached prefix stays byte-stable regardless of idle time (#448)"
        );
    }

    #[test]
    fn cold_prefix_repack_protects_warm_prefix_even_when_enabled() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        crate::test_env::remove_var("LEAN_CTX_PROXY_COLD_PREFIX_REPACK");
        crate::core::config::Config::update_global(|c| {
            c.proxy.role_aggressiveness.system = Some(0.9);
            c.proxy.cold_prefix_repack = Some(true);
        })
        .unwrap();

        let prose = big_prose();
        let (messages, body) = cached_prefix_body("cold-repack-warm-session", &prose);
        // Warm: touched 1 minute ago → the prediction must keep protecting.
        super::super::cold_prefix::test_seed_last_touch(&messages, 60);

        let bytes = serde_json::to_vec(&body).unwrap();
        let (out, _o, _c) = compress_request_body(body, bytes.len());
        let parsed: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(
            parsed["system"].as_str().unwrap(),
            prose,
            "a warm prefix must stay protected even with repack enabled — only LARGE gaps trigger"
        );
    }
}
