// SPDX-License-Identifier: Apache-2.0
//! Final egress control in the proxy (G6): plan cases 41–46 on the request
//! shapes the proxy forwards. Credential fixtures are split with `concat!`
//! so the repository's own secret scanners never see a literal key.

use lean_ctx_protocol::context_gateway::{
    ClassificationV1, DeliveryOutcomeV1, DestinationLocalityV1, EgressAdmissionOutcomeV1,
};
use serde_json::{Value, json};
use std::path::Path;

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

fn write_project_policy(root: &Path, extra: &str) {
    let directory = root.join(".lean-ctx");
    std::fs::create_dir_all(&directory).expect("project policy directory");
    std::fs::write(
        directory.join("policy.toml"),
        format!("name = \"egress-test\"\nversion = \"1.0.0\"\ndescription = \"test\"\n{extra}"),
    )
    .expect("project policy");
}

fn admit_in_project(doc: &Value, target: &EgressTarget<'_>, root: &Path) -> egress::EgressOutcome {
    crate::core::policy::runtime::with_explicit_project_source_view(root, |authority| {
        let fingerprint = authority.fingerprint_bytes();
        egress::admit_request_for_project(doc, target, &fingerprint)
    })
    .expect("project policy authority remained current")
}

fn simple_text_request(text: &str) -> Value {
    json!({"model":"m","messages":[{"role":"user","content":text}]})
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
    let receipt = egress::finish(&outcome, Some(b"{}"), 100, None)
        .expect("receipt storage")
        .expect("receipt");
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
    let receipt = egress::finish(&outcome, Some(b"{}"), 4096, None)
        .expect("receipt storage")
        .expect("receipt");
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
    match admit_request(&marked, &REMOTE).body {
        EgressBody::Refused(reason) => {
            assert!(reason.contains("destination.remote_restricted"), "{reason}");
            assert!(!reason.contains("launch window"), "{reason}");
        }
        other => panic!("restricted remote content must refuse the whole request: {other:?}"),
    }
    assert!(
        matches!(admit_request(&marked, &LOCAL).body, EgressBody::Unchanged),
        "a local model may receive it"
    );
}

/// The 25./26.10. lockouts: one restricted block in a conversation's history
/// refused every later request of that session. The block is now withheld in
/// place, the rest of the request goes, and every re-send of the history is
/// decided the same way — the same notice, the same id.
#[test]
#[serial_test::serial(cache_telemetry)]
fn a_restricted_block_in_the_history_is_withheld_in_place_and_the_session_continues() {
    let _env = isolated("");
    let turn = |extra: &[Value]| {
        let mut messages = vec![
            json!({"role": "user", "content": "*** TOP SECRET ***\nlaunch window and codes"}),
            json!({"role": "assistant", "content": "Routine status update."}),
        ];
        messages.extend_from_slice(extra);
        json!({"model": "m", "messages": messages})
    };
    let first = admit_request(&turn(&[]), &REMOTE);
    assert!(
        !matches!(first.body, EgressBody::Refused(_)),
        "one restricted block does not refuse the request: {:?}",
        first.body
    );
    let first_doc = rewritten(first.body);
    let notice = first_doc["messages"][0]["content"]
        .as_str()
        .expect("notice")
        .to_owned();
    assert!(
        notice.starts_with("[lean-ctx gateway: content withheld"),
        "{notice}"
    );
    assert!(notice.contains("destination.remote_restricted"), "{notice}");
    assert!(
        !first_doc.to_string().contains("launch window"),
        "{first_doc}"
    );
    assert_eq!(
        first_doc["messages"][1]["content"],
        "Routine status update."
    );
    let id = notice
        .split("lean-ctx gateway why ")
        .nth(1)
        .and_then(|rest| rest.strip_suffix(']'))
        .expect("the notice names its id")
        .to_owned();
    let recorded = super::withheld::lookup(&id).expect("explanation recorded");
    assert_eq!(recorded.position, "message 0");
    assert_eq!(recorded.provider, "anthropic");
    assert!(
        recorded
            .reason_codes
            .contains(&"destination.remote_restricted".to_owned())
    );

    // Later turns re-send the history: same decision, same bytes.
    let later = admit_request(
        &turn(&[json!({"role": "user", "content": "next step please"})]),
        &REMOTE,
    );
    let later_doc = rewritten(later.body);
    assert_eq!(later_doc["messages"][0]["content"], notice.as_str());
    assert_eq!(later_doc["messages"][2]["content"], "next step please");
    assert_eq!(
        rewritten(admit_request(&turn(&[]), &REMOTE).body),
        first_doc,
        "deterministic (#498)"
    );
}

/// Only a person's grant lets exactly that block go, to that provider; in
/// governed mode no grant is honoured.
#[test]
#[serial_test::serial(cache_telemetry)]
fn a_human_grant_lets_exactly_that_block_go_and_is_receipted() {
    let _env = isolated("");
    let request = json!({"model": "m", "messages": [
        {"role": "user", "content": "*** TOP SECRET ***\nlaunch window and codes"},
        {"role": "assistant", "content": "Routine status update."}
    ]});
    let notice = rewritten(admit_request(&request, &REMOTE).body)["messages"][0]["content"]
        .as_str()
        .expect("notice")
        .to_owned();
    let id = notice
        .split("lean-ctx gateway why ")
        .nth(1)
        .and_then(|rest| rest.strip_suffix(']'))
        .expect("id")
        .to_owned();
    let block = super::withheld::lookup(&id).expect("recorded");
    super::withheld::write_grant(&block, 1).expect("grant");

    let allowed = admit_request(&request, &REMOTE);
    assert!(
        matches!(allowed.body, EgressBody::Unchanged),
        "{:?}",
        allowed.body
    );
    let receipt = egress::finish(&allowed, Some(b"forwarded"), 64, None)
        .expect("receipt storage")
        .expect("receipt");
    assert!(
        receipt.decisions.iter().any(|decision| decision
            .reason_codes
            .iter()
            .any(|code| code.as_str() == super::withheld::ALLOWED_ONCE)),
        "the grant is in the receipt"
    );
    // Another provider is not covered by the grant.
    let other = admit_request(&request, &OPENAI_REMOTE);
    assert!(
        matches!(other.body, EgressBody::Rewritten(_)),
        "{:?}",
        other.body
    );
    // Different bytes, different block: not covered either.
    let edited = json!({"model": "m", "messages": [
        {"role": "user", "content": "*** TOP SECRET ***\nlaunch window and codes!"},
        {"role": "assistant", "content": "Routine status update."}
    ]});
    assert!(matches!(
        admit_request(&edited, &REMOTE).body,
        EgressBody::Rewritten(_)
    ));

    drop(_env);
    let _env = isolated("[context_gateway]\nmode = \"governed\"\n");
    // A new data directory has a new receipt key, and so a new id.
    let id = super::withheld::block_id(&super::content_digest(
        "*** TOP SECRET ***\nlaunch window and codes",
    ))
    .expect("id");
    let block = super::withheld::WithheldBlock {
        id,
        mode: "governed".into(),
        ..block
    };
    super::withheld::write_grant(&block, 1).expect("grant");
    let governed = admit_request(&request, &REMOTE);
    assert!(
        !matches!(governed.body, EgressBody::Unchanged),
        "governed mode ignores grants: {:?}",
        governed.body
    );
}

