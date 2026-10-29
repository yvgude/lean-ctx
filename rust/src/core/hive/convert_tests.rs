// SPDX-License-Identifier: Apache-2.0
//! T-SEAL-LATER, engine side (SPEC-ENGINE §6.2, V13.E3): a plain vault
//! sealed after creation is converted by the lease holder against an
//! in-memory Hive that checks what the server checks (lease, source,
//! conversion ranges, E38 parts, digests, completeness).

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::Path;

use ed25519_dalek::SigningKey;
use serde_json::{Value, json};
use zeroize::Zeroizing;

use super::*;
use crate::core::hive::replica::log::StoredEvent;

const VAULT: &str = "3f1c2b6a-8d4e-4a7b-9c10-5e2f6a7b8c01";
const EPOCH_KEY: [u8; 32] = [0x6b; 32];
const PLACE_SECRET: [u8; 32] = [0x5c; 32];

fn fixture(rel: &str) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("crates/lean-ctx-protocol/tests/fixtures/hive-v2")
        .join(rel);
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

fn seed(name: &str) -> SigningKey {
    let v = fixture("vectors/signing.vectors.json");
    let mut s = [0u8; 32];
    hex::decode_to_slice(v["keys"][name]["seed_hex"].as_str().unwrap(), &mut s).unwrap();
    SigningKey::from_bytes(&s)
}

fn statement(seq: u64) -> String {
    format!("Plaintext claim number {seq} of the vault.")
}

/// A plain vault's log: node.add per seq, a retract at 7, a field.set at
/// 9, `purged` seqs without content.
fn plain_log(n: u64, purged: &[u64]) -> Vec<StoredEvent> {
    (1..=n)
        .map(|seq| {
            let (op, target, body) = match seq {
                7 => ("retract", "n_3", json!({"reason_code": "superseded", "body_nonce": "00"})),
                9 => ("field.set", "n_2", json!({"field": "evidence", "value": "tested", "body_nonce": "00"})),
                _ => ("node.add", "", json!({"statement": statement(seq), "evidence": "observed", "body_nonce": "00"})),
            };
            let target = if target.is_empty() {
                format!("n_{seq}")
            } else {
                target.to_string()
            };
            let mut event = json!({
                "schema": "leanctx.hive.event/v2",
                "event_id": format!("01JBZ8D3K4M5N6P7Q8R{seq:07}"),
                "place": {"kind": "vault", "id": VAULT},
                "op": op,
                "target": target,
                "hlc": {"wall_ms": 1000 + seq, "counter": 0, "node": "x"},
                "actor": {"kind": "worker"},
                "worker_id": "wkr_aaaaaaaaaaaaaaaaaaaa",
                "key_id": "47983724220c8521",
                "signature": format!("{seq:0128}"),
                "seq": seq,
                "body": body,
            });
            if op == "node.add" {
                event["node_type"] = json!("fact");
            }
            if purged.contains(&seq) {
                event.as_object_mut().unwrap().remove("body");
                event["purged"] = json!(true);
            }
            StoredEvent {
                seq,
                event,
                sig_invalid: false,
            }
        })
        .collect()
}

fn replica(data: &Path, n: u64, purged: &[u64]) -> Replica {
    let (mut replica, _) = Replica::open(data, "pl_conv", &[4; 32]).unwrap();
    replica.insert_events(&plain_log(n, purged)).unwrap();
    replica.refold().unwrap();
    replica
}

/// What the server keeps per seq: the envelope digest.
fn server_digests(replica: &Replica, from_seq: u64) -> BTreeMap<u64, String> {
    replica
        .originals_upto(from_seq)
        .unwrap()
        .into_iter()
        .filter(|o| !o.purged)
        .map(|o| (o.seq, sign::envelope_digest(&o.envelope).unwrap()))
        .collect()
}

/// An in-memory Hive for E38a, E5 and E38 of one vault.
struct Fake {
    source: &'static str,
    from_seq: u64,
    leased_by_other: bool,
    expected: BTreeMap<u64, String>,
    /// Pushed copies by event id, with the conversion range they came in.
    copies: RefCell<BTreeMap<String, Value>>,
    ranges: RefCell<Vec<(u64, u64)>>,
    /// Injected: answer the first E38 with incomplete / a mismatch at seq.
    incomplete_once: RefCell<bool>,
    mismatch_at: RefCell<Option<(u64, u32)>>,
    parts_seen: RefCell<Vec<(u64, u64)>>,
    received: RefCell<Vec<(u64, String)>>,
    requests: RefCell<Vec<String>>,
}

