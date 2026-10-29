// SPDX-License-Identifier: Apache-2.0
//! `fold.vectors.json` (C0 2.0 `vectors/fold.vectors.json`): event logs and
//! the projection the engine's fold makes of them, in every listed arrival
//! order. The file is what this fold produces; a fold change that moves it
//! fails here. Regenerate deliberately with
//! `LEAN_CTX_BLESS_FOLD_VECTORS=1 cargo test --lib fold_vectors`.

use serde_json::{Value, json};

use super::super::*;

const FILE: &str = "src/core/hive/replica/fold.vectors.json";

fn event_json(e: &FoldEvent) -> Value {
    let mut v = json!({"seq": e.seq, "event_id": e.event_id, "op": e.op, "body": e.body});
    if let Some(t) = &e.target {
        v["target"] = json!(t);
    }
    if let Some(t) = &e.node_type {
        v["node_type"] = json!(t);
    }
    if e.sig_invalid {
        v["sig_invalid"] = json!(true);
    }
    v
}

fn event_of(v: &Value) -> FoldEvent {
    let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
    FoldEvent {
        seq: v["seq"].as_u64().unwrap(),
        event_id: s("event_id").unwrap(),
        op: s("op").unwrap(),
        target: s("target"),
        node_type: s("node_type"),
        body: v.get("body").filter(|b| !b.is_null()).cloned(),
        sig_invalid: v["sig_invalid"].as_bool().unwrap_or(false),
        provenance: None,
        review_state: None,
    }
}

/// The projection two folds must agree on (server fields left out).
fn projection(g: &Graph) -> Value {
    let nodes: serde_json::Map<String, Value> = g
        .nodes
        .iter()
        .map(|(id, n)| {
            (
                id.clone(),
                json!({"node_type": n.node_type, "fields": n.fields,
                       "retracted": n.retracted, "purged": n.purged}),
            )
        })
        .collect();
    let edges: serde_json::Map<String, Value> = g
        .edges
        .iter()
        .map(|(id, e)| {
            (
                id.clone(),
                json!({"kind": e.kind, "from": e.from, "to": e.to, "retracted": e.retracted}),
            )
        })
        .collect();
    let purged: serde_json::Map<String, Value> = g
        .purged_nodes
        .iter()
        .map(|(id, p)| (id.clone(), json!(p.reason)))
        .collect();
    let live: Vec<&String> = g.live_nodes().map(|(id, _)| id).collect();
    json!({
        "cursor": g.cursor,
        "digest": g.digest(),
        "nodes": nodes,
        "edges": edges,
        "live_nodes": live,
        "purged_nodes": purged,
        "tombstones": g.tombstones.iter().map(|(p, s)| json!([p, s])).collect::<Vec<_>>(),
    })
}

fn add(seq: u64, target: &str, body: Value) -> FoldEvent {
    FoldEvent {
        seq,
        event_id: format!("01M5NKA7VG00{seq:014}"),
        op: "node.add".into(),
        target: Some(target.into()),
        node_type: Some("fact".into()),
        body: Some(body),
        sig_invalid: false,
        provenance: None,
        review_state: None,
    }
}

fn op(seq: u64, op: &str, target: Option<&str>, body: Value) -> FoldEvent {
    FoldEvent {
        target: target.map(str::to_string),
        node_type: None,
        op: op.into(),
        ..add(seq, "", body)
    }
}

const NA: &str = "n_aaaaaaaaaaaaaaaaaaaaaaaaaa";
const NB: &str = "n_bbbbbbbbbbbbbbbbbbbbbbbbbb";
const EAB: &str = "e_abababababababababababab";

fn claim(text: &str) -> Value {
    json!({"statement": text, "evidence": "stated", "body_nonce": "0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a"})
}