/// What cannot be replaced in place still refuses — and the refusal names
/// the withheld block, so a person can look it up and decide.
#[test]
#[serial_test::serial(cache_telemetry)]
fn a_refusal_names_the_withheld_blocks() {
    let _env = isolated("");
    let alone = json!({"model": "m", "messages": [
        {"role": "user", "content": "*** TOP SECRET ***\nlaunch window and codes"}
    ]});
    let outcome = admit_request(&alone, &REMOTE);
    assert!(
        matches!(outcome.body, EgressBody::Refused(_)),
        "{:?}",
        outcome.body
    );
    let blocks = outcome.refused_blocks();
    assert_eq!(blocks.len(), 1, "{blocks:?}");
    assert!(super::withheld::valid_id(&blocks[0]));
    assert!(super::withheld::lookup(&blocks[0]).is_some());
}

/// The trigger of 25./26.10.: a directory listing whose entry has the name
/// of the top marking level, in lowercase, is ordinary tool output.
#[test]
#[serial_test::serial(cache_telemetry)]
fn a_listing_with_a_lowercase_entry_named_like_a_marking_is_delivered() {
    let _env = isolated("");
    let request = json!({"model": "m", "messages": [
        {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "toolu_1",
            "content": [{"type": "text", "text": "objects\npg\npg.log\nsecret\n"}]}]}
    ]});
    let outcome = admit_request(&request, &REMOTE);
    assert!(
        matches!(outcome.body, EgressBody::Unchanged),
        "{:?}",
        outcome.body
    );
    assert_ne!(outcome.classification, Some(ClassificationV1::Restricted));
}

/// A key is structural only at its protocol position. Before, every member
/// called `name`, `id`, `stop`, `stop_sequences`, `format`, `response_format`
/// (at any depth), every `signature`/`encrypted_content`, and every object
/// key left the machine uninspected.
#[test]
#[serial_test::serial(cache_telemetry)]
fn caller_controlled_content_is_inspected_whatever_its_member_is_called() {
    let _env = isolated("");
    let leak = format!("key {AWS_KEY}");
    let input = json!({
        "name": leak, "id": leak, "signature": leak, "encrypted_content": leak, "data": leak
    });
    let schema = json!({
        "type": "object",
        "properties": {"q": {"description": leak, "default": leak, "enum": [leak]}}
    });
    let rewritten_shapes = [
        (
            "anthropic tool_use input",
            json!({"model": "m", "stop_sequences": [leak], "messages": [{"role": "assistant",
                "content": [{"type": "tool_use", "id": "toolu_1", "name": "lookup",
                    "input": input}]}]}),
        ),
        (
            "gemini functionCall args",
            json!({"contents": [{"role": "model", "parts": [
                {"functionCall": {"name": "lookup", "args": input}}]}]}),
        ),
        (
            "openai chat tool schema, stop and response_format",
            json!({"model": "m", "stop": leak, "messages": [{"role": "user", "content": "hi"}],
                "tools": [{"type": "function", "function": {"name": "lookup",
                    "description": leak, "parameters": schema}}],
                "response_format": {"type": "json_schema", "json_schema":
                    {"name": "answer", "schema": schema}}}),
        ),
        (
            "openai responses function_call_output and text.format",
            json!({"model": "m", "input": [
                {"type": "function_call", "call_id": "call_1", "name": "lookup",
                    "arguments": "{}"},
                {"type": "function_call_output", "call_id": "call_1", "output": leak}],
                "text": {"format": {"type": "json_schema", "name": "answer",
                    "schema": schema}}}),
        ),
    ];
    for (shape, request) in rewritten_shapes {
        let admitted = rewritten(admit_request(&request, &REMOTE).body);
        assert_clean(&admitted);
        // Protocol identifiers at their positions are byte-identical.
        for pointer in [
            "/model",
            "/messages/0/content/0/id",
            "/messages/0/content/0/name",
        ] {
            assert_eq!(
                admitted.pointer(pointer),
                request.pointer(pointer),
                "{shape}"
            );
        }
    }

    // Immutable positions are never rewritten: the request is refused.
    let immutable = [
        json!({"model": "m", "messages": [{"role": "assistant", "content": [
            {"type": "tool_use", "id": "toolu_1", "name": "lookup", "input": {(leak.as_str()): 1}}]}]}),
        json!({"model": "m", "messages": [{"role": "user", "content": "hi"}],
            "tools": [{"name": "lookup", "input_schema": {"type": "object",
                "properties": {(leak.as_str()): {"type": "string"}}}}]}),
        json!({"contents": [{"role": "model", "parts": [
            {"functionCall": {"name": leak, "args": {}}}]}]}),
    ];
    for request in immutable {
        match admit_request(&request, &REMOTE).body {
            EgressBody::Refused(reason) => {
                assert!(reason.contains("egress.immutable_position"), "{reason}");
                assert!(!reason.contains(AWS_KEY), "{reason}");
            }
            other => panic!("a finding in an immutable position must refuse: {other:?}"),
        }
    }

    // Controls: a clean body and a genuine signed block stay byte-identical.
    let sealed = json!({"model": "m", "messages": [{"role": "assistant", "content": [
        {"type": "thinking", "thinking": "plan the answer", "signature": "c2lnbmVkLWJ5LXByb3ZpZGVy"},
        {"type": "tool_use", "id": "toolu_1", "name": "lookup", "input": {"name": "Ada"}}]}]});
    assert!(matches!(
        admit_request(&sealed, &REMOTE).body,
        EgressBody::Unchanged
    ));
}

#[test]
#[serial_test::serial(cache_telemetry)]
fn anthropic_safeguard_boolean_is_protocol_metadata_only_at_its_position() {
    let _env = isolated("");
    let safeguard = |flag: Value| json!({"classifier_context": {"restricted": flag}});
    let request = |safeguards: Value| {
        json!({"model": "m", "messages": [{"role": "user", "content": "hi"}],
            "safeguards": safeguards})
    };
    for flag in [false, true] {
        let doc = request(json!([safeguard(json!(flag))]));
        let outcome = admit_request(&doc, &REMOTE);
        assert!(
            matches!(outcome.body, EgressBody::Unchanged),
            "the provider's boolean flag is not a content marking: {:?}",
            outcome.body
        );
    }

    // A lowercase key is not a written marking anywhere (26.10.), so the same
    // key in another shape, position or provider is delivered as well — the
    // protocol exception is no longer what keeps these requests alive.
    let valid = json!([safeguard(json!(false))]);
    let key_in = |key: &str| {
        [
            request(json!({"classifier_context": {key: false}})),
            request(json!([[{"classifier_context": {key: false}}]])),
            request(json!([{"classifier_context": [{key: false}]}])),
            request(json!([{"classifier_context": {key: "not a boolean"}}])),
            json!({"model": "m", "metadata": {"safeguards": [{"classifier_context": {key: false}}]}}),
            json!({"model": "m", "messages": [{"role": "assistant", "content": [
                {"type": "tool_use", "id": "toolu_1", "name": "lookup",
                    "input": {key: false}}]}]}),
        ]
    };
    for doc in key_in("restricted") {
        assert!(
            !matches!(admit_request(&doc, &REMOTE).body, EgressBody::Refused(_)),
            "{doc}"
        );
    }
    // A written marking as a key is a finding in an immutable position: the
    // request is refused on every shape. These also follow the valid cases
    // to catch memo reuse.
    for doc in key_in("RESTRICTED").into_iter().chain(key_in("SECRET")) {
        assert!(
            matches!(admit_request(&doc, &REMOTE).body, EgressBody::Refused(_)),
            "{doc}"
        );
    }
    let doc = request(valid);
    for provider in ["OpenAI", "Bedrock", "Copilot", "Unknown"] {
        let other_provider = EgressTarget { provider, ..REMOTE };
        assert!(!matches!(
            admit_request(&doc, &other_provider).body,
            EgressBody::Refused(_)
        ));
    }

    let mut marked = doc.clone();
    marked["messages"][0]["content"] = json!("RESTRICTED\nprivate content");
    assert!(matches!(
        admit_request(&marked, &REMOTE).body,
        EgressBody::Refused(_)
    ));
    let mut sensitive = doc.clone();
    sensitive["safeguards"][0]["classifier_context"]["credential"] = json!(AWS_KEY);
    sensitive["safeguards"][0]["classifier_context"]["type"] = json!("thinking");
    sensitive["safeguards"][0]["classifier_context"]["signature"] = json!(AWS_KEY);
    assert_clean(&rewritten(admit_request(&sensitive, &REMOTE).body));

    let project = tempfile::tempdir().expect("project root");
    write_project_policy(
        project.path(),
        "[filters.blocked_patterns]\nflag = '^restricted$'\n",
    );
    assert!(matches!(
        admit_in_project(&doc, &REMOTE, project.path()).body,
        EgressBody::Refused(_)
    ));
}

