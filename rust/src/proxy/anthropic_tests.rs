use super::super::compress::compress_tool_result;
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
    // Config and tee paths are process-global; isolate them so concurrent tests
    // cannot change proxy behavior between the two identical inputs.
    let _data = crate::core::data_dir::isolated_data_dir();
    // #498: the proxy rewrite must be a pure function of the body so the
    // provider prompt-cache prefix stays byte-identical across turns.
    let bytes = serde_json::to_vec(&forge_log_body("Bash")).unwrap();
    let a = compress_request_body(serde_json::from_slice(&bytes).unwrap(), bytes.len()).0;
    let b = compress_request_body(serde_json::from_slice(&bytes).unwrap(), bytes.len()).0;
    assert_eq!(a, b, "identical input must yield byte-identical output");
}

/// A large, highly-compressible foreign log so the live path tees + stubs it.
fn big_log() -> String {
    (0..200)
        .map(|i| format!("[info] processed item {i:04} ok, latency {i}ms, queue normal"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn inband_ccr_emit_echo_splice_round_trip() {
    // Full #493 cycle through the real Anthropic request path: a lossy stub
    // emits an <lc_expand:HASH> marker, the model echoes it, and the proxy
    // splices the verbatim original back inline on the next request.
    let _iso = crate::core::data_dir::isolated_data_dir();
    crate::test_env::remove_var("LEAN_CTX_PROXY_CCR_INBAND");
    crate::core::config::Config::update_global(|c| {
        c.proxy.ccr_inband = Some(true);
    })
    .unwrap();

    // EMIT: live-compress a foreign tool_result → recovery stub with a marker.
    let log = big_log();
    let emit = serde_json::json!({
        "messages": [
            {"role": "assistant", "content": [{"type": "tool_use", "id": "t1", "name": "bash", "input": {}}]},
            {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "t1", "content": log}]}
        ]
    });
    let bytes = serde_json::to_vec(&emit).unwrap();
    let (out, _o, _c) = compress_request_body(emit, bytes.len());
    let emitted: Value = serde_json::from_slice(&out).unwrap();
    let stub = emitted["messages"][1]["content"][0]["content"]
        .as_str()
        .unwrap();
    assert!(
        stub.contains("<lc_expand:"),
        "in-band stub must advertise an echo-able marker: {stub}"
    );
    assert!(
        !stub.contains("/tee/proxy_"),
        "in-band stub must not leak the unreachable local tee path: {stub}"
    );

    // The marker the model would copy into its next turn.
    let start = stub.find("<lc_expand:").unwrap();
    let end = stub[start..].find('>').unwrap() + start + 1;
    let marker = &stub[start..end];

    // ECHO + SPLICE: the model echoes the marker; the proxy splices the
    // verbatim original (recovered from the local tee store) back inline.
    let echo = serde_json::json!({
        "messages": [
            {"role": "user", "content": [{"type": "text", "text": "look again"}]},
            {"role": "assistant", "content": format!("revisiting that output: {marker}")}
        ]
    });
    let bytes = serde_json::to_vec(&echo).unwrap();
    let (out, _o, _c) = compress_request_body(echo, bytes.len());
    let spliced: Value = serde_json::from_slice(&out).unwrap();
    let assistant = spliced["messages"][1]["content"].as_str().unwrap();
    assert!(
        assistant.contains("processed item 0007 ok")
            && assistant.contains("processed item 0199 ok"),
        "the verbatim original must be spliced back in full: {assistant}"
    );
    assert!(
        !assistant.contains("<lc_expand:"),
        "the marker must be consumed by the splice"
    );
}

#[test]
fn inband_marker_less_turn_is_byte_identical_on_or_off() {
    // Cache-safety (#493): enabling in-band must be a strict no-op on a turn
    // with no marker — same bytes on the wire, so the provider cache prefix is
    // never perturbed unless the model actually asked to expand. Uses a body
    // with nothing to prune/compress, isolating the splice from stub emission.
    let _iso = crate::core::data_dir::isolated_data_dir();
    crate::test_env::remove_var("LEAN_CTX_PROXY_CCR_INBAND");
    let body = serde_json::json!({
        "messages": [
            {"role": "user", "content": [{"type": "text", "text": "hello there"}]},
            {"role": "assistant", "content": "hi — how can I help?"}
        ]
    });
    let bytes = serde_json::to_vec(&body).unwrap();

    crate::core::config::Config::update_global(|c| c.proxy.ccr_inband = Some(false)).unwrap();
    let off = compress_request_body(body.clone(), bytes.len()).0;
    crate::core::config::Config::update_global(|c| c.proxy.ccr_inband = Some(true)).unwrap();
    let on = compress_request_body(body, bytes.len()).0;

    assert_eq!(
        off, on,
        "a marker-less request must be byte-identical whether in-band is on or off"
    );
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
    let mk =
        || serde_json::json!({"system": prose, "messages": [{"role": "user", "content": "hi"}]});
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

#[test]
fn json_envelope_tool_result_is_compressed() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    let log = long_git_status();
    let expected = compress_tool_result(&log, Some("Bash"));
    let envelope = serde_json::to_string(&serde_json::json!({
        "content": [{"type": "text", "text": log}],
        "isError": false,
    }))
    .unwrap();
    let body = serde_json::json!({
        "messages": [
            {"role": "assistant", "content": [{
                "type": "tool_use",
                "id": "t1",
                "name": "Bash",
                "input": {}
            }]},
            {"role": "user", "content": [{
                "type": "tool_result",
                "tool_use_id": "t1",
                "content": envelope
            }]}
        ]
    });
    let bytes = serde_json::to_vec(&body).unwrap();
    let (out, orig, comp) = compress_request_body(body, bytes.len());

    assert!(comp < orig, "JSON envelope tool result should shrink");
    let parsed: Value = serde_json::from_slice(&out).unwrap();
    let content = parsed["messages"][1]["content"][0]["content"]
        .as_str()
        .unwrap();
    let envelope: Value = serde_json::from_str(content).unwrap();
    assert_eq!(envelope["content"][0]["text"].as_str().unwrap(), expected);
}

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
    crate::test_env::set_var("LEAN_CTX_PROXY_MODE", "cache");
    crate::test_env::remove_var("LEAN_CTX_PROXY_COLD_PREFIX_REPACK");
    crate::core::config::Config::update_global(|c| {
        c.proxy.role_aggressiveness.system = Some(0.9);
        c.proxy.cold_prefix_repack = Some(true);
    })
    .unwrap();

    // The prefix must clear the cacheable floor: with premium defaults the
    // net-cost gate (#986, on by default) skips repacking a sub-1024-token
    // prefix the provider could never cache. A real cold prefix worth
    // re-seeding is large, so size the system prose accordingly.
    let prose = big_prose().repeat(6);
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
fn cold_prefix_repack_skipped_for_subcacheable_prefix_by_default() {
    // #986 premium default: cache_policy is on, so the net-cost gate skips a
    // cold repack of a prefix below the provider's cacheable minimum —
    // re-seeding it could never produce a cache the provider keeps. Repack is
    // enabled and the prefix is cold, but it is too small (≈345 tokens) to
    // cache, so the system prose must stay protected (unchanged).
    let _iso = crate::core::data_dir::isolated_data_dir();
    crate::test_env::remove_var("LEAN_CTX_PROXY_COLD_PREFIX_REPACK");
    crate::test_env::remove_var("LEAN_CTX_PROXY_CACHE_POLICY");
    crate::core::config::Config::update_global(|c| {
        c.proxy.role_aggressiveness.system = Some(0.9);
        c.proxy.cold_prefix_repack = Some(true);
    })
    .unwrap();

    let prose = big_prose();
    let (messages, body) = cached_prefix_body("cold-repack-subcacheable-session", &prose);
    super::super::cold_prefix::test_seed_last_touch(&messages, 3 * 60 * 60);

    let bytes = serde_json::to_vec(&body).unwrap();
    let (out, _o, _c) = compress_request_body(body, bytes.len());
    let parsed: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(
        parsed["system"].as_str().unwrap(),
        prose,
        "the net-cost gate must skip repacking a sub-cacheable prefix (premium default)"
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

#[test]
fn cache_policy_attribution_is_measurement_only() {
    // #986: enabling cache-economics records miss-attribution telemetry but
    // must never change the bytes on the wire — the same request compressed
    // with the policy off vs on is byte-identical (strictly cache-safe).
    let _iso = crate::core::data_dir::isolated_data_dir();
    crate::test_env::remove_var("LEAN_CTX_PROXY_CACHE_POLICY");
    crate::test_env::remove_var("LEAN_CTX_PROXY_COLD_PREFIX_REPACK");

    let prose = big_prose();
    let (_messages, body) = cached_prefix_body("cache-policy-measurement-session", &prose);
    let bytes = serde_json::to_vec(&body).unwrap();

    crate::core::config::Config::update_global(|c| {
        c.proxy.cache_policy = Some(false);
    })
    .unwrap();
    let (off, _o, _c) = compress_request_body(body.clone(), bytes.len());

    crate::core::config::Config::update_global(|c| {
        c.proxy.cache_policy = Some(true);
    })
    .unwrap();
    let (on, _o, _c) = compress_request_body(body, bytes.len());

    assert_eq!(
        off, on,
        "miss attribution is measurement-only: the wire bytes must not change"
    );
}

#[test]
fn effort_control_dials_adaptive_thinking_only() {
    // #834 end-to-end: fill output_config.effort when the client already
    // asked for adaptive thinking, but never enable thinking otherwise.
    let _iso = crate::core::data_dir::isolated_data_dir();
    crate::test_env::remove_var("LEAN_CTX_PROXY_EFFORT");
    crate::core::config::Config::update_global(|c| {
        c.proxy.effort = Some("medium".into());
    })
    .unwrap();

    let adaptive = serde_json::json!({
        "model": "claude-opus-4-8",
        "thinking": {"type": "adaptive"},
        "messages": [{"role": "user", "content": "hi"}]
    });
    let bytes = serde_json::to_vec(&adaptive).unwrap();
    let (out, _o, _c) = compress_request_body(adaptive, bytes.len());
    assert_eq!(
        serde_json::from_slice::<Value>(&out).unwrap()["output_config"]["effort"],
        "medium"
    );

    // No thinking field → the proxy must not add output_config (no surprise
    // reasoning cost, no 400 risk).
    let plain = serde_json::json!({
        "model": "claude-opus-4-8",
        "messages": [{"role": "user", "content": "hi"}]
    });
    let bytes = serde_json::to_vec(&plain).unwrap();
    let (out, _o, _c) = compress_request_body(plain, bytes.len());
    assert!(
        serde_json::from_slice::<Value>(&out)
            .unwrap()
            .get("output_config")
            .is_none()
    );
}

#[test]
fn verbosity_steer_applies_to_treatment_skips_control() {
    // #895: the holdout control arm must be byte-unchanged (so its output is
    // the measurement baseline); the treatment arm gets the constant steer.
    let _iso = crate::core::data_dir::isolated_data_dir();
    crate::test_env::remove_var("LEAN_CTX_PROXY_VERBOSITY_STEER");
    crate::test_env::remove_var("LEAN_CTX_PROXY_OUTPUT_HOLDOUT");
    crate::test_env::remove_var("LEAN_CTX_PROXY_EFFORT");

    let req = serde_json::json!({
        "model": "claude-opus-4-8",
        "messages": [{"role": "user", "content": "Summarize the design."}]
    });
    let bytes = serde_json::to_vec(&req).unwrap();

    // Steer on, holdout = 0 → everyone Treatment → steered.
    crate::core::config::Config::update_global(|c| {
        c.proxy.verbosity_steer = Some(true);
        c.proxy.output_holdout = Some(0.0);
    })
    .unwrap();
    let (out, _o, _c) = compress_request_body(req.clone(), bytes.len());
    let v: Value = serde_json::from_slice(&out).unwrap();
    assert!(
        v["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains(crate::proxy::verbosity::STEER),
        "treatment arm must receive the verbosity steer"
    );

    // Steer on, holdout = 1.0 → everyone Control → byte-unchanged, no steer.
    crate::core::config::Config::update_global(|c| {
        c.proxy.output_holdout = Some(1.0);
    })
    .unwrap();
    let (out2, _o, _c) = compress_request_body(req, bytes.len());
    let v2: Value = serde_json::from_slice(&out2).unwrap();
    assert!(
        !v2["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains(crate::proxy::verbosity::STEER),
        "control arm must NOT be steered (measurement baseline)"
    );
}

/// A system prompt comfortably over Anthropic's minimum cacheable size, so
/// the #939 breakpoint gate fires.
fn cacheable_system() -> String {
    "You are a careful, senior software engineer who writes maintainable code. ".repeat(400)
}

#[test]
fn cache_breakpoint_injected_on_unanchored_system_when_opt_in() {
    // #939: opt-in on AND the client set no cache_control of its own → exactly
    // one ephemeral breakpoint lands on `system`, wrapping the verbatim text.
    let _iso = crate::core::data_dir::isolated_data_dir();
    crate::test_env::remove_var("LEAN_CTX_PROXY_CACHE_BREAKPOINT");
    let body = serde_json::json!({
        "model": "claude-opus-4-8",
        "system": cacheable_system(),
        "messages": [{"role": "user", "content": "Refactor the parser."}]
    });
    let bytes = serde_json::to_vec(&body).unwrap();

    crate::core::config::Config::update_global(|c| c.proxy.cache_breakpoint = Some(true)).unwrap();
    let (out, _o, _c) = compress_request_body(body, bytes.len());
    let v: Value = serde_json::from_slice(&out).unwrap();

    assert_eq!(
        v["system"][0]["cache_control"]["type"], "ephemeral",
        "an unanchored system prompt must receive one ephemeral breakpoint"
    );
    assert!(
        v["system"][0]["text"]
            .as_str()
            .unwrap()
            .contains("senior software engineer"),
        "the system text must be preserved verbatim under the marker"
    );
}

#[test]
fn cache_breakpoint_off_by_default_is_byte_unchanged() {
    // Default off → the request must be byte-identical (no system reshape),
    // preserving the meter-only / cache-stable contract.
    let _iso = crate::core::data_dir::isolated_data_dir();
    crate::test_env::set_var("LEAN_CTX_PROXY_MODE", "cache");
    crate::test_env::remove_var("LEAN_CTX_PROXY_CACHE_BREAKPOINT");
    let body = serde_json::json!({
        "model": "claude-opus-4-8",
        "system": cacheable_system(),
        "messages": [{"role": "user", "content": "Refactor the parser."}]
    });
    let bytes = serde_json::to_vec(&body).unwrap();
    let (out, _o, _c) = compress_request_body(body, bytes.len());
    assert_eq!(
        out, bytes,
        "default-off must leave the request byte-identical"
    );
}

#[test]
fn cache_breakpoint_respects_client_anchor() {
    // #939 safety: a client cache_control on a message means `system` is part
    // of the already-cached prefix — never add a second, prefix-shifting anchor.
    let _iso = crate::core::data_dir::isolated_data_dir();
    crate::test_env::remove_var("LEAN_CTX_PROXY_CACHE_BREAKPOINT");
    let body = serde_json::json!({
        "model": "claude-opus-4-8",
        "system": cacheable_system(),
        "messages": [{
            "role": "user",
            "content": [{
                "type": "text",
                "text": "hello",
                "cache_control": {"type": "ephemeral"}
            }]
        }]
    });
    let bytes = serde_json::to_vec(&body).unwrap();
    crate::core::config::Config::update_global(|c| c.proxy.cache_breakpoint = Some(true)).unwrap();
    let (out, _o, _c) = compress_request_body(body, bytes.len());
    let v: Value = serde_json::from_slice(&out).unwrap();
    assert!(
        v["system"].is_string(),
        "with a client anchor present, system must be left untouched (no second breakpoint)"
    );
}

#[test]
fn cache_aligner_measures_without_mutating_body() {
    // #940: the volatile-field scan is telemetry-only — enabling it must leave
    // the request byte-identical (measurement, not a rewrite), even on a
    // volatile-field-rich system prompt.
    let _iso = crate::core::data_dir::isolated_data_dir();
    crate::test_env::set_var("LEAN_CTX_PROXY_MODE", "cache");
    crate::test_env::remove_var("LEAN_CTX_PROXY_CACHE_ALIGNER");
    crate::test_env::remove_var("LEAN_CTX_PROXY_CACHE_BREAKPOINT");
    let body = serde_json::json!({
        "model": "claude-opus-4-8",
        "system": "Today is 2026-06-22. Session 550e8400-e29b-41d4-a716-446655440000.",
        "messages": [{"role": "user", "content": "Hello."}]
    });
    let bytes = serde_json::to_vec(&body).unwrap();
    crate::core::config::Config::update_global(|c| c.proxy.cache_aligner = Some(true)).unwrap();
    let (out, _o, _c) = compress_request_body(body, bytes.len());
    assert_eq!(
        out, bytes,
        "cache-aligner telemetry must never mutate the request body"
    );
}

/// A cacheable-size system prompt that carries one volatile field (a date),
/// so the #974 relocate has something to move and clears the size floor.
fn cacheable_system_with_date() -> String {
    format!("Today is 2026-06-27. {}", cacheable_system())
}

fn clear_relocate_env() {
    crate::test_env::remove_var("LEAN_CTX_PROXY_CACHE_ALIGN_RELOCATE");
    crate::test_env::remove_var("LEAN_CTX_PROXY_CACHE_BREAKPOINT");
    crate::test_env::remove_var("LEAN_CTX_PROXY_CACHE_ALIGNER");
    crate::test_env::remove_var("LEAN_CTX_PROXY_OUTPUT_HOLDOUT");
}

#[test]
fn cache_align_relocate_moves_volatiles_to_tail_when_opt_in() {
    // #974: opt-in on, client anchored nothing → the date leaves the cacheable
    // prefix for an uncached tail block, and the stable block carries the one
    // ephemeral breakpoint.
    let _iso = crate::core::data_dir::isolated_data_dir();
    clear_relocate_env();
    crate::test_env::set_var("LEAN_CTX_PROXY_MODE", "cache");
    let body = serde_json::json!({
        "model": "claude-opus-4-8",
        "system": cacheable_system_with_date(),
        "messages": [{"role": "user", "content": "Refactor the parser."}]
    });
    let bytes = serde_json::to_vec(&body).unwrap();
    crate::core::config::Config::update_global(|c| {
        c.proxy.cache_align_relocate = Some(true);
        c.proxy.output_holdout = Some(0.0);
    })
    .unwrap();
    let (out, _o, _c) = compress_request_body(body, bytes.len());
    let v: Value = serde_json::from_slice(&out).unwrap();

    assert!(v["system"].is_array(), "system reshaped into a block array");
    assert_eq!(v["system"][0]["cache_control"]["type"], "ephemeral");
    assert!(
        !v["system"][0]["text"]
            .as_str()
            .unwrap()
            .contains("2026-06-27"),
        "the volatile date must leave the cacheable prefix"
    );
    assert!(
        v["system"][1].get("cache_control").is_none(),
        "the relocated tail block stays uncached"
    );
    assert!(
        v["system"][1]["text"]
            .as_str()
            .unwrap()
            .contains("2026-06-27"),
        "the date must be re-stated in the tail"
    );
}

#[test]
fn cache_align_relocate_off_by_default_is_byte_unchanged() {
    // Default off → byte-identical request, preserving the cache-stable
    // contract even with a volatile-rich system prompt.
    let _iso = crate::core::data_dir::isolated_data_dir();
    crate::test_env::set_var("LEAN_CTX_PROXY_MODE", "cache");
    clear_relocate_env();
    let body = serde_json::json!({
        "model": "claude-opus-4-8",
        "system": cacheable_system_with_date(),
        "messages": [{"role": "user", "content": "Refactor the parser."}]
    });
    let bytes = serde_json::to_vec(&body).unwrap();
    let (out, _o, _c) = compress_request_body(body, bytes.len());
    assert_eq!(
        out, bytes,
        "default-off must leave the request byte-identical"
    );
}

#[test]
fn cache_align_relocate_skips_control_arm() {
    // #895 holdout: a control-arm conversation must be byte-unchanged so its
    // cache behaviour is the measurement baseline.
    let _iso = crate::core::data_dir::isolated_data_dir();
    clear_relocate_env();
    let body = serde_json::json!({
        "model": "claude-opus-4-8",
        "system": cacheable_system_with_date(),
        "messages": [{"role": "user", "content": "Refactor the parser."}]
    });
    let bytes = serde_json::to_vec(&body).unwrap();
    crate::core::config::Config::update_global(|c| {
        c.proxy.cache_align_relocate = Some(true);
        c.proxy.output_holdout = Some(1.0);
    })
    .unwrap();
    let (out, _o, _c) = compress_request_body(body, bytes.len());
    let v: Value = serde_json::from_slice(&out).unwrap();
    assert!(
        v["system"].is_string(),
        "control arm must not be relocated (measurement baseline)"
    );
}

#[test]
fn cache_align_relocate_respects_client_anchor() {
    // Safety: a client cache_control means `system` is part of the cached
    // prefix — never relocate it (that would shift the cached prefix, #448).
    let _iso = crate::core::data_dir::isolated_data_dir();
    clear_relocate_env();
    let body = serde_json::json!({
        "model": "claude-opus-4-8",
        "system": cacheable_system_with_date(),
        "messages": [{
            "role": "user",
            "content": [{
                "type": "text",
                "text": "hello",
                "cache_control": {"type": "ephemeral"}
            }]
        }]
    });
    let bytes = serde_json::to_vec(&body).unwrap();
    crate::core::config::Config::update_global(|c| {
        c.proxy.cache_align_relocate = Some(true);
        c.proxy.output_holdout = Some(0.0);
    })
    .unwrap();
    let (out, _o, _c) = compress_request_body(body, bytes.len());
    let v: Value = serde_json::from_slice(&out).unwrap();
    assert!(
        v["system"].is_string(),
        "with a client anchor present, system must be left untouched"
    );
}

#[test]
fn cache_align_relocate_composes_with_breakpoint_to_one_anchor() {
    // Both opt-ins on: relocate adds the breakpoint to the stable block, so the
    // #939 injection sees an anchored prefix and stays a no-op — exactly one
    // breakpoint, on the stable block, with the volatile tail uncached.
    let _iso = crate::core::data_dir::isolated_data_dir();
    clear_relocate_env();
    crate::test_env::set_var("LEAN_CTX_PROXY_MODE", "cache");
    let body = serde_json::json!({
        "model": "claude-opus-4-8",
        "system": cacheable_system_with_date(),
        "messages": [{"role": "user", "content": "Refactor the parser."}]
    });
    let bytes = serde_json::to_vec(&body).unwrap();
    crate::core::config::Config::update_global(|c| {
        c.proxy.cache_align_relocate = Some(true);
        c.proxy.cache_breakpoint = Some(true);
        c.proxy.output_holdout = Some(0.0);
    })
    .unwrap();
    let (out, _o, _c) = compress_request_body(body, bytes.len());
    let v: Value = serde_json::from_slice(&out).unwrap();
    let blocks = v["system"].as_array().expect("system is a block array");
    assert_eq!(blocks.len(), 2, "stable block + volatile tail");
    assert_eq!(blocks[0]["cache_control"]["type"], "ephemeral");
    assert!(
        blocks[1].get("cache_control").is_none(),
        "no second breakpoint — the tail stays uncached"
    );
}

#[test]
fn copilot_detected_by_editor_version_header() {
    let req = Request::builder()
        .uri("/v1/messages")
        .header("editor-version", "vscode/1.0")
        .header("content-type", "application/json")
        .body(Body::empty())
        .unwrap();
    assert!(super::is_copilot_request(&req, "https://api.openai.com"));
}

#[test]
fn copilot_detected_by_integration_id_header() {
    let req = Request::builder()
        .uri("/v1/messages")
        .header("copilot-integration-id", "vscode-chat")
        .body(Body::empty())
        .unwrap();
    assert!(super::is_copilot_request(&req, "https://api.openai.com"));
}

#[test]
fn copilot_detected_by_ghu_token() {
    let req = Request::builder()
        .uri("/v1/messages")
        .header("authorization", "Bearer ghu_abc123")
        .body(Body::empty())
        .unwrap();
    assert!(super::is_copilot_request(&req, "https://api.openai.com"));
}

#[test]
fn copilot_detected_by_upstream_host() {
    let req = Request::builder()
        .uri("/v1/messages")
        .body(Body::empty())
        .unwrap();
    assert!(super::is_copilot_request(
        &req,
        "https://api.githubcopilot.com"
    ));
}

#[test]
fn not_copilot_when_standard_anthropic() {
    let req = Request::builder()
        .uri("/v1/messages")
        .header("x-api-key", "sk-ant-abc123")
        .header("anthropic-version", "2023-06-01")
        .body(Body::empty())
        .unwrap();
    assert!(!super::is_copilot_request(&req, "https://api.openai.com"));
}

#[test]
fn extract_proxy_ep_from_token() {
    assert_eq!(
        super::extract_proxy_ep("tid=abc;exp=123;proxy-ep=api.business.githubcopilot.com"),
        Some("api.business.githubcopilot.com")
    );
    assert_eq!(super::extract_proxy_ep("ghu_abc123"), None);
    assert_eq!(super::extract_proxy_ep(""), None);
}

#[test]
fn copilot_upstream_uses_proxy_ep() {
    let req = Request::builder()
        .uri("/v1/messages")
        .header(
            "authorization",
            "Bearer tid=x;exp=9;proxy-ep=api.business.githubcopilot.com",
        )
        .body(Body::empty())
        .unwrap();
    let upstream = super::resolve_copilot_upstream(&req, "https://api.githubcopilot.com");
    assert_eq!(upstream, "https://api.business.githubcopilot.com");
}

#[test]
fn copilot_upstream_falls_back_to_openai_upstream() {
    let req = Request::builder()
        .uri("/v1/messages")
        .header("authorization", "Bearer ghu_simple_token")
        .body(Body::empty())
        .unwrap();
    let upstream = super::resolve_copilot_upstream(&req, "https://api.githubcopilot.com");
    assert_eq!(upstream, "https://api.githubcopilot.com");
}

#[test]
fn is_copilot_host_detects_variants() {
    assert!(super::is_copilot_host("https://api.githubcopilot.com"));
    assert!(super::is_copilot_host(
        "https://api.business.githubcopilot.com"
    ));
    assert!(super::is_copilot_host(
        "https://api.individual.githubcopilot.com"
    ));
    assert!(!super::is_copilot_host("https://api.anthropic.com"));
    assert!(!super::is_copilot_host("https://api.openai.com"));
}

/// A warm conversation whose whole message history is client-cached: the
/// opening turn (unique `first_text` → unique conversation key), `turns` prior
/// exchanges, and a closing user turn carrying the cache breakpoint.
fn warm_body(first_text: &str, system: &str, turns: usize, filler: &str) -> Value {
    let mut messages = vec![serde_json::json!({"role": "user", "content": first_text})];
    for i in 0..turns {
        messages.push(serde_json::json!({"role": "assistant", "content": format!("done {i}")}));
        messages.push(serde_json::json!({"role": "user", "content": filler}));
    }
    messages.push(serde_json::json!({"role": "user", "content": [
        {"type": "text", "text": "continue", "cache_control": {"type": "ephemeral"}}
    ]}));
    serde_json::json!({"model": "claude-opus-4-5", "system": system, "messages": messages})
}

fn warm_gate_config(cache_policy: bool) {
    crate::test_env::remove_var("LEAN_CTX_PROXY_CACHE_POLICY");
    crate::test_env::remove_var("LEAN_CTX_PROXY_COLD_PREFIX_REPACK");
    crate::core::config::Config::update_global(|c| {
        c.proxy.role_aggressiveness.system = Some(0.9);
        c.proxy.cold_prefix_repack = Some(false);
        c.proxy.cache_policy = Some(cache_policy);
    })
    .unwrap();
}

fn system_after(body: Value) -> String {
    let bytes = serde_json::to_vec(&body).unwrap();
    let (out, _o, _c) = compress_request_body(body, bytes.len());
    serde_json::from_slice::<Value>(&out).unwrap()["system"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[test]
fn warm_system_gate_preserves_when_the_bust_does_not_pay() {
    // #1912: one prior turn in front of a huge cached tail — rewriting the
    // system block would re-write the whole prefix for a tiny per-turn saving.
    let _iso = crate::core::data_dir::isolated_data_dir();
    warm_gate_config(true);
    let prose = big_prose().repeat(8);
    let huge_tail = "lorem ipsum dolor sit amet consectetur ".repeat(4000);
    let body = warm_body("warm-gate-preserve-session", &prose, 1, &huge_tail);
    assert_eq!(
        system_after(body),
        prose,
        "Preserve must keep the prefix verbatim"
    );
}

#[test]
fn warm_system_gate_mutates_long_sessions_and_latches() {
    // #1912: a long session repays the one-off bust, so the gate compresses the
    // system prompt — and the per-conversation latch keeps every later turn on
    // that same compressed prefix, even a turn the gate alone would preserve.
    let _iso = crate::core::data_dir::isolated_data_dir();
    warm_gate_config(true);
    let prose = big_prose().repeat(8);
    let first = "warm-gate-latch-session";

    let compressed = system_after(warm_body(first, &prose, 200, "next"));
    assert!(
        compressed.len() < prose.len(),
        "a paying rewrite must compress"
    );
    assert_eq!(
        system_after(warm_body(first, &prose, 200, "next")),
        compressed,
        "a replayed turn must produce the identical prefix"
    );

    let huge_tail = "lorem ipsum dolor sit amet consectetur ".repeat(4000);
    assert_eq!(
        system_after(warm_body(first, &prose, 1, &huge_tail)),
        compressed,
        "once latched, the conversation must never flip back to the verbatim prefix"
    );
}

#[test]
fn warm_system_gate_needs_cache_economics() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    warm_gate_config(false);
    let prose = big_prose().repeat(8);
    let body = warm_body("warm-gate-policy-off-session", &prose, 200, "next");
    assert_eq!(
        system_after(body),
        prose,
        "without cache economics a cached prefix is never rewritten (#448)"
    );
}
