// SPDX-License-Identifier: Apache-2.0
//! The driver against an in-memory place: real signed events, a signed
//! manifest, JSON-line segments and tail pages as C0 2.0 section 6 shapes
//! them.

use std::cell::RefCell;

use ed25519_dalek::{Signer as _, SigningKey};
use serde_json::{Value, json};

use super::*;
use crate::core::hive::capture::CaptureCandidate;
use crate::core::hive::events::build::{self, Place, Signer};
use crate::core::hive::events::hlc::Stamp;
use crate::core::hive::events::sign;
use crate::core::knowledge::{EvidenceLevel, NodeType};
use crate::core::sensitivity::SensitivityLevel;

const PLACE: &str = "vault:3f1c2b6a-8d4e-4a7b-9c10-5e2f6a7b8c01";
const SALT: [u8; 32] = [7; 32];

fn instance_key() -> SigningKey {
    SigningKey::from_bytes(&[0x49; 32])
}

fn snapshot_key() -> SigningKey {
    SigningKey::from_bytes(&[0x5a; 32])
}

/// A signed node.add for `statement`, as an engine pushes it.
fn claim(statement: &str, wall_ms: u64) -> Value {
    let c = CaptureCandidate {
        node_type: NodeType::Fact,
        statement: statement.into(),
        steps: vec![],
        commands: vec![],
        refs: vec![],
        evidence: EvidenceLevel::Stated,
        sensitivity: SensitivityLevel::Public,
    };
    let body = build::claim_body(&c, &["https://docs.rs/blake3/".into()], vec![], false).unwrap();
    let key = instance_key();
    let place = Place {
        kind: "vault".into(),
        id: "3f1c2b6a-8d4e-4a7b-9c10-5e2f6a7b8c01".into(),
        pk_id: SALT,
        causal_base: 0,
    };
    let signer = Signer {
        key: &key,
        hlc_node: "6f2b9c4e-1a3d-4e5f-8a7b-0c1d2e3f4a5b".into(),
        worker_id: "wkr_vk4oelpv5lzjmev44hun".into(),
    };
    build::node_add(
        &c,
        &body,
        &place,
        Stamp {
            wall_ms,
            counter: 0,
        },
        &signer,
    )
    .unwrap()
    .0
}

/// The server: its log, how many leading events its snapshot covers, and
/// how many segment fetches to answer with a corrupted record.
struct Server {
    log: RefCell<Vec<Value>>,
    snapshot_upto: usize,
    corrupt_fetches: RefCell<usize>,
    epoch: std::cell::Cell<u64>,
    /// The month's transfer is used up: E4 answers only the purge
    /// tombstones after `since`, `next_since` = `since`.
    quota_used: std::cell::Cell<bool>,
}

impl Server {
    fn new(statements: &[&str], snapshot_upto: usize) -> Self {
        let s = Self {
            log: RefCell::new(Vec::new()),
            snapshot_upto,
            corrupt_fetches: RefCell::new(0),
            epoch: std::cell::Cell::new(0),
            quota_used: std::cell::Cell::new(false),
        };
        for (i, st) in statements.iter().enumerate() {
            s.append(claim(st, 1_792_800_000_000 + i as u64));
        }
        s
    }

    fn append(&self, mut event: Value) -> u64 {
        let mut log = self.log.borrow_mut();
        let seq = log.len() as u64 + 1;
        event["seq"] = json!(seq);
        event["received_at"] = json!(1_792_800_100_000u64 + seq);
        event["provenance"] = json!("own");
        event["review_state"] = json!("confirmed");
        event["reviewed_by"] = json!("none");
        log.push(event);
        seq
    }

    /// A failover to a standby that lacks every event after `keep`; the
    /// promotion bumps the epoch, a backup restore does not.
    fn fail_over(&self, keep: usize, bump_epoch: bool) {
        self.log.borrow_mut().truncate(keep);
        if bump_epoch {
            self.epoch.set(self.epoch.get() + 1);
        }
    }

    /// The key-store row of `event_id` is gone: served without content.
    fn lose_key(&self, event_id: &str) {
        for e in self.log.borrow_mut().iter_mut() {
            if e["event_id"] == event_id {
                e["body"] = Value::Null;
                e["key_lost"] = json!(true);
            }
        }
    }

    fn event_ids(&self) -> std::collections::BTreeSet<String> {
        self.log
            .borrow()
            .iter()
            .map(|e| e["event_id"].as_str().unwrap().to_string())
            .collect()
    }