#[test]
#[serial_test::serial(cache_telemetry)]
fn project_block_checks_original_before_gateway_redaction() {
    let _env = isolated("");
    let project = tempfile::tempdir().expect("project root");
    write_project_policy(
        project.path(),
        &format!("[filters.blocked_patterns]\ncustomer = '{AWS_KEY}'\n"),
    );
    let request = simple_text_request(&format!("deploy with {AWS_KEY}"));
    let outcome = admit_in_project(&request, &REMOTE, project.path());
    match outcome.body {
        EgressBody::Refused(reason) => assert!(!reason.contains(AWS_KEY)),
        other => panic!("project block must withhold the request, got {other:?}"),
    }
}

#[test]
#[serial_test::serial(cache_telemetry)]
fn project_policy_still_redacts_when_gateway_is_disabled() {
    let _env = isolated("[context_gateway]\nenabled = false\n");
    let project = tempfile::tempdir().expect("project root");
    write_project_policy(project.path(), "[redaction]\ncustomer = 'K-[0-9]{6}'\n");
    let request = simple_text_request("Contact K-482193");
    let outcome = admit_in_project(&request, &REMOTE, project.path());
    let admitted = rewritten(outcome.body);
    let text = admitted["messages"][0]["content"].as_str().unwrap();
    assert_eq!(text, "Contact [REDACTED:customer]");
    assert!(!text.contains("K-482193"));
}

/// The sealed trace of the newest proxy receipt.
fn newest_proxy_trace() -> super::receipt_store::trace::TraceSidecarV1 {
    let stored = super::receipt_store::latest(PROXY_RECEIPT_KEY, 1);
    let receipt = stored[0].1.as_ref().expect("stored receipt verifies");
    match super::receipt_store::trace::load(&receipt.digest) {
        super::receipt_store::trace::StoredTrace::Bound { sidecar, .. } => *sidecar,
        other => panic!("the receipt has no bound trace sidecar: {other:?}"),
    }
}

#[test]
#[serial_test::serial(cache_telemetry)]
fn a_verified_unpin_without_a_floor_is_receipted_and_named_not_passed_through() {
    use super::receipt_store::trace::OrgPolicyState;
    use crate::core::policy::org::Observation;
    use crate::core::policy::runtime::with_observed_view_for_test;

    let _env = isolated("[context_gateway]\nenabled = false\n");
    let project = tempfile::tempdir().expect("project root");
    let fingerprint = b"project-fingerprint";
    // Nothing is active: no gateway, no project pack, no destination rule.
    // Without an organization decision that is a passthrough without record —
    // for a document with a string and for one with none.
    for request in [simple_text_request("hello"), json!({"n": 1})] {
        let community = with_observed_view_for_test(project.path(), None, || {
            egress::admit_request_for_project(&request, &REMOTE, fingerprint)
        });
        assert!(matches!(community.body, EgressBody::Unchanged));
        assert!(
            egress::finish(&community, Some(b"{}"), 2, None)
                .expect("no receipt owed")
                .is_none(),
            "no organization decision: nothing is recorded"
        );
    }
    // A signed unpin leaves no floor and no policy. It is an organization
    // decision all the same: the request is receipted, the receipt claims no
    // inspected source, and the sealed trace names the decision.
    for request in [simple_text_request("hello"), json!({"n": 1})] {
        let observed = Observation::unpinned_for_test(7, 'a');
        let outcome = with_observed_view_for_test(project.path(), Some(observed), || {
            egress::admit_request_for_project(&request, &REMOTE, fingerprint)
        });
        assert!(matches!(outcome.body, EgressBody::Unchanged));
        let bytes = serde_json::to_vec(&request).expect("request bytes");
        let receipt = egress::finish(&outcome, Some(&bytes), bytes.len(), None)
            .expect("receipt storage")
            .expect("an observed organization decision owes a receipt");
        assert_eq!(receipt.decisions.len(), 1);
        assert!(
            receipt.decisions[0]
                .reason_codes
                .iter()
                .any(|code| code.as_str() == super::capture::NO_SOURCE_DECISION),
            "{:?}",
            receipt.decisions
        );
        assert_eq!(receipt.security.redactions, 0, "no count is made up");
        let trace = newest_proxy_trace();
        let named = trace.org_policy.expect("the trace names the decision");
        assert_eq!(named.state, OrgPolicyState::Unpinned);
        assert_eq!(named.policy_version, 7);
        assert_eq!(named.statement.hex(), "a".repeat(64));
        assert_eq!(named.valid_until, None, "an unpin has no horizon");
        assert!(!trace.org_policy_diverged && !trace.org_policy_unavailable);
    }
    // An opaque body takes the decision of the view it is admitted in and
    // asks nothing itself; outside a view it records that none is available.
    let observed = Observation::unpinned_for_test(7, 'a');
    let opaque = with_observed_view_for_test(project.path(), Some(observed), || {
        egress::admit_opaque_for_project_with_binding(
            4,
            &REMOTE,
            fingerprint,
            egress::TrustedDestinationBinding::default(),
        )
    });
    egress::finish(&opaque, Some(&[0xff, 0xfe, 0x00, b'a']), 4, None)
        .expect("receipt storage")
        .expect("receipt");
    assert_eq!(
        newest_proxy_trace()
            .org_policy
            .map(|named| named.policy_version),
        Some(7)
    );
    // Two admissions under different decisions joined into one outcome name
    // neither.
    let joined = [('a', 7), ('b', 8)].map(|(fill, version)| {
        let observed = Observation::unpinned_for_test(version, fill);
        with_observed_view_for_test(project.path(), Some(observed), || {
            egress::admit_request_for_project(&simple_text_request("hello"), &REMOTE, fingerprint)
        })
    });
    let [earlier, later] = joined;
    let joined = earlier.join(later);
    egress::finish(&joined, Some(b"{}"), 2, None)
        .expect("receipt storage")
        .expect("receipt");
    let trace = newest_proxy_trace();
    assert!(trace.org_policy.is_none() && trace.org_policy_diverged);

    // The reader takes an organization record only in the form an observation
    // produces. A stored horizon is history: one long past is read, never
    // compared with today.
    use super::receipt_store::trace;
    let base = serde_json::to_value(&trace).expect("sidecar document");
    let reads = |org: Value, diverged: bool| {
        let mut document = base.clone();
        document["org_policy"] = org;
        document["org_policy_diverged"] = json!(diverged);
        trace::decode(&serde_json::to_vec(&document).expect("bytes")).is_some()
    };
    let statement = format!("sha256:{}", "a".repeat(64));
    let unpinned = json!({"state": "unpinned", "statement": statement, "policy_version": 7});
    let required = json!({"state": "required", "statement": statement, "policy_version": 7,
        "valid_until": "2020-01-01T00:00:00.000000000Z"});
    assert!(reads(unpinned.clone(), false));
    assert!(reads(required.clone(), false), "a past horizon is history");
    assert!(
        !reads(unpinned.clone(), true),
        "a decision and 'none named'"
    );
    let altered = |base: &Value, field: &str, value: Value| {
        let mut org = base.clone();
        org[field] = value;
        org
    };
    for refused in [
        altered(
            &unpinned,
            "valid_until",
            json!("2020-01-01T00:00:00.000000000Z"),
        ),
        altered(&required, "valid_until", Value::Null),
        altered(&required, "valid_until", json!("2020-01-01T00:00:00Z")),
        altered(
            &required,
            "valid_until",
            json!("2020-01-01T01:00:00.000000000+01:00"),
        ),
        altered(&unpinned, "policy_version", json!(0)),
        altered(
            &unpinned,
            "policy_version",
            json!(9_007_199_254_740_992_u64),
        ),
        altered(&unpinned, "statement", json!("sha256:abc")),
        altered(&unpinned, "note", json!(1)),
    ] {
        assert!(!reads(refused.clone(), false), "{refused}");
    }
}