impl Fake {
    fn new(expected: BTreeMap<u64, String>, from_seq: u64) -> Self {
        Self {
            source: SOURCE,
            from_seq,
            leased_by_other: false,
            expected,
            copies: RefCell::default(),
            ranges: RefCell::default(),
            incomplete_once: RefCell::new(false),
            mismatch_at: RefCell::new(None),
            parts_seen: RefCell::default(),
            received: RefCell::default(),
            requests: RefCell::default(),
        }
    }
}

fn error(reason: &str) -> Value {
    json!({"error": {"code": "invalid_state", "reason": reason, "message": "x"}})
}

impl ConvertApi for Fake {
    fn answer(
        &self,
        _method: &str,
        path: &str,
        body: Option<&Value>,
    ) -> Result<(u16, Value), HiveError> {
        let body = body.cloned().unwrap_or(Value::Null);
        self.requests.borrow_mut().push(body.to_string());
        if path.ends_with("/conversion/lease") {
            if self.leased_by_other {
                let mut e = error("conversion_leased");
                e["error"]["holder_expires_at"] = json!(1_792_800_600_000u64);
                return Ok((409, e));
            }
            if body["source"] != self.source {
                return Ok((409, error("conversion_source")));
            }
            return Ok((
                200,
                json!({"conversion_from_seq": self.from_seq, "expires_at": 1, "lease_instance": "i", "source": self.source}),
            ));
        }
        if path.ends_with("/events") {
            let conv = &body["conversion"];
            let (from, to) = (
                conv["from_seq"].as_u64().unwrap(),
                conv["to_seq"].as_u64().unwrap(),
            );
            assert!(
                1 <= from && from <= to && to <= self.from_seq,
                "conversion range"
            );
            assert_eq!(conv["vault"], VAULT);
            assert_eq!(conv["source"], SOURCE);
            self.ranges.borrow_mut().push((from, to));
            let results: Vec<Value> = body["events"]
                .as_array()
                .unwrap()
                .iter()
                .map(|e| {
                    let id = e["event_id"].as_str().unwrap().to_string();
                    let dup = self.copies.borrow().contains_key(&id);
                    self.copies.borrow_mut().insert(id.clone(), e.clone());
                    json!({"event_id": id, "status": if dup { "duplicate" } else { "accepted" }, "seq": 1000})
                })
                .collect();
            return Ok((200, json!({ "results": results })));
        }
        assert!(path.ends_with("/conversion/done"));
        let part = body["part"].as_u64().unwrap();
        let parts = body["parts"].as_u64().unwrap();
        self.parts_seen.borrow_mut().push((part, parts));
        if part == 1 {
            self.received.borrow_mut().clear();
        }
        for d in body["seq_digests"].as_array().unwrap() {
            let seq = d["seq"].as_u64().unwrap();
            let digest = d["envelope_digest"].as_str().unwrap().to_string();
            let injected = *self.mismatch_at.borrow();
            if let Some((at, left)) = injected
                && at == seq
                && left > 0
            {
                *self.mismatch_at.borrow_mut() = Some((at, left - 1));
                let mut e = error("conversion_digest_mismatch");
                e["error"]["seq"] = json!(seq);
                return Ok((409, e));
            }
            if self.expected.get(&seq) != Some(&digest) {
                let mut e = error("conversion_digest_mismatch");
                e["error"]["seq"] = json!(seq);
                return Ok((409, e));
            }
            self.received.borrow_mut().push((seq, digest));
        }
        if part < parts {
            return Ok((202, json!({"next_part": part + 1})));
        }
        let complete = self.received.borrow().len() == self.expected.len()
            && body["last_seq"] == self.from_seq
            && body["events_written"].as_u64().unwrap() >= self.copies.borrow().len() as u64;
        if *self.incomplete_once.borrow() || !complete {
            *self.incomplete_once.borrow_mut() = false;
            return Ok((409, error("conversion_incomplete")));
        }
        Ok((204, Value::Null))
    }
}

struct Keys {
    alias: SigningKey,
    instance: SigningKey,
    seal: SealKey,
    place: Place,
}

