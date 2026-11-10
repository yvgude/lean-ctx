// SPDX-License-Identifier: Apache-2.0
use ed25519_dalek::SigningKey;
use serde_json::json;

use super::*;
use crate::delivery::knowledge_state::{self, State};
use crate::events::build::{Place, Signer};
use crate::replica::fold::Edge;

const PK: [u8; 32] = [9; 32];
const A: &str = "n_aaaaaaaaaaaaaaaaaaaaaaaaaa";
const B: &str = "n_bbbbbbbbbbbbbbbbbbbbbbbbbb";
const C: &str = "n_cccccccccccccccccccccccccc";

fn fact_node(subject: Option<&str>) -> Node {
    let mut applies = vec![json!("file:src/lib.rs")];
    if let Some(s) = subject {
        applies.push(json!(s));
    }
    Node {
        node_type: "fact".into(),
        fields: json!({"statement": "s", "evidence": "observed", "applies_to": applies})
            .as_object()
            .unwrap()
            .clone(),
        ..Node::default()
    }
}

fn edge(kind: &str, from: &str, to: &str) -> Edge {
    Edge {
        kind: kind.into(),
        from: from.into(),
        to: to.into(),
        ..Edge::default()
    }
}

/// Two Instances' values of one fact: A from the other Instance, B own.
fn two_values() -> (Graph, Inputs) {
    let subject = subject_ref(&PK, "fact", "payments-retry");
    let mut graph = Graph::default();
    graph.nodes.insert(A.into(), fact_node(Some(&subject)));
    graph.nodes.insert(B.into(), fact_node(Some(&subject)));
    let mut inputs = Inputs::default();
    inputs.own.insert(
        B.into(),
        Own {
            project_hash: "ph".into(),
            subject: Some(subject),
        },
    );
    (graph, inputs)
}

#[test]
fn the_subject_names_category_and_key_not_the_value() {
    let s = subject_ref(&PK, "fact", "payments-retry");
    assert!(s.starts_with(SUBJECT_PREFIX));
    assert!(s.len() <= 120, "fits an applies_to entry");
    assert_eq!(s, subject_ref(&PK, "fact", "payments-retry"));
    assert_ne!(s, subject_ref(&PK, "fact", "payments-backoff"));
    assert_ne!(s, subject_ref(&PK, "decision", "payments-retry"));
    assert_ne!(s, subject_ref(&[8; 32], "fact", "payments-retry"), "keyed");
    assert!(!s.contains("payments"), "no key text leaves the machine");
}

#[test]
fn two_values_of_one_fact_are_a_conflict_written_by_the_larger_nodes_author() {
    let (graph, inputs) = two_values();
    let planned = plan(&graph, &PK, &inputs);
    assert_eq!(planned, vec![contradiction(&PK, A, B)]);
    assert_eq!((planned[0].from.as_str(), planned[0].to.as_str()), (B, A));

    // The other Instance (author of A) folds the same log and writes nothing:
    // one writer, no race.
    let mut other = Inputs::default();
    other.own.insert(
        A.into(),
        Own {
            project_hash: "ph-other".into(),
            subject: None,
        },
    );
    assert!(plan(&graph, &PK, &other).is_empty());
}

#[test]
fn the_conflict_edge_is_the_same_on_every_instance() {
    let one = contradiction(&PK, A, B);
    assert_eq!(
        one,
        contradiction(&PK, B, A),
        "order of discovery is irrelevant"
    );
    assert!(one.edge_id.starts_with("e_") && one.edge_id.len() == 28);
    assert_ne!(one.edge_id, contradiction(&PK, A, C).edge_id);
}

#[test]
fn a_marked_queued_or_superseded_pair_is_not_written_again() {
    let (mut graph, mut inputs) = two_values();
    let c = contradiction(&PK, A, B);

    inputs.queued_edges.insert(c.edge_id.clone());
    assert!(plan(&graph, &PK, &inputs).is_empty(), "already queued");
    inputs.queued_edges.clear();

    // Any contradicts edge between the two, in either direction, by anyone.
    graph.edges.insert("e_x".into(), edge("contradicts", A, B));
    assert!(plan(&graph, &PK, &inputs).is_empty(), "already marked");
    graph.edges.clear();

    graph.edges.insert("e_s".into(), edge("supersedes", B, A));
    assert!(plan(&graph, &PK, &inputs).is_empty(), "A is superseded");
    graph.edges.clear();

    inputs.queued_superseded.insert(A.into());
    assert!(
        plan(&graph, &PK, &inputs).is_empty(),
        "A is being superseded"
    );
}