#[test]
#[serial_test::serial(cache_telemetry)]
fn an_opaque_body_outside_a_view_records_that_no_organization_decision_is_available() {
    let _env = isolated("[context_gateway]\nmode = \"governed\"\n");
    let outcome = egress::admit_opaque_for_project_with_binding(
        4,
        &REMOTE,
        b"project-fingerprint",
        egress::TrustedDestinationBinding::default(),
    );
    egress::finish(&outcome, None, 4, None)
        .expect("receipt storage")
        .expect("receipt");
    let trace = newest_proxy_trace();
    assert!(trace.org_policy.is_none() && !trace.org_policy_diverged);
    assert!(trace.org_policy_unavailable);
}

#[test]
#[serial_test::serial(cache_telemetry)]
fn conflicting_project_and_gateway_redactions_withhold_the_request() {
    let _env = isolated("");
    let project = tempfile::tempdir().expect("project root");
    write_project_policy(
        project.path(),
        &format!("[redaction]\ncustomer = '{AWS_KEY}'\n"),
    );
    let request = simple_text_request(&format!("deploy with {AWS_KEY}"));
    let outcome = admit_in_project(&request, &REMOTE, project.path());
    assert!(
        matches!(outcome.body, EgressBody::Refused(_)),
        "order-dependent redactions must not publish either version"
    );
}

#[test]
#[serial_test::serial(cache_telemetry)]
fn project_redaction_cannot_downgrade_original_gateway_classification() {
    let _env = isolated("");
    let project = tempfile::tempdir().expect("project root");
    write_project_policy(
        project.path(),
        "[redaction]\nbanner = '\\*\\*\\* TOP SECRET \\*\\*\\*'\n",
    );
    let request = json!({
        "model":"m",
        "messages":[{"role":"user","content":"*** TOP SECRET ***\nlaunch window and codes\n"}]
    });
    let outcome = admit_in_project(&request, &REMOTE, project.path());
    assert_eq!(outcome.classification, Some(ClassificationV1::Restricted));
    match outcome.body {
        EgressBody::Refused(reason) => {
            assert!(reason.contains("destination.remote_restricted"), "{reason}");
            assert!(!reason.contains("launch window"), "{reason}");
        }
        other => {
            panic!("a project redaction cannot authorize restricted remote delivery: {other:?}")
        }
    }
}

#[test]
#[serial_test::serial(cache_telemetry)]
fn project_policy_change_invalidates_clean_memo_and_receipt_digest() {
    let _env = isolated("");
    let project = tempfile::tempdir().expect("project root");
    let request = simple_text_request("K-482193");
    let original = serde_json::to_vec(&request).expect("original request bytes");

    let first = admit_in_project(&request, &REMOTE, project.path());
    assert!(matches!(first.body, EgressBody::Unchanged));
    let first_receipt = egress::finish_admission(&first, Some(&original), original.len(), None)
        .expect("first receipt storage")
        .and_then(|evidence| evidence.decision_receipt)
        .expect("first decision receipt");
    let first_policy = first_receipt.policy.expect("first policy digest");

    write_project_policy(
        project.path(),
        "[filters.blocked_patterns]\ncustomer = 'K-482193'\n",
    );
    let second = admit_in_project(&request, &REMOTE, project.path());
    assert!(matches!(second.body, EgressBody::Refused(_)));
    let second_receipt = egress::finish_admission(&second, None, original.len(), None)
        .expect("second receipt storage")
        .and_then(|evidence| evidence.decision_receipt)
        .expect("second decision receipt");
    let second_policy = second_receipt.policy.expect("second policy digest");
    assert_ne!(first_policy.digest, second_policy.digest);
}

#[test]
#[serial_test::serial(cache_telemetry)]
fn gateway_reload_invalidates_project_egress_policy_epoch() {
    let _env = isolated("[context_gateway]\nenabled = false\n");
    let project = tempfile::tempdir().expect("project root");
    let request = simple_text_request("A clean request");
    let (outcome, project_fingerprint) =
        crate::core::policy::runtime::with_explicit_project_source_view(
            project.path(),
            |authority| {
                let fingerprint = authority.fingerprint_bytes();
                let outcome = egress::admit_request_for_project(&request, &REMOTE, &fingerprint);
                (outcome, fingerprint)
            },
        )
        .expect("project policy authority remained current");
    assert!(outcome.policy_epoch_current_for_project(&project_fingerprint));

    let config = crate::core::paths::config_dir_read_only().expect("config directory");
    std::fs::write(
        config.join("config.toml"),
        "[context_gateway]\nmode = \"governed\"\n",
    )
    .expect("reloaded gateway config");
    assert!(
        !outcome.policy_epoch_current_for_project(&project_fingerprint),
        "prepared output must not outlive the Gateway policy epoch"
    );
}

#[test]
#[serial_test::serial(cache_telemetry)]
fn project_policy_epoch_change_before_publication_returns_no_result() {
    let _env = isolated("");
    let project = tempfile::tempdir().expect("project root");
    write_project_policy(project.path(), "[redaction]\ncustomer = 'K-[0-9]{6}'\n");
    let request = simple_text_request("K-482193");
    let original = serde_json::to_vec(&request).expect("original request bytes");
    let result = crate::core::policy::runtime::with_explicit_project_source_view(
        project.path(),
        |authority| {
            let fingerprint = authority.fingerprint_bytes();
            let outcome = egress::admit_request_for_project(&request, &REMOTE, &fingerprint);
            let forwarded = match &outcome.body {
                EgressBody::Unchanged => original.clone(),
                EgressBody::Rewritten(body) => serde_json::to_vec(body).expect("forwarded bytes"),
                EgressBody::Refused(_) => Vec::new(),
            };
            egress::finish_admission(
                &outcome,
                (!forwarded.is_empty()).then_some(forwarded.as_slice()),
                original.len(),
                None,
            )
            .expect("prepared receipt storage");
            write_project_policy(
                project.path(),
                "[filters.blocked_patterns]\ncustomer = 'K-482193'\n",
            );
            outcome
        },
    );
    assert!(
        result.is_err(),
        "stale prepared content must not be published"
    );
}