    fn record(event: &Value) -> Value {
        let mut envelope = sign::envelope(event).unwrap();
        envelope.insert("signature".into(), event["signature"].clone());
        let mut record = json!({
            "seq": event["seq"],
            "envelope": Value::Object(envelope),
            "server_fields": {
                "seq": event["seq"], "received_at": event["received_at"],
                "provenance": event["provenance"], "review_state": event["review_state"],
                "reviewed_by": event["reviewed_by"],
            },
            "body": event["body"],
        });
        // A shredded event before the snapshot rebuild: envelope, no body.
        if event.get("purged").is_some() {
            let obj = record.as_object_mut().unwrap();
            obj.remove("body");
            obj.insert("purged".into(), json!(true));
        }
        record
    }

    /// A purge of `targets` (event ids): their bodies are shredded and the
    /// tombstone is appended, signed like the place's other events.
    fn purge(&self, targets: &[String]) {
        for e in self.log.borrow_mut().iter_mut() {
            if targets.iter().any(|t| e["event_id"] == t.as_str()) {
                e["body"] = Value::Null;
                e["purged"] = json!(true);
            }
        }
        let body = json!({"purge_id": "prg_aaaaaaaaaaaaaaaaaaaaaaezcm", "reason": "leaked_secret",
                          "targets": targets, "cascade": [], "bus_cascade": []});
        let mut tombstone = json!({
            "schema": crate::core::hive::events::build::EVENT_SCHEMA,
            "event_id": crate::core::hive::events::ids::new_event_id(1_792_803_000_000).unwrap(),
            "place": {"kind": "vault", "id": "3f1c2b6a-8d4e-4a7b-9c10-5e2f6a7b8c01"},
            "op": "purge",
            "body_commit": sign::body_commit(&SALT, &body).unwrap(),
            "hlc": {"wall_ms": 1_792_803_000_000u64, "counter": 0,
                    "node": "6f2b9c4e-1a3d-4e5f-8a7b-0c1d2e3f4a5b"},
            "actor": {"kind": "server"},
            "body": body,
        });
        sign::sign(&mut tombstone, &instance_key()).unwrap();
        self.append(tombstone);
    }

    fn manifest(&self) -> Value {
        let log = self.log.borrow();
        let covered = &log[..self.snapshot_upto];
        let leaves: Vec<[u8; 32]> = covered
            .iter()
            .map(|e| records::leaf(&Self::record(e)["envelope"]).unwrap())
            .collect();
        let key = snapshot_key();
        let mut m = json!({
            "schema": "leanctx.hive.snapshot/v1",
            "place": PLACE, "rev": 1, "seq": covered.len(), "epoch": self.epoch.get(),
            "id_salt": hex::encode(SALT),
            "created_at": 1_792_800_200_000u64, "server_time": 1_792_800_200_500u64,
            "segments": [{
                "n": 0, "seq_from": 1, "seq_to": covered.len(), "count": covered.len(),
                "envelope_root": hex::encode(records::root(&leaves).unwrap()),
            }],
            "key_id": sign::key_id(key.verifying_key().as_bytes()),
        });
        let input =
            lean_ctx_protocol::hive::signing_input_without_signature(records::SNAPSHOT_DOMAIN, &m)
                .unwrap();
        m["signature"] = json!(hex::encode(key.sign(&input).to_bytes()));
        m
    }
}

impl PlaceApi for Server {
    fn get(&self, path: &str) -> Result<Value, HiveError> {
        if path == format!("/places/{PLACE}/snapshot") {
            return Ok(self.manifest());
        }
        if let Some(id) = path
            .strip_prefix(&format!("/places/{PLACE}/events/"))
            .and_then(|rest| rest.strip_suffix("/body"))
        {
            let log = self.log.borrow();
            let e = log.iter().find(|e| e["event_id"] == id).expect("E6 event");
            return Ok(if e.get("key_lost").is_some() {
                json!({"body": null, "key_lost": true})
            } else {
                json!({"body": e["body"]})
            });
        }
        let since: u64 = path
            .split_once("/events?since=")
            .map(|(_, s)| s.parse().unwrap())
            .expect("tail path");
        let log = self.log.borrow();
        let used = self.quota_used.get();
        let events: Vec<Value> = log
            .iter()
            .filter(|e| e["seq"].as_u64().unwrap() > since && (!used || e["op"] == "purge"))
            .take(if used { 100 } else { 2 })
            .cloned()
            .collect();
        let next = if used {
            Some(since)
        } else {
            events.last().map(|e| e["seq"].as_u64().unwrap())
        };
        Ok(
            json!({"epoch": self.epoch.get(), "events": events, "next_since": next,
                  "head_seq": log.len(), "server_time": 0, "withheld": [],
                  "released": [], "held_results": [], "trust_rev": 0}),
        )
    }