#[test]
fn other_subjects_retracted_nodes_and_one_checkouts_values_are_no_conflict() {
    let (mut graph, inputs) = two_values();
    graph.nodes.insert(
        A.into(),
        fact_node(Some(&subject_ref(&PK, "fact", "other"))),
    );
    assert!(plan(&graph, &PK, &inputs).is_empty(), "different subjects");

    let (mut graph, inputs) = two_values();
    graph.nodes.get_mut(A).unwrap().retracted = true;
    assert!(plan(&graph, &PK, &inputs).is_empty(), "A is retracted");

    let (mut graph, inputs) = two_values();
    graph.nodes.get_mut(A).unwrap().node_type = "gotcha".into();
    assert!(plan(&graph, &PK, &inputs).is_empty(), "only facts");

    // Both values from one checkout: the capture lifecycle supersedes.
    let (graph, mut inputs) = two_values();
    inputs.own.insert(
        A.into(),
        Own {
            project_hash: "ph".into(),
            subject: None,
        },
    );
    assert!(plan(&graph, &PK, &inputs).is_empty());
    // From another checkout of this machine it is a conflict.
    inputs.own.get_mut(A).unwrap().project_hash = "ph2".into();
    assert_eq!(plan(&graph, &PK, &inputs).len(), 1);
}

#[test]
fn an_own_node_captured_before_subjects_is_found_by_its_source() {
    let (mut graph, mut inputs) = two_values();
    graph.nodes.insert(B.into(), fact_node(None));
    assert_eq!(plan(&graph, &PK, &inputs), vec![contradiction(&PK, A, B)]);
    inputs.own.get_mut(B).unwrap().subject = None;
    assert!(plan(&graph, &PK, &inputs).is_empty());
}

#[test]
fn three_values_conflict_pairwise_and_stay_bounded() {
    let (mut graph, mut inputs) = two_values();
    let subject = subject_ref(&PK, "fact", "payments-retry");
    graph.nodes.insert(C.into(), fact_node(Some(&subject)));
    inputs.own.insert(
        C.into(),
        Own {
            project_hash: "ph3".into(),
            subject: Some(subject),
        },
    );
    let planned = plan(&graph, &PK, &inputs);
    let mut want = vec![
        contradiction(&PK, A, B),
        contradiction(&PK, A, C),
        contradiction(&PK, B, C),
    ];
    want.sort();
    assert_eq!(planned, want);
}

/// End to end on a replica: own sources and the pending queue feed the
/// plan; the queued edge is signed, deterministic, and once folded both
/// values are served as conflicting.
#[test]
fn detection_queues_one_signed_edge_and_both_values_become_conflicting() {
    let data = tempfile::tempdir().unwrap();
    let (replica, _) = Replica::open(data.path(), "pl_conflicts", &[3; 32]).unwrap();
    let key = SigningKey::from_bytes(&[0x49; 32]);
    let place = Place {
        kind: "vault".into(),
        id: "3f1c2b6a-8d4e-4a7b-9c10-5e2f6a7b8c01".into(),
        pk_id: PK,
        causal_base: 0,
    };
    let signer = Signer {
        key: &key,
        hlc_node: "6f2b9c4e-1a3d-4e5f-8a7b-0c1d2e3f4a5b".into(),
        worker_id: "wkr_vk4oelpv5lzjmev44hun".into(),
    };
    let target = Target {
        replica: &replica,
        place: &place,
        signer: &signer,
    };
    replica
        .put_own_source(
            B,
            "fact",
            &json!({"project_hash": "ph", "category": "fact", "key": "payments-retry", "value_digest": "d"}),
        )
        .unwrap();
    let (mut graph, _) = two_values();
    let planned = plan(&graph, &PK, &inputs(&replica, &PK).unwrap());
    assert_eq!(planned, vec![contradiction(&PK, A, B)]);
    let start = Stamp {
        wall_ms: 1_792_800_000_000,
        counter: 0,
    };
    let (queued, clock) = queue_all(&target, &planned, start, start.wall_ms).unwrap();
    assert_eq!(queued, 1);
    assert!(clock > start);

    let events: Vec<Value> = replica
        .pending()
        .unwrap()
        .iter()
        .map(|(_, b)| serde_json::from_slice(b).unwrap())
        .collect();
    assert_eq!(events.len(), 1);
    let e = &events[0];
    assert!(crate::events::sign::verify(
        e,
        key.verifying_key().as_bytes()
    ));
    assert_eq!(
        (e["op"].as_str(), e["target"].as_str()),
        (Some("edge.add"), Some(planned[0].edge_id.as_str()))
    );
    assert_eq!(e["body"]["kind"], "contradicts");
    assert_eq!(
        (e["body"]["from"].as_str(), e["body"]["to"].as_str()),
        (Some(B), Some(A))
    );

    // A second sync before the push queues nothing more.
    let again = plan(&graph, &PK, &inputs(&replica, &PK).unwrap());
    assert!(again.is_empty(), "{again:?}");

    // Folded on any Instance: both values are conflicting.
    graph
        .edges
        .insert(planned[0].edge_id.clone(), edge("contradicts", B, A));
    for id in [A, B] {
        let (state, _) = knowledge_state::of(&graph, id, &graph.nodes[id]);
        assert_eq!(state, State::Conflicting, "{id}");
    }
}