#[test]
#[serial_test::serial(cache_telemetry)]
fn explicit_project_scope_rejects_a_nested_foreign_source_view() {
    let first = tempfile::tempdir().expect("first project root");
    let second = tempfile::tempdir().expect("second project root");
    let result =
        crate::core::policy::runtime::with_explicit_project_source_view(first.path(), |_| {
            crate::core::policy::runtime::REQUEST_PROJECT.sync_scope(
                std::cell::RefCell::new(None),
                || {
                    crate::core::policy::runtime::with_explicit_project_source_view(
                        second.path(),
                        |_| (),
                    )
                },
            )
        });
    assert!(
        result.is_err(),
        "a nested foreign source view must be rejected"
    );
}

#[test]
#[serial_test::serial(cache_telemetry)]
fn explicit_project_scope_rejects_a_trusted_request_root_mismatch() {
    let trusted = tempfile::tempdir().expect("trusted project root");
    let requested = tempfile::tempdir().expect("requested project root");
    let result = crate::core::policy::runtime::REQUEST_PROJECT.sync_scope(
        std::cell::RefCell::new(Some(trusted.path().to_path_buf())),
        || {
            crate::core::policy::runtime::with_explicit_project_source_view(
                requested.path(),
                |_| (),
            )
        },
    );
    assert!(
        result.is_err(),
        "an explicit root cannot replace request authority"
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
    let receipt = egress::finish(&outcome, Some(&forwarded), 4000, Some("codex-1"))
        .expect("receipt storage")
        .expect("receipt");
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
    let refused = egress::finish(&outcome, None, 4000, None)
        .expect("receipt storage")
        .expect("receipt");
    assert!(refused.final_context.is_none());
}

fn obstruct_receipt_storage() {
    let data = crate::core::data_dir::lean_ctx_data_dir().expect("isolated data directory");
    std::fs::create_dir_all(&data).expect("data directory");
    // A regular file cannot contain the receipts directory. This fails on all
    // platforms, including privileged test runners where chmod is insufficient.
    std::fs::write(data.join("gateway"), b"blocked test store").expect("store obstruction");
}

#[test]
#[serial_test::serial(cache_telemetry)]
fn mandatory_receipt_failure_refuses_egress_and_developer_remains_best_effort() {
    for mode in ["governed", "sovereign", "developer"] {
        let _env = isolated(&format!("[context_gateway]\nmode = \"{mode}\"\n"));
        let request = json!({"model": "m", "messages": [{"role": "user", "content": "hello"}]});
        let outcome = admit_request(&request, &LOCAL);
        assert!(matches!(outcome.body, EgressBody::Unchanged));
        obstruct_receipt_storage();
        let bytes = serde_json::to_vec(&request).expect("request bytes");
        let result = egress::finish(&outcome, Some(&bytes), bytes.len(), None);
        if mode == "developer" {
            // Best effort: the request still goes. The obstructed store also
            // holds no receipt key, and without it no receipt is issued —
            // never one with plain object digests (AP10-C4).
            assert!(result.expect("best effort").is_none());
        } else {
            assert_eq!(result.unwrap_err(), egress::RECEIPT_UNAVAILABLE);
        }
    }
}

#[test]
#[serial_test::serial(cache_telemetry)]
fn opaque_receipt_digest_binds_original_binary_bytes() {
    let _env = isolated("");
    let bytes = [0xff, 0xfe, 0x00, b'a'];
    let outcome = admit_opaque(bytes.len(), &REMOTE);
    let receipt = egress::finish(&outcome, Some(&bytes), bytes.len(), None)
        .expect("receipt storage")
        .expect("receipt");
    assert_eq!(
        receipt.final_context,
        Some(super::fingerprint_digest(&bytes))
    );
    assert_eq!(receipt.tokens.delivered, 1);
    assert!(receipt.validate().is_ok());
}

#[test]
#[serial_test::serial(cache_telemetry)]
fn existing_tampered_receipt_cannot_satisfy_a_persistence_requirement() {
    let _env = isolated("[context_gateway]\nmode = \"governed\"\n");
    let request = json!({"model":"m","messages":[{"role":"user","content":"hello"}]});
    let outcome = admit_request(&request, &LOCAL);
    let bytes = serde_json::to_vec(&request).expect("request bytes");
    let receipt = egress::finish(&outcome, Some(&bytes), bytes.len(), None)
        .expect("receipt storage")
        .expect("receipt");
    let digest = receipt.canonical_digest().expect("receipt digest");
    let dir = crate::core::data_dir::lean_ctx_data_dir()
        .expect("data directory")
        .join("gateway");
    std::fs::write(
        dir.join("receipts")
            .join(format!("{}.json.zst", digest.hex())),
        b"{}",
    )
    .expect("tamper with stored object");
    assert!(super::receipt_store::persist_in(&dir, &receipt, PROXY_RECEIPT_KEY, None).is_err());
}

#[test]
#[serial_test::serial(cache_telemetry)]
fn lifecycle_receipt_withholds_even_if_a_caller_supplies_forwarded_bytes() {
    let _env = isolated("[context_gateway]\nmode = \"governed\"\n");
    let outcome = admit_request(&thinking_request(), &REMOTE);
    assert!(matches!(outcome.body, EgressBody::Refused(_)));
    let evidence = egress::finish_admission(&outcome, Some(b"must not be prepared"), 2048, None)
        .expect("required decision and lifecycle receipts")
        .expect("governed admission evidence");
    assert_eq!(
        evidence.admission.as_ref().expect("admission").outcome,
        EgressAdmissionOutcomeV1::Withheld
    );
    assert!(
        evidence
            .admission
            .as_ref()
            .expect("admission")
            .prepared_bytes_digest
            .is_none(),
        "refused bytes are never labeled Prepared"
    );
    assert!(
        evidence
            .decision_receipt
            .as_ref()
            .expect("legacy decision receipt")
            .final_context
            .is_none()
    );
    let report = lean_ctx_protocol::EngineEgressHostReportRequestV1 {
        schema_version: lean_ctx_protocol::ENGINE_EGRESS_SCHEMA_VERSION,
        admission_receipt_digest: evidence.admission_digest.expect("stored withheld record"),
        attempt: 1,
        transport: lean_ctx_protocol::context_gateway::EgressTransportV1::Http,
        result: lean_ctx_protocol::context_gateway::EgressTransportResultV1::ResponseObserved,
        http_status: Some(200),
        failure: None,
    };
    assert!(
        egress::record_host_observation(&report).is_err(),
        "a withheld admission cannot receive a transport report"
    );
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

// ─── Q6: provider-sealed material by provider, position and shape ───────────

const GEMINI_REMOTE: EgressTarget<'static> = EgressTarget {
    provider: "Gemini",
    model: Some("gemini-pro"),
    upstream_base: "https://generativelanguage.googleapis.com",
};

const OPENAI_REMOTE: EgressTarget<'static> = EgressTarget {
    provider: "OpenAI",
    model: Some("gpt-test"),
    upstream_base: "https://api.openai.com",
};