fn keys(from_seq: u64) -> Keys {
    Keys {
        alias: SigningKey::from_bytes(&[0x41; 32]),
        instance: seed("instance_a"),
        seal: SealKey {
            epoch: 1,
            key: Zeroizing::new(EPOCH_KEY),
        },
        place: Place {
            kind: "vault".into(),
            id: VAULT.into(),
            pk_id: ids::sealed_pk_id(&PLACE_SECRET),
            causal_base: from_seq,
        },
    }
}

fn convert(api: &Fake, replica: &Replica, data: &Path, k: &Keys) -> Result<Outcome, HiveError> {
    let signer = Signer {
        key: &k.alias,
        hlc_node: "0a1b2c3d4e5f6071".into(),
        worker_id: "wkr_sealconvertaliasxx".into(),
    };
    let c = Conversion {
        vault_id: VAULT,
        place: &k.place,
        seal: &k.seal,
        signer: &signer,
        instance_key: &k.instance,
    };
    let clock = RefCell::new(1_792_800_000_000u64);
    let now = || {
        *clock.borrow_mut() += 1;
        *clock.borrow()
    };
    run(api, replica, data, &c, &now)
}

fn open(event: &Value) -> Value {
    let mut e = event.clone();
    let key = |epoch: u32| (epoch == 1).then(|| Zeroizing::new(EPOCH_KEY));
    assert!(sealed_body::open_received(
        &mut e,
        &format!("vault:{VAULT}"),
        &key
    ));
    e
}

#[test]
fn a_vault_sealed_later_is_converted_by_the_lease_holder() {
    let data = tempfile::tempdir().unwrap();
    let mut replica = replica(data.path(), 300, &[5]);
    let before = replica.refold().unwrap().digest();
    let api = Fake::new(server_digests(&replica, 300), 300);
    let k = keys(300);

    let outcome = convert(&api, &replica, data.path(), &k).unwrap();
    assert_eq!(
        outcome,
        Outcome::Done {
            events_written: 299,
            parts: 1
        }
    );
    let copies = api.copies.borrow();
    assert_eq!(copies.len(), 299, "the purged record is not re-encrypted");
    let alias_public = k.alias.verifying_key().to_bytes();
    let originals = replica.originals_upto(300).unwrap();
    for o in originals.iter().filter(|o| !o.purged) {
        let id = ids::seal_conversion_event_id(&k.place.pk_id, o.seq, &o.event_id);
        let copy = copies
            .get(&id)
            .expect("one copy per plaintext event, id by derivation");
        assert!(copy.get("cause").is_none(), "no cause on a converted event");
        assert_eq!(copy["worker_id"], "wkr_sealconvertaliasxx");
        assert_eq!(copy["actor"], json!({"kind": "worker"}));
        assert_eq!(copy["epoch"], 1);
        assert!(copy["ck_wrapped"].is_string() && copy["body"].get("ct").is_some());
        assert!(
            sign::verify(copy, &alias_public),
            "signed by the seal-convert alias"
        );
        // A second member holding the epoch key reads the same content.
        let opened = open(copy);
        assert_eq!(opened["body"], *o.body.as_ref().unwrap());
        assert_eq!(opened["op"], o.op.as_str());
        assert_eq!(opened["target"], o.envelope["target"]);
        assert_eq!(
            opened["body_commit"],
            sign::body_commit(&k.place.pk_id, &opened["body"]).unwrap()
        );
    }
    // Batches of 200 in seq order, within 1..=conversion_from_seq.
    assert_eq!(*api.ranges.borrow(), [(1, 201), (202, 300)]);
    // No plaintext key or plaintext content ever went to the server.
    for request in api.requests.borrow().iter() {
        assert!(!request.contains(&hex::encode(EPOCH_KEY)));
        assert!(!request.contains(&hex::encode(PLACE_SECRET)));
        assert!(
            !request.contains("Plaintext claim number"),
            "bodies travel sealed"
        );
    }
    assert!(
        !cursor_path(data.path(), VAULT).exists(),
        "progress is dropped once done"
    );

    // The copies arrive through the tail; the plaintext rows fold as no-ops.
    let mut seq = 300;
    let mut stored = Vec::new();
    for o in originals.iter().filter(|o| !o.purged) {
        let id = ids::seal_conversion_event_id(&k.place.pk_id, o.seq, &o.event_id);
        seq += 1;
        let mut e = open(&copies[&id]);
        e["seq"] = json!(seq);
        stored.push(StoredEvent {
            seq,
            event: e,
            sig_invalid: false,
        });
    }
    replica.insert_events(&stored).unwrap();
    assert_eq!(settle_converted(&replica, &k.place.pk_id).unwrap(), 299);
    let after = replica.refold().unwrap();
    assert_eq!(after.digest(), before, "the fold is identical");
    assert_eq!(
        after.purged_nodes.len(),
        0,
        "nothing but the purged event is labelled purged"
    );
    assert!(
        replica
            .originals_upto(300)
            .unwrap()
            .iter()
            .all(|o| o.purged),
        "only the purged row stays unconverted below the seal"
    );
}

