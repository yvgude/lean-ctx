// SPDX-License-Identifier: Apache-2.0
//! Final egress control in the proxy (G6): plan cases 41–46 on the request
//! shapes the proxy forwards. Credential fixtures are split with `concat!`
//! so the repository's own secret scanners never see a literal key.

use lean_ctx_protocol::context_gateway::{DeliveryOutcomeV1, DestinationLocalityV1};
use serde_json::{Value, json};

use super::egress::{
    self, EgressBody, EgressTarget, PROXY_RECEIPT_KEY, admit_opaque, admit_request,
};

const AWS_KEY: &str = concat!("AK", "IAIOSFODNN7EXAMPLE");
const VALID_CARD: &str = "4111 1111 1111 1111";

struct Isolated {
    _data: crate::core::data_dir::IsolatedDataDir,
}

fn isolated(config: &str) -> Isolated {
    let data = crate::core::data_dir::isolated_data_dir();
    let dir = crate::core::paths::config_dir_read_only().expect("isolated config dir");
    std::fs::create_dir_all(&dir).expect("config dir");
    std::fs::write(dir.join("config.toml"), config).expect("config");
    Isolated { _data: data }
}

const REMOTE: EgressTarget<'static> = EgressTarget {
    provider: "Anthropic",
    model: Some("claude-sonnet-5-5"),
    upstream_base: "https://api.anthropic.com",
};

const LOCAL: EgressTarget<'static> = EgressTarget {
    provider: "OpenAI",
    model: Some("llama"),
    upstream_base: "http://127.0.0.1:11434",
};

fn rewritten(body: EgressBody) -> Value {
    match body {
        EgressBody::Rewritten(doc) => doc,
        other => panic!("expected a rewritten body, got {other:?}"),
    }
}

fn assert_clean(doc: &Value) {
    let text = doc.to_string();
    assert!(
        !text.contains(AWS_KEY),
        "raw credential leaves the machine: {text}"
    );
    assert!(
        !text.contains(VALID_CARD),
        "raw card number leaves the machine: {text}"
    );
}

fn anthropic_request() -> Value {
    json!({
        "model": "claude-sonnet-5-5",
        "system": format!("Deploy with key {AWS_KEY}."),
        "messages": [
            {"role": "user", "content": [
                {"type": "text", "text": format!("Charge card {VALID_CARD}.")},
                {"type": "tool_result", "tool_use_id": "toolu_01", "content": format!("aws_key = {AWS_KEY}")}
            ]},
            {"role": "assistant", "content": [
                {"type": "tool_use", "id": "toolu_02", "name": "bash",
                 "input": {"command": format!("export KEY={AWS_KEY}")}}
            ]}
        ],
        "max_tokens": 1024
    })
}

// ─── Case 41: content is admitted on every provider shape ───────────────────

#[test]
#[serial_test::serial(cache_telemetry)]
fn case_41_every_request_shape_leaves_masked() {
    let _env = isolated("");
    let anthropic = rewritten(admit_request(&anthropic_request(), &REMOTE).body);
    assert_clean(&anthropic);
    assert_eq!(
        anthropic["model"], "claude-sonnet-5-5",
        "structure is untouched"
    );
    assert_eq!(
        anthropic["messages"][0]["content"][1]["tool_use_id"],
        "toolu_01"
    );
    assert_eq!(anthropic["messages"][1]["content"][0]["name"], "bash");
    assert_eq!(anthropic["max_tokens"], 1024);

    let openai = json!({
        "model": "gpt-5",
        "messages": [
            {"role": "user", "content": format!("key {AWS_KEY}")},
            {"role": "assistant", "tool_calls": [{"id": "call_1", "type": "function",
              "function": {"name": "run", "arguments": format!("{{\"card\":\"{VALID_CARD}\"}}")}}]},
            {"role": "tool", "tool_call_id": "call_1", "content": [{"type": "text", "text": format!("ok {AWS_KEY}")}]}
        ]
    });
    assert_clean(&rewritten(admit_request(&openai, &REMOTE).body));

    let responses = json!({
        "model": "gpt-5",
        "instructions": format!("never print {AWS_KEY}"),
        "input": [{"type": "message", "role": "user", "content": [{"type": "input_text", "text": format!("card {VALID_CARD}")}]}]
    });
    assert_clean(&rewritten(admit_request(&responses, &REMOTE).body));

    let gemini = json!({
        "systemInstruction": {"parts": [{"text": format!("key {AWS_KEY}")}]},
        "contents": [{"role": "user", "parts": [{"text": format!("card {VALID_CARD}")}]}]
    });
    assert_clean(&rewritten(admit_request(&gemini, &REMOTE).body));
}