/// One genuine provider-sealed item per wire shape, around clean content.
fn sealed_provider_requests() -> Vec<(&'static str, EgressTarget<'static>, Value)> {
    vec![
        (
            "anthropic thinking signature",
            REMOTE,
            json!({"model": "claude", "messages": [{"role": "assistant", "content": [
                {"type": "thinking", "thinking": "plan the answer", "signature": "c2lnbmVk"}]}]}),
        ),
        (
            "anthropic redacted_thinking data",
            REMOTE,
            json!({"model": "claude", "messages": [{"role": "assistant", "content": [
                {"type": "redacted_thinking", "data": "ZW5jcnlwdGVk"}]}]}),
        ),
        (
            "gemini thoughtSignature beside a functionCall",
            GEMINI_REMOTE,
            json!({"contents": [{"role": "model", "parts": [
                {"functionCall": {"name": "lookup", "args": {"q": "weather"}},
                 "thoughtSignature": "c2lnbmVk"}]}]}),
        ),
        (
            "gemini thoughtSignature beside text",
            GEMINI_REMOTE,
            json!({"contents": [{"role": "model", "parts": [
                {"text": "the answer", "thoughtSignature": "c2lnbmVk"}]}]}),
        ),
        (
            "openai responses reasoning item",
            OPENAI_REMOTE,
            json!({"model": "gpt-test", "input": [
                {"type": "reasoning", "id": "rs_1", "summary": [],
                 "encrypted_content": "Z0FBQUFB"}]}),
        ),
        (
            "openai responses compaction item",
            OPENAI_REMOTE,
            json!({"model": "gpt-test", "input": [
                {"type": "compaction", "id": "cmp_1", "encrypted_content": "Z0FBQUFB"}]}),
        ),
        (
            "openai responses program fingerprint",
            OPENAI_REMOTE,
            json!({"model": "gpt-test", "input": [
                {"type": "program", "id": "prog_1", "fingerprint": "ZnBfb3BhcXVl"}]}),
        ),
    ]
}

/// Developer mode forwards a genuine provider-sealed item byte-identical and
/// records it as opaque — never as inspected.
#[test]
#[serial_test::serial(cache_telemetry)]
fn provider_sealed_item_is_preserved_and_recorded_as_opaque_in_developer_mode() {
    let _env = isolated("");
    for (shape, target, request) in sealed_provider_requests() {
        let outcome = admit_request(&request, &target);
        assert!(
            matches!(outcome.body, EgressBody::Unchanged),
            "{shape}: {:?}",
            outcome.body
        );
        let receipt = egress::finish(&outcome, Some(b"{}"), 100, None)
            .expect("receipt storage")
            .expect("receipt");
        assert!(
            receipt.decisions.iter().any(|decision| decision
                .reason_codes
                .iter()
                .any(|code| code.as_str() == "egress.opaque_provider_material")),
            "{shape}: the receipt names the opaque material"
        );
    }
}

/// Where full inspection is demanded, material no detector can read is not
/// forwarded on trust: the request is refused with that reason.
#[test]
#[serial_test::serial(cache_telemetry)]
fn provider_sealed_item_is_refused_where_full_inspection_is_demanded() {
    let _env = isolated("[context_gateway]\nmode = \"governed\"\n");
    for (shape, target, request) in sealed_provider_requests() {
        match admit_request(&request, &target).body {
            EgressBody::Refused(reason) => {
                assert!(
                    reason.contains("egress.opaque_provider_material")
                        && reason.contains("coverage.uninspected"),
                    "{shape}: {reason}"
                );
            }
            other => panic!("{shape}: governed mode must refuse, got {other:?}"),
        }
    }
}

/// A member that merely shares a sealed member's name is ordinary caller
/// data: wrong block type, wrong position or another provider's shape — it
/// is inspected and masked like any other content.
#[test]
#[serial_test::serial(cache_telemetry)]
fn namesake_of_a_sealed_member_in_ordinary_data_is_inspected() {
    let _env = isolated("");
    let leak = format!("key {AWS_KEY}");
    let namesakes = [
        (
            "anthropic text block with a signature member",
            REMOTE,
            json!({"model": "claude", "messages": [{"role": "user", "content": [
                {"type": "text", "text": "hi", "signature": leak}]}]}),
        ),
        (
            "anthropic text block with a data member",
            REMOTE,
            json!({"model": "claude", "messages": [{"role": "user", "content": [
                {"type": "text", "text": "hi", "data": leak}]}]}),
        ),
        (
            "gemini part with a thoughtSignature but no functionCall or text",
            GEMINI_REMOTE,
            json!({"contents": [{"role": "user", "parts": [
                {"functionResponse": {"name": "lookup", "response": {"ok": true}},
                 "thoughtSignature": leak}]}]}),
        ),
        (
            "openai message item with an encrypted_content member",
            OPENAI_REMOTE,
            json!({"model": "gpt-test", "input": [
                {"type": "message", "role": "user", "content": "hi",
                 "encrypted_content": leak}]}),
        ),
        (
            "openai function_call_output item with a fingerprint member",
            OPENAI_REMOTE,
            json!({"model": "gpt-test", "input": [
                {"type": "function_call_output", "call_id": "call_1", "output": "ok",
                 "fingerprint": leak}]}),
        ),
        (
            "an anthropic thinking shape sent to an openai destination",
            OPENAI_REMOTE,
            json!({"model": "gpt-test", "messages": [{"role": "assistant", "content": [
                {"type": "thinking", "thinking": "plan", "signature": leak}]}]}),
        ),
    ];
    for (shape, target, request) in namesakes {
        match admit_request(&request, &target).body {
            EgressBody::Rewritten(admitted) => assert!(
                !admitted.to_string().contains(AWS_KEY),
                "{shape}: the value left unmasked"
            ),
            EgressBody::Refused(reason) => assert!(!reason.contains(AWS_KEY), "{shape}"),
            EgressBody::Unchanged => panic!("{shape}: the namesake member was not inspected"),
        }
    }
}

/// One admission is one project-policy snapshot: a policy that changes after
/// the first string was decided does not apply to the second string of the
/// same admission.
#[test]
#[serial_test::serial(cache_telemetry)]
fn project_policy_is_one_snapshot_for_every_string_of_an_admission() {
    let _env = isolated("");
    let _live = crate::core::policy::runtime::lock_test_override();
    let project = tempfile::tempdir().expect("project root");
    write_project_policy(project.path(), "[redaction]\ncustomer = 'K-[0-9]{6}'\n");
    let request = json!({"model": "m", "messages": [
        {"role": "user", "content": "first K-482193"},
        {"role": "user", "content": "second K-482193"}]});
    let root = project.path().to_path_buf();
    egress::failpoint::arm_after_first_string(move || write_project_policy(&root, ""));

    let outcome = crate::core::policy::runtime::REQUEST_PROJECT.sync_scope(
        std::cell::RefCell::new(Some(project.path().to_path_buf())),
        || egress::admit_request_for_project(&request, &REMOTE, &[7_u8; 32]),
    );

    let admitted = rewritten(outcome.body);
    for index in 0..2 {
        let text = admitted["messages"][index]["content"]
            .as_str()
            .expect("message text");
        assert!(
            text.contains("[REDACTED:customer]") && !text.contains("K-482193"),
            "message {index} was decided under another project policy: {text}"
        );
    }
}

/// The parts of `request` that the walk hands the resolver of an admission
/// as not host-authored, sorted, and whether it saw a reference to state
/// only the provider holds.
fn non_host_parts(
    request: &Value,
    target: &EgressTarget<'_>,
) -> (Vec<(egress::PartKind, String)>, bool) {
    use std::cell::RefCell;
    use std::rc::Rc;
    let seen = Rc::new(RefCell::new((Vec::new(), false)));
    let sink = Rc::clone(&seen);
    let _ = egress::with_sealed_resolver(
        Box::new(move |walked: &egress::SealedRequest<'_>| {
            let mut parts: Vec<(egress::PartKind, String)> = walked
                .parts
                .iter()
                .map(|part| (part.kind, part.digest.clone()))
                .collect();
            parts.sort_by(|one, other| one.1.cmp(&other.1));
            *sink.borrow_mut() = (parts, walked.server_state);
            Vec::new()
        }),
        || admit_request(request, target),
    );
    seen.take()
}