#[test]
fn refusals_are_handled_as_the_contract_says() {
    let data = tempfile::tempdir().unwrap();
    let replica = replica(data.path(), 40, &[]);
    let k = keys(40);

    // Another source: an engine bug, nothing is pushed.
    let mut api = Fake::new(server_digests(&replica, 40), 40);
    api.source = "legacy";
    let err = convert(&api, &replica, data.path(), &k).unwrap_err();
    assert!(err.to_string().contains("conversion_source"), "{err}");
    assert!(api.copies.borrow().is_empty());

    // Another holder: wait.
    let mut api = Fake::new(server_digests(&replica, 40), 40);
    api.leased_by_other = true;
    assert_eq!(
        convert(&api, &replica, data.path(), &k).unwrap(),
        Outcome::Waiting {
            holder_expires_at: Some(1_792_800_600_000)
        }
    );

    // Incomplete once: everything is pushed again (duplicates) and posted.
    let api = Fake::new(server_digests(&replica, 40), 40);
    *api.incomplete_once.borrow_mut() = true;
    assert!(matches!(
        convert(&api, &replica, data.path(), &k).unwrap(),
        Outcome::Done { .. }
    ));
    assert_eq!(api.ranges.borrow().len(), 2, "pushed, then pushed again");

    // One digest mismatch: recomputed and posted again; a second one for
    // the same seq stops with "conversion refused at seq 17".
    let api = Fake::new(server_digests(&replica, 40), 40);
    *api.mismatch_at.borrow_mut() = Some((17, 1));
    assert!(matches!(
        convert(&api, &replica, data.path(), &k).unwrap(),
        Outcome::Done { .. }
    ));
    let api = Fake::new(server_digests(&replica, 40), 40);
    *api.mismatch_at.borrow_mut() = Some((17, 2));
    let err = convert(&api, &replica, data.path(), &k).unwrap_err();
    assert_eq!(
        err.to_string(),
        HiveError::Refused("conversion refused at seq 17".into()).to_string()
    );
    // Progress survives for the next run.
    assert_eq!(load_cursor(data.path(), VAULT).last_seq, 40);
}

#[test]
fn a_vault_of_10_001_events_posts_e38_in_two_parts() {
    let data = tempfile::tempdir().unwrap();
    let replica = replica(data.path(), 10_001, &[]);
    let api = Fake::new(server_digests(&replica, 10_001), 10_001);
    let k = keys(10_001);
    assert_eq!(
        convert(&api, &replica, data.path(), &k).unwrap(),
        Outcome::Done {
            events_written: 10_001,
            parts: 2
        }
    );
    assert_eq!(*api.parts_seen.borrow(), [(1, 2), (2, 2)]);
    assert_eq!(api.received.borrow().len(), 10_001);
}

/// The E38 `v2_plain` body equals the C0 fixture byte for byte, signature
/// included (instance key `instance_a` of the signing vectors).
#[test]
fn the_done_body_matches_the_fixture() {
    let want = fixture("routes/conversion_done.v2_plain.request.json");
    let digests: Vec<(u64, String)> = want["seq_digests"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| {
            (
                d["seq"].as_u64().unwrap(),
                d["envelope_digest"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    let bodies = done_bodies(&seed("instance_a"), 299, 300, &digests).unwrap();
    assert_eq!(bodies, [want]);
    assert_eq!(
        fixture("routes/conversion_lease.v2_plain.request.json"),
        json!({ "source": SOURCE })
    );
}
