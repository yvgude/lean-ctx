// SPDX-License-Identifier: Apache-2.0
//! T-MAINTAIN host part (SPEC-ENGINE §6.2, V9.4): an anchor change in the
//! checkout yields `to_verify`, a gone file `gone`; host mode makes no E42
//! call; the deterministic task needs no model; the place, not the engine,
//! holds a large retraction, which stays pending as sent to review.

use std::cell::RefCell;
use std::path::Path;

use ed25519_dalek::SigningKey;
use serde_json::{Value, json};

use super::*;
use crate::core::hive::replica::log::StoredEvent;
use crate::core::hive::sync::driver::PlaceApi;

const PK: [u8; 32] = [9; 32];
const LIB_V1: &str = "pub fn relay() {}\n\npub fn other() -> u8 {\n    1\n}\n";
const LIB_V2: &str = "pub fn relay() {}\n\npub fn other() -> u8 {\n    2\n}\n";

fn git(root: &Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

fn sha(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// A checkout at its first commit, with the digests claims anchored then.
fn checkout() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("pay");
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/lib.rs"), LIB_V1).unwrap();
    std::fs::write(root.join("README.md"), "# pay\n").unwrap();
    std::fs::write(root.join("src/old.rs"), "pub fn old() {}\n").unwrap();
    git(&root, &["init", "-q"]);
    git(&root, &["add", "."]);
    git(&root, &["commit", "-q", "-m", "one"]);
    (dir, root)
}

/// A commit no test checkout contains.
const FOREIGN_COMMIT: &str = "4f0a1b2c3d4e5f60718293a4b5c6d7e8f9a0b1c2";

fn anchor(r: &str, bytes: &[u8], commit: &str) -> Value {
    json!({"ref": r, "digest": sha(bytes), "commit": commit})
}

/// The checkout's `HEAD`, the commit its claims were anchored at.
fn head(root: &Path) -> String {
    Checkout::new(root).head().unwrap()
}

fn add(seq: u64, node: &str, anchors: &[Value]) -> StoredEvent {
    StoredEvent {
        seq,
        event: json!({
            "event_id": format!("E{seq:025}"),
            "op": "node.add",
            "target": node,
            "node_type": "fact",
            "hlc": {"wall_ms": 1000 + seq, "counter": 0, "node": "x"},
            "worker_id": "wkr_aaaaaaaaaaaaaaaaaaaa",
            "signature": "00",
            "seq": seq,
            "body": {"statement": format!("Claim {node}."), "anchors": anchors},
        }),
        sig_invalid: false,
    }
}

/// Four anchored claims: the whole file (changes), the unchanged symbol
/// `relay`, a file that is deleted, and the README (unchanged).
fn replica(data: &Path) -> Replica {
    replica_at(data, FOREIGN_COMMIT)
}

/// [`replica`] with the claims anchored at `commit`.
fn replica_at(data: &Path, commit: &str) -> Replica {
    let (mut replica, _) = Replica::open(data, "pl_maint", &[5; 32]).unwrap();
    let relay =
        crate::core::knowledge::anchors::anchored(LIB_V1.as_bytes(), "src/lib.rs", Some("relay"))
            .unwrap()
            .to_vec();
    replica
        .insert_events(&[
            add(
                1,
                "n_fileaaaaaaaaaaaaaaaaaaaaaa",
                &[anchor("file:src/lib.rs", LIB_V1.as_bytes(), commit)],
            ),
            add(
                2,
                "n_symaaaaaaaaaaaaaaaaaaaaaaa",
                &[anchor("sym:src/lib.rs::relay", &relay, commit)],
            ),
            add(
                3,
                "n_goneaaaaaaaaaaaaaaaaaaaaaa",
                &[anchor("file:src/old.rs", b"pub fn old() {}\n", commit)],
            ),
            add(
                4,
                "n_readmeaaaaaaaaaaaaaaaaaaaa",
                &[anchor("file:README.md", b"# pay\n", commit)],
            ),
        ])
        .unwrap();
    replica.refold().unwrap();
    replica
}

fn place() -> Place {
    Place {
        kind: "vault".into(),
        id: "3f1c2b6a-8d4e-4a7b-9c10-5e2f6a7b8c01".into(),
        pk_id: PK,
        causal_base: 4,
    }
}

fn key() -> SigningKey {
    SigningKey::from_bytes(&[0x49; 32])
}

/// Built as the maintain CLI builds it, from the instance id a 1.x
/// enrolment stores (`inst_<hex>`): the dogfood confirm was refused
/// `invalid_event shape /hlc/node` while the tests used a ready UUID.
fn signer(key: &SigningKey) -> Signer<'_> {
    Signer::workspace(
        key,
        "inst_6f2b9c4e1a3d4e5f8a7b0c1d2e3f4a5b",
        "wkr_vk4oelpv5lzjmev44hun",
    )
}