/// Where full inspection is demanded the walk tells host-authored text,
/// tool results and assistant parts apart by provider shape and position,
/// and digests a tool result per decoded string leaf with the Engine's own
/// digest — so a text block the Engine delivered is found whatever object a
/// host nests it in. Identifiers and tool names at their protocol position
/// are not parts; a string a host adds INSIDE a tool result is one (it is
/// then a leaf the Engine has no record of).
#[test]
#[serial_test::serial(cache_telemetry)]
fn walk_classifies_parts_by_shape_and_digests_tool_results_per_string_leaf() {
    use egress::PartKind::{Assistant, Source};
    let governed = isolated("[context_gateway]\nmode = \"governed\"\n");
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let digest = crate::core::archive::delivered::digest;
    let sorted = |mut parts: Vec<(egress::PartKind, String)>| {
        parts.sort_by(|one, other| one.1.cmp(&other.1));
        parts
    };
    let delivered = "fn kept() {}\n";

    let anthropic = json!({"model": "claude", "system": "be brief", "messages": [
        {"role": "user", "content": [{"type": "text", "text": "read it"}]},
        {"role": "assistant", "content": [
            {"type": "text", "text": "reading"},
            {"type": "tool_use", "id": "toolu_1", "name": "read", "input": {"path": "a.rs"}}]},
        {"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": "toolu_1",
             "content": [{"type": "text", "text": delivered}]},
            {"type": "text", "text": "go on"}]}]});
    assert_eq!(
        non_host_parts(&anthropic, &REMOTE),
        (
            sorted(vec![
                (Assistant, digest("reading")),
                (Assistant, digest("a.rs")),
                (Source, digest(delivered)),
            ]),
            false
        ),
        "anthropic"
    );

    let gemini = |response: Value| {
        json!({"contents": [
            {"role": "user", "parts": [{"text": "read it"}]},
            {"role": "model", "parts": [
                {"functionCall": {"name": "read", "args": {"path": "a.rs"}}}]},
            {"role": "user", "parts": [
                {"functionResponse": {"name": "read", "response": response}}]}]})
    };
    assert_eq!(
        non_host_parts(
            &gemini(json!({"output": delivered, "lines": 1})),
            &GEMINI_REMOTE
        ),
        (
            sorted(vec![
                (Assistant, digest("a.rs")),
                (Source, digest(delivered))
            ]),
            false
        ),
        "gemini: the delivered text is one leaf of the response object"
    );
    let nested = json!({"content": [{"type": "text", "text": delivered}]});
    assert_eq!(
        non_host_parts(&gemini(nested), &GEMINI_REMOTE),
        (
            sorted(vec![
                (Assistant, digest("a.rs")),
                (Source, digest(delivered)),
                (Source, digest("text")),
            ]),
            false
        ),
        "gemini: a discriminator string inside the response is a leaf of its own"
    );

    let stored = json!({"model": "gpt-test", "previous_response_id": "resp_1",
        "input": [{"type": "function_call_output", "call_id": "c1", "output": delivered}]});
    assert_eq!(
        non_host_parts(&stored, &OPENAI_REMOTE),
        (vec![(Source, digest(delivered))], true),
        "responses: a stored response is state only the provider holds"
    );

    // Developer mode: no lineage is derived, the resolver is not asked.
    drop(governed);
    let _developer = isolated("");
    assert_eq!(non_host_parts(&anthropic, &REMOTE), (Vec::new(), false));
}

/// Doc 19, one decision contract on the egress routes: a request the proxy
/// dispatches and one a host dispatches itself are decided alike — and like
/// the call routes (`every_call_route_yields_the_same_decision_…`): clean
/// content is allowed —, each lineage names its route, and only an observed
/// transport report turns "prepared" into an observed model egress, never the
/// admission alone.
#[test]
#[serial_test::serial(cache_telemetry)]
fn egress_routes_share_one_lineage_vocabulary_and_claim_egress_only_when_observed() {
    use super::receipt_store;
    use lean_ctx_protocol::context_lineage::{
        ContextDecisionOutcomeV1, ContextReceiptStateV1, IntegrationRouteV1,
    };

    let _env = isolated("[context_gateway]\nmode = \"governed\"\n");
    let project = tempfile::tempdir().expect("project");
    let root = project
        .path()
        .canonicalize()
        .expect("canonical root")
        .to_string_lossy()
        .into_owned();
    let doc = json!({"model": "claude", "messages": [
        {"role": "user", "content": "pub fn parity_body() {}\n"}
    ]});
    let wire = doc.to_string();
    let mut outcomes = Vec::new();
    for (route, caller) in [
        (IntegrationRouteV1::Proxy, egress::EgressCaller::Proxy),
        (
            IntegrationRouteV1::HostEgress,
            egress::EgressCaller::Host {
                project_root: &root,
                operation: "egress-prepare",
            },
        ),
    ] {
        let outcome = admit_request(&doc, &REMOTE);
        let evidence =
            egress::finish_admission_for(&outcome, Some(wire.as_bytes()), wire.len(), None, caller)
                .expect("receipts stored")
                .expect("governed admission evidence");
        let admission = evidence.admission.as_ref().expect("admission record");
        let decision = admission
            .decision_receipt_digest
            .clone()
            .expect("decision receipt digest");
        let lineage = || {
            let stored = receipt_store::load(decision.hex()).expect("stored decision receipt");
            receipt_store::view::lineage(&stored).expect("lineage of an egress route")
        };
        let prepared = lineage();
        assert_eq!(prepared.request.route, route);
        assert_eq!(
            prepared.receipt_state,
            ContextReceiptStateV1::Prepared,
            "{route:?}: admitted, nothing observed yet"
        );
        let report = lean_ctx_protocol::EngineEgressHostReportRequestV1 {
            schema_version: lean_ctx_protocol::ENGINE_EGRESS_SCHEMA_VERSION,
            admission_receipt_digest: evidence.admission_digest.clone().expect("stored admission"),
            attempt: 1,
            transport: lean_ctx_protocol::context_gateway::EgressTransportV1::Http,
            result: lean_ctx_protocol::context_gateway::EgressTransportResultV1::ResponseObserved,
            http_status: Some(200),
            failure: None,
        };
        egress::record_host_observation(&report).expect("transport observation recorded");
        assert_eq!(
            lineage().receipt_state,
            ContextReceiptStateV1::ObservedModelEgress,
            "{route:?}: the observed request is an observed model egress"
        );
        outcomes.push(
            prepared
                .outcomes
                .iter()
                .map(|outcome| outcome.outcome)
                .collect::<Vec<_>>(),
        );
    }
    assert_eq!(
        outcomes[0], outcomes[1],
        "the two egress routes decided differently"
    );
    assert!(
        !outcomes[0].is_empty()
            && outcomes[0]
                .iter()
                .all(|outcome| *outcome == ContextDecisionOutcomeV1::Allow),
        "clean content is allowed, as on the call routes: {outcomes:?}"
    );
}