/// A fact `node` of the payments-retry subject, first saved at `seq` by a
/// writer that had folded the log up to `causal_base` (`None`: an event
/// without one, unknown history).
fn saved(seq: u64, node: &str, causal_base: Option<u64>) -> crate::replica::log::StoredEvent {
    let subject = subject_ref(&PK, "fact", "payments-retry");
    let mut event = json!({
        "event_id": format!("E{seq:025}"),
        "op": "node.add",
        "target": node,
        "node_type": "fact",
        "hlc": {"wall_ms": 1000 + seq, "counter": 0, "node": "x"},
        "worker_id": "wkr_aaaaaaaaaaaaaaaaaaaa",
        "signature": "00",
        "body": {"statement": node, "evidence": "observed",
                 "applies_to": ["file:src/lib.rs", subject]},
    });
    if let Some(base) = causal_base {
        event["causal_base"] = json!(base);
    }
    crate::replica::log::StoredEvent {
        seq,
        event,
        sig_invalid: false,
    }
}

/// The replica of the Instance that authored B (checkout `ph`), holding
/// `log`, and its folded graph.
fn instance_b(
    data: &std::path::Path,
    log: &[crate::replica::log::StoredEvent],
) -> (Replica, Graph) {
    let (mut replica, _) = Replica::open(data, "pl_links", &[3; 32]).unwrap();
    replica.insert_events(log).unwrap();
    let graph = replica.refold().unwrap();
    replica
        .put_own_source(
            B,
            "fact",
            &json!({"project_hash": "ph", "category": "fact", "key": "payments-retry", "value_digest": "d"}),
        )
        .unwrap();
    (replica, graph)
}

/// What the Instance that authored A (another checkout) plans on the same
/// log: its own node and the same save history.
fn instance_a_plan(graph: &Graph, replica: &Replica) -> Vec<Link> {
    let mut other = Inputs {
        saves: replica.first_saves().unwrap(),
        ..Inputs::default()
    };
    other.own.insert(
        A.into(),
        Own {
            project_hash: "ph-other".into(),
            subject: None,
        },
    );
    plan(graph, &PK, &other)
}

/// Review r1 (#286): Instance A saves a value (seq 1); Instance B folds it
/// (causal_base 1) and saves a new value of the same fact (seq 2). That is
/// a sequential edit, not a conflict: B writes one `supersedes` from its
/// newer value to the older one, A writes nothing, and once folded the
/// older value is outdated and the newer one current.
#[test]
fn a_sequential_edit_across_instances_supersedes_and_is_no_conflict() {
    let data = tempfile::tempdir().unwrap();
    let (replica, mut graph) =
        instance_b(data.path(), &[saved(1, A, Some(0)), saved(2, B, Some(1))]);
    let planned = plan(&graph, &PK, &inputs(&replica, &PK).unwrap());
    assert_eq!(planned, vec![supersession(&PK, B, A)]);
    assert_eq!(
        (
            planned[0].kind,
            planned[0].from.as_str(),
            planned[0].to.as_str()
        ),
        (Kind::Supersedes, B, A)
    );
    assert!(instance_a_plan(&graph, &replica).is_empty(), "one writer");

    // Queued signed, with its deterministic id; a second sync adds nothing.
    let key = SigningKey::from_bytes(&[0x49; 32]);
    let place = Place {
        kind: "vault".into(),
        id: "3f1c2b6a-8d4e-4a7b-9c10-5e2f6a7b8c01".into(),
        pk_id: PK,
        causal_base: 2,
    };
    let signer = Signer {
        key: &key,
        hlc_node: "6f2b9c4e-1a3d-4e5f-8a7b-0c1d2e3f4a5b".into(),
        worker_id: "wkr_vk4oelpv5lzjmev44hun".into(),
    };
    let target = Target {
        replica: &replica,
        place: &place,
        signer: &signer,
    };
    let start = Stamp {
        wall_ms: 1_792_800_000_000,
        counter: 0,
    };
    queue_all(&target, &planned, start, start.wall_ms).unwrap();
    let pending = replica.pending().unwrap();
    assert_eq!(pending.len(), 1);
    let e: Value = serde_json::from_slice(&pending[0].1).unwrap();
    assert!(crate::events::sign::verify(
        &e,
        key.verifying_key().as_bytes()
    ));
    assert_eq!(e["target"].as_str(), Some(planned[0].edge_id.as_str()));
    assert_eq!(e["body"]["kind"], "supersedes");
    assert_eq!(
        (e["body"]["from"].as_str(), e["body"]["to"].as_str()),
        (Some(B), Some(A))
    );
    assert!(plan(&graph, &PK, &inputs(&replica, &PK).unwrap()).is_empty());

    // Folded: one fact, the newer value current, the older outdated.
    graph
        .edges
        .insert(planned[0].edge_id.clone(), edge("supersedes", B, A));
    let state = |id: &str| knowledge_state::of(&graph, id, &graph.nodes[id]).0;
    assert_eq!(state(A), State::Outdated);
    assert_ne!(state(B), State::Conflicting);
    assert_ne!(state(B), State::Outdated);
}