fn req(args: &[&str]) -> Request {
    parse(&args.iter().map(|a| (*a).to_string()).collect::<Vec<_>>()).unwrap()
}

fn pending(replica: &Replica) -> Vec<Value> {
    replica
        .pending()
        .unwrap()
        .iter()
        .map(|(_, b)| serde_json::from_slice(b).unwrap())
        .collect()
}

/// The C0 2.0 event schema, with its common definitions.
pub(crate) fn assert_schema_valid(event: &Value) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("crates/lean-ctx-protocol/tests/fixtures/hive-v2/schemas");
    let load = |name: &str| -> Value {
        serde_json::from_slice(&std::fs::read(dir.join(name)).unwrap()).unwrap()
    };
    let mut registry = jsonschema::Registry::new();
    for entry in std::fs::read_dir(&dir).unwrap() {
        let name = entry.unwrap().file_name().to_string_lossy().into_owned();
        if name.ends_with(".schema.json") {
            registry = registry
                .add(
                    format!("https://contracts.leanctx.com/hive-v2/schemas/{name}"),
                    load(&name),
                )
                .unwrap();
        }
    }
    let registry = registry.prepare().unwrap();
    let validator = jsonschema::options()
        .with_registry(&registry)
        .build(&load("event.schema.json"))
        .unwrap();
    let errors: Vec<String> = validator
        .iter_errors(event)
        .map(|e| e.to_string())
        .collect();
    assert!(errors.is_empty(), "{errors:?}\n{event:#}");
}

/// Records every request; answers a push with `held` for the event ids in
/// `hold` and `accepted` for the rest.
#[derive(Default)]
struct Recorder {
    calls: RefCell<Vec<String>>,
    sent: RefCell<usize>,
    hold: Vec<String>,
}

impl PlaceApi for Recorder {
    fn get(&self, path: &str) -> Result<Value, HiveError> {
        self.calls.borrow_mut().push(format!("GET {path}"));
        Err(HiveError::NotFound)
    }
    fn get_bytes(&self, path: &str, _max: usize) -> Result<Vec<u8>, HiveError> {
        self.calls.borrow_mut().push(format!("GET {path}"));
        Err(HiveError::NotFound)
    }
    fn post(&self, path: &str, body: &Value) -> Result<Value, HiveError> {
        self.calls.borrow_mut().push(format!("POST {path}"));
        *self.sent.borrow_mut() += body["events"].as_array().map_or(0, Vec::len);
        let results: Vec<Value> = body["events"]
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
            .map(|(i, e)| {
                let id = e["event_id"].as_str().unwrap();
                if self.hold.iter().any(|h| h == id) {
                    json!({"event_id": id, "status": "held", "review_id": "rv_01"})
                } else {
                    json!({"event_id": id, "status": "accepted", "seq": 100 + i})
                }
            })
            .collect();
        Ok(json!({ "results": results }))
    }
}