#[test]
#[serial_test::serial(cache_telemetry)]
fn case_41_clean_requests_keep_their_bytes_and_masks_are_deterministic() {
    let _env = isolated("");
    let clean = json!({"model": "claude", "messages": [{"role": "user", "content": "Refactor the parser."}]});
    assert!(
        matches!(admit_request(&clean, &REMOTE).body, EgressBody::Unchanged),
        "nothing to change keeps the exact bytes (#1912)"
    );
    // Prompt caches key on prefix bytes: the same request is rewritten to
    // the same bytes on every turn.
    let first = serde_json::to_vec(&rewritten(
        admit_request(&anthropic_request(), &REMOTE).body,
    ));
    let second = serde_json::to_vec(&rewritten(
        admit_request(&anthropic_request(), &REMOTE).body,
    ));
    assert_eq!(first.unwrap(), second.unwrap());
}

// ─── Case 42/44: sealed and opaque content is never claimed inspected ───────

fn thinking_request() -> Value {
    json!({
        "model": "claude",
        "messages": [{"role": "assistant", "content": [
            {"type": "thinking", "thinking": format!("the key is {AWS_KEY}"), "signature": "EqQBCkYIBxgCKkA"}
        ]}]
    })
}

#[test]
#[serial_test::serial(cache_telemetry)]
fn case_42_sealed_content_is_never_rewritten() {
    let _env = isolated("");
    let outcome = admit_request(&thinking_request(), &REMOTE);
    assert!(
        matches!(outcome.body, EgressBody::Unchanged),
        "a signed thinking block is delivered unchanged in developer mode"
    );
    let receipt = egress::finish(&outcome, Some(b"{}"), 100, None).expect("receipt");
    assert!(
        receipt.decisions.iter().any(|d| d
            .reason_codes
            .iter()
            .any(|r| r.as_str() == "egress.sealed_content")),
        "the receipt says the sealed finding was not masked"
    );

    drop(_env);
    let _env = isolated("[context_gateway]\nmode = \"governed\"\n");
    assert!(
        matches!(
            admit_request(&thinking_request(), &REMOTE).body,
            EgressBody::Refused(_)
        ),
        "governed mode refuses what it cannot mask"
    );
}

#[test]
#[serial_test::serial(cache_telemetry)]
fn case_44_media_and_opaque_bodies_are_marked_never_claimed_clean() {
    let _env = isolated("");
    let image = json!({"model": "claude", "messages": [{"role": "user", "content": [
        {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "A".repeat(4096)}}
    ]}]});
    let outcome = admit_request(&image, &REMOTE);
    assert!(matches!(outcome.body, EgressBody::Unchanged));
    let receipt = egress::finish(&outcome, Some(b"{}"), 4096, None).expect("receipt");
    assert!(
        receipt.security.incomplete_coverage > 0
            || receipt.decisions.iter().any(|d| {
                d.reason_codes
                    .iter()
                    .any(|r| r.as_str() == "coverage.unsupported_media")
            })
    );
    let opaque = admit_opaque(512, &REMOTE);
    assert!(
        matches!(opaque.body, EgressBody::Unchanged),
        "developer mode forwards, marked"
    );

    drop(_env);
    let _env = isolated("[context_gateway]\nmode = \"governed\"\n");
    assert!(matches!(
        admit_request(&image, &REMOTE).body,
        EgressBody::Refused(_)
    ));
    assert!(matches!(
        admit_opaque(512, &REMOTE).body,
        EgressBody::Refused(_)
    ));
}

// ─── Case 43/46: the destination is part of the authorization ───────────────

#[test]
#[serial_test::serial(cache_telemetry)]
fn case_43_restricted_content_never_goes_to_a_remote_model() {
    let _env = isolated("");
    // A banner marking classifies the whole object `restricted`; a mere
    // prose mention of the words would not.
    let marked = json!({"model": "m", "messages": [
        {"role": "user", "content": "*** TOP SECRET ***\nlaunch window and codes\n"}
    ]});
    let remote = rewritten(admit_request(&marked, &REMOTE).body);
    let text = remote["messages"][0]["content"].as_str().unwrap();
    assert!(
        text.starts_with("[lean-ctx gateway: content withheld"),
        "{text}"
    );
    assert!(text.contains("destination.remote_restricted"), "{text}");
    assert!(
        matches!(admit_request(&marked, &LOCAL).body, EgressBody::Unchanged),
        "a local model may receive it"
    );
}

