// SPDX-License-Identifier: Apache-2.0
//! Proof forms against the C0 consent routes and the pledge-digest vectors.

use super::*;
use crate::core::hive::consent::{Channel, PresenceSignature, REQUEST_TTL_MS};

fn fixture(name: &str) -> Value {
    let path = format!(
        "{}/crates/lean-ctx-protocol/tests/fixtures/hive-v2/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn pledge(i: usize) -> Value {
    fixture("vectors/pledge_digest.vectors.json")["cases"][i]["pledge"].clone()
}

/// The approved record behind a C0 `consent_proof` object.
fn record_of(proof: &Value, alg: &str) -> Record {
    Record {
        consent_id: proof["consent_id"].as_str().unwrap().into(),
        kind: Kind::parse(proof["kind"].as_str().unwrap()).unwrap(),
        subject: proof["subject"].clone(),
        subject_digest: proof["subject_digest"].as_str().unwrap().into(),
        created_at: 0,
        expires_at: REQUEST_TTL_MS,
        state: State::Approved,
        channel: Some(Channel::OsPresence),
        approved_at: proof["approved_at"].as_u64(),
        proof: Some(Proof::Presence(PresenceSignature {
            key_id: proof["presence_key_id"].as_str().unwrap().into(),
            alg: alg.into(),
            signature: proof["signature"].as_str().unwrap().into(),
        })),
    }
}

#[test]
fn pledge_digests_match_the_vectors() {
    for case in fixture("vectors/pledge_digest.vectors.json")["cases"]
        .as_array()
        .unwrap()
    {
        assert_eq!(
            pledge_digest(&case["pledge"]).unwrap(),
            case["pledge_digest"].as_str().unwrap()
        );
    }
    // Absent fields count as null.
    let mut partial = pledge(0);
    partial.as_object_mut().unwrap().remove("window");
    assert_eq!(
        pledge_digest(&partial).unwrap(),
        pledge_digest(&pledge(0)).unwrap()
    );
}

#[test]
fn form_b_reproduces_the_swarm_join_request() {
    let request = fixture("routes/swarm_join.consent_proof.request.json");
    let proof = &request["consent_proof"];
    let subject = swarm_join_subject(
        proof["subject"]["swarm"].as_str().unwrap(),
        &pledge(0),
        "codex",
    )
    .unwrap();
    assert_eq!(subject, proof["subject"]);
    let record = record_of(proof, "ed25519");
    let at = record.approved_at.unwrap();
    let choice = choose(&record, true, at + 60_000).unwrap();
    let mut body = json!({"cli": "codex"});
    choice.attach(&mut body).unwrap();
    assert_eq!(body, request);
}

#[test]
fn form_b_reproduces_the_loosening_pledge_put() {
    let request = fixture("routes/pledge.loosen.consent_proof.request.json");
    let proof = &request["consent_proof"];
    let subject = pledge_change_subject(
        proof["subject"]["swarm"].as_str().unwrap(),
        &pledge(0),
        &pledge(1),
    )
    .unwrap();
    assert_eq!(subject, proof["subject"]);
    let record = record_of(proof, "p256_ecdsa_sha256");
    let mut body = pledge(1);
    choose(&record, true, record.approved_at.unwrap())
        .unwrap()
        .attach(&mut body)
        .unwrap();
    assert_eq!(body, request);
    assert!(loosens(&pledge(0), &pledge(1)));
    assert!(loosens(&pledge(0), &pledge(2)), "tasks 5 -> 6");
    assert!(!loosens(&pledge(2), &pledge(0)), "tasks 6 -> 5 tightens");
}

#[test]
fn windows_and_modes_loosen_or_tighten() {
    let mut narrow = pledge(1);
    narrow["window"]["days"] = json!(["mon"]);
    assert!(!loosens(&pledge(1), &narrow), "fewer days");
    assert!(loosens(&narrow, &pledge(1)), "more days");
    let mut later = pledge(1);
    later["window"]["to"] = "07:00".into();
    assert!(loosens(&pledge(1), &later), "changed hours");
    let mut interactive = pledge(1);
    interactive["mode"] = "interactive".into();
    assert!(loosens(&interactive, &pledge(1)), "to unattended");
    assert!(!loosens(&pledge(1), &interactive), "to interactive");
}

#[test]
fn the_engine_falls_back_to_the_web_when_form_b_cannot_be_used() {
    let request = fixture("routes/swarm_join.consent_proof.request.json");
    let record = record_of(&request["consent_proof"], "ed25519");
    let at = record.approved_at.unwrap();
    let web = Choice::NeedsWeb {
        consent_id: record.consent_id.clone(),
    };
    assert_eq!(
        choose(&record, false, at).unwrap(),
        web,
        "allow_local_consent off"
    );
    assert_eq!(
        choose(&record, true, at + PROOF_MAX_AGE_MS + 1).unwrap(),
        web,
        "stale"
    );
    let mut mac = record.clone();
    mac.proof = Some(Proof::Mac("00".repeat(32)));
    assert_eq!(choose(&mac, true, at).unwrap(), web, "tty-only");
    let mut statement = record.clone();
    statement.proof = Some(Proof::Statement(fixture("consent/statement.allow.json")));
    assert_eq!(
        choose(&statement, false, at).unwrap(),
        Choice::ConsentId(record.consent_id.clone())
    );
    let mut procedure = record.clone();
    procedure.kind = Kind::ProcedureConfirm;
    assert!(matches!(
        choose(&procedure, true, at).unwrap(),
        Choice::NeedsWeb { .. }
    ));
    let mut local = record.clone();
    local.kind = Kind::LinkVault;
    assert!(choose(&local, true, at).is_err(), "no server route");
    let mut pending = record;
    pending.state = State::Pending;
    assert!(matches!(
        choose(&pending, true, at),
        Err(ConsentError::Required { .. })
    ));
    assert!(web.attach(&mut json!({})).is_err());
}

#[test]
fn consent_refusals_parse() {
    let missing = ServerRefusal::parse(&fixture("routes/consent_required.error.json")).unwrap();
    assert_eq!(missing.kind, Some(Kind::SwarmJoin));
    assert_eq!(missing.reason.as_deref(), Some("missing"));
    assert!(missing.needs_new_approval());
    let used = ServerRefusal::parse(&fixture("routes/consent_required.used.error.json")).unwrap();
    assert_eq!(used.kind, Some(Kind::PledgeChange));
    assert!(used.needs_new_approval());
    let bare =
        ServerRefusal::parse(&fixture("routes/swarm_join.error.consent_required.json")).unwrap();
    assert_eq!(bare.reason, None);
    let signature = ServerRefusal {
        kind: Some(Kind::SwarmJoin),
        reason: Some("presence_key".into()),
    };
    assert!(!signature.needs_new_approval());
    assert!(ServerRefusal::parse(&json!({"error": {"code": "not_found"}})).is_none());
}