    fn get_bytes(&self, path: &str, _max: usize) -> Result<Vec<u8>, HiveError> {
        assert!(path.ends_with("/snapshot/1/segments/0"), "{path}");
        let log = self.log.borrow();
        let mut lines = String::new();
        for (i, e) in log[..self.snapshot_upto].iter().enumerate() {
            let mut r = Self::record(e);
            if i == 1 && *self.corrupt_fetches.borrow() > 0 {
                r["envelope"]["hlc"]["counter"] = json!(9);
            }
            lines.push_str(&serde_json::to_string(&r).unwrap());
            lines.push('\n');
        }
        let mut c = self.corrupt_fetches.borrow_mut();
        *c = c.saturating_sub(1);
        // E3 is served `Content-Encoding: zstd`.
        Ok(zstd::encode_all(lines.as_bytes(), 0).unwrap())
    }

    fn post(&self, path: &str, body: &Value) -> Result<Value, HiveError> {
        assert_eq!(path, format!("/places/{PLACE}/events"));
        let results: Vec<Value> = body["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| {
                // Idempotent by event id; a key_lost event gets its content
                // back from the identical re-push (repair).
                let held = self
                    .log
                    .borrow_mut()
                    .iter_mut()
                    .find(|l| l["event_id"] == e["event_id"])
                    .map(|l| {
                        let repaired = l.get("key_lost").is_some();
                        if repaired {
                            l["body"] = e["body"].clone();
                            l.as_object_mut().unwrap().remove("key_lost");
                        }
                        (l["seq"].clone(), repaired)
                    });
                match held {
                    Some((seq, true)) => json!({"event_id": e["event_id"], "seq": seq,
                        "status": "accepted", "review_state": "confirmed"}),
                    Some((seq, false)) => {
                        json!({"event_id": e["event_id"], "seq": seq, "status": "duplicate"})
                    }
                    None => {
                        let seq = self.append(e.clone());
                        json!({"event_id": e["event_id"], "seq": seq, "status": "accepted",
                               "review_state": "confirmed"})
                    }
                }
            })
            .collect();
        Ok(json!({"epoch": self.epoch.get(), "results": results, "server_time": 0}))
    }
}

fn verifier_parts() -> ([[u8; 32]; 1], [u8; 32]) {
    (
        [*snapshot_key().verifying_key().as_bytes()],
        *instance_key().verifying_key().as_bytes(),
    )
}

const STATEMENTS: [&str; 7] = [
    "The relay drops frames when the link is slow.",
    "The outbox retries a refused push after the lease is renewed.",
    "Sealed vaults wrap every content key with the epoch key.",
    "The scheduler tails every replica on each tick.",
    "Snapshot segments are verified against the signed manifest.",
    "A purge removes the body but keeps the signed envelope.",
    "Capture counts never carry any text from rejected facts.",
];

#[test]
fn a_clone_takes_the_snapshot_then_the_tail() {
    let server = Server::new(&STATEMENTS, 5);
    let data = tempfile::tempdir().unwrap();
    let (mut replica, _) = Replica::open(data.path(), "pl_clone", &[1; 32]).unwrap();
    let (snap, inst) = verifier_parts();
    let ok = |e: &Value| sign::verify(e, &inst);
    let v = Verifier {
        snapshot_keys: &snap,
        signature_ok: &ok,
        sealed: false,
        epoch_key: None,
    };
    let report = clone_place(&server, &mut replica, PLACE, &v).unwrap();
    assert!(report.cloned);
    assert_eq!(report.stored, 7);
    assert_eq!(report.cursor, Some(7));
    let graph = replica.refold().unwrap();
    assert_eq!(graph.live_nodes().count(), 7);
    assert_eq!(replica.id_salt().unwrap(), Some(SALT));

    // Nothing new: a tail stores nothing and keeps the cursor.
    let again = tail(&server, &mut replica, PLACE, &v).unwrap();
    assert_eq!((again.stored, again.cursor), (0, Some(7)));
}