/// E05: the carrier matrix in `docs/reference/carrier-matrix.md` is the
/// behaviour, not a description of it. Each row's carrier goes through the
/// function that classifies it, in developer and in governed mode; the
/// observed verdict and reason code must equal the row, and the document's
/// rows must equal this table — a carrier whose handling changes, or a row
/// edited by hand, fails here.
#[test]
#[serial_test::serial(cache_telemetry)]
fn the_carrier_matrix_is_what_each_carrier_gets() {
    #[derive(Clone, Copy)]
    enum Carrier {
        Request(fn() -> (Value, EgressTarget<'static>)),
        Opaque,
        ToolResultMedia,
    }
    fn image() -> (Value, EgressTarget<'static>) {
        let request = json!({"model": "claude", "messages": [{"role": "user", "content": [
            {"type": "image", "source": {"type": "base64", "media_type": "image/png",
             "data": "A".repeat(4096)}}]}]});
        (request, REMOTE)
    }
    fn sealed() -> (Value, EgressTarget<'static>) {
        let (_, target, request) = sealed_provider_requests().remove(0);
        (request, target)
    }
    fn text() -> (Value, EgressTarget<'static>) {
        (simple_text_request("pub fn parity_body() {}\n"), REMOTE)
    }
    // What the carrier observably got — "forward", "forward-marked" (sent,
    // the receipt names why it was not inspected) or "refuse" — and the
    // reason codes recorded ("-" when none).
    fn observe(carrier: Carrier) -> (&'static str, String) {
        let outcome = match carrier {
            Carrier::Request(build) => {
                let (request, target) = build();
                admit_request(&request, &target)
            }
            Carrier::Opaque => admit_opaque(512, &REMOTE),
            Carrier::ToolResultMedia => {
                let gate = super::carrier::ReleaseGate::current();
                return match gate.media(512) {
                    Ok(()) => ("forward", "-".to_owned()),
                    Err(withheld) => ("refuse", withheld.codes().join(",")),
                };
            }
        };
        match &outcome.body {
            EgressBody::Refused(reason) => ("refuse", reason.clone()),
            EgressBody::Unchanged | EgressBody::Rewritten(_) => {
                let codes = egress::finish(&outcome, Some(b"{}"), 512, None)
                    .expect("receipt storage")
                    .map(|receipt| {
                        receipt
                            .decisions
                            .iter()
                            .flat_map(|decision| decision.reason_codes.iter())
                            .map(|code| code.as_str().to_owned())
                            .collect::<Vec<_>>()
                            .join(",")
                    })
                    .unwrap_or_default();
                if codes.is_empty() {
                    ("forward", "-".to_owned())
                } else {
                    ("forward-marked", codes)
                }
            }
        }
    }

    // (carrier, how it reaches the gateway, developer, governed, reason code)
    let matrix: [(&str, Carrier, &str, &str, &str); 5] = [
        (
            "json-text",
            Carrier::Request(text),
            "forward",
            "forward",
            "-",
        ),
        (
            "media-block",
            Carrier::Request(image),
            "forward-marked",
            "refuse",
            "coverage.unsupported_media",
        ),
        (
            "provider-sealed",
            Carrier::Request(sealed),
            "forward-marked",
            "refuse",
            "egress.opaque_provider_material",
        ),
        (
            "opaque-body",
            Carrier::Opaque,
            "forward-marked",
            "refuse",
            "egress.opaque_payload",
        ),
        (
            "tool-result-media",
            Carrier::ToolResultMedia,
            "forward",
            "refuse",
            "coverage.unsupported_media",
        ),
    ];
    let mut observed = Vec::new();
    for (mode, config) in [
        ("developer", ""),
        ("governed", "[context_gateway]\nmode = \"governed\"\n"),
    ] {
        let _env = isolated(config);
        for (name, carrier, ..) in &matrix {
            let (verdict, codes) = observe(*carrier);
            observed.push((*name, mode, verdict, codes));
        }
    }
    for (name, _, developer, governed, code) in &matrix {
        for (mode, expected) in [("developer", developer), ("governed", governed)] {
            let (_, _, verdict, codes) = observed
                .iter()
                .find(|(n, m, ..)| n == name && *m == mode)
                .expect("observed");
            assert_eq!(verdict, expected, "{name} in {mode}: {observed:#?}");
            // A refusal names the carrier's code, and so does the receipt of a
            // marked carrier: an uninspected payload is never claimed clean.
            if *verdict == "forward" {
                assert_eq!(codes, "-", "{name} in {mode}");
            } else {
                assert!(codes.contains(code), "{name} in {mode}: {codes}");
            }
        }
    }

    // The document lists exactly these rows.
    let document = include_str!("../../../../docs/reference/carrier-matrix.md");
    let rows: Vec<Vec<String>> = document
        .lines()
        .filter(|line| line.starts_with("| `"))
        .map(|line| {
            line.trim_matches('|')
                .split('|')
                .map(|cell| cell.trim().trim_matches('`').to_owned())
                .collect()
        })
        .collect();
    let expected: Vec<Vec<String>> = matrix
        .iter()
        .map(|(name, _, developer, governed, code)| {
            vec![*name, developer, governed, code]
                .into_iter()
                .map(str::to_owned)
                .collect()
        })
        .collect();
    let documented: Vec<Vec<String>> = rows
        .iter()
        .map(|row| {
            // Carrier, developer, governed, reason code; the prose columns
            // (what it is, where it is decided) are not compared.
            [0, 3, 4, 5]
                .iter()
                .map(|&column| row.get(column).cloned().unwrap_or_default())
                .collect()
        })
        .collect();
    assert_eq!(documented, expected, "docs/reference/carrier-matrix.md");
}

/// E07: after one governed round on a call route (an MCP tool reading a file,
/// the session recording a task) and one on the proxy, no file anywhere under
/// the data directory — receipts, trace sidecars, egress records, session,
/// caches, indexes, the debug log — holds the credential or the card number
/// the round carried. Each store has its own test; this one catches a new
/// store, log or sidecar that writes what the gateway masked.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial_test::serial(cache_telemetry)]
async fn a_governed_round_leaves_no_canary_anywhere_under_the_data_dir() {
    let env = isolated("debug_log = true\n[context_gateway]\nmode = \"governed\"\n");
    let canaries = [AWS_KEY, VALID_CARD];
    let leaked = format!("aws_access_key_id = {AWS_KEY}\ncard = {VALID_CARD}\n");
    let project = tempfile::tempdir().expect("project");
    std::fs::create_dir(project.path().join(".git")).expect("project marker");
    std::fs::write(project.path().join("deploy.cfg"), &leaked).expect("source file");

    // Call route: an MCP tool reads the file, the session records a task.
    let engine = crate::engine::ContextEngine::with_project_root(project.path());
    let path = project.path().join("deploy.cfg");
    let read = engine
        .call_tool_text(
            "ctx_read",
            Some(json!({"path": path.to_string_lossy(), "mode": "full"})),
        )
        .await
        .expect("ctx_read");
    let task = engine
        .call_tool_text(
            "ctx_session",
            Some(json!({"action": "task", "value": format!("rotate {AWS_KEY}")})),
        )
        .await
        .expect("ctx_session");
    for (tool, text) in [("ctx_read", &read), ("ctx_session", &task)] {
        for canary in canaries {
            assert!(!text.contains(canary), "{tool} returned it: {text}");
        }
    }

    // Proxy route: the same content in a model request.
    let doc = simple_text_request(&leaked);
    let outcome = admit_request(&doc, &REMOTE);
    let forwarded = match &outcome.body {
        EgressBody::Rewritten(masked) => masked.to_string(),
        EgressBody::Refused(_) => String::new(),
        EgressBody::Unchanged => panic!("governed mode forwarded the canaries"),
    };
    egress::finish_admission_for(
        &outcome,
        (!forwarded.is_empty()).then_some(forwarded.as_bytes()),
        doc.to_string().len(),
        None,
        egress::EgressCaller::Proxy,
    )
    .expect("receipts stored");

    let mut pending = vec![env._data.path().to_path_buf()];
    let mut swept = 0;
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory).expect("read dir").flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
                continue;
            }
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            swept += 1;
            for canary in canaries {
                assert!(
                    !bytes
                        .windows(canary.len())
                        .any(|window| window == canary.as_bytes()),
                    "{} holds a canary",
                    path.display()
                );
            }
        }
    }
    assert!(swept > 0, "the round wrote nothing to sweep");
}
