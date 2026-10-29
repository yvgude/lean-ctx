// SPDX-License-Identifier: Apache-2.0

use super::*;

/// The 25./26.10. lockouts: a 403 `permission_error` made Claude Code report
/// `authentication_failed` and ask for `/login`. A withheld request is a 400
/// that names the withheld blocks and how a person proceeds.
#[tokio::test]
async fn a_withheld_request_is_actionable_and_never_reads_as_an_auth_failure() {
    let refusal = GatewayRefusal::from_reason(
        "request withheld by the context gateway (classification.flagged, destination.remote_restricted)",
    )
    .with_blocks(vec!["w-0123456789ab".into(), "not an id; rm -rf".into()]);
    assert_eq!(refusal.status(), StatusCode::BAD_REQUEST);
    let message = refusal.message();
    assert!(
        message.starts_with(
            "request withheld by the context gateway (classification.flagged, destination.remote_restricted)"
        ),
        "{message}"
    );
    assert!(message.contains("not a login problem"), "{message}");
    assert!(
        message.contains("`lean-ctx gateway why w-0123456789ab`"),
        "{message}"
    );
    assert!(
        message.contains("`lean-ctx gateway allow-once w-0123456789ab`"),
        "{message}"
    );
    assert!(
        !message.contains("rm -rf"),
        "only block ids survive: {message}"
    );
    assert_eq!(message, refusal.message(), "deterministic (#498)");
    assert_eq!(
        GatewayRefusal::from_reason(&message),
        GatewayRefusal::from_reason(
            "request withheld by the context gateway (classification.flagged, destination.remote_restricted)"
        ),
        "the codes stay parsable from the message"
    );
    for label in ["Anthropic", "OpenAI", "Gemini"] {
        let response = refusal.response(label);
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(response.headers()[WITHHELD_BLOCKS_HEADER], "w-0123456789ab");
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("refusal body");
        let body: Value = serde_json::from_slice(&body).expect("JSON refusal");
        let text = body.to_string();
        assert!(!text.contains("permission_error"), "{label}: {text}");
        assert!(!text.contains("authentication"), "{label}: {text}");
        assert!(!text.contains("PERMISSION_DENIED"), "{label}: {text}");
        assert_eq!(body["lean_ctx"]["blocks"][0], "w-0123456789ab", "{label}");
    }
    let unavailable = GatewayRefusal::unavailable("receipt.unavailable");
    assert_eq!(unavailable.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn refusal_is_typed_per_provider_shape_and_never_echoes_input() {
    let refusal = GatewayRefusal::from_reason(
        "request withheld by the context gateway (destination.remote_restricted, Leaked VALUE 42!)",
    );
    assert_eq!(refusal.status(), StatusCode::BAD_REQUEST);
    for (label, pointer) in [
        ("Anthropic", "/error/type"),
        ("OpenAI", "/error/code"),
        ("Gemini", "/error/status"),
    ] {
        let response = refusal.response(label);
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(response.headers()[REFUSAL_HEADER], "context_gateway");
        assert_eq!(
            response.headers()[REASON_CODES_HEADER],
            "destination.remote_restricted,eaked42",
            "only the reason-code alphabet survives"
        );
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("refusal body");
        let body: Value = serde_json::from_slice(&body).expect("JSON refusal");
        assert!(body.pointer(pointer).is_some(), "{label}: {body}");
        assert_eq!(
            body["lean_ctx"]["reason_codes"][0],
            "destination.remote_restricted"
        );
        assert!(!body.to_string().contains("VALUE"), "{label}: {body}");
    }
}

/// A signed unpin leaves no policy and is an organization decision all
/// the same: the proxy decides such a request inside the one view that
/// observed it, project-bound, and both payload shapes carry the
/// observation. It is no project policy: an opaque body is not withheld
/// for it. Without an observation and without a policy nothing is bound.
#[test]
fn an_observed_organization_decision_governs_a_request_that_has_no_policy() {
    use crate::core::context_admission::capture::OrgEvidence;
    use crate::core::policy::org::Observation;
    use crate::core::policy::runtime::with_observed_view_for_test;

    let _data = crate::core::data_dir::isolated_data_dir();
    let project = tempfile::tempdir().expect("project root");
    let target = EgressTarget {
        provider: "OpenAI",
        model: None,
        upstream_base: "http://127.0.0.1:9",
    };
    let binding = TrustedDestinationBinding::default();
    let request =
        serde_json::json!({"model": "m", "messages": [{"role": "user", "content": "hello"}]});
    for payload in [Payload::Document(&request), Payload::Opaque(4)] {
        let unbound =
            with_observed_view_for_test(project.path(), None, || admit(payload, &target, binding))
                .expect("admission");
        assert!(unbound.authority.project.is_none());
        assert!(!matches!(
            unbound.outcome.org_evidence(),
            Some(OrgEvidence::One(_))
        ));

        let observed = Observation::unpinned_for_test(7, 'a');
        let governed = with_observed_view_for_test(project.path(), Some(observed), || {
            admit(payload, &target, binding)
        })
        .expect("admission");
        assert!(governed.authority.project.is_some(), "project-bound");
        assert!(governed.project_epoch_bound);
        assert!(!governed.is_refused(), "an unpin withholds nothing");
        match governed.outcome.org_evidence() {
            Some(OrgEvidence::One(named)) => assert_eq!(named.policy_version(), 7),
            other => panic!("the admission carries no observation: {other:?}"),
        }
    }
}

/// The decision on the bytes that leave is taken on the JOINED
/// classification: when a transformation removed the marking, the final
/// outcome still carries what the caller's content was classified as.
/// (The receipt has no classification field of its own; the outcome's is
/// the one the restricted-to-remote rule and the adapter act on.)
#[test]
fn final_admission_keeps_the_classification_of_the_callers_content() {
    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let target = EgressTarget {
        provider: "OpenAI",
        model: None,
        upstream_base: "http://127.0.0.1:9",
    };
    let binding = TrustedDestinationBinding::default();
    let request = |text: &str| serde_json::json!({"model": "m", "messages": [{"role": "user", "content": text}]});
    let marked = request("CONFIDENTIAL\nquarterly plan");
    let unmarked = request("quarterly plan");

    let alone = admit(Payload::Document(&unmarked), &target, binding)
        .expect("admission")
        .outcome
        .classification;
    let caller =
        admit_caller(Payload::Document(&marked), &target, binding, None).expect("caller admission");
    let original = caller.admission.outcome.classification;
    assert!(original > alone, "the marking raises the classification");

    let (joined, _) = admit_final(
        Payload::Document(&unmarked),
        &target,
        binding,
        caller,
        false,
    )
    .expect("final admission");
    assert!(!joined.is_refused());
    assert_eq!(joined.outcome.classification, original);
}

#[test]
fn a_body_is_bound_only_to_the_origin_of_its_admitted_upstream() {
    let base = "http://127.0.0.1:8080";
    assert!(bound_url(base, "http://127.0.0.1:8080/v1/messages?beta=true").is_some());
    for elsewhere in [
        "http://127.0.0.1:8080@other.example/v1/messages",
        "http://127.0.0.1:8081/v1/messages",
        "https://127.0.0.1:8080/v1/messages",
        "http://127.0.0.1:8080.other.example/v1/messages",
        "http://127.0.0.1:8080*",
    ] {
        assert!(bound_url(base, elsewhere).is_none(), "{elsewhere}");
    }
}

/// The receipt of a request states the blocks that stayed whole under
/// protection: one note per block, so its counts per code are those of
/// `KeptWhole` — and nothing else about the receipt moves. A request with
/// nothing kept whole has the receipt it has without the call.
#[test]
#[serial_test::serial(cache_telemetry)]
fn blocks_kept_whole_are_noted_in_the_receipt_and_are_no_sources() {
    use super::super::compress::{self, KeptWhole, Reader};
    use lean_ctx_protocol::context_gateway::{ContextDecisionReceiptV1, ContextDecisionV1};
    use std::collections::BTreeMap;

    let _data = crate::core::data_dir::isolated_data_dir();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);
    let target = EgressTarget {
        provider: "OpenAI",
        model: None,
        upstream_base: "http://127.0.0.1:9",
    };
    let binding = TrustedDestinationBinding::default();
    let request = serde_json::json!({"model": "m", "messages": [
        {"role": "tool", "tool_call_id": "c1", "content": "first block"},
        {"role": "tool", "tool_call_id": "c2", "content": "second block"},
        {"role": "user", "content": "continue"},
    ]});
    let bytes = serde_json::to_vec(&request).expect("bytes");
    let receipt_of = |kept: Option<&KeptWhole>| {
        let admission = admit(Payload::Document(&request), &target, binding).expect("admission");
        let admission = match kept {
            Some(kept) => admission.note_kept_whole(kept),
            None => admission,
        };
        egress::finish_admission(&admission.outcome, Some(&bytes), bytes.len(), None)
            .expect("receipt")
            .expect("evidence")
            .decision_receipt
            .expect("decision receipt")
    };
    // Two blocks, two different reasons; a block is counted once and its
    // first code stands.
    let ((), kept) = compress::observing(Reader::Model, || {
        compress::note_kept_whole("first block", "recovery.unknown_lineage");
        compress::note_kept_whole("second block", "recovery.no_project");
        compress::note_kept_whole("first block", "recovery.no_project");
    });
    let is_note = |decision: &&ContextDecisionV1| {
        decision
            .reason_codes
            .iter()
            .any(|code| code.as_str().starts_with("recovery."))
    };
    let without_clock = |receipt: &ContextDecisionReceiptV1| {
        let mut value = serde_json::to_value(receipt).expect("a receipt serializes");
        let fields = value.as_object_mut().expect("a receipt is an object");
        fields.remove("receipt_id");
        fields.remove("duration_us");
        value
    };

    let plain = receipt_of(None);
    assert_eq!(plain.decisions.iter().filter(is_note).count(), 0);
    assert_eq!(
        without_clock(&receipt_of(Some(&KeptWhole::default()))),
        without_clock(&plain),
        "nothing kept whole: the receipt of today"
    );

    let noted = receipt_of(Some(&kept));
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for decision in noted.decisions.iter().filter(is_note) {
        assert_eq!(decision.reason_codes.len(), 1, "one code per note");
        *counts.entry(decision.reason_codes[0].as_str()).or_insert(0) += 1;
    }
    assert_eq!(
        counts,
        kept.counts(),
        "exactly two notes, with the codes and counts of the pass"
    );
    assert_eq!(counts.values().sum::<usize>(), 2);
    assert_eq!(noted.sources, plain.sources, "a note is not a source");
    assert_eq!(noted.security, plain.security);
    assert_eq!(noted.final_context, plain.final_context);
    assert_eq!(
        noted
            .decisions
            .iter()
            .filter(|decision| !is_note(decision))
            .collect::<Vec<_>>(),
        plain.decisions.iter().collect::<Vec<_>>(),
        "the decisions about sources are untouched"
    );
    let written = serde_json::to_string(&noted).expect("a receipt serializes");
    assert!(
        !written.contains("first block") && !written.contains("second block"),
        "a note names no content"
    );
}
