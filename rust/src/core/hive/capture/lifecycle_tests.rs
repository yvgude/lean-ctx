// SPDX-License-Identifier: Apache-2.0
use ed25519_dalek::SigningKey;

use super::*;
use crate::core::hive::events::build::{Place, Signer};

fn own(node: &str, key: &str, digest: &str) -> OwnNode {
    OwnNode {
        node_id: node.into(),
        category: "decision".into(),
        key: key.into(),
        value_digest: digest.into(),
    }
}

fn fact(key: &str, digest: &str, current: bool) -> StoreFact {
    StoreFact {
        category: "decision".into(),
        key: key.into(),
        value_digest: digest.into(),
        current,
        confirmed_here: false,
    }
}

fn live(_: &str) -> Option<NodeView> {
    Some(NodeView {
        live: true,
        marked: false,
        has_anchors: false,
    })
}

#[test]
fn a_current_fact_leaves_its_node_alone() {
    let actions = plan(
        &[own("n_a", "db", "d1")],
        &[fact("db", "d1", true)],
        &BTreeSet::new(),
        live,
        &Present::default(),
    );
    assert!(actions.is_empty());
}

#[test]
fn a_superseded_fact_whose_successor_has_a_node_is_superseded_not_retracted() {
    let actions = plan(
        &[own("n_old", "db", "d1"), own("n_new", "db", "d2")],
        &[fact("db", "d1", false), fact("db", "d2", true)],
        &BTreeSet::new(),
        live,
        &Present::default(),
    );
    assert_eq!(
        actions,
        vec![Action::Supersede {
            old: "n_old".into(),
            new: "n_new".into()
        }]
    );
}

#[test]
fn a_removed_fact_is_withdrawn() {
    let actions = plan(
        &[own("n_gone", "queue", "d9")],
        &[fact("db", "d1", true)],
        &BTreeSet::new(),
        live,
        &Present::default(),
    );
    assert_eq!(
        actions,
        vec![Action::Retract {
            node: "n_gone".into()
        }]
    );
}

#[test]
fn a_superseded_fact_whose_successor_was_not_shared_is_withdrawn() {
    // The successor stayed local (the capture gate refused it): the old
    // text is no longer what its author holds, so it is withdrawn.
    let actions = plan(
        &[own("n_old", "db", "d1")],
        &[fact("db", "d1", false), fact("db", "d2", true)],
        &BTreeSet::new(),
        live,
        &Present::default(),
    );
    assert_eq!(
        actions,
        vec![Action::Retract {
            node: "n_old".into()
        }]
    );
}

#[test]
fn an_empty_or_unreadable_store_withdraws_nothing() {
    let actions = plan(
        &[own("n_a", "db", "d1")],
        &[],
        &BTreeSet::new(),
        live,
        &Present::default(),
    );
    assert!(actions.is_empty());
    // Unless the journal records that very fact as removed.
    let removed = BTreeSet::from([("decision".to_owned(), "db".to_owned())]);
    let actions = plan(
        &[own("n_a", "db", "d1"), own("n_b", "queue", "d2")],
        &[],
        &removed,
        live,
        &Present::default(),
    );
    assert_eq!(actions, vec![Action::Retract { node: "n_a".into() }]);
    let actions = plan(
        &[own("n_a", "db", "d1")],
        &[],
        &BTreeSet::new(),
        live,
        &Present::default(),
    );
    assert!(actions.is_empty());
}

#[test]
fn retracted_nodes_and_already_queued_actions_are_not_repeated() {
    let gone = |id: &str| {
        Some(NodeView {
            live: id != "n_dead",
            marked: false,
            has_anchors: false,
        })
    };
    let mut present = Present::default();
    present.retracting.insert("n_pending".into());
    present.supersedes.insert(("n_new".into(), "n_old".into()));
    let actions = plan(
        &[
            own("n_dead", "x", "dx"),
            own("n_pending", "y", "dy"),
            own("n_old", "db", "d1"),
            own("n_new", "db", "d2"),
        ],
        &[
            fact("db", "d1", false),
            fact("db", "d2", true),
            fact("z", "dz", true),
        ],
        &BTreeSet::new(),
        gone,
        &present,
    );
    assert!(actions.is_empty(), "{actions:?}");
}

#[test]
fn a_successor_captured_in_this_sync_supersedes_through_the_pending_queue() {
    let graph = Graph::default();
    let mut present = Present::default();
    present.adding.insert("n_new".into());
    // n_old is folded in the graph-less test only through the pending set too.
    present.adding.insert("n_old".into());
    let actions = plan(
        &[own("n_old", "db", "d1"), own("n_new", "db", "d2")],
        &[fact("db", "d1", false), fact("db", "d2", true)],
        &BTreeSet::new(),
        |id| present.view(&graph, id),
        &present,
    );
    assert_eq!(
        actions,
        vec![Action::Supersede {
            old: "n_old".into(),
            new: "n_new".into()
        }]
    );
}

