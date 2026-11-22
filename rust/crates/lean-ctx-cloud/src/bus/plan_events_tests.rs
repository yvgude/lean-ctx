// SPDX-License-Identifier: Apache-2.0
//! The frames are inline: the synced C0 fixture tree is a byte copy of the
//! Cloud contract and gains `frame.plan_advanced.json` /
//! `frame.decision_changed.json` from Cloud (contract §37.7).

use super::*;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use ed25519_dalek::{Signer as _, SigningKey};

const PLAN_FRAME: &str =
    r##"{"t":"plan.advanced","vault":"#acme/payments","plan_id":"plan_K7mQ2xVb9Z","version":3}"##;
const DECISION_FRAME: &str = r##"{"t":"decision.changed","vault":"#acme/payments","decision_id":"dec-0042","status":"superseded","superseded_by":"dec-0051"}"##;

fn signer() -> SigningKey {
    SigningKey::from_bytes(&[7u8; 32])
}

fn keys() -> ControlKeys {
    BTreeMap::from([("ctl-1".to_string(), signer().verifying_key())])
}

/// The enrolled vault id as the vault map records it (UUID with hyphens).
const VAULT_ID: &str = "3f1c2b6a-8d4e-4a7b-9c10-5e2f6a7b8c01";
/// The same vault as Cloud writes it into the plan document (32 hex).
const PROJECT_ID: &str = "3f1c2b6a8d4e4a7b9c105e2f6a7b8c01";

/// The plan body `leanctx.plan-body/v1` of the fixture plan.
fn plan_body() -> Value {
    json!({
        "title": "Refunds",
        "goal": "Ship refunds for card payments.",
        "decisions": ["dec-0042"],
        "sessions": [
            {"slot": "api", "role": "backend", "brief": "Build POST /refunds.", "folder": "api"},
            {"slot": "web", "role": "frontend", "brief": "Add the refund button.", "folder": null}
        ]
    })
}

fn digest_of(bytes: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
}

/// The body as Cloud serves it (base64 of its canonical bytes) and its digest.
fn body() -> (String, String) {
    let bytes = lean_ctx_protocol::cloud::canonical_json(&plan_body()).expect("canonical");
    (STANDARD.encode(&bytes), digest_of(&bytes))
}

/// Plan v3 of the fixture frame, signed with `signer()`.
fn signed_doc() -> Value {
    let (body, digest) = body();
    let mut doc = json!({
        "schema": PLAN_SCHEMA,
        "plan_id": "plan_K7mQ2xVb9Z",
        "vault": PROJECT_ID,
        "version": 3,
        "supersedes_version": 2,
        "content_sha256": digest,
        "issued_by": "per_01",
        "body": body,
        "sessions": [
            {"slot": "api", "task_id": "t-7", "worker_id": "w-api",
             "brief_sha256": digest_of(b"Build POST /refunds."), "changed": true},
            {"slot": "web", "task_id": null, "worker_id": "w-web",
             "brief_sha256": digest_of(b"Add the refund button."), "changed": false}
        ]
    });
    sign(&mut doc);
    doc
}

/// Sign `doc` as Cloud does (p37 `cloud_keys::sign`): the statement
/// `{schema, vault, plan_id, version, content_sha256, issued_by, key_id}`,
/// `domain ‖ canonical_json(statement)`, the signature as 128 hex.
fn sign(doc: &mut Value) {
    let statement = json!({
        "schema": "leanctx.cloud.plan-statement/v1",
        "vault": doc["vault"],
        "plan_id": doc["plan_id"],
        "version": doc["version"],
        "content_sha256": doc["content_sha256"],
        "issued_by": doc["issued_by"],
        "key_id": "ctl-1",
    });
    let input = lean_ctx_protocol::cloud::signing_input_without_signature(PLAN_DOMAIN, &statement)
        .expect("signable");
    let sig = hex::encode(signer().sign(&input).to_bytes());
    doc["signature"] = json!({"key_id": "ctl-1", "sig": sig});
}

fn refused_signature(doc: &Value, frame: &PlanAdvanced) -> bool {
    matches!(
        verify(doc, frame, VAULT_ID, &keys()),
        Err(Rejected::Refuse {
            refusal: PlanRefusal::SignatureInvalid,
            ..
        })
    )
}

fn frame() -> PlanAdvanced {
    let Ok(super::super::v2::Inbound::PlanAdvanced(event)) =
        super::super::v2::parse_frame(PLAN_FRAME)
    else {
        panic!("plan.advanced parses");
    };
    *event
}