/// Review r1 (#286): two values saved without either writer having folded
/// the other's (both on causal_base 0), or with an unknown save history,
/// are a conflict: exactly one `contradicts` link over both Instances.
#[test]
fn edits_made_without_seeing_each_other_give_exactly_one_contradicts() {
    for log in [
        [saved(1, A, Some(0)), saved(2, B, Some(0))],
        [saved(1, A, None), saved(2, B, Some(1))],
    ] {
        let data = tempfile::tempdir().unwrap();
        let (replica, graph) = instance_b(data.path(), &log);
        let mut links = plan(&graph, &PK, &inputs(&replica, &PK).unwrap());
        links.extend(instance_a_plan(&graph, &replica));
        assert_eq!(links, vec![contradiction(&PK, A, B)], "{log:?}");
        assert_eq!(links[0].kind, Kind::Contradicts);
    }
}

/// e2e `conflict.fact-detected` (#286): A and B write the same fact at the
/// same moment. The Instance that pulled both values serves both as
/// `[conflicting]` at once, from the save history, before the one
/// `contradicts` edge the resolution names has reached it. Before the fix
/// both were served as plain claims until that edge travelled through
/// the Hive, so the two Instances disagreed for a sync round.
#[test]
fn concurrent_values_read_conflicting_on_every_instance_before_the_edge_arrives() {
    use crate::delivery::{Bucket, items_in, render};
    for log in [
        [saved(1, A, Some(0)), saved(2, B, Some(0))],
        [saved(1, A, None), saved(2, B, Some(1))],
    ] {
        let data = tempfile::tempdir().unwrap();
        let (replica, graph) = instance_b(data.path(), &log);
        assert!(graph.edges.is_empty(), "no edge folded yet");
        let saves = replica.first_saves().unwrap();

        // Edge-only states: what recall served before the fix.
        let edges_only = knowledge_state::Index::new(&graph);
        for id in [A, B] {
            assert_ne!(edges_only.state(id, &graph.nodes[id]).0, State::Conflicting);
        }

        let states = knowledge_state::Index::with_saves(&graph, &saves);
        for id in [A, B] {
            assert_eq!(
                states.state(id, &graph.nodes[id]),
                (State::Conflicting, Some("contradicted")),
                "{id} {log:?}"
            );
        }
        let none = |_: &str| -> Option<String> { None };
        let list = items_in(&graph, &states, &none, "");
        assert_eq!(
            list.iter()
                .map(|i| (i.node_id.as_str(), i.bucket))
                .collect::<Vec<_>>(),
            vec![(A, Bucket::Conflicting), (B, Bucket::Conflicting)]
        );
        let out = render("acme/billing", &list, 1_500);
        assert!(
            out.contains(&format!("- [conflicting] fact node:{A}")),
            "{out}"
        );
        assert!(
            out.contains(&format!("- [conflicting] fact node:{B}")),
            "{out}"
        );

        // Once the author's edge is folded, nothing changes and nothing
        // is derived twice.
        let mut linked = graph.clone();
        let c = contradiction(&PK, A, B);
        linked
            .edges
            .insert(c.edge_id.clone(), edge("contradicts", &c.from, &c.to));
        assert!(derived(&linked, &saves).is_empty());
        let after = knowledge_state::Index::with_saves(&linked, &saves);
        for id in [A, B] {
            assert_eq!(after.state(id, &linked.nodes[id]).0, State::Conflicting);
        }
    }
}