#[test]
fn a_marked_claim_is_confirmed_only_when_the_local_fact_holds_here() {
    let marked = |_: &str| {
        Some(NodeView {
            live: true,
            marked: true,
            has_anchors: true,
        })
    };
    let mut holds = fact("db", "d1", true);
    holds.confirmed_here = true;
    assert_eq!(
        plan(
            &[own("n_a", "db", "d1")],
            &[holds],
            &BTreeSet::new(),
            marked,
            &Present::default()
        ),
        vec![Action::Confirm { node: "n_a".into() }]
    );
    // Under review locally, or its anchors do not match here: left marked.
    assert!(
        plan(
            &[own("n_a", "db", "d1")],
            &[fact("db", "d1", true)],
            &BTreeSet::new(),
            marked,
            &Present::default()
        )
        .is_empty()
    );
}

#[test]
fn queued_events_are_signed_sealed_like_curation_and_carry_no_content() {
    let data = tempfile::tempdir().unwrap();
    let (replica, _) = Replica::open(data.path(), "pl_lifecycle", &[3; 32]).unwrap();
    let key = SigningKey::from_bytes(&[0x49; 32]);
    let place = Place {
        kind: "vault".into(),
        id: "3f1c2b6a-8d4e-4a7b-9c10-5e2f6a7b8c01".into(),
        pk_id: [9; 32],
        causal_base: 0,
    };
    let signer = Signer {
        key: &key,
        hlc_node: "6f2b9c4e-1a3d-4e5f-8a7b-0c1d2e3f4a5b".into(),
        worker_id: "wkr_vk4oelpv5lzjmev44hun".into(),
    };
    let target = maintain::Target {
        replica: &replica,
        place: &place,
        signer: &signer,
        seal: None,
    };
    let repo = crate::core::hive::capture::tests::repo();
    let mut checkout = Checkout::new(repo.path());
    let start = Stamp {
        wall_ms: 1_792_800_000_000,
        counter: 0,
    };
    let actions = [
        Action::Supersede {
            old: "n_old".into(),
            new: "n_new".into(),
        },
        Action::Retract {
            node: "n_gone".into(),
        },
    ];
    let queued = queue(
        &target,
        &Graph::default(),
        &mut checkout,
        &actions,
        start,
        start.wall_ms,
    )
    .unwrap();
    assert_eq!(
        (queued.superseded, queued.retracted, queued.confirmed),
        (1, 1, 0)
    );
    assert!(queued.clock > start);

    let events: Vec<Value> = replica
        .pending()
        .unwrap()
        .iter()
        .map(|(_, bytes)| serde_json::from_slice(bytes).unwrap())
        .collect();
    assert_eq!(events.len(), 2);
    for event in &events {
        assert!(crate::core::hive::events::sign::verify(
            event,
            key.verifying_key().as_bytes()
        ));
    }
    let edge = events.iter().find(|e| e["op"] == "edge.add").unwrap();
    assert_eq!(edge["body"]["kind"], "supersedes");
    assert_eq!(
        (edge["body"]["from"].as_str(), edge["body"]["to"].as_str()),
        (Some("n_new"), Some("n_old"))
    );
    let retract = events.iter().find(|e| e["op"] == "retract").unwrap();
    assert_eq!(retract["target"], "n_gone");
    assert_eq!(retract["body"]["reason"], WITHDRAWN_REASON);

    // Present sees what was queued: a second plan queues nothing more.
    let seen = present(&Graph::default(), &replica).unwrap();
    assert!(seen.supersedes.contains(&("n_new".into(), "n_old".into())));
    assert!(seen.retracting.contains("n_gone"));
}

#[test]
fn own_nodes_are_this_checkouts_fact_sources() {
    let data = tempfile::tempdir().unwrap();
    let (replica, _) = Replica::open(data.path(), "pl_own", &[4; 32]).unwrap();
    let source = |project: &str, key: &str| serde_json::json!({"project_hash": project, "category": "decision", "key": key, "value_digest": "d"});
    replica
        .put_own_source("n_mine", "fact", &source("ph", "db"))
        .unwrap();
    replica
        .put_own_source("n_other", "fact", &source("other", "db"))
        .unwrap();
    replica
        .put_own_source("n_episode", "episode", &source("ph", "e"))
        .unwrap();
    let nodes = own_nodes(&replica, "ph").unwrap();
    assert_eq!(nodes, vec![own("n_mine", "db", "d")]);
}