#[test]
fn the_frames_parse_and_bad_ids_do_not() {
    let event = frame();
    assert_eq!(
        event,
        PlanAdvanced::new("#acme/payments", "plan_K7mQ2xVb9Z", 3)
    );
    let Ok(super::super::v2::Inbound::DecisionChanged(changed)) =
        super::super::v2::parse_frame(DECISION_FRAME)
    else {
        panic!("decision.changed parses");
    };
    assert_eq!(changed.status, DecisionStatus::Superseded);
    assert_eq!(changed.superseded_by.as_deref(), Some("dec-0051"));
    for bad in [
        r##"{"t":"plan.advanced","vault":"#a/b","plan_id":"plan_short","version":1}"##,
        r##"{"t":"plan.advanced","vault":"#a/b","plan_id":"plan_K7mQ2xVb9Z","version":0}"##,
        r##"{"t":"plan.advanced","vault":"#a/b","plan_id":"plan_K7mQ2xVb9Z","version":1,"text":"x"}"##,
        r##"{"t":"decision.changed","vault":"#a/b","decision_id":"d-1","status":"revoked"}"##,
        r##"{"t":"decision.changed","vault":"#a/b","decision_id":"dec-1","status":"gone"}"##,
    ] {
        assert!(super::super::v2::parse_frame(bad).is_err(), "{bad}");
    }
}

#[test]
fn decision_ids_follow_clouds_kebab_shape_up_to_80_chars() {
    let frame = |id: &str| {
        format!(
            r##"{{"t":"decision.changed","vault":"#a/b","decision_id":"{id}","status":"revoked"}}"##
        )
    };
    let longest = format!("dec-{}-{}", "a".repeat(38), "9".repeat(37));
    assert_eq!(longest.len(), 80);
    for good in [longest.as_str(), "dec-0042", "dec-use-postgres-16"] {
        assert!(
            super::super::v2::parse_frame(&frame(good)).is_ok(),
            "{good}"
        );
    }
    let too_long = format!("{longest}0");
    assert_eq!(too_long.len(), 81);
    for bad in [
        too_long.as_str(),
        "dec-",
        "dec-a--b",
        "dec--a",
        "dec-a-",
        "dec-Use-postgres",
        "dec-a_b",
    ] {
        assert!(super::super::v2::parse_frame(&frame(bad)).is_err(), "{bad}");
    }
}

#[test]
fn a_signed_plan_verifies_and_a_changed_one_is_refused() {
    let doc = verify(&signed_doc(), &frame(), VAULT_ID, &keys()).expect("verified");
    assert_eq!(doc.sessions.len(), 2);
    let opened = doc.opened().expect("the verified body opens");
    assert_eq!(opened.goal, "Ship refunds for card payments.");
    assert_eq!(
        opened.session("web").map(|s| s.role.as_str()),
        Some("frontend")
    );

    // The signed statement binds the version, the vault, the body digest and
    // the issuer: each changed field (with a frame that announces it) fails.
    let mut later = signed_doc();
    later["version"] = json!(4);
    assert!(refused_signature(
        &later,
        &PlanAdvanced::new("#acme/payments", "plan_K7mQ2xVb9Z", 4)
    ));

    // A document moved to another vault, read from that vault, is not what
    // Cloud signed.
    let mut moved = signed_doc();
    moved["vault"] = json!("aaaabbbbccccddddeeeeffff00001111");
    assert!(matches!(
        verify(
            &moved,
            &frame(),
            "aaaabbbb-cccc-dddd-eeee-ffff00001111",
            &keys()
        ),
        Err(Rejected::Refuse {
            refusal: PlanRefusal::SignatureInvalid,
            ..
        })
    ));

    // Another body with its own (matching) digest is not what Cloud signed.
    let mut rewritten = signed_doc();
    let body = b"another body";
    rewritten["body"] = json!(STANDARD.encode(body));
    rewritten["content_sha256"] = json!(format!("sha256:{}", hex::encode(Sha256::digest(body))));
    assert!(refused_signature(&rewritten, &frame()));

    let mut reissued = signed_doc();
    reissued["issued_by"] = json!("per_02");
    assert!(refused_signature(&reissued, &frame()));

    let mut unsigned = signed_doc();
    unsigned["signature"] = Value::Null;
    assert!(refused_signature(&unsigned, &frame()));

    let mut unknown_key = signed_doc();
    unknown_key["signature"]["key_id"] = json!("ctl-9");
    assert!(refused_signature(&unknown_key, &frame()));

    let mut swapped = signed_doc();
    swapped["body"] = json!(STANDARD.encode(b"another body"));
    assert!(matches!(
        verify(&swapped, &frame(), VAULT_ID, &keys()),
        Err(Rejected::Refuse {
            refusal: PlanRefusal::DigestMismatch,
            ..
        })
    ));

    // Another version than announced is no answer to this frame.
    let other = PlanAdvanced::new("#acme/payments", "plan_K7mQ2xVb9Z", 4);
    assert!(matches!(
        verify(&signed_doc(), &other, VAULT_ID, &keys()),
        Err(Rejected::Invalid(_))
    ));
}

