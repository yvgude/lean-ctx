// SPDX-License-Identifier: Apache-2.0
//! Bus v2 shapes against the C0 2.0 `bus/` fixtures and control vectors.

use super::*;

const PUBLISH_FIELDS: [&str; 14] = [
    "schema",
    "client_msg_id",
    "to",
    "type",
    "worker_id",
    "thread_id",
    "reply_to",
    "refs",
    "body",
    "ck_wrapped",
    "epoch",
    "about",
    "counts",
    "window",
];

fn fixture(name: &str) -> Value {
    let path = format!(
        "{}/crates/lean-ctx-protocol/tests/fixtures/hive-v2/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// The `replace`, `remove` and `add` ops the invalid cases use.
fn patched(base: &Value, ops: &[Value]) -> Value {
    let mut doc = base.clone();
    for op in ops {
        let path = op["path"].as_str().unwrap();
        let (parent, last) = path.rsplit_once('/').unwrap();
        let target = doc.pointer_mut(parent).unwrap().as_object_mut().unwrap();
        match op["op"].as_str().unwrap() {
            "remove" => {
                target.remove(last);
            }
            "replace" | "add" => {
                target.insert(last.to_string(), op["value"].clone());
            }
            other => panic!("op {other}"),
        }
    }
    doc
}

/// A stored message reduced to what its engine published.
fn as_publish(message: &Value) -> Value {
    let mut out = Map::new();
    for field in PUBLISH_FIELDS {
        if let Some(v) = message.get(field) {
            out.insert(field.to_string(), v.clone());
        }
    }
    Value::Object(out)
}

fn publish_ok(doc: &Value) -> Result<(), Invalid> {
    serde_json::from_value::<Publish>(doc.clone())
        .map_err(|e| Invalid(e.to_string()))?
        .validate()
}

fn control_keys() -> ControlKeys {
    let keys = fixture("vectors/signing.vectors.json");
    let control = &keys["keys"]["hive_control"];
    let mut public = [0u8; 32];
    hex::decode_to_slice(control["public_key_hex"].as_str().unwrap(), &mut public).unwrap();
    BTreeMap::from([(
        control["key_id"].as_str().unwrap().to_string(),
        VerifyingKey::from_bytes(&public).unwrap(),
    )])
}

#[test]
fn collaboration_findings_carry_counts_and_a_window_without_refs() {
    let message = fixture("bus/message.finding.collaboration.json");
    let publish: Publish = serde_json::from_value(as_publish(&message)).unwrap();
    publish.validate().unwrap();
    assert_eq!(publish.about, Some(About::Collaboration));
    let counts = publish.counts.unwrap();
    assert_eq!(
        (counts.agent_seconds, counts.merge_conflict),
        (Some(5400), None)
    );
    assert_eq!(
        publish.window.unwrap().to_ms - publish.window.unwrap().from_ms,
        3_600_000
    );
    // The same finding without `about` needs refs and may not carry counts.
    let mut plain = publish.clone();
    plain.about = None;
    assert!(plain.validate().is_err());
    plain.counts = None;
    plain.window = None;
    assert!(plain.validate().is_err(), "a plain finding needs refs");
    let mut question = publish;
    question.kind = MessageType::Question;
    assert!(question.validate().is_err(), "findings only");
}

#[test]
fn engine_publishes_match_the_fixtures() {
    for name in ["publish.question", "publish.answer", "publish.sealed"] {
        let doc = fixture(&format!("bus/{name}.json"));
        publish_ok(&doc).unwrap_or_else(|e| panic!("{name}: {e}"));
        let back = serde_json::to_value(serde_json::from_value::<Publish>(doc.clone()).unwrap());
        assert_eq!(back.unwrap(), doc, "{name} round-trips");
    }
    // The frame form is the same body plus `t`.
    let frame = fixture("bus/frame.place_publish.json");
    let mut body = frame.clone();
    body.as_object_mut().unwrap().remove("t");
    publish_ok(&body).unwrap();
}

#[test]
fn the_invalid_cases_are_refused_before_they_leave() {
    let keys = control_keys();
    let dir = format!(
        "{}/crates/lean-ctx-protocol/tests/fixtures/hive-v2/bus/invalid",
        env!("CARGO_MANIFEST_DIR")
    );
    let mut checked = 0;
    for entry in std::fs::read_dir(dir).unwrap() {
        let case: Value =
            serde_json::from_str(&std::fs::read_to_string(entry.unwrap().path()).unwrap()).unwrap();
        let base_name = case["base"].as_str().unwrap();
        let base = fixture(base_name);
        let doc = patched(&base, case["patch"].as_array().unwrap());
        let name = case["case"].as_str().unwrap_or_default();
        let refused = match base_name.trim_start_matches("bus/") {
            b if b.starts_with("publish.") => publish_ok(&doc).is_err(),
            b if b.starts_with("message.") => {
                publish_ok(&as_publish(&doc)).is_err() || sender_of(&doc).is_err()
            }
            b if b.starts_with("control.") => serde_json::from_value::<Control>(doc.clone())
                .map_or(true, |c| c.verify(c.lifted_at.is_some(), &keys).is_err()),
            b if b.starts_with("frame.control") => match parse_frame(&doc.to_string()) {
                Ok(Inbound::Control { lifted, control }) => control.verify(lifted, &keys).is_err(),
                _ => true,
            },
            // Unknown frames and extra frame members are the server's to
            // refuse; the engine ignores what it does not read.
            _ => continue,
        };
        assert!(refused, "{name} must be refused");
        checked += 1;
    }
    assert!(checked >= 11, "{checked}");
}

/// C0 b000ffd: `issued_by.label` is read (a control carrying it is not
/// dropped as malformed) and is covered by the signature (C0 f5f72158
/// signs the fixture's own label).
#[test]
fn an_issuer_label_is_read_and_signed() {
    let keys = control_keys();
    let doc = fixture("bus/control.hold.json");
    let signed: Control = serde_json::from_value(doc.clone()).unwrap();
    let label = signed
        .issued_by
        .label
        .clone()
        .expect("the fixture is labelled");
    signed.verify(false, &keys).unwrap();
    let mut changed = doc.clone();
    changed["issued_by"]["label"] = format!("{label}-x").into();
    let mut removed = doc;
    removed["issued_by"]
        .as_object_mut()
        .unwrap()
        .remove("label");
    for (what, tampered) in [("changed", changed), ("removed", removed)] {
        let control: Control = serde_json::from_value(tampered).unwrap();
        assert_eq!(
            control.verify(false, &keys),
            Err(ControlRejection::SigInvalid),
            "a {what} label breaks the signature"
        );
    }
}

#[test]
fn a_web_only_place_role_on_a_stored_sender_is_refused() {
    let message = fixture("bus/message.question.json");
    assert!(matches!(sender_of(&message), Ok(Sender::Worker { .. })));
    let mut with_role = message;
    with_role["sender"]["place_role"] = "owner".into();
    assert!(sender_of(&with_role).is_err());
}

#[test]
fn controls_verify_against_the_vectors_and_tampering_fails() {
    let keys = control_keys();
    let vectors = fixture("vectors/control.vectors.json");
    for case in vectors["cases"].as_array().unwrap() {
        let control: Control =
            serde_json::from_value(fixture(case["fixture"].as_str().unwrap())).unwrap();
        let mut unsigned = serde_json::to_value(&control).unwrap();
        unsigned.as_object_mut().unwrap().remove("signature");
        let input = lean_ctx_protocol::hive::signing_input(CONTROL_DOMAIN, &unsigned).unwrap();
        assert_eq!(
            String::from_utf8(input).unwrap(),
            case["signing_input"].as_str().unwrap()
        );
        control.verify(control.lifted_at.is_some(), &keys).unwrap();
        // A key without purpose `control` (here: unknown) is refused.
        assert_eq!(
            control.verify(control.lifted_at.is_some(), &ControlKeys::new()),
            Err(ControlRejection::SigInvalid)
        );
    }
}

#[test]
fn frames_the_engine_reads_parse() {
    let text = |name: &str| fixture(&format!("bus/{name}.json")).to_string();
    assert!(matches!(
        parse_frame(&text("frame.message_stored")).unwrap(),
        Inbound::Stored { seq, .. } if seq >= 1
    ));
    assert!(matches!(
        parse_frame(&text("frame.presence_update")).unwrap(),
        Inbound::Presence {
            state: PresenceState::Idle,
            ..
        }
    ));
    assert!(matches!(
        parse_frame(&text("frame.control.lifted")).unwrap(),
        Inbound::Control { lifted: true, .. }
    ));
    assert!(matches!(
        parse_frame(&text("frame.place_denied")).unwrap(),
        Inbound::PlaceDenied { .. }
    ));
    assert!(matches!(
        parse_frame(&text("frame.resync")).unwrap(),
        Inbound::Resync { .. }
    ));
    assert!(matches!(
        parse_frame(&text("frame.error")).unwrap(),
        Inbound::Error { .. }
    ));
    assert!(matches!(
        parse_frame(&text("frame.ping")).unwrap(),
        Inbound::Ping
    ));
    assert!(matches!(
        parse_frame(&text("frame.message_deliver")).unwrap(),
        Inbound::Deliver(_)
    ));
    assert!(matches!(
        parse_frame(&text("frame.lease_granted")).unwrap(),
        Inbound::Other(_)
    ));
}

#[test]
fn ulids_are_well_formed_and_ordered_by_time() {
    let a = new_ulid(1_792_800_000_000);
    let b = new_ulid(1_792_800_000_001);
    assert!(is_ulid(&a) && is_ulid(&b), "{a} {b}");
    assert!(a[..10] < b[..10]);
    assert!(is_ulid("01M5NKA2Z8B000000000000001"));
    assert!(!is_ulid("01m5nka2z8b000000000000001"));
    assert!(!is_ulid("01M5NKA2Z8B00000000000000I"));
}

#[test]
fn phase_a_types_on_vault_buses() {
    let a: Vec<_> = [
        "question", "answer", "finding", "offer", "handoff", "progress",
    ]
    .iter()
    .map(|t| MessageType::parse(t).unwrap())
    .collect();
    assert!(a.iter().all(|t| t.in_phase_a()));
    for t in ["need", "claim", "result"] {
        assert!(!MessageType::parse(t).unwrap().in_phase_a());
    }
    assert_eq!(MessageType::parse("status"), None);
}
