// SPDX-License-Identifier: Apache-2.0

use std::path::Path;

use ed25519_dalek::SigningKey;
use serde_json::Value;

use super::*;

fn fixture(rel: &str) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("crates/lean-ctx-protocol/tests/fixtures/hive-v2")
        .join(rel);
    serde_json::from_slice(&std::fs::read(&path).expect("C0 2.0 fixture")).expect("json")
}

fn hex32(v: &Value) -> [u8; 32] {
    hex::decode(v.as_str().expect("hex"))
        .expect("hex")
        .try_into()
        .expect("32 bytes")
}

fn test_key(name: &str) -> [u8; 32] {
    let keys = fixture("vectors/signing.vectors.json");
    *SigningKey::from_bytes(&hex32(&keys["keys"][name]["seed_hex"]))
        .verifying_key()
        .as_bytes()
}

fn id_salt() -> [u8; 32] {
    hex32(&fixture("records/snapshot_manifest.json")["id_salt"])
}

#[test]
fn roots_match_the_vectors() {
    let v = fixture("vectors/envelope_root.vectors.json");
    for case in v["synthetic"].as_array().unwrap() {
        let n = case["leaves"].as_u64().unwrap();
        let leaves: Vec<[u8; 32]> = (0..n)
            .map(|i| leaf(&serde_json::json!({ "i": i })).unwrap())
            .collect();
        assert_eq!(hex::encode(leaves[0]), case["first_leaf"].as_str().unwrap());
        assert_eq!(
            hex::encode(root(&leaves).unwrap()),
            case["envelope_root"].as_str().unwrap(),
            "{n} leaves"
        );
    }
    for case in v["fixtures"].as_array().unwrap() {
        let leaves: Vec<[u8; 32]> = case["leaves"]
            .as_array()
            .unwrap()
            .iter()
            .map(hex32)
            .collect();
        assert_eq!(
            hex::encode(root(&leaves).unwrap()),
            case["envelope_root"].as_str().unwrap()
        );
    }
    assert!(root(&[]).is_none());
}

#[test]
fn the_snapshot_manifest_verifies_with_the_hive_snapshot_key_only() {
    let manifest = fixture("records/snapshot_manifest.json");
    assert!(manifest_verifies(&manifest, &test_key("hive_snapshot")));
    assert!(!manifest_verifies(&manifest, &test_key("hive_event")));
    let mut forged = manifest.clone();
    forged["seq"] = Value::from(19);
    assert!(!manifest_verifies(&forged, &test_key("hive_snapshot")));
    assert!(manifest_verifies(
        &fixture("records/snapshot_manifest.sealed.json"),
        &test_key("hive_snapshot")
    ));
}

/// The fixture segment holds records 1, 4, 5 and 6 of the 18 in the
/// manifest: their leaves are the vector's leaves at those seqs.
#[test]
fn segment_records_give_the_vector_leaves() {
    let leaves = &fixture("vectors/envelope_root.vectors.json")["fixtures"][0]["leaves"];
    let seg = fixture("routes/snapshot_segment.response.json");
    for record in seg["records"].as_array().unwrap() {
        let seq = record["seq"].as_u64().unwrap();
        let got = match record.get("leaf") {
            Some(l) => l.as_str().unwrap().to_string(),
            None => hex::encode(leaf(&record["envelope"]).unwrap()),
        };
        assert_eq!(
            got,
            leaves[usize::try_from(seq - 1).unwrap()].as_str().unwrap(),
            "seq {seq}"
        );
    }
}