#[test]
fn the_document_vault_binds_to_the_enrolled_vault_not_the_frame_address() {
    // The frame names the subscription address "#acme/payments", the document
    // the vault id (32 hex): the address only routes.
    assert_eq!(frame().vault, "#acme/payments");
    assert!(verify(&signed_doc(), &frame(), VAULT_ID, &keys()).is_ok());
    // The same id with hyphens, in upper case or bare, is the same vault.
    for same in [VAULT_ID.to_ascii_uppercase().as_str(), PROJECT_ID] {
        assert!(
            verify(&signed_doc(), &frame(), same, &keys()).is_ok(),
            "{same}"
        );
    }
    // A validly signed plan of another vault, read from this one, is refused.
    assert!(matches!(
        verify(
            &signed_doc(),
            &frame(),
            "aaaabbbb-cccc-dddd-eeee-ffff00001111",
            &keys()
        ),
        Err(Rejected::Invalid(_))
    ));
}

#[test]
fn an_enrolled_vault_id_that_is_no_32_hex_id_binds_nothing() {
    for id in [
        "",
        "#acme/payments",
        "zzzzbbbbccccddddeeeeffff00001111",
        "3f1c2b6a",
    ] {
        let mut doc = signed_doc();
        doc["vault"] = json!(id);
        assert!(
            matches!(
                verify(&doc, &frame(), id, &keys()),
                Err(Rejected::Invalid(_))
            ),
            "{id:?}"
        );
    }
}

#[test]
fn versions_only_move_forward() {
    let mut seen = PlansSeen::new();
    assert!(admit(&mut seen, "#acme/payments", "plan_K7mQ2xVb9Z", 3));
    assert!(!admit(&mut seen, "#acme/payments", "plan_K7mQ2xVb9Z", 3));
    assert!(!admit(&mut seen, "#acme/payments", "plan_K7mQ2xVb9Z", 2));
    assert!(admit(&mut seen, "#acme/payments", "plan_K7mQ2xVb9Z", 4));
    assert_eq!(applied(&seen, "#acme/payments", "plan_K7mQ2xVb9Z"), Some(4));
}

#[test]
fn the_catch_up_pulls_only_newer_versions_in_plan_order() {
    let mut seen = PlansSeen::new();
    admit(&mut seen, "#acme/payments", "plan_K7mQ2xVb9Z", 3);
    let listed = json!({"schema": "leanctx.cloud.plans/v1", "vault": PROJECT_ID, "items": [
        {"plan_id": "plan_Zz00000000", "version": 1},
        {"plan_id": "plan_K7mQ2xVb9Z", "version": 3},
        {"plan_id": "plan_Aa11111111", "version": 2},
        {"plan_id": "bad", "version": 9}
    ]});
    let events = active_newer(&listed, "#acme/payments", &seen);
    let ids: Vec<(&str, u64)> = events
        .iter()
        .map(|e| (e.plan_id.as_str(), e.version))
        .collect();
    assert_eq!(ids, [("plan_Aa11111111", 2), ("plan_Zz00000000", 1)]);
    // The earlier `plans` name still reads.
    let earlier = json!({"plans": [{"plan_id": "plan_Aa11111111", "version": 2}]});
    assert_eq!(active_newer(&earlier, "#acme/payments", &seen).len(), 1);
}

#[test]
fn named_agents_get_their_session_and_the_rest_a_plan_wide_directive() {
    let doc = verify(&signed_doc(), &frame(), VAULT_ID, &keys()).unwrap();
    let local = vec![
        ("agent-a".to_string(), Some("w-api".to_string())),
        ("agent-b".to_string(), None),
    ];
    let issued = plan_directives(&doc, "vlt_1", &local);
    let (agent, issue) = &issued[0];
    assert_eq!(agent, "agent-a");
    assert_eq!(issue.directive_id, "plan_K7mQ2xVb9Z:3:api");
    assert_eq!(issue.task_id.as_deref(), Some("t-7"));
    assert_eq!(
        issue.brief_sha256.as_deref(),
        Some(digest_of(b"Build POST /refunds.").as_str())
    );
    let goal = digest_of(b"Ship refunds for card payments.");
    assert_eq!(issue.goal_sha256.as_deref(), Some(goal.as_str()));
    // An offered plan directive carries no text (§37.4).
    assert_eq!(issue.text, "");
    let (agent, issue) = &issued[1];
    assert_eq!(agent, "agent-b");
    assert_eq!(issue.directive_id, "plan_K7mQ2xVb9Z:3:all");
    assert_eq!(issue.slot.as_deref(), Some(ALL_SLOT));
    assert_eq!(issue.worker_id, None);
    assert_eq!(issue.brief_sha256, None);
    assert_eq!(issue.goal_sha256.as_deref(), Some(goal.as_str()));
}