/// `(name, what it shows, events in seq order, arrival orders)`.
fn cases() -> Vec<(&'static str, &'static str, Vec<FoldEvent>, Vec<Vec<usize>>)> {
    let contract: Vec<FoldEvent> = [
        "node.add.fact.json",
        "node.add.procedure.json",
        "purged.finding.json",
        "edge.add.json",
        "field.set.json",
        "retract.json",
        "restore.json",
        "system.retract.person.json",
    ]
    .iter()
    .map(|n| super::fixture_event(n))
    .collect();
    let two_engines = vec![
        add(
            1,
            NA,
            claim("The relay drops frames when the link is slow."),
        ),
        add(
            2,
            NB,
            claim("The outbox retries a refused push after renewal."),
        ),
        op(
            3,
            "field.set",
            Some(NB),
            json!({"field": "evidence", "value": "observed"}),
        ),
        add(
            4,
            NA,
            claim("The relay drops frames when the link is slow!"),
        ),
        op(
            5,
            "edge.add",
            Some(EAB),
            json!({"kind": "depends_on", "from": NA, "to": NB}),
        ),
        op(6, "retract", Some(NB), json!({"reason": "Superseded."})),
        op(7, "restore", Some(NB), json!({"reason": "Still true."})),
        op(8, "retract", Some(EAB), json!({"reason": "Wrong link."})),
    ];
    let purge = {
        let first = add(1, NA, claim("A token was pasted into this claim."));
        let purged_event = first.event_id.clone();
        vec![
            first,
            op(
                2,
                "field.set",
                Some(NA),
                json!({"field": "evidence", "value": "observed"}),
            ),
            add(
                3,
                NB,
                claim("The outbox retries a refused push after renewal."),
            ),
            op(
                4,
                "purge",
                None,
                json!({"purge_id": "prg_aaaaaaaaaaaaaaaaaaaaaaezcm", "reason": "leaked_secret",
                       "targets": [purged_event], "cascade": [], "bus_cascade": []}),
            ),
            op(
                5,
                "field.set",
                Some(NA),
                json!({"field": "evidence", "value": "stated"}),
            ),
        ]
    };
    let mut forged = add(1, NA, claim("A forged claim slipped into the log."));
    forged.sig_invalid = true;
    let mut stub = add(2, NB, Value::Null);
    stub.body = None;
    let skipped = vec![
        forged,
        stub,
        add(
            3,
            NA,
            claim("The relay drops frames when the link is slow."),
        ),
    ];
    let bad_reason = vec![
        add(
            1,
            NA,
            claim("The relay drops frames when the link is slow."),
        ),
        op(
            2,
            "purge",
            None,
            json!({"purge_id": "prg_x", "reason": "because", "targets": []}),
        ),
    ];
    let conflict = vec![
        add(
            5,
            NA,
            claim("The relay drops frames when the link is slow."),
        ),
        FoldEvent {
            event_id: "01M5NKA7VG00OTHER000000005".into(),
            ..add(
                5,
                NB,
                claim("The outbox retries a refused push after renewal."),
            )
        },
    ];
    let rev = |n: usize| (0..n).rev().collect::<Vec<_>>();
    vec![
        (
            "contract_events",
            "the C0 event fixtures in seq order: field.set wins over the added value, a purged stub folds without text",
            contract.clone(),
            vec![rev(contract.len()), vec![4, 0, 6, 1, 7, 2, 5, 3]],
        ),
        (
            "two_engines_interleaved",
            "two engines add the same claim (first text kept, only the seq moves), retract/restore by seq, edge retract",
            two_engines,
            vec![
                rev(8),
                vec![1, 3, 5, 7, 0, 2, 4, 6],
                vec![7, 0, 6, 1, 5, 2, 4, 3],
            ],
        ),
        (
            "purge_in_a_later_tail",
            "a purge shreds the node its target event added; a field.set after it does not bring text back",
            purge,
            vec![rev(5), vec![3, 0, 4, 1, 2]],
        ),
        (
            "invalid_signature_and_withheld_body",
            "an event whose signature failed moves the cursor but never folds; a body-less node.add folds as purged",
            skipped,
            vec![rev(3)],
        ),
        (
            "purge_reason_invalid",
            "a purge reason outside the four owner-approved values stops the fold (fail closed)",
            bad_reason,
            vec![],
        ),
        (
            "seq_conflict",
            "two different events with one seq stop the fold",
            conflict,
            vec![],
        ),
    ]
}

fn fold_value(events: &[FoldEvent]) -> Value {
    let mut g = Graph::default();
    match g.fold(events) {
        Ok(()) => projection(&g),
        Err(FoldError::PurgeReasonInvalid(_)) => json!({"error": "purge_reason_invalid"}),
        Err(FoldError::SeqConflict(seq)) => json!({"error": "seq_conflict", "seq": seq}),
    }
}

fn generate() -> Value {
    let cases: Vec<Value> = cases()
        .into_iter()
        .map(|(name, shows, events, arrivals)| {
            let expect = fold_value(&events);
            json!({
                "name": name,
                "shows": shows,
                "events": events.iter().map(event_json).collect::<Vec<_>>(),
                "arrivals": arrivals,
                "expect": expect,
            })
        })
        .collect();
    json!({
        "rules": {
            "version": 1,
            "fold": "apply events in seq order whatever their arrival order; an event at or below the cursor is skipped; two different events with one seq -> error seq_conflict; sig_invalid events move the cursor and never fold; node.add creates the node once (a second add of the same target keeps the first fields; a missing body or a purged event id folds a purged node without fields); field.set sets one field on a live, unpurged node; edge.add creates the edge once; retract/restore set retracted on the node or edge; purge: reason must be one of leaked_secret, gdpr_erasure, retention_schedule, illegal_content (else error purge_reason_invalid), its targets and cascade event ids are shredded and the nodes those events added lose all fields and become purged",
            "fields": "the body without body_nonce",
            "digest": "first 16 hex chars of blake3 over, for each node in id order, canonical_json([id, node_type, fields, retracted, purged]) + \"\\n\", then for each edge in id order canonical_json([id, kind, from, to, retracted]) + \"\\n\"",
            "arrivals": "each list is an arrival order of the events by index; every order must fold to expect",
            "live_nodes": "nodes neither retracted nor purged, in id order",
        },
        "cases": cases,
    })
}

#[test]
fn the_fold_matches_its_vectors() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(FILE);
    let generated = generate();
    if std::env::var_os("LEAN_CTX_BLESS_FOLD_VECTORS").is_some() {
        let mut text = serde_json::to_string_pretty(&generated).unwrap();
        text.push('\n');
        std::fs::write(&path, text).unwrap();
    }
    let file: Value =
        serde_json::from_slice(&std::fs::read(&path).expect("fold.vectors.json")).expect("json");
    assert_eq!(
        file, generated,
        "fold.vectors.json is what the engine fold makes"
    );
    // Every arrival order folds to the expected projection, read from the file.
    for case in file["cases"].as_array().unwrap() {
        let events: Vec<FoldEvent> = case["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(event_of)
            .collect();
        assert_eq!(fold_value(&events), case["expect"], "{}", case["name"]);
        for order in case["arrivals"].as_array().unwrap() {
            let shuffled: Vec<FoldEvent> = order
                .as_array()
                .unwrap()
                .iter()
                .map(|i| events[usize::try_from(i.as_u64().unwrap()).unwrap()].clone())
                .collect();
            assert_eq!(shuffled.len(), events.len());
            assert_eq!(
                fold_value(&shuffled),
                case["expect"],
                "{} {order}",
                case["name"]
            );
        }
    }
}