/// Records 4..=6 are contiguous: a manifest entry over them imports, and any
/// changed envelope byte, a missing record or a gap is refused.
#[test]
fn a_segment_imports_only_when_count_run_and_root_match() {
    let seg = fixture("routes/snapshot_segment.response.json");
    let records: Vec<Value> = seg["records"].as_array().unwrap()[1..].to_vec();
    let leaves: Vec<[u8; 32]> = records
        .iter()
        .map(|r| match r.get("leaf") {
            Some(l) => hex32(l),
            None => leaf(&r["envelope"]).unwrap(),
        })
        .collect();
    let meta = SegmentMeta {
        n: 0,
        seq_from: 4,
        seq_to: 6,
        count: 3,
        envelope_root: hex::encode(root(&leaves).unwrap()),
    };
    let instance = test_key("instance_a");
    let hive = test_key("hive_event");
    let signed = |e: &Value| sign::verify(e, &instance) || sign::verify(e, &hive);
    let events = import_segment(&meta, &records, &id_salt(), false, &signed).unwrap();
    assert_eq!(events.len(), 2, "the withheld record is not stored");
    assert!(events.iter().all(|e| !e.sig_invalid), "{events:?}");
    assert_eq!(events[0].event["purged"], true);
    assert!(events[0].event.get("body").is_none());

    let mut flipped = records.clone();
    flipped[2]["envelope"]["target"] = Value::from("n_aaaaaaaaaaaaaaaaaaaaaaaaaa");
    assert_eq!(
        import_segment(&meta, &flipped, &id_salt(), false, &signed),
        Err(SegmentError::Root)
    );
    assert!(matches!(
        import_segment(&meta, &records[..2], &id_salt(), false, &signed),
        Err(SegmentError::Count { .. })
    ));
    let mut gap = records.clone();
    gap[1]["seq"] = Value::from(9);
    assert_eq!(
        import_segment(&meta, &gap, &id_salt(), false, &signed),
        Err(SegmentError::SeqRun)
    );

    // A body that does not match its commitment is kept, never folded.
    let mut tampered = records.clone();
    tampered[2]["body"]["statement"] = Value::from("Something else entirely, said here.");
    let events = import_segment(&meta, &tampered, &id_salt(), false, &signed).unwrap();
    assert!(events[1].sig_invalid);
}

/// C0 1459f77: a purge tombstone in a sealed place carries an unkeyed
/// body_commit; the same tombstone in a plain place, or a changed body,
/// does not verify.
#[test]
fn sealed_purge_tombstones_commit_unkeyed() {
    let v = fixture("vectors/body_commit.vectors.json");
    let hive = test_key("hive_event");
    let signed = |e: &Value| sign::verify(e, &hive);
    for case in v["sealed_tombstone"].as_array().unwrap() {
        let mut event = fixture(case["fixture"].as_str().unwrap());
        assert_eq!(event["body_commit"], case["body_commit"]);
        assert!(event_verifies(&event, &[0; 32], true, &signed));
        assert!(
            !event_verifies(&event, &[0; 32], false, &signed),
            "keyed rule outside sealed places"
        );
        event["body"]["reason"] = Value::from("gdpr_erasure");
        assert!(!event_verifies(&event, &[0; 32], true, &signed));
    }
    // A worker event in a sealed place stays keyed.
    let fact = fixture("events/node.add.fact.json");
    let instance = test_key("instance_a");
    assert!(!event_verifies(&fact, &[0; 32], true, &|e| sign::verify(
        e, &instance
    )));
    assert!(event_verifies(&fact, &id_salt(), true, &|e| sign::verify(
        e, &instance
    )));
}

#[test]
fn tail_pages_verify_each_event() {
    let page = fixture("routes/events_tail.response.json");
    let instance = test_key("instance_a");
    let tail = tail_page(&page, &id_salt(), false, &|e| sign::verify(e, &instance)).unwrap();
    assert_eq!(tail.events.len(), 3);
    assert!(tail.events.iter().all(|e| !e.sig_invalid));
    assert_eq!(
        (tail.next_since, tail.head_seq, tail.epoch),
        (Some(4), 18, Some(0))
    );
    let wrong = tail_page(&page, &id_salt(), false, &|e| {
        sign::verify(e, &test_key("instance_b"))
    })
    .unwrap();
    assert!(wrong.events.iter().all(|e| e.sig_invalid));
}