/// Cloud does not check the routing against the body; the engine does: a
/// routed slot the body lacks, or another brief digest, offers nothing.
#[test]
fn the_routing_must_name_parts_of_the_verified_body() {
    let refused_digest = |doc: &Value| {
        matches!(
            verify(doc, &frame(), VAULT_ID, &keys()),
            Err(Rejected::Refuse {
                refusal: PlanRefusal::DigestMismatch,
                ..
            })
        )
    };
    let mut unknown_slot = signed_doc();
    unknown_slot["sessions"][1]["slot"] = json!("ops");
    assert!(refused_digest(&unknown_slot));
    let mut other_brief = signed_doc();
    other_brief["sessions"][0]["brief_sha256"] = json!(digest_of(b"Delete the refunds table."));
    assert!(refused_digest(&other_brief));
    // A session routed to every agent (`all`) names no part and has no brief.
    let mut everyone = signed_doc();
    everyone["sessions"][1]["slot"] = json!("all");
    let doc = verify(&everyone, &frame(), VAULT_ID, &keys()).expect("all routes");
    let local = vec![("agent-w".to_string(), Some("w-web".to_string()))];
    let issued = plan_directives(&doc, "vlt_1", &local);
    assert_eq!(issued[0].1.directive_id, "plan_K7mQ2xVb9Z:3:all");
    assert_eq!(issued[0].1.brief_sha256, None);
}

/// A signed body that is no `leanctx.plan-body/v1` is not taken.
#[test]
fn a_signed_body_that_is_no_plan_body_is_invalid() {
    for bad in [
        json!({"title": "t", "goal": "g", "sessions": [], "owner": "x"}),
        json!({"title": "t", "goal": "g", "sessions": [
            {"slot": "api", "role": "r", "brief": "b"},
            {"slot": "api", "role": "r", "brief": "b"}]}),
        json!({"title": "t", "goal": "g\u{202e}", "sessions": []}),
        json!({"title": "t", "goal": "g", "decisions": ["d-1"], "sessions": []}),
        json!({"title": "t", "goal": "x".repeat(4001), "sessions": []}),
    ] {
        let bytes = serde_json::to_vec(&bad).unwrap();
        let mut doc = signed_doc();
        doc["body"] = json!(STANDARD.encode(&bytes));
        doc["content_sha256"] = json!(digest_of(&bytes));
        doc["sessions"] = json!([]);
        sign(&mut doc);
        assert!(
            matches!(
                verify(&doc, &frame(), VAULT_ID, &keys()),
                Err(Rejected::Invalid(_))
            ),
            "{bad}"
        );
    }
    let mut empty = signed_doc();
    empty["body"] = json!("");
    empty["content_sha256"] = json!(digest_of(b""));
    sign(&mut empty);
    assert!(matches!(
        verify(&empty, &frame(), VAULT_ID, &keys()),
        Err(Rejected::Invalid(_))
    ));
}

#[test]
fn acks_group_per_version_without_local_ids_or_reasons() {
    let directive = |agent: &str, state, reason_code: Option<&str>| Directive {
        directive_id: format!("plan_K7mQ2xVb9Z:3:{agent}"),
        agent_id: agent.to_string(),
        project_root: "/p".to_string(),
        kind: DirectiveKind::Plan,
        subject: "plan_K7mQ2xVb9Z".to_string(),
        version: 3,
        slot: Some("api".to_string()),
        worker_id: Some("w-api".to_string()),
        task_id: None,
        vault_id: "vlt_1".to_string(),
        digest: body().1,
        message_id: "m".to_string(),
        state,
        reason_code: reason_code.map(str::to_string),
        reason: Some("private words".to_string()),
        reported: None,
        text: String::new(),
        release_sha256: None,
        release_id: None,
        released_at: None,
        brief_sha256: None,
        goal_sha256: None,
    };
    let a = directive("agent-a", DirectiveState::Accepted, None);
    let b = directive("agent-b", DirectiveState::Refused, Some("task_finished"));
    let requests = ack_requests(&[&b, &a], &|agent| format!("ag-{agent}"), &|_| "claude");
    assert_eq!(requests.len(), 1);
    let body = &requests[0].body;
    assert_eq!(body["version"], 3);
    assert_eq!(body["agents"][0]["agent_ref"], "ag-agent-a");
    assert_eq!(body["agents"][0]["state"], "acked");
    assert_eq!(body["agents"][1]["state"], "refused");
    assert_eq!(body["agents"][1]["reason_code"], "task_finished");
    assert!(!body.to_string().contains("private words"));
    // Same states, same key; a moved state, a new key.
    let again = ack_requests(&[&a, &b], &|agent| format!("ag-{agent}"), &|_| "claude");
    assert_eq!(again[0].key, requests[0].key);
    let pending = directive("agent-a", DirectiveState::Pending, None);
    let moved = ack_requests(
        &[&pending, &b],
        &|agent| format!("ag-{agent}"),
        &|_| "claude",
    );
    assert_ne!(moved[0].key, requests[0].key);

    let refusal = refusal_request(
        "vlt_1",
        "plan_K7mQ2xVb9Z",
        3,
        &super::tests::body().1,
        PlanRefusal::DigestMismatch,
        Some(2),
    );
    assert_eq!(
        refusal.body["refused"],
        json!({"reason_code": "digest_mismatch", "applied_version": 2})
    );
}