/// T-SYNC-3 (SPEC-ENGINE §6.2, §3.4.3): a purge of 3 events in a 10 000-event
/// vault, then a clone before the snapshot rebuild. The zstd record stream
/// carries the purged envelopes as stubs, every root still matches, the 3
/// events fold as purged; a segment with a flipped envelope byte on every
/// fetch stops the clone with `segment_unavailable`.
#[test]
fn a_clone_before_the_snapshot_rebuild_folds_the_purge() {
    let texts: Vec<String> = (0..10_000)
        .map(|i| format!("Claim number {i} of the large vault holds true."))
        .collect();
    let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
    let mut server = Server::new(&refs, 0);
    let ids: Vec<String> = [10, 20, 30]
        .iter()
        .map(|i| {
            server.log.borrow()[*i]["event_id"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect();
    server.purge(&ids);
    server.snapshot_upto = server.log.borrow().len();
    let (snap, inst) = verifier_parts();
    let ok = |e: &Value| sign::verify(e, &inst);
    let v = Verifier {
        snapshot_keys: &snap,
        signature_ok: &ok,
        sealed: false,
        epoch_key: None,
    };
    let data = tempfile::tempdir().unwrap();
    let (mut replica, _) = Replica::open(data.path(), "pl_big", &[1; 32]).unwrap();
    let report = clone_place(&server, &mut replica, PLACE, &v).unwrap();
    assert_eq!(report.stored, 10_001);
    let graph = replica.refold().unwrap();
    assert_eq!(graph.purged_nodes.len(), 3);
    assert_eq!(graph.live_nodes().count(), 9_997);
    assert_eq!(graph.tombstones.len(), 1);

    *server.corrupt_fetches.borrow_mut() = 2;
    let (mut other, _) = Replica::open(data.path(), "pl_big_bad", &[1; 32]).unwrap();
    let err = clone_place(&server, &mut other, PLACE, &v).unwrap_err();
    assert!(err.to_string().contains("segment_unavailable"), "{err}");
    assert!(other.fold_events().unwrap().is_empty());
}

#[test]
fn a_bad_segment_is_fetched_once_more_then_refused() {
    let (snap, inst) = verifier_parts();
    let ok = |e: &Value| sign::verify(e, &inst);
    let v = Verifier {
        snapshot_keys: &snap,
        signature_ok: &ok,
        sealed: false,
        epoch_key: None,
    };
    let data = tempfile::tempdir().unwrap();

    let flaky = Server::new(&STATEMENTS, 5);
    *flaky.corrupt_fetches.borrow_mut() = 1;
    let (mut replica, _) = Replica::open(data.path(), "pl_flaky", &[1; 32]).unwrap();
    assert_eq!(
        clone_place(&flaky, &mut replica, PLACE, &v).unwrap().stored,
        7
    );

    let broken = Server::new(&STATEMENTS, 5);
    *broken.corrupt_fetches.borrow_mut() = 2;
    let (mut replica, _) = Replica::open(data.path(), "pl_broken", &[1; 32]).unwrap();
    let err = clone_place(&broken, &mut replica, PLACE, &v).unwrap_err();
    assert!(err.to_string().contains("segment_unavailable"), "{err}");
    assert!(
        replica.fold_events().unwrap().is_empty(),
        "nothing of it stored"
    );

    // A manifest signed by another key is refused before any segment.
    let other = [*SigningKey::from_bytes(&[1; 32]).verifying_key().as_bytes()];
    let wrong = Verifier {
        snapshot_keys: &other,
        signature_ok: &ok,
        sealed: false,
        epoch_key: None,
    };
    let (mut replica, _) = Replica::open(data.path(), "pl_wrong", &[1; 32]).unwrap();
    assert!(clone_place(&Server::new(&STATEMENTS, 5), &mut replica, PLACE, &wrong).is_err());
}

/// Two engines clone the same place; one pushes while offline-queued, both
/// tail: they converge on the same graph, and the pusher's pending queue
/// empties only once the log delivers its event.
#[test]
fn two_engines_converge_on_the_same_graph() {
    let server = Server::new(&STATEMENTS[..4], 4);
    let (snap, inst) = verifier_parts();
    let ok = |e: &Value| sign::verify(e, &inst);
    let v = Verifier {
        snapshot_keys: &snap,
        signature_ok: &ok,
        sealed: false,
        epoch_key: None,
    };
    let data = tempfile::tempdir().unwrap();
    let (mut a, _) = Replica::open(data.path(), "pl_a", &[1; 32]).unwrap();
    let (mut b, _) = Replica::open(&data.path().join("b"), "pl_a", &[2; 32]).unwrap();
    clone_place(&server, &mut a, PLACE, &v).unwrap();
    clone_place(&server, &mut b, PLACE, &v).unwrap();

    let own = claim(STATEMENTS[4], 1_792_800_900_000);
    let id = own["event_id"].as_str().unwrap().to_string();
    a.add_pending(
        &id,
        1_792_800_900_000,
        0,
        None,
        &serde_json::to_vec(&own).unwrap(),
    )
    .unwrap();
    let pushed = push_pending(&server, &a, PLACE, &[]).unwrap();
    assert_eq!((pushed.sent, pushed.logged), (1, 1));
    assert_eq!(
        a.pending().unwrap().len(),
        1,
        "pending until the tail has it"
    );

    // The other engine pushes its own claim before it has seen the first.
    let theirs = claim(STATEMENTS[5], 1_792_800_900_001);
    let their_id = theirs["event_id"].as_str().unwrap().to_string();
    b.add_pending(
        &their_id,
        1_792_800_900_001,
        0,
        None,
        &serde_json::to_vec(&theirs).unwrap(),
    )
    .unwrap();
    assert_eq!(push_pending(&server, &b, PLACE, &[]).unwrap().logged, 1);

    tail(&server, &mut a, PLACE, &v).unwrap();
    tail(&server, &mut b, PLACE, &v).unwrap();
    assert!(a.pending().unwrap().is_empty());
    assert!(b.pending().unwrap().is_empty());
    let (ga, gb) = (a.refold().unwrap(), b.refold().unwrap());
    assert_eq!(ga, gb);
    assert_eq!(ga.digest(), gb.digest(), "`hive replica` shows one digest");
    // B knows A's claim for capture dedupe without ever having captured it.
    let dk = crate::core::hive::capture::dedupe::exact_key(&SALT, NodeType::Fact, STATEMENTS[4]);
    assert!(b.dedupe_node(&dk).unwrap().is_some());
    assert_eq!(ga.live_nodes().count(), 6);
}

#[test]
fn a_forged_tail_event_is_stored_but_never_folded() {
    let server = Server::new(&STATEMENTS[..2], 2);
    let (snap, inst) = verifier_parts();
    let ok = |e: &Value| sign::verify(e, &inst);
    let v = Verifier {
        snapshot_keys: &snap,
        signature_ok: &ok,
        sealed: false,
        epoch_key: None,
    };
    let data = tempfile::tempdir().unwrap();
    let (mut r, _) = Replica::open(data.path(), "pl_forge", &[1; 32]).unwrap();
    clone_place(&server, &mut r, PLACE, &v).unwrap();
    let mut forged = claim(STATEMENTS[2], 1_792_800_950_000);
    forged["body"]["statement"] = json!("A different statement slipped in by someone.");
    server.append(forged);
    let report = tail(&server, &mut r, PLACE, &v).unwrap();
    assert_eq!(report.stored, 1);
    assert_eq!(r.refold().unwrap().live_nodes().count(), 2);
}

/// An event whose signer key was not usable yet (its worker registered
/// after the key was cached) is stored unverified; once the key verifies,
/// the re-check lets it fold. A forged body stays unverified.
#[test]
fn unverified_events_fold_after_a_later_check_passes() {
    let server = Server::new(&STATEMENTS[..3], 2);
    server.append({
        let mut forged = claim(STATEMENTS[3], 1_792_800_960_000);
        forged["body"]["statement"] = json!("Not what was signed.");
        forged
    });
    let (snap, inst) = verifier_parts();
    let unknown_yet = |_: &Value| false;
    let v = Verifier {
        snapshot_keys: &snap,
        signature_ok: &unknown_yet,
        sealed: false,
        epoch_key: None,
    };
    let data = tempfile::tempdir().unwrap();
    let (mut r, _) = Replica::open(data.path(), "pl_recheck", &[1; 32]).unwrap();
    clone_place(&server, &mut r, PLACE, &v).unwrap();
    assert_eq!(r.refold().unwrap().live_nodes().count(), 0);
    let stored = r.unverified().unwrap();
    assert_eq!(stored.len(), 4);
    assert!(stored.iter().all(|(_, e)| e.get("body").is_some()));

    let ok = |e: &Value| sign::verify(e, &inst);
    for (id, event) in stored {
        if records::event_verifies(&event, &SALT, false, &ok) {
            r.mark_verified(&id).unwrap();
        }
    }
    assert_eq!(r.refold().unwrap().live_nodes().count(), 3);
    assert_eq!(r.unverified().unwrap().len(), 1, "the forged one stays out");
}

/// Queue `n` own claims in `replica` and push them.
fn push_own(server: &Server, replica: &Replica, tag: &str, n: u64) -> Vec<String> {
    let mut ids = Vec::new();
    for i in 0..n {
        let wall = 1_792_801_000_000 + i;
        let e = claim(
            &format!("The {tag} engine learned claim number {i} about the relay."),
            wall,
        );
        let id = e["event_id"].as_str().unwrap().to_string();
        replica
            .add_pending(&id, wall, 0, None, &serde_json::to_vec(&e).unwrap())
            .unwrap();
        ids.push(id);
    }
    push_pending(server, replica, PLACE, &[]).unwrap();
    ids
}

/// T-EPOCH-REPUSH (SPEC-ENGINE §6.2, R48), in process: A and B push, the
/// place fails over to a standby that lost the last events and bumps its
/// epoch, someone else writes into the reused seqs; each engine re-pushes
/// its 24-h history once, re-tails, drops the rows whose seq changed, and
/// both converge on the place's log with the union of their events.
#[test]
fn a_failover_is_reconciled_from_both_histories() {
    let server = Server::new(&STATEMENTS[..4], 4);
    let (snap, inst) = verifier_parts();
    let ok = |e: &Value| sign::verify(e, &inst);
    let v = Verifier {
        snapshot_keys: &snap,
        signature_ok: &ok,
        sealed: false,
        epoch_key: None,
    };
    let data = tempfile::tempdir().unwrap();
    let (mut a, _) = Replica::open(data.path(), "pl_ea", &[1; 32]).unwrap();
    let (mut b, _) = Replica::open(data.path(), "pl_eb", &[2; 32]).unwrap();
    clone_place(&server, &mut a, PLACE, &v).unwrap();
    clone_place(&server, &mut b, PLACE, &v).unwrap();
    let mine = push_own(&server, &a, "first", 3);
    let theirs = push_own(&server, &b, "second", 2);
    tail(&server, &mut a, PLACE, &v).unwrap();
    tail(&server, &mut b, PLACE, &v).unwrap();
    let before = server.event_ids();
    assert_eq!(before.len(), 9);
    assert_eq!(a.epoch().unwrap(), Some(0));

    // The standby lacks the last 4 events (2 of A, 2 of B); someone else
    // writes seq 6 on the promoted primary.
    server.fail_over(5, true);
    server.append(claim(STATEMENTS[6], 1_792_802_000_000));

    let ra = tail(&server, &mut a, PLACE, &v).unwrap();
    assert!(ra.reconciled);
    assert_eq!(ra.repushed, 3, "A re-pushes its whole history once");
    assert!(ra.dropped >= 1, "A's rows whose seq changed are dropped");
    let rb = tail(&server, &mut b, PLACE, &v).unwrap();
    assert!(rb.reconciled);
    assert_eq!(rb.repushed, 2);
    // No new trigger: A only tails what B brought back.
    let again = tail(&server, &mut a, PLACE, &v).unwrap();
    assert!(!again.reconciled);

    let after = server.event_ids();
    assert!(before.is_subset(&after), "every pushed event is held again");
    for id in mine.iter().chain(&theirs) {
        assert!(after.contains(id));
    }
    let (ga, gb) = (a.refold().unwrap(), b.refold().unwrap());
    assert_eq!(ga, gb);
    assert_eq!(ga.digest(), gb.digest());
    assert_eq!(ga.live_nodes().count(), 10);
    assert_eq!(a.epoch().unwrap(), Some(1));
    assert_eq!(b.epoch().unwrap(), Some(1));
    assert_eq!(a.cursor().unwrap(), Some(10));
}

/// A backup restore: the log's `head_seq` drops below the cursor without an
/// epoch change, and the same reconcile runs.
#[test]
fn a_restored_log_below_the_cursor_is_reconciled() {
    let server = Server::new(&STATEMENTS[..3], 3);
    let (snap, inst) = verifier_parts();
    let ok = |e: &Value| sign::verify(e, &inst);
    let v = Verifier {
        snapshot_keys: &snap,
        signature_ok: &ok,
        sealed: false,
        epoch_key: None,
    };
    let data = tempfile::tempdir().unwrap();
    let (mut a, _) = Replica::open(data.path(), "pl_ra", &[1; 32]).unwrap();
    clone_place(&server, &mut a, PLACE, &v).unwrap();
    push_own(&server, &a, "restored", 2);
    tail(&server, &mut a, PLACE, &v).unwrap();
    assert_eq!(a.cursor().unwrap(), Some(5));
    server.fail_over(3, false);
    let r = tail(&server, &mut a, PLACE, &v).unwrap();
    assert!(r.reconciled);
    assert_eq!(r.repushed, 2);
    assert_eq!(server.event_ids().len(), 5);
    assert_eq!(a.cursor().unwrap(), Some(5));
    assert_eq!(a.epoch().unwrap(), Some(0), "no epoch change");
    assert!(a.pending().unwrap().is_empty());
}

/// A `key_lost` event is stored without content and never folded; its
/// author re-pushes it from the history at once and the place has its
/// content back.
#[test]
fn a_key_lost_event_is_kept_out_and_repaired_by_its_author() {
    let server = Server::new(&STATEMENTS[..2], 2);
    let (snap, inst) = verifier_parts();
    let ok = |e: &Value| sign::verify(e, &inst);
    let v = Verifier {
        snapshot_keys: &snap,
        signature_ok: &ok,
        sealed: false,
        epoch_key: None,
    };
    let data = tempfile::tempdir().unwrap();
    let (mut a, _) = Replica::open(data.path(), "pl_ka", &[1; 32]).unwrap();
    let (mut b, _) = Replica::open(data.path(), "pl_kb", &[2; 32]).unwrap();
    clone_place(&server, &mut a, PLACE, &v).unwrap();
    clone_place(&server, &mut b, PLACE, &v).unwrap();
    let lost = push_own(&server, &a, "lost", 1).remove(0);
    server.lose_key(&lost);

    let rb = tail(&server, &mut b, PLACE, &v).unwrap();
    assert_eq!(rb.repushed, 0, "B holds no copy of A's event");
    assert_eq!(b.key_lost_ids().unwrap(), vec![lost.clone()]);
    assert_eq!(b.refold().unwrap().live_nodes().count(), 2, "never folded");

    let ra = tail(&server, &mut a, PLACE, &v).unwrap();
    assert_eq!(ra.repushed, 1, "A repairs it from its history");
    {
        let held = server.log.borrow();
        let repaired = held
            .iter()
            .find(|e| e["event_id"] == lost.as_str())
            .unwrap();
        assert!(repaired.get("key_lost").is_none());
        assert!(!repaired["body"].is_null());
    }

    // The next tail of each engine fetches the content again (E6) and folds it.
    tail(&server, &mut a, PLACE, &v).unwrap();
    tail(&server, &mut b, PLACE, &v).unwrap();
    assert!(b.key_lost_ids().unwrap().is_empty());
    assert!(a.key_lost_ids().unwrap().is_empty());
    let (ga, gb) = (a.refold().unwrap(), b.refold().unwrap());
    assert_eq!(gb.live_nodes().count(), 3);
    assert_eq!(ga.digest(), gb.digest());
}

/// Records every path and serves the scoped requests from the plain place.
struct Scoped<'a> {
    inner: &'a Server,
    paths: RefCell<Vec<String>>,
}

impl Scoped<'_> {
    fn strip(&self, path: &str) -> String {
        self.paths.borrow_mut().push(path.to_string());
        path.replace("?scope=subtree:n_goal", "")
    }
}

impl PlaceApi for Scoped<'_> {
    fn get(&self, path: &str) -> Result<Value, HiveError> {
        self.inner.get(&self.strip(path))
    }
    fn get_bytes(&self, path: &str, max: usize) -> Result<Vec<u8>, HiveError> {
        self.inner.get_bytes(&self.strip(path), max)
    }
    fn post(&self, path: &str, body: &Value) -> Result<Value, HiveError> {
        self.inner.post(path, body)
    }
}