#[test]
fn arguments_follow_the_spec() {
    let all = req(&["acme/pay"]);
    assert!(all.stale && all.contradictions && all.dedupe);
    assert_eq!(all.limit, DEFAULT_LIMIT);
    let one = req(&["acme/pay", "--dedupe", "--limit", "200"]);
    assert!(!one.stale && one.dedupe && !one.contradictions);
    assert_eq!(one.limit, 200);
    let confirm = req(&[
        "acme/pay",
        "--stale",
        "--confirm",
        "n_fileaaaaaaaaaaaaaaaaaaaaaa",
    ]);
    assert_eq!(
        confirm.confirm.as_deref(),
        Some("n_fileaaaaaaaaaaaaaaaaaaaaaa")
    );
    let bad = |args: &[&str]| parse(&args.iter().map(|a| (*a).to_string()).collect::<Vec<_>>());
    assert!(bad(&["acme/pay", "--limit", "0"]).is_err());
    assert!(bad(&["acme/pay", "--limit", "201"]).is_err());
    assert!(
        bad(&["acme/pay", "--confirm", "n_fileaaaaaaaaaaaaaaaaaaaaaa"]).is_err(),
        "only with --stale"
    );
    assert!(bad(&["--stale"]).is_err(), "a vault is named");
}

#[test]
fn an_anchor_change_yields_to_verify_and_a_gone_file_gone() {
    let (_dir, root) = checkout();
    let data = tempfile::tempdir().unwrap();
    let replica = replica_at(data.path(), &head(&root));
    std::fs::write(root.join("src/lib.rs"), LIB_V2).unwrap();
    std::fs::remove_file(root.join("src/old.rs")).unwrap();
    git(&root, &["commit", "-qam", "two"]);

    let key = key();
    let signer = signer(&key);
    let place = place();
    let target = Target {
        replica: &replica,
        place: &place,
        signer: &signer,
        seal: None,
    };
    let mut co = Checkout::new(&root);
    // No model, no network: the deterministic task alone.
    let counts = run_stale(
        &target,
        &mut co,
        &req(&["acme/pay", "--stale"]),
        1_792_800_000_000,
    )
    .unwrap();
    assert_eq!(
        counts,
        Counts {
            done: 2,
            to_verify: 1,
            gone: 1,
            skipped: 2,
            ..Counts::default()
        }
    );
    let events = pending(&replica);
    let state_of = |node: &str| {
        events
            .iter()
            .find(|e| e["target"] == node)
            .map(|e| e["body"]["value"].clone())
    };
    assert_eq!(
        state_of("n_fileaaaaaaaaaaaaaaaaaaaaaa"),
        Some(json!("to_verify"))
    );
    assert_eq!(
        state_of("n_goneaaaaaaaaaaaaaaaaaaaaaa"),
        Some(json!("gone"))
    );
    assert_eq!(
        state_of("n_symaaaaaaaaaaaaaaaaaaaaaaa"),
        None,
        "the anchored symbol did not change"
    );
    assert_eq!(state_of("n_readmeaaaaaaaaaaaaaaaaaaaa"), None);
    let public = key.verifying_key().to_bytes();
    for e in &events {
        assert_eq!(e["op"], "field.set");
        assert_eq!(e["body"]["field"], "anchor_state");
        assert_eq!(e["provenance_claim"], "workspace");
        assert_eq!(e["worker_id"], "wkr_vk4oelpv5lzjmev44hun");
        assert!(sign::verify(e, &public), "signed with the instance key");
        assert_schema_valid(e);
    }
    let shown = counts.line();
    assert_eq!(
        shown,
        "done 2 · to_verify 1 · gone 1 · merged 0 · retracted 0 · sent to review 0 · skipped 2"
    );
    assert!(!shown.contains("Claim"), "never node texts");
}