#[test]
fn a_decision_change_asks_for_a_re_read() {
    let Ok(super::super::v2::Inbound::DecisionChanged(changed)) =
        super::super::v2::parse_frame(DECISION_FRAME)
    else {
        panic!("decision.changed parses");
    };
    let issue = decision_directive(&changed, "vlt_1", &[]);
    assert_eq!(issue.directive_id, "dec-0042:superseded:1");
    assert!(
        issue
            .text
            .starts_with("LeanCTX Cloud: team decision dec-0042 was superseded by dec-0051; work that relies on dec-0042 may no longer apply."),
        "{}",
        issue.text
    );
}

#[test]
fn an_active_decision_is_not_called_stale() {
    let Ok(super::super::v2::Inbound::DecisionChanged(mut changed)) =
        super::super::v2::parse_frame(DECISION_FRAME)
    else {
        panic!("decision.changed parses");
    };
    changed.status = DecisionStatus::Active;
    changed.superseded_by = None;
    let issue = decision_directive(&changed, "vlt_1", &[]);
    assert!(
        issue.text.starts_with(
            "LeanCTX Cloud: team decision dec-0042 is now active; follow dec-0042 from here on."
        ),
        "{}",
        issue.text
    );
    assert!(!issue.text.contains("no longer apply"), "{}", issue.text);
}

#[test]
fn clouds_409_not_ready_means_plan_updates_are_not_available() {
    let answer = |code: &str| {
        crate::CloudError::from_response(
            409,
            &serde_json::json!({"error": {"code": code, "message": "no control key"}}),
        )
    };
    let not_ready = answer("not_ready");
    assert!(plans_not_ready(&not_ready));
    assert!(PLANS_NOT_READY.contains("not available"));
    // Any other refusal stays an error.
    assert!(!plans_not_ready(&answer("revision_conflict")));
    assert!(!plans_not_ready(&crate::CloudError::Server {
        status: 503,
        code: "not_ready".into(),
    }));
}

#[test]
fn a_decision_made_active_again_gets_a_new_directive() {
    let change = |status: DecisionStatus| DecisionChanged {
        t: None,
        vault: "#acme/payments".to_string(),
        decision_id: "dec-0042".to_string(),
        status,
        superseded_by: None,
    };
    let record = |issue: &DirectiveIssue| Directive {
        directive_id: issue.directive_id.clone(),
        agent_id: "agent-a".to_string(),
        project_root: "/p".to_string(),
        kind: DirectiveKind::Decision,
        subject: issue.subject.clone(),
        version: 0,
        slot: None,
        worker_id: None,
        task_id: None,
        vault_id: "vlt_1".to_string(),
        digest: String::new(),
        message_id: "m".to_string(),
        state: DirectiveState::Accepted,
        reason_code: None,
        reason: None,
        reported: None,
        text: String::new(),
        release_sha256: None,
        release_id: None,
        released_at: None,
        brief_sha256: None,
        goal_sha256: None,
    };
    let mut known = Vec::new();
    for (status, id) in [
        (DecisionStatus::Active, "dec-0042:active:1"),
        (DecisionStatus::Superseded, "dec-0042:superseded:2"),
        // A frame delivered twice keeps its directive.
        (DecisionStatus::Superseded, "dec-0042:superseded:2"),
        (DecisionStatus::Active, "dec-0042:active:3"),
    ] {
        let issue = decision_directive(&change(status), "vlt_1", &known);
        assert_eq!(issue.directive_id, id);
        known.push(record(&issue));
    }
}

#[test]
fn plan_frames_are_taken_only_when_the_hello_ack_names_the_feature() {
    assert!(frames_taken(
        &json!({"t": "hello.ack", "features": ["plan_updates"]})
    ));
    assert!(!frames_taken(
        &json!({"t": "hello.ack", "features": ["policy_advanced"]})
    ));
    assert!(!frames_taken(&json!({"t": "hello.ack"})));
}

