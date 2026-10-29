// SPDX-License-Identifier: Apache-2.0

use std::path::Path;

use super::*;

#[path = "fold_vectors_tests.rs"]
mod vectors;

fn fixture_event(name: &str) -> FoldEvent {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("crates/lean-ctx-protocol/tests/fixtures/hive-v2/events")
        .join(name);
    let e: Value = serde_json::from_slice(&std::fs::read(&path).expect("fixture")).expect("json");
    FoldEvent {
        seq: e["seq"].as_u64().expect("seq"),
        event_id: e["event_id"].as_str().expect("event_id").to_string(),
        op: e["op"].as_str().expect("op").to_string(),
        target: e["target"].as_str().map(str::to_string),
        node_type: e["node_type"].as_str().map(str::to_string),
        body: e.get("body").filter(|b| !b.is_null()).cloned(),
        sig_invalid: false,
        provenance: e["provenance"].as_str().map(str::to_string),
        review_state: e["review_state"].as_str().map(str::to_string),
    }
}

fn ev(seq: u64, op: &str, target: &str, body: Value) -> FoldEvent {
    FoldEvent {
        seq,
        event_id: format!("E{seq:025}"),
        op: op.into(),
        target: Some(target.into()),
        node_type: (op == "node.add").then(|| "fact".to_string()),
        body: Some(body),
        sig_invalid: false,
        provenance: None,
        review_state: None,
    }
}

#[test]
fn the_contract_events_fold() {
    let names = [
        "node.add.fact.json",
        "node.add.procedure.json",
        "purged.finding.json",
        "edge.add.json",
        "field.set.json",
        "retract.json",
        "restore.json",
        "system.retract.person.json",
    ];
    let events: Vec<FoldEvent> = names.iter().map(|n| fixture_event(n)).collect();
    let mut g = Graph::default();
    g.fold(&events).unwrap();
    let fact = &g.nodes["n_tyvnza7clspg4cpmb7ew7fvl7b"];
    assert_eq!(fact.node_type, "fact");
    assert!(!fact.fields.contains_key("body_nonce"));
    assert_eq!(
        fact.fields["applies_to"],
        serde_json::json!(["services/payments/retry.rs", "services/payments/client.rs"]),
        "field.set wins over the added value"
    );
    assert!(
        g.nodes["n_zcjqmmor453nyxqgshlgpogwdm"].purged,
        "a purged stub folds without text"
    );
    assert_eq!(g.edges["e_aaaaaaaaaaaaaaaaaaaaaacqba"].kind, "references");
    assert_eq!(g.cursor, 12);
}

#[test]
fn any_arrival_order_folds_to_the_same_graph() {
    let mut events = Vec::new();
    for i in 0..40u64 {
        let node = format!("n_{i:026}");
        events.push(ev(
            i * 10 + 1,
            "node.add",
            &node,
            serde_json::json!({"statement": format!("Claim {i}."), "body_nonce": "00"}),
        ));
        events.push(ev(
            i * 10 + 2,
            "field.set",
            &node,
            serde_json::json!({"field": "evidence", "value": "stated"}),
        ));
        events.push(ev(
            i * 10 + 3,
            "field.set",
            &node,
            serde_json::json!({"field": "evidence", "value": format!("v{i}")}),
        ));
        if i % 3 == 0 {
            events.push(ev(
                i * 10 + 4,
                "retract",
                &node,
                serde_json::json!({"reason": "Gone."}),
            ));
        }
        if i % 6 == 0 {
            events.push(ev(
                i * 10 + 5,
                "restore",
                &node,
                serde_json::json!({"reason": "Back."}),
            ));
        }
        events.push(ev(
            i * 10 + 6,
            "edge.add",
            &format!("e_{i:026}"),
            serde_json::json!({"kind": "depends_on", "from": node, "to": "n_00000000000000000000000000"}),
        ));
    }
    let mut expected = Graph::default();
    expected.fold(&events).unwrap();

    let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for _ in 0..1000 {
        let mut shuffled = events.clone();
        for i in (1..shuffled.len()).rev() {
            let j = usize::try_from(next() % (i as u64 + 1)).unwrap();
            shuffled.swap(i, j);
        }
        let mut g = Graph::default();
        g.fold(&shuffled).unwrap();
        assert_eq!(g, expected);
        assert_eq!(g.digest(), expected.digest());
    }
    // Folding in two halves (tail after clone) gives the same graph too.
    let (head, tail) = events.split_at(events.len() / 2);
    let mut g = Graph::default();
    g.fold(head).unwrap();
    g.fold(tail).unwrap();
    g.fold(head).unwrap(); // already folded: skipped
    assert_eq!(g, expected);
    assert_eq!(
        expected.nodes["n_00000000000000000000000003"].fields["evidence"],
        "v3"
    );
    assert!(
        !expected.nodes["n_00000000000000000000000006"].retracted,
        "restored"
    );
    assert!(expected.nodes["n_00000000000000000000000003"].retracted);
}

#[test]
fn a_purge_in_a_later_tail_shreds_an_earlier_node() {
    let mut g = Graph::default();
    let add = ev(
        1,
        "node.add",
        "n_a",
        serde_json::json!({"statement": "Secret claim here."}),
    );
    g.fold(std::slice::from_ref(&add)).unwrap();
    let purge = FoldEvent {
        seq: 2,
        event_id: "P".into(),
        op: "purge".into(),
        target: None,
        node_type: None,
        body: Some(
            serde_json::json!({"purge_id": "prg_x", "reason": "leaked_secret", "targets": [add.event_id], "cascade": [], "bus_cascade": []}),
        ),
        sig_invalid: false,
        provenance: None,
        review_state: None,
    };
    g.fold(&[purge]).unwrap();
    let node = &g.nodes["n_a"];
    assert!(node.purged && node.fields.is_empty());
    assert_eq!(g.purged_nodes["n_a"].reason, "leaked_secret");
    assert_eq!(g.live_nodes().count(), 0);
}

#[test]
fn bad_purges_and_forged_events_do_not_fold() {
    let mut g = Graph::default();
    let mut forged = ev(
        1,
        "node.add",
        "n_f",
        serde_json::json!({"statement": "Forged."}),
    );
    forged.sig_invalid = true;
    g.fold(&[forged]).unwrap();
    assert!(g.nodes.is_empty());
    assert_eq!(g.cursor, 1, "skipped, not refetched");

    let purge = FoldEvent {
        seq: 2,
        event_id: "P".into(),
        op: "purge".into(),
        target: None,
        node_type: None,
        body: Some(serde_json::json!({"purge_id": "prg_x", "reason": "because", "targets": []})),
        sig_invalid: false,
        provenance: None,
        review_state: None,
    };
    assert_eq!(
        g.fold(&[purge]),
        Err(FoldError::PurgeReasonInvalid("because".into()))
    );
    let a = ev(5, "node.add", "n_1", serde_json::json!({}));
    let b = ev(5, "node.add", "n_2", serde_json::json!({}));
    assert_eq!(
        Graph::default().fold(&[a, b]),
        Err(FoldError::SeqConflict(5))
    );
}