#[test]
fn a_state_already_folded_is_not_written_again() {
    let (_dir, root) = checkout();
    let data = tempfile::tempdir().unwrap();
    let mut replica = replica_at(data.path(), &head(&root));
    std::fs::write(root.join("src/lib.rs"), LIB_V2).unwrap();
    git(&root, &["commit", "-qam", "two"]);
    let mut set = add(5, "n_fileaaaaaaaaaaaaaaaaaaaaaa", &[]);
    set.event["op"] = json!("field.set");
    set.event["body"] = json!({"field": "anchor_state", "value": "to_verify"});
    set.event.as_object_mut().unwrap().remove("node_type");
    replica.insert_events(&[set]).unwrap();
    replica.refold().unwrap();

    let key = key();
    let signer = signer(&key);
    let place = place();
    let target = Target {
        replica: &replica,
        place: &place,
        signer: &signer,
        seal: None,
    };
    let counts = run_stale(
        &target,
        &mut Checkout::new(&root),
        &req(&["acme/pay", "--stale"]),
        1_792_800_000_000,
    )
    .unwrap();
    assert_eq!(counts.done, 0);
    assert_eq!(counts.skipped, 4);
    assert!(pending(&replica).is_empty());
}

/// v5 2.2.1: matching bytes never clear a mark on their own (an engine on
/// an older commit would flip it back); a confirm of a marked claim writes
/// `fresh` right after the new anchors (knowledge state outdated → back).
#[test]
fn only_a_confirm_clears_an_earlier_mark_with_fresh() {
    let (_dir, root) = checkout();
    let data = tempfile::tempdir().unwrap();
    let mut replica = replica_at(data.path(), &head(&root));
    let mut set = add(5, "n_fileaaaaaaaaaaaaaaaaaaaaaa", &[]);
    set.event["op"] = json!("field.set");
    set.event["body"] = json!({"field": "anchor_state", "value": "to_verify"});
    set.event.as_object_mut().unwrap().remove("node_type");
    replica.insert_events(&[set]).unwrap();
    replica.refold().unwrap();
    let key = key();
    let signer = signer(&key);
    let place = place();
    let target = Target {
        replica: &replica,
        place: &place,
        signer: &signer,
        seal: None,
    };
    let start = crate::core::hive::events::hlc::Stamp::default();
    let (counts, _) = run_stale_at(
        &target,
        &mut Checkout::new(&root),
        &req(&["acme/pay", "--stale"]),
        1_792_800_000_000,
        start,
    )
    .unwrap();
    assert_eq!(counts.done, 0);
    assert!(pending(&replica).is_empty(), "no automatic fresh");
    let (counts, clock) = run_stale_at(
        &target,
        &mut Checkout::new(&root),
        &req(&[
            "acme/pay",
            "--stale",
            "--confirm",
            "n_fileaaaaaaaaaaaaaaaaaaaaaa",
        ]),
        1_792_800_000_000,
        start,
    )
    .unwrap();
    assert_eq!(counts.done, 1);
    let events = pending(&replica);
    let fields: Vec<(&Value, &Value)> = events
        .iter()
        .map(|e| (&e["body"]["field"], &e["body"]["value"]))
        .filter(|(f, _)| *f == "anchor_state")
        .collect();
    assert_eq!(events.len(), 2, "anchors, then anchor_state");
    assert_eq!(fields, vec![(&json!("anchor_state"), &json!("fresh"))]);
    for e in &events {
        assert_eq!(e["hlc"]["node"], "6f2b9c4e-1a3d-4e5f-8a7b-0c1d2e3f4a5b");
        assert_schema_valid(e);
    }
    let last = events
        .iter()
        .map(|e| e["hlc"]["counter"].as_u64().unwrap())
        .max()
        .unwrap();
    assert_eq!(
        u64::from(clock.counter),
        last,
        "the returned clock is the last event's"
    );
}