#[test]
fn a_decision_changed_frame_is_delivered_only_from_the_frame() {
    // Cloud sends it only after the person's confirmed "Send to agents":
    // the relay delivers it in the frame's arm and nowhere else.
    assert!(matches!(
        super::super::v2::parse_frame(DECISION_FRAME),
        Ok(super::super::v2::Inbound::DecisionChanged(_))
    ));
    let session = include_str!("ws/session.rs");
    let arm = session
        .find("Inbound::DecisionChanged(event) =>")
        .expect("the relay takes the frame");
    let call = session
        .find("plan_events::deliver_decision(")
        .expect("the relay delivers it");
    assert!(
        call > arm && call - arm < 400,
        "delivered in the frame's arm"
    );
    assert_eq!(session.matches("deliver_decision(").count(), 1);
    // No catch-up, sync or list delivers or issues one.
    for (name, source) in [
        ("plan_events.rs", include_str!("plan_events.rs")),
        ("relay_v2.rs", include_str!("relay_v2.rs")),
        ("task_events.rs", include_str!("task_events.rs")),
        ("../sync/auto.rs", include_str!("../sync/auto.rs")),
    ] {
        // The delivery itself posts through the store's method; nothing
        // else calls it.
        let calls = source.matches("deliver_decision(").count();
        let defined = source.matches("fn deliver_decision(").count()
            + source.matches("store.deliver_decision(").count();
        assert_eq!(calls, defined, "{name} delivers no decision");
        let issued = source.matches("decision_directive(").count();
        let defined = source.matches("fn decision_directive(").count();
        assert_eq!(
            issued,
            defined + source.matches("= decision_directive(event").count(),
            "{name} issues no decision directive"
        );
    }
}

#[test]
fn another_successor_gets_a_new_decision_directive() {
    let change = |by: &str| DecisionChanged {
        t: None,
        vault: "#acme/payments".to_string(),
        decision_id: "dec-0042".to_string(),
        status: DecisionStatus::Superseded,
        superseded_by: Some(by.to_string()),
    };
    let first = decision_directive(&change("dec-0051"), "vlt_1", &[]);
    assert_eq!(first.digest, "dec-0051");
    let known = vec![Directive {
        directive_id: first.directive_id.clone(),
        agent_id: "agent-a".to_string(),
        project_root: "/p".to_string(),
        kind: DirectiveKind::Decision,
        subject: first.subject.clone(),
        version: 0,
        slot: None,
        worker_id: None,
        task_id: None,
        vault_id: "vlt_1".to_string(),
        digest: first.digest.clone(),
        message_id: "m".to_string(),
        state: DirectiveState::Pending,
        reason_code: None,
        reason: None,
        reported: None,
        text: String::new(),
        release_sha256: None,
        release_id: None,
        released_at: None,
        brief_sha256: None,
        goal_sha256: None,
    }];
    let again = decision_directive(&change("dec-0051"), "vlt_1", &known);
    assert_eq!(again.directive_id, "dec-0042:superseded:1");
    let other = decision_directive(&change("dec-0060"), "vlt_1", &known);
    assert_eq!(other.directive_id, "dec-0042:superseded:2");
}

#[cfg(feature = "http-server")]
mod decision_delivery {
    use super::*;
    use crate::vault::{VaultMap, VaultRecord};
    use lean_ctx::core::agents::{AgentRegistry, DirectiveStore};

    struct Setup {
        _data: lean_ctx::core::data_dir::IsolatedDataDir,
        _dir: tempfile::TempDir,
        registry: AgentRegistry,
        map: VaultMap,
        agent: String,
    }

    fn setup() -> Setup {
        let data = lean_ctx::core::data_dir::isolated_data_dir();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_string_lossy().into_owned();
        let mut registry = AgentRegistry::new();
        let agent = registry
            .register("claude", Some("backend"), &root, None)
            .unwrap();
        let mut map = VaultMap::default();
        map.vaults.insert(
            "s".repeat(64),
            VaultRecord {
                vault_id: "vlt_1".into(),
                ns: "acme".into(),
                name: "payments".into(),
                sealed: false,
                roots: vec![root],
            },
        );
        Setup {
            _data: data,
            _dir: dir,
            registry,
            map,
            agent,
        }
    }

    fn frame() -> DecisionChanged {
        let Ok(super::super::super::v2::Inbound::DecisionChanged(changed)) =
            super::super::super::v2::parse_frame(DECISION_FRAME)
        else {
            panic!("decision.changed parses");
        };
        *changed
    }

    fn posted(registry: &AgentRegistry) -> usize {
        registry
            .scratchpad
            .iter()
            .filter(|e| e.metadata.get("decision_id").map(String::as_str) == Some("dec-0042"))
            .count()
    }

    #[test]
    fn the_frame_posts_the_decision_to_the_agents_at_once() {
        let mut s = setup();
        let mut store = DirectiveStore::default();
        let event = frame();
        let delivered = deliver_decision(&mut s.registry, &mut store, &s.map, &event).unwrap();
        assert_eq!(delivered, 1);
        assert_eq!(posted(&s.registry), 1);
        let record = &store.directives[0];
        assert_eq!(record.agent_id, s.agent);
        assert_eq!(record.directive_id, "dec-0042:superseded:1");
        assert_eq!(record.state, DirectiveState::Pending, "no second offer");
        assert!(record.release_id.is_none());
    }