#[test]
fn a_scoped_clone_asks_for_the_subtree_in_manifest_and_segments() {
    let server = Server::new(&STATEMENTS[..3], 2);
    let scoped = Scoped {
        inner: &server,
        paths: RefCell::new(Vec::new()),
    };
    let data = tempfile::tempdir().unwrap();
    let (mut replica, _) = Replica::open(data.path(), "pl_scoped", &[1; 32]).unwrap();
    let (snap, inst) = verifier_parts();
    let ok = |e: &Value| sign::verify(e, &inst);
    let v = Verifier {
        snapshot_keys: &snap,
        signature_ok: &ok,
        sealed: false,
        epoch_key: None,
    };
    let report = clone_scoped(&scoped, &mut replica, PLACE, Some("subtree:n_goal"), &v).unwrap();
    assert!(report.cloned);
    let paths = scoped.paths.borrow();
    assert_eq!(
        paths[0],
        format!("/places/{PLACE}/snapshot?scope=subtree:n_goal")
    );
    assert_eq!(
        paths[1],
        format!("/places/{PLACE}/snapshot/1/segments/0?scope=subtree:n_goal")
    );
    assert!(
        paths[2..].iter().all(|p| !p.contains("scope")),
        "the tail is not scoped"
    );
}

/// E2E-FINAL: with the month's transfer used up the tail carries only the
/// purge tombstones (`next_since` = `since`). They are applied on top of the
/// replica's graph and its purged bodies shredded, but never stored: the
/// cursor stays, so the events between arrive with the next full tail. An
/// ordinary page answers `None` (the quota is back).
#[test]
fn deletions_apply_while_transfer_is_used_up_without_moving_the_cursor() {
    let server = Server::new(&STATEMENTS[..5], 5);
    let data = tempfile::tempdir().unwrap();
    let (mut replica, _) = Replica::open(data.path(), "pl_quota", &[1; 32]).unwrap();
    let (snap, inst) = verifier_parts();
    let ok = |e: &Value| sign::verify(e, &inst);
    let v = Verifier {
        snapshot_keys: &snap,
        signature_ok: &ok,
        sealed: false,
        epoch_key: None,
    };
    clone_place(&server, &mut replica, PLACE, &v).unwrap();
    assert_eq!(replica.cursor().unwrap(), Some(5));
    // One more claim arrives, then a purge of the second one.
    server.append(claim(STATEMENTS[5], 1_792_800_900_000));
    let target = server.log.borrow()[1]["event_id"]
        .as_str()
        .unwrap()
        .to_string();
    server.purge(std::slice::from_ref(&target));
    server.quota_used.set(true);
    let graph = deletions(&server, &mut replica, PLACE, &v)
        .unwrap()
        .expect("a deletions-only page");
    assert_eq!(graph.tombstones.len(), 1);
    assert_eq!(graph.purged_nodes.len(), 1);
    assert!(graph.purged_events.contains(&target));
    assert_eq!(replica.cursor().unwrap(), Some(5), "the cursor stays");
    let shredded = replica
        .fold_events()
        .unwrap()
        .into_iter()
        .find(|e| e.event_id == target)
        .unwrap();
    assert!(
        shredded.body.is_none(),
        "the purged body is shredded at once"
    );
    // A forged tombstone is never applied.
    let (mut other, _) = Replica::open(data.path(), "pl_quota_forged", &[1; 32]).unwrap();
    server.quota_used.set(false);
    let snapshot_only = Server::new(&STATEMENTS[..5], 5);
    clone_place(&snapshot_only, &mut other, PLACE, &v).unwrap();
    server.quota_used.set(true);
    let none = |_: &Value| false;
    let strict = Verifier {
        snapshot_keys: &snap,
        signature_ok: &none,
        sealed: false,
        epoch_key: None,
    };
    let forged = deletions(&server, &mut other, PLACE, &strict)
        .unwrap()
        .unwrap();
    assert!(forged.purged_events.is_empty() && forged.tombstones.is_empty());
    // The quota is back: an ordinary page, then the full tail brings all.
    server.quota_used.set(false);
    assert!(
        deletions(&server, &mut replica, PLACE, &v)
            .unwrap()
            .is_none()
    );
    tail(&server, &mut replica, PLACE, &v).unwrap();
    assert_eq!(replica.cursor().unwrap(), Some(7));
    assert_eq!(replica.refold().unwrap().tombstones.len(), 1);
}