#[test]
fn confirm_reanchors_the_claim_at_head_with_verified_at() {
    let (_dir, root) = checkout();
    let data = tempfile::tempdir().unwrap();
    let replica = replica_at(data.path(), &head(&root));
    std::fs::write(root.join("src/lib.rs"), LIB_V2).unwrap();
    git(&root, &["commit", "-qam", "two"]);
    let key = key();
    let signer = signer(&key);
    let place = place();
    let target = Target {
        replica: &replica,
        place: &place,
        signer: &signer,
        seal: None,
    };
    let now = 1_792_800_018_000;
    let mut co = Checkout::new(&root);
    let head = co.head().unwrap();
    let counts = run_stale(
        &target,
        &mut co,
        &req(&[
            "acme/pay",
            "--stale",
            "--confirm",
            "n_fileaaaaaaaaaaaaaaaaaaaaaa",
        ]),
        now,
    )
    .unwrap();
    assert_eq!(counts.done, 1);
    let events = pending(&replica);
    assert_eq!(events.len(), 1, "one field.set, never a second event");
    let e = &events[0];
    assert_eq!(e["body"]["field"], "anchors");
    assert_eq!(e["body"]["verified_at"], now);
    assert_eq!(
        e["body"]["value"],
        json!([{"ref": "file:src/lib.rs", "digest": sha(LIB_V2.as_bytes()), "commit": head, "verified_at": now}])
    );
    assert_schema_valid(e);
    // A claim whose anchored file is gone cannot be confirmed.
    std::fs::remove_file(root.join("src/old.rs")).unwrap();
    git(&root, &["commit", "-qam", "three"]);
    assert!(
        run_stale(
            &target,
            &mut Checkout::new(&root),
            &req(&[
                "acme/pay",
                "--stale",
                "--confirm",
                "n_goneaaaaaaaaaaaaaaaaaaaaaa"
            ]),
            now
        )
        .is_err()
    );
}

#[test]
fn host_mode_dedupe_makes_no_e42_call() {
    let (_dir, root) = checkout();
    let data = tempfile::tempdir().unwrap();
    let replica = replica_at(data.path(), &head(&root));
    std::fs::write(root.join("src/lib.rs"), LIB_V2).unwrap();
    git(&root, &["commit", "-qam", "two"]);
    let key = key();
    let signer = signer(&key);
    let place = place();
    let target = Target {
        replica: &replica,
        place: &place,
        signer: &signer,
        seal: None,
    };
    let api = Recorder::default();
    for args in [
        &["acme/pay", "--dedupe"][..],
        &["acme/pay", "--contradictions"],
        &["acme/pay"],
    ] {
        let (_, lines) = run_host(
            &target,
            &mut Checkout::new(&root),
            &req(args),
            Some((&api, "vault:pl_maint")),
            1_792_800_000_000,
        )
        .unwrap();
        assert!(
            lines
                .iter()
                .any(|l| l
                    == "Judgment tasks run in the review loop: lean-ctx review attach acme/pay"),
            "{lines:?}"
        );
    }
    assert_eq!(*api.sent.borrow(), 1, "the stale event once, never twice");
    let calls = api.calls.borrow();
    assert!(
        calls
            .iter()
            .all(|c| c == "POST /places/vault:pl_maint/events"),
        "only E5 pushes of the stale events: {calls:?}"
    );
    assert!(!calls.iter().any(|c| c.contains("/maintenance")));
}

#[test]
fn the_place_holds_the_51st_event_and_it_stays_pending_as_sent_to_review() {
    let data = tempfile::tempdir().unwrap();
    let replica = replica(data.path());
    let key = key();
    let signer = signer(&key);
    let place = place();
    let mut clock = Stamp::default();
    let mut ids = Vec::new();
    for i in 0..51u64 {
        clock = hlc::send(clock, 1_792_800_000_000 + i);
        let e = field_set(
            &place,
            "n_fileaaaaaaaaaaaaaaaaaaaaaa",
            "anchor_state",
            json!("to_verify"),
            None,
            clock,
            &signer,
        )
        .unwrap();
        ids.push(e["event_id"].as_str().unwrap().to_string());
        let target = Target {
            replica: &replica,
            place: &place,
            signer: &signer,
            seal: None,
        };
        queue(&target, e, clock).unwrap();
    }
    let api = Recorder {
        hold: vec![ids[50].clone()],
        ..Recorder::default()
    };
    let target = Target {
        replica: &replica,
        place: &place,
        signer: &signer,
        seal: None,
    };
    let (counts, _) = run_host(
        &target,
        &mut Checkout::new(data.path()),
        &Request {
            stale: false,
            ..req(&["acme/pay", "--dedupe"])
        },
        Some((&api, "vault:pl_maint")),
        1_792_800_000_000,
    )
    .unwrap();
    assert_eq!(counts.sent_to_review, 1);
    // All 51 went out (no engine-side threshold); the held one waits.
    assert_eq!(*api.sent.borrow(), 51);
    let left: Vec<String> = replica
        .pending()
        .unwrap()
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    assert!(left.contains(&ids[50]), "held stays pending");
}