    #[test]
    fn a_frame_delivered_twice_posts_once() {
        let mut s = setup();
        let mut store = DirectiveStore::default();
        let event = frame();
        deliver_decision(&mut s.registry, &mut store, &s.map, &event).unwrap();
        let again = deliver_decision(&mut s.registry, &mut store, &s.map, &event).unwrap();
        assert_eq!(again, 0);
        assert_eq!(posted(&s.registry), 1);
        // Another successor is a new change.
        let mut other = event.clone();
        other.superseded_by = Some("dec-0060".to_string());
        assert_eq!(
            deliver_decision(&mut s.registry, &mut store, &s.map, &other).unwrap(),
            1
        );
        assert_eq!(posted(&s.registry), 2);
    }

    /// A frame whose post failed after its directive was recorded leaves it
    /// offered; the next frame releases it instead of skipping it.
    #[test]
    fn a_decision_left_offered_is_released_by_the_next_frame() {
        let mut s = setup();
        let mut store = DirectiveStore::default();
        let event = frame();
        let (record, root) = record_of(&s.map, &event.vault).expect("mapped vault");
        let issue = decision_directive(&event, &record.vault_id, &store.directives);
        let root = root.to_string();
        // What a release that failed right after `issue()` leaves behind.
        assert!(store.issue(&root, &s.agent, &issue).unwrap());
        assert_eq!(store.directives[0].state, DirectiveState::Offered);
        assert_eq!(posted(&s.registry), 0);

        let delivered = deliver_decision(&mut s.registry, &mut store, &s.map, &event).unwrap();
        assert_eq!(delivered, 1);
        assert_eq!(posted(&s.registry), 1);
        assert_eq!(store.directives[0].state, DirectiveState::Pending);
        assert_eq!(
            deliver_decision(&mut s.registry, &mut store, &s.map, &event).unwrap(),
            0
        );
        assert_eq!(posted(&s.registry), 1);
    }

    /// One agent that cannot take the decision does not keep it from the
    /// others; its error is reported.
    #[test]
    fn a_failing_agent_does_not_stop_the_others() {
        let mut s = setup();
        let root = s.map.vaults.values().next().unwrap().roots[0].clone();
        // A second presence of this test process, next to the first.
        let other = s
            .registry
            .register_child("claude", "frontend", &root, &s.agent)
            .unwrap();
        let mut store = DirectiveStore::default();
        let event = frame();
        let (record, _) = record_of(&s.map, &event.vault).expect("mapped vault");
        let issue = decision_directive(&event, &record.vault_id, &store.directives);
        // A record for the first agent that cannot be released.
        assert!(store.issue(&root, &s.agent, &issue).unwrap());
        store.directives[0].release_id = None;

        let error = deliver_decision(&mut s.registry, &mut store, &s.map, &event)
            .expect_err("the broken record is reported");
        assert!(error.contains(&format!("agent {}", s.agent)), "{error}");
        assert!(error.contains("reached 1 agent(s)"), "{error}");
        assert_eq!(posted(&s.registry), 1);
        let theirs = store
            .directives
            .iter()
            .find(|d| d.agent_id == other)
            .expect("the other agent's record");
        assert_eq!(theirs.state, DirectiveState::Pending);
    }

    #[test]
    fn a_frame_for_another_vault_posts_nothing() {
        let mut s = setup();
        let mut store = DirectiveStore::default();
        let mut event = frame();
        event.vault = "#acme/other".to_string();
        assert_eq!(
            deliver_decision(&mut s.registry, &mut store, &s.map, &event).unwrap(),
            0
        );
        assert_eq!(posted(&s.registry), 0);
        assert!(store.directives.is_empty());
    }
}

/// Cloud's signed test vector (TEST key: `CloudKeys::from_seeds`, control
/// seed 0x01..0x20), byte for byte as LeanCTX Cloud keeps it in
/// `contracts/cloud-v2/fixtures/vectors/plan_statement.vectors.json`
/// (sha256 `7d1fa2bb03457bf1db78ddc59da0bcd8bffa75f49829189435c9d3ae9681ef26`):
/// the pulled document, its signed statement and the publish request for
/// the same body, as Cloud builds them.
const PLAN_VECTOR: &str = include_str!("../../tests/fixtures/plan.vector.json");

