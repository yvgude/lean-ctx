// SPDX-License-Identifier: Apache-2.0
//! Contradiction and retraction: the queued events are the C0 2.0 shapes
//! (`edge.add contradicts`, `retract`), signed as every vault writer signs,
//! and once Hive has them delivery marks the pair conflicting and drops
//! the retracted claim (BAUPLAN v1.1 §5.1).

use ed25519_dalek::SigningKey;
use serde_json::{Value, json};

use super::*;
use crate::core::hive::delivery::{Bucket, items};
use crate::core::hive::replica::Replica;
use crate::core::hive::replica::log::StoredEvent;
use crate::core::hive::review::maintain::tests::assert_schema_valid;

const X: &str = "n_xaaaaaaaaaaaaaaaaaaaaaaaaa";
const Y: &str = "n_yaaaaaaaaaaaaaaaaaaaaaaaaa";
const Z: &str = "n_zaaaaaaaaaaaaaaaaaaaaaaaaa";
const NOW: u64 = 1_792_800_020_000;

fn stored(seq: u64, mut event: Value) -> StoredEvent {
    event["seq"] = json!(seq);
    event["provenance"] = json!("workspace");
    event["review_state"] = json!("confirmed");
    StoredEvent {
        seq,
        event,
        sig_invalid: false,
    }
}

fn claim(seq: u64, node: &str, statement: &str) -> StoredEvent {
    stored(
        seq,
        json!({
            "event_id": format!("E{seq:025}"),
            "op": "node.add",
            "target": node,
            "node_type": "fact",
            "hlc": {"wall_ms": 1000 + seq, "counter": 0, "node": "x"},
            "worker_id": "wkr_aaaaaaaaaaaaaaaaaaaa",
            "signature": "00",
            "body": {"statement": statement, "evidence": "observed"},
        }),
    )
}

fn replica(data: &std::path::Path) -> Replica {
    let (mut replica, _) = Replica::open(data, "pl_curate", &[5; 32]).unwrap();
    replica
        .insert_events(&[
            claim(1, X, "Retries are unlimited."),
            claim(2, Y, "Retries stop after five tries."),
            claim(3, Z, "The retry cap lives in retry.rs."),
        ])
        .unwrap();
    replica.refold().unwrap();
    replica
}

fn place() -> Place {
    Place {
        kind: "vault".into(),
        id: "3f1c2b6a-8d4e-4a7b-9c10-5e2f6a7b8c01".into(),
        pk_id: [9; 32],
        causal_base: 3,
    }
}

fn pending(replica: &Replica) -> Vec<Value> {
    replica
        .pending()
        .unwrap()
        .iter()
        .map(|(_, b)| serde_json::from_slice(b).unwrap())
        .collect()
}

fn screen() -> Screen {
    Screen::with_rules(
        Vec::new(),
        std::sync::Arc::new(Vec::new()),
        vec![regex::Regex::new("project-aurora").unwrap()],
    )
}