#[test]
fn a_sealed_place_gets_its_maintenance_events_sealed() {
    let (_dir, root) = checkout();
    let data = tempfile::tempdir().unwrap();
    let replica = replica_at(data.path(), &head(&root));
    std::fs::write(root.join("src/lib.rs"), LIB_V2).unwrap();
    git(&root, &["commit", "-qam", "two"]);
    let key = key();
    let signer = signer(&key);
    let place = place();
    let seal = crate::core::hive::events::sealed_body::SealKey {
        epoch: 1,
        key: zeroize::Zeroizing::new([3; 32]),
    };
    let target = Target {
        replica: &replica,
        place: &place,
        signer: &signer,
        seal: Some(&seal),
    };
    let counts = run_stale(
        &target,
        &mut Checkout::new(&root),
        &req(&["acme/pay", "--stale"]),
        1_792_800_000_000,
    )
    .unwrap();
    assert_eq!(counts.to_verify, 1);
    let events = pending(&replica);
    assert_eq!(events.len(), 1);
    let e = &events[0];
    assert!(e["body"].get("ct").is_some(), "the body is ciphertext");
    assert!(e["body"].get("field").is_none());
    assert!(e["ck_wrapped"].is_string());
    assert_eq!(e["epoch"], 1);
    assert_eq!(
        e["flags"],
        json!({"commands": false}),
        "sealed events carry flags"
    );
    assert!(sign::verify(e, &key.verifying_key().to_bytes()));
}