#[test]
fn clouds_signed_vector_verifies_opens_routes_and_publishes_the_same_bytes() {
    let vector: Value = serde_json::from_str(PLAN_VECTOR).expect("vector");
    let text = |field: &str| vector[field].as_str().expect(field).to_owned();
    assert_eq!(
        super::super::plan_body::sha256_of(PLAN_VECTOR.as_bytes()),
        "sha256:7d1fa2bb03457bf1db78ddc59da0bcd8bffa75f49829189435c9d3ae9681ef26"
    );
    // (a) The public key is the control seed's, the key id is the first 16
    // hex of blake3(public key), and the signature is over exactly the
    // domain and the canonical statement.
    assert_eq!(text("purpose"), "control");
    let seed: [u8; 32] = hex::decode(text("seed_hex")).unwrap().try_into().unwrap();
    let pk = ed25519_dalek::SigningKey::from_bytes(&seed)
        .verifying_key()
        .to_bytes();
    assert_eq!(hex::encode(pk), text("public_key_hex"));
    let key_id = &hex::encode(blake3::hash(&pk).as_bytes())[..16];
    assert_eq!(key_id, text("key_id"));
    let key = ed25519_dalek::VerifyingKey::from_bytes(&pk).unwrap();
    let mut doc_value = vector["document"].clone();
    assert_eq!(doc_value["signature"]["key_id"], key_id);
    let sig: [u8; 64] = hex::decode(doc_value["signature"]["sig"].as_str().unwrap())
        .unwrap()
        .try_into()
        .unwrap();
    let signed = text("signed_message_utf8");
    assert_eq!(text("domain"), PLAN_DOMAIN);
    assert_eq!(signed, format!("{}{}", text("domain"), text("statement")));
    assert_eq!(
        hex::decode(text("signed_message_hex")).unwrap(),
        signed.as_bytes()
    );
    assert_eq!(
        text("signature"),
        doc_value["signature"]["sig"].as_str().unwrap()
    );
    assert_eq!(text("body_b64"), doc_value["body"].as_str().unwrap());
    key.verify_strict(
        signed.as_bytes(),
        &ed25519_dalek::Signature::from_bytes(&sig),
    )
    .expect("the vector's signature");
    // The engine rebuilds the same canonical statement from the document
    // alone.
    let doc: PlanDoc = serde_json::from_value(doc_value.clone()).expect("plan doc");
    let statement = plan_statement(&doc, key_id);
    assert_eq!(
        lean_ctx_protocol::cloud::canonical_json(&statement).unwrap(),
        text("statement").as_bytes()
    );
    assert_eq!(
        lean_ctx_protocol::cloud::signing_input_without_signature(
            PLAN_DOMAIN,
            &plan_statement(&doc, key_id)
        )
        .unwrap(),
        signed.as_bytes()
    );
    // A field this engine does not know is tolerated, and the whole
    // document verifies against the enrolled vault.
    doc_value["sealed"] = json!(null);
    let keys = BTreeMap::from([(key_id.to_owned(), key)]);
    let frame = PlanAdvanced::new("#acme/vector", &doc.plan_id, doc.version);
    let taken = verify(&doc_value, &frame, &doc.vault, &keys).expect("the vector verifies");
    assert_eq!(
        taken.published_at.as_deref(),
        Some("2026-11-20T12:00:00.000Z")
    );
    // (b) The body decodes to body_utf8, hashes to content_sha256 and is
    // a plan body without decisions.
    let bytes = super::super::plan_body::decode_body(&taken.body).unwrap();
    assert_eq!(bytes, text("body_utf8").as_bytes());
    assert_eq!(
        super::super::plan_body::sha256_of(&bytes),
        text("content_sha256")
    );
    assert_eq!(taken.content_sha256, text("content_sha256"));
    let body = taken.opened().expect("a verified plan body");
    assert_eq!(
        (body.title.as_str(), body.decisions.as_ref()),
        ("Vector plan", None)
    );
    // (c) Each routed session's brief digest is the SHA-256 of its brief.
    assert_eq!(taken.sessions.len(), 2);
    for session in &taken.sessions {
        let part = body.session(&session.slot).expect("a slot of the body");
        assert_eq!(session.brief_sha256, digest_of(part.brief.as_bytes()));
        assert!(session.changed && session.task_id.is_none() && session.worker_id.is_none());
    }
    // (d) The engine's publish of body_utf8 under the plan id is Cloud's
    // request, with the same base64 body as the document.
    let body_value: Value = serde_json::from_str(&text("body_utf8")).unwrap();
    let input = json!({"idempotency_key": "6f1c2b6a-8d4e-4a7b-9c10-5e2f6a7b8c01",
                       "plan_id": doc.plan_id, "body": body_value});
    let publish = super::super::plan_body::publish_request(&input.to_string()).expect("publish");
    assert_eq!(publish.request, vector["publish_request"]);
    assert_eq!(publish.request["body"], vector["document"]["body"]);
    assert_eq!(publish.content_sha256, text("content_sha256"));
}