#[test]
fn case_46_locality_is_proven_never_assumed() {
    for (base, expected) in [
        ("http://localhost:11434", DestinationLocalityV1::Local),
        ("http://127.0.0.1:8080/v1", DestinationLocalityV1::Local),
        ("http://[::1]:9000", DestinationLocalityV1::Local),
        ("http://ollama.localhost", DestinationLocalityV1::Local),
        ("https://api.openai.com", DestinationLocalityV1::Remote),
        ("http://10.0.0.5:8000", DestinationLocalityV1::Remote),
        ("not a url", DestinationLocalityV1::Unknown),
    ] {
        let target = EgressTarget {
            provider: "OpenAI",
            model: None,
            upstream_base: base,
        };
        assert_eq!(egress::destination(&target).locality, expected, "{base}");
    }
}

// ─── Case 45: one receipt per request binds what left ───────────────────────

#[test]
#[serial_test::serial(cache_telemetry)]
fn case_45_receipt_binds_destination_policy_and_forwarded_bytes() {
    let _env = isolated("");
    let outcome = admit_request(&anthropic_request(), &REMOTE);
    let forwarded = serde_json::to_vec(&rewritten(
        admit_request(&anthropic_request(), &REMOTE).body,
    ))
    .unwrap();
    let receipt =
        egress::finish(&outcome, Some(&forwarded), 4000, Some("codex-1")).expect("receipt");
    assert_eq!(receipt.destination.provider.as_str(), "anthropic");
    assert_eq!(receipt.destination.locality, DestinationLocalityV1::Remote);
    assert_eq!(receipt.outcome, DeliveryOutcomeV1::Delivered);
    assert!(receipt.policy.is_some(), "the deciding policy is named");
    assert!(receipt.security.redactions >= 3, "{:?}", receipt.security);
    let digest = receipt.final_context.as_ref().expect("final digest");
    use sha2::{Digest, Sha256};
    let expected = crate::core::agent_identity::hex_encode(&Sha256::digest(&forwarded));
    assert_eq!(digest.as_str(), format!("sha256:{expected}"));

    let stored = super::receipt_store::latest(PROXY_RECEIPT_KEY, 1);
    assert_eq!(stored.len(), 1, "persisted under the proxy key");
    assert!(stored[0].1.is_ok(), "and verifies against its digest");

    // A refused request names no delivered context.
    let refused = egress::finish(&outcome, None, 4000, None).expect("receipt");
    assert!(refused.final_context.is_none());
}

/// What the final egress check adds to one proxied turn: a long coding
/// conversation (this crate's sources as tool results), first turn (memo
/// cold) and every later turn (prefix already admitted). Run in release:
/// `cargo test --release --lib egress_turn_cost -- --ignored --nocapture`.
#[test]
#[ignore = "measurement, not a correctness test"]
fn egress_turn_cost() {
    use std::time::{Duration, Instant};
    let _env = isolated("");
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let sources: Vec<String> = walkdir::WalkDir::new(&root)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "rs"))
        .take(120)
        .filter_map(|e| std::fs::read_to_string(e.path()).ok())
        .collect();
    let messages: Vec<Value> = sources
        .iter()
        .enumerate()
        .map(|(i, source)| {
            json!({"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": format!("toolu_{i}"), "content": source}
            ]})
        })
        .collect();
    let request = json!({"model": "claude-sonnet-5-5", "messages": messages});
    let bytes = request.to_string().len();
    let time = |cold: bool| -> Duration {
        let mut runs: Vec<Duration> = (0..5)
            .map(|_| {
                if cold {
                    super::clear_clean_memo();
                }
                let started = Instant::now();
                let _ = admit_request(&request, &REMOTE);
                started.elapsed()
            })
            .collect();
        runs.sort();
        runs[2]
    };
    let cold = time(true);
    let warm = time(false);
    println!(
        "{} messages, {:.1} MB request · first turn {cold:?} · later turns {warm:?}",
        messages.len(),
        bytes as f64 / 1e6
    );
}