/// A sequential edit read before its `supersedes` edge arrives: the older
/// value leaves delivery, the newer one is current, no conflict.
#[test]
fn a_sequential_edit_reads_superseded_before_the_edge_arrives() {
    use crate::delivery::{items_in, render};
    let data = tempfile::tempdir().unwrap();
    let (replica, graph) = instance_b(data.path(), &[saved(1, A, Some(0)), saved(2, B, Some(1))]);
    let saves = replica.first_saves().unwrap();
    assert_eq!(
        derived(&graph, &saves),
        vec![(Kind::Supersedes, B.to_owned(), A.to_owned())]
    );
    let states = knowledge_state::Index::with_saves(&graph, &saves);
    assert_eq!(
        states.state(A, &graph.nodes[A]),
        (State::Outdated, Some("superseded"))
    );
    assert!(states.state(B, &graph.nodes[B]).0.served_as_truth());
    let none = |_: &str| -> Option<String> { None };
    let out = render("acme/billing", &items_in(&graph, &states, &none, ""), 1_500);
    assert!(!out.contains(&format!("node:{A}")), "{out}");
    assert!(!out.contains("[conflicting]"), "{out}");
    assert!(out.contains(&format!("fact node:{B}")), "{out}");
}

/// Three values: B replaced A, C was written without seeing either. The
/// open values B and C conflict; A is superseded and no longer linked.
/// Values of other subjects, retracted values and non-facts never link.
#[test]
fn derived_links_follow_subjects_and_supersede_before_they_conflict() {
    let data = tempfile::tempdir().unwrap();
    let (mut replica, mut graph) = instance_b(
        data.path(),
        &[
            saved(1, A, Some(0)),
            saved(2, B, Some(1)),
            saved(3, C, Some(0)),
        ],
    );
    let saves = replica.first_saves().unwrap();
    assert_eq!(
        derived(&graph, &saves),
        vec![
            (Kind::Contradicts, C.to_owned(), B.to_owned()),
            (Kind::Supersedes, B.to_owned(), A.to_owned()),
        ]
    );
    graph.nodes.get_mut(C).unwrap().retracted = true;
    graph.nodes.get_mut(B).unwrap().node_type = "gotcha".into();
    assert!(derived(&graph, &saves).is_empty());
    let mut graph = replica.refold().unwrap();
    graph.nodes.insert(
        C.into(),
        fact_node(Some(&subject_ref(&PK, "fact", "other"))),
    );
    assert_eq!(
        derived(&graph, &saves),
        vec![(Kind::Supersedes, B.to_owned(), A.to_owned())]
    );
}

/// Review 5!232 (1): a link a curator retracted is a decision. Reading
/// derives neither the `contradicts` of concurrent values nor the
/// `supersedes` of a sequential edit again once its edge is retracted.
#[test]
fn a_retracted_link_is_not_derived_again() {
    for (log, kind) in [
        ([saved(1, A, Some(0)), saved(2, B, Some(0))], "contradicts"),
        ([saved(1, A, Some(0)), saved(2, B, Some(1))], "supersedes"),
    ] {
        let data = tempfile::tempdir().unwrap();
        let (replica, mut graph) = instance_b(data.path(), &log);
        let saves = replica.first_saves().unwrap();
        assert!(!derived(&graph, &saves).is_empty(), "{kind}");
        let mut retracted = edge(kind, B, A);
        retracted.retracted = true;
        graph.edges.insert("e_retracted".into(), retracted);
        assert!(derived(&graph, &saves).is_empty(), "{kind}");
        let states = knowledge_state::Index::with_saves(&graph, &saves);
        for id in [A, B] {
            assert!(
                states.state(id, &graph.nodes[id]).0.served_as_truth(),
                "{kind} {id}"
            );
        }
    }
}

/// Review 5!232 (2): a pair a folded `contradicts` edge already marks is
/// decided before the save history is read, as in `plan`: a sequential
/// history does not add a `supersedes` and both values stay conflicting.
#[test]
fn a_marked_pair_is_not_superseded_on_read() {
    let data = tempfile::tempdir().unwrap();
    let (replica, mut graph) =
        instance_b(data.path(), &[saved(1, A, Some(0)), saved(2, B, Some(1))]);
    let saves = replica.first_saves().unwrap();
    let c = contradiction(&PK, A, B);
    graph
        .edges
        .insert(c.edge_id.clone(), edge("contradicts", &c.from, &c.to));
    assert!(derived(&graph, &saves).is_empty());
    let states = knowledge_state::Index::with_saves(&graph, &saves);
    for id in [A, B] {
        assert_eq!(
            states.state(id, &graph.nodes[id]).0,
            State::Conflicting,
            "{id}"
        );
    }
}