/// KNOWLEDGE-5's lesson (v5 2.2.1): the engine wrote `anchor_state` on
/// facts while C0 allowed it on references only, and the server refused
/// every such event. Every `field_set(…)` call passes a field listed in
/// `FIELD_SET_FIELDS`, nothing else builds a `field.set`, and each listed
/// field is mutable in the C0 node-types for every node type with anchors.
#[test]
fn every_field_the_engine_sets_is_mutable_in_c0() {
    fn walk(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|x| x == "rs")
                && !p.file_name().unwrap().to_string_lossy().contains("test")
            {
                out.push(p);
            }
        }
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    walk(&root.join("src"), &mut files);
    files.sort();
    let mut seen = std::collections::BTreeSet::new();
    for p in &files {
        let all = std::fs::read_to_string(p).unwrap();
        // Only a file's test module may build events by hand.
        let src = all.split("#[cfg(test)]").next().unwrap_or_default();
        if src.contains("\"op\": \"field.set\"") {
            assert!(
                p.ends_with("core/hive/review/maintain.rs"),
                "{}: builds a field.set outside maintain::field_set",
                p.display()
            );
        }
        for (i, _) in src.match_indices("field_set(") {
            let before = &src[..i];
            // The definition, and other `*field_set(` functions.
            if before.ends_with("fn ")
                || before.ends_with(|c: char| c.is_alphanumeric() || c == '_')
            {
                continue;
            }
            // The third argument is the field: a string literal.
            let mut depth = 0;
            let mut parts = vec![String::new()];
            for c in src[i + "field_set(".len()..].chars() {
                match c {
                    ')' | ']' | '}' if depth == 0 => break,
                    '(' | '[' | '{' => depth += 1,
                    ')' | ']' | '}' => depth -= 1,
                    ',' if depth == 0 => {
                        parts.push(String::new());
                        continue;
                    }
                    _ => {}
                }
                parts.last_mut().unwrap().push(c);
            }
            let field = parts
                .get(2)
                .map(|s| s.trim().to_string())
                .unwrap_or_default();
            assert!(
                field.len() >= 2 && field.starts_with('"') && field.ends_with('"'),
                "{}: field_set with a non-literal field {field:?}",
                p.display()
            );
            seen.insert(field.trim_matches('"').to_string());
        }
    }
    let listed: std::collections::BTreeSet<String> =
        FIELD_SET_FIELDS.iter().map(ToString::to_string).collect();
    assert_eq!(seen, listed, "FIELD_SET_FIELDS lists what the engine sets");

    let types: Value = serde_json::from_str(
        &std::fs::read_to_string(
            root.join("crates/lean-ctx-protocol/tests/fixtures/hive-v2/enums/node-types.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let anchored: Vec<&Value> = types["values"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| t["anchors"] == true)
        .collect();
    assert!(anchored.len() >= 3, "fact, gotcha, procedure carry anchors");
    for t in anchored {
        let mutable: Vec<&str> = t["mutable"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        for f in FIELD_SET_FIELDS {
            assert!(
                mutable.contains(&f),
                "C0 node-types: {f} is not mutable on {}",
                t["value"]
            );
        }
    }
}

/// Dogfood 25.10. (anchor flapping): only an engine whose `HEAD` contains
/// the commit a claim was anchored at judges the anchor. Engines on
/// different commits otherwise kept marking each other's claims.
#[test]
fn only_a_head_that_contains_the_anchor_commit_judges_it() {
    let (_dir, root) = checkout();
    let one = head(&root);
    std::fs::write(root.join("src/lib.rs"), LIB_V2).unwrap();
    git(&root, &["commit", "-qam", "two"]);
    let two = head(&root);
    let data = tempfile::tempdir().unwrap();
    let (mut replica, _) = Replica::open(data.path(), "pl_flap", &[5; 32]).unwrap();
    // A newer engine's claim (at two) and an older engine's (at one).
    let newer = "n_neweraaaaaaaaaaaaaaaaaaaaa";
    let older = "n_olderaaaaaaaaaaaaaaaaaaaaa";
    replica
        .insert_events(&[
            add(
                1,
                newer,
                &[anchor("file:src/lib.rs", LIB_V2.as_bytes(), &two)],
            ),
            add(
                2,
                older,
                &[anchor("file:src/lib.rs", LIB_V1.as_bytes(), &one)],
            ),
        ])
        .unwrap();
    let graph = replica.refold().unwrap();
    let tasks = |root: &Path| {
        let (tasks, _) = anchor_tasks(&graph, &mut Checkout::new(root), DEFAULT_LIMIT);
        tasks
            .into_iter()
            .map(|t| (t.node_id, t.verdict))
            .collect::<Vec<_>>()
    };
    // At two (newer than the older claim): the older claim is judged.
    assert_eq!(
        tasks(&root),
        vec![(older.to_string(), AnchorVerdict::ToVerify)]
    );
    // Back at one (older than the newer claim): nothing is judged; the
    // newer claim's anchor is unknown here, not outdated.
    git(&root, &["checkout", "-q", &one]);
    assert_eq!(tasks(&root), vec![]);
    // A branch that diverged at one: the newer claim stays unknown, the
    // claim at one (an ancestor) is judged against the changed bytes.
    git(&root, &["checkout", "-q", "-b", "side"]);
    std::fs::write(root.join("src/lib.rs"), "pub fn relay() {}\n// side\n").unwrap();
    git(&root, &["commit", "-qam", "side"]);
    assert_eq!(
        tasks(&root),
        vec![(older.to_string(), AnchorVerdict::ToVerify)]
    );
}