#[test]
fn contradict_and_retract_queue_c0_events_and_delivery_follows() {
    let data = tempfile::tempdir().unwrap();
    let mut replica = replica(data.path());
    let key = SigningKey::from_bytes(&[0x49; 32]);
    // Signed as the CLI signs: from the stored 1.x instance id.
    let signer = Signer::workspace(
        &key,
        "inst_6f2b9c4e1a3d4e5f8a7b0c1d2e3f4a5b",
        "wkr_vk4oelpv5lzjmev44hun",
    );
    let place = place();
    let target = Target {
        replica: &replica,
        place: &place,
        signer: &signer,
        seal: None,
    };
    assert!(queue_contradiction(&target, X, Y, NOW).unwrap());
    assert!(
        !queue_contradiction(&target, Y, X, NOW).unwrap(),
        "the same pair is queued once, in either order"
    );
    assert!(
        queue_retraction(
            &target,
            Z,
            "Moved to the policy module.",
            &mut screen(),
            NOW
        )
        .unwrap()
    );
    assert!(!queue_retraction(&target, Z, "Again.", &mut screen(), NOW).unwrap());

    let events = pending(&replica);
    assert_eq!(events.len(), 2);
    let public = key.verifying_key().to_bytes();
    for e in &events {
        assert_schema_valid(e);
        assert!(sign::verify(e, &public));
        assert_eq!(e["hlc"]["node"], "6f2b9c4e-1a3d-4e5f-8a7b-0c1d2e3f4a5b");
        assert_eq!(e["provenance_claim"], "workspace");
        assert_eq!(
            e["body_commit"],
            sign::body_commit(&place.pk_id, &e["body"]).unwrap()
        );
    }
    // Both carry the same HLC stamp here; the queue orders them by id.
    let of = |op: &str| events.iter().find(|e| e["op"] == op).unwrap();
    let edge = of("edge.add");
    assert!(edge["target"].as_str().unwrap().starts_with("e_"));
    assert_eq!(edge["body"]["kind"], "contradicts");
    assert_eq!(
        (&edge["body"]["from"], &edge["body"]["to"]),
        (&json!(X), &json!(Y))
    );
    let retract = of("retract");
    assert_eq!(retract["target"], Z);
    assert_eq!(retract["body"]["reason"], "Moved to the policy module.");

    // Hive accepts both; the next tail folds them.
    let accepted: Vec<StoredEvent> = events
        .into_iter()
        .zip(4..)
        .map(|(e, seq)| stored(seq, e))
        .collect();
    replica.insert_events(&accepted).unwrap();
    let graph = replica.refold().unwrap();
    let no_trust = |_: &str| -> Option<String> { None };
    let got: Vec<(String, Bucket)> = items(&graph, &no_trust, "")
        .into_iter()
        .map(|i| (i.node_id, i.bucket))
        .collect();
    assert_eq!(
        got,
        vec![
            (X.to_string(), Bucket::Conflicting),
            (Y.to_string(), Bucket::Conflicting),
        ],
        "conflicting is labelled, rejected is not delivered"
    );

    // Nothing more to do on a folded pair or a retracted node.
    let target = Target {
        replica: &replica,
        place: &place,
        signer: &signer,
        seal: None,
    };
    assert!(pending(&replica).is_empty() || settle_all(&replica));
    assert!(!queue_contradiction(&target, X, Y, NOW).unwrap());
    assert!(queue_retraction(&target, Z, "Gone already.", &mut screen(), NOW).is_err());
}

/// The tail settles what it delivered; here by hand when the insert did not.
fn settle_all(replica: &Replica) -> bool {
    for (id, _) in replica.pending().unwrap() {
        replica.settle_pending(&id).unwrap();
    }
    pending(replica).is_empty()
}

#[test]
fn refusals_happen_before_anything_is_queued() {
    let data = tempfile::tempdir().unwrap();
    let replica = replica(data.path());
    let key = SigningKey::from_bytes(&[0x49; 32]);
    let signer = Signer::workspace(
        &key,
        "inst_6f2b9c4e1a3d4e5f8a7b0c1d2e3f4a5b",
        "wkr_vk4oelpv5lzjmev44hun",
    );
    let place = place();
    let target = Target {
        replica: &replica,
        place: &place,
        signer: &signer,
        seal: None,
    };
    let unknown = "n_qaaaaaaaaaaaaaaaaaaaaaaaaa";
    assert!(queue_contradiction(&target, X, X, NOW).is_err(), "itself");
    assert!(
        queue_contradiction(&target, X, unknown, NOW).is_err(),
        "unknown end"
    );
    assert!(queue_retraction(&target, unknown, "No such claim.", &mut screen(), NOW).is_err());
    assert!(
        queue_retraction(&target, X, "x", &mut screen(), NOW).is_err(),
        "too short"
    );
    let long = "y".repeat(501);
    assert!(
        queue_retraction(&target, X, &long, &mut screen(), NOW).is_err(),
        "too long"
    );
    let blocked = queue_retraction(
        &target,
        X,
        "Superseded by project-aurora notes.",
        &mut screen(),
        NOW,
    );
    assert!(matches!(blocked, Err(HiveError::Refused(_))), "{blocked:?}");
    assert!(pending(&replica).is_empty());
}
