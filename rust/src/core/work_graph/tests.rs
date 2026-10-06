// SPDX-License-Identifier: Apache-2.0

#[test]
fn delivery_attempts_are_fenced_sequential_and_overflow_safe() {
    let mut graph = super::BoundedWorkGraph::default();
    graph
        .add_root(
            "parent".into(),
            "agent".into(),
            "capsule".into(),
            budget(100, 200),
        )
        .unwrap();
    graph
        .queue_child(
            "parent",
            "root".into(),
            "child-agent".into(),
            "child-capsule".into(),
            budget(50, 100),
        )
        .unwrap();
    assert!(graph.begin_delivery_attempt("root", "missing", 1).is_err());
    let fence = graph
        .claim_or_resume_active_until("root", u64::MAX)
        .unwrap()
        .execution_fence
        .clone()
        .unwrap();
    assert!(graph.begin_delivery_attempt("root", &fence, 2).is_err());
    assert!(graph.begin_delivery_attempt("root", "stale", 1).is_err());
    graph.begin_delivery_attempt("root", &fence, 1).unwrap();
    assert!(graph.begin_delivery_attempt("root", &fence, 1).is_err());
    let encoded = serde_json::to_vec(&graph).unwrap();
    let mut restored: super::BoundedWorkGraph = serde_json::from_slice(&encoded).unwrap();
    restored.begin_delivery_attempt("root", &fence, 2).unwrap();
    assert_eq!(restored.get_node("root").unwrap().delivery_attempt, Some(2));
    assert!(restored.begin_delivery_attempt("root", &fence, 3).is_err());
    restored.nodes.get_mut("root").unwrap().delivery_attempt = Some(255);
    assert!(restored.begin_delivery_attempt("root", &fence, 1).is_err());
}

use crate::core::work_graph::{
    BoundedWorkGraph, ChildExecutionReceipt, ChildOutcome, MAX_DEPTH, MAX_FAN_OUT,
    MeasuredExecutionSpend, NodeStatus, ResultClaim, StopReason, WorkGraphError, WorkNodeBudget,
};

#[test]
fn observation_is_compact_deterministic_and_does_not_expose_context() {
    let mut graph = BoundedWorkGraph::default();
    graph
        .add_root(
            "root".into(),
            "agent".into(),
            "private-capsule".into(),
            budget(100, 200),
        )
        .expect("root");
    let before = serde_json::to_string(&graph).expect("graph");
    let first = graph.observation();
    assert_eq!(first, graph.observation());
    assert_eq!(first["nodes"][0]["tokens_remaining"], 100);
    assert_eq!(first["nodes"][0]["accepted"], false);
    let compact = first.to_string();
    assert!(!compact.contains("private-capsule"));
    assert!(!compact.contains("execution_fence"));
    assert!(compact.len() < before.len());
    assert_eq!(before, serde_json::to_string(&graph).expect("unchanged"));
    let revision = first["revision"].as_str().expect("revision");
    let cached = graph.observe_if_changed(Some(revision));
    assert_eq!(cached["unchanged"], true);
    assert!(cached.get("nodes").is_none());
    assert!(cached.to_string().len() < compact.len());
    graph.consume_budget("root", 1, 1).expect("consume");
    let changed = graph.observe_if_changed(Some(revision));
    assert_eq!(changed["unchanged"], false);
    assert_ne!(changed["revision"], first["revision"]);
    assert_eq!(changed["nodes"][0]["tokens_remaining"], 99);
}

fn budget(tokens: u64, cost: u64) -> WorkNodeBudget {
    WorkNodeBudget {
        tokens_allocated: tokens,
        tokens_consumed: 0,
        cost_micros_allocated: cost,
        cost_micros_consumed: 0,
    }
}

#[test]
fn basic_delegation_and_budget_inheritance() {
    let mut g = BoundedWorkGraph::default();
    g.add_root(
        "root".into(),
        "parent-agent".into(),
        "capsule:abc".into(),
        budget(1000, 500),
    )
    .unwrap();
    g.delegate(
        "root",
        "child-1".into(),
        "child-agent".into(),
        "capsule:def".into(),
        budget(400, 200),
    )
    .unwrap();
    assert_eq!(g.active_count(), 2);
    assert_eq!(g.children_of("root"), &["child-1"]);
}

#[test]
fn child_cannot_exceed_parent_budget() {
    let mut g = BoundedWorkGraph::default();
    g.add_root(
        "root".into(),
        "a".into(),
        "capsule:x".into(),
        budget(100, 50),
    )
    .unwrap();
    assert!(matches!(
        g.delegate(
            "root",
            "c".into(),
            "b".into(),
            "capsule:y".into(),
            budget(200, 30)
        ),
        Err(WorkGraphError::BudgetExceedsParent { .. })
    ));
}

#[test]
fn fan_out_limit_enforced() {
    let mut g = BoundedWorkGraph::new(2, MAX_DEPTH);
    g.add_root(
        "root".into(),
        "a".into(),
        "capsule:x".into(),
        budget(1000, 1000),
    )
    .unwrap();
    g.delegate(
        "root",
        "c1".into(),
        "b".into(),
        "capsule:1".into(),
        budget(100, 100),
    )
    .unwrap();
    g.delegate(
        "root",
        "c2".into(),
        "b".into(),
        "capsule:2".into(),
        budget(100, 100),
    )
    .unwrap();
    assert!(matches!(
        g.delegate(
            "root",
            "c3".into(),
            "b".into(),
            "capsule:3".into(),
            budget(100, 100)
        ),
        Err(WorkGraphError::FanOutExceeded(2))
    ));
}

#[test]
fn depth_limit_enforced() {
    let mut g = BoundedWorkGraph::new(MAX_FAN_OUT, 2);
    g.add_root(
        "n0".into(),
        "a".into(),
        "capsule:0".into(),
        budget(1000, 1000),
    )
    .unwrap();
    g.delegate(
        "n0",
        "n1".into(),
        "b".into(),
        "capsule:1".into(),
        budget(500, 500),
    )
    .unwrap();
    g.delegate(
        "n1",
        "n2".into(),
        "c".into(),
        "capsule:2".into(),
        budget(200, 200),
    )
    .unwrap();
    assert!(matches!(
        g.delegate(
            "n2",
            "n3".into(),
            "d".into(),
            "capsule:3".into(),
            budget(100, 100)
        ),
        Err(WorkGraphError::DepthExceeded(2))
    ));
}

#[test]
fn stop_cascades_to_children() {
    let mut g = BoundedWorkGraph::default();
    g.add_root(
        "root".into(),
        "a".into(),
        "capsule:r".into(),
        budget(1000, 1000),
    )
    .unwrap();
    g.delegate(
        "root",
        "c1".into(),
        "b".into(),
        "capsule:1".into(),
        budget(300, 300),
    )
    .unwrap();
    g.delegate(
        "c1",
        "gc1".into(),
        "c".into(),
        "capsule:gc".into(),
        budget(100, 100),
    )
    .unwrap();
    let stopped = g.stop("c1", StopReason::Stale).unwrap();
    assert_eq!(stopped, vec!["c1", "gc1"]);
    assert_eq!(g.get_node("c1").unwrap().status, NodeStatus::Stopped);
    assert_eq!(
        g.get_node("gc1").unwrap().stop_reason,
        Some(StopReason::ParentStopped)
    );
}

#[test]
fn budget_exhaustion_auto_stops() {
    let mut g = BoundedWorkGraph::default();
    g.add_root(
        "root".into(),
        "a".into(),
        "capsule:r".into(),
        budget(100, 100),
    )
    .unwrap();
    let exhausted = g.consume_budget("root", 100, 50).unwrap();
    assert!(exhausted);
    assert_eq!(g.get_node("root").unwrap().status, NodeStatus::Stopped);
}

#[test]
fn allocate_child_budget_uses_requested_fraction() {
    let mut g = BoundedWorkGraph::default();
    g.add_root(
        "root".into(),
        "a".into(),
        "capsule:r".into(),
        budget(10_000, 5_000),
    )
    .unwrap();

    let child_budget = g.allocate_child_budget("root", "child", 0.5).unwrap();

    assert_eq!(child_budget, budget(5_000, 2_500));
    let chain = g.chain_budget_for("root").unwrap();
    assert_eq!(chain.total_allocated_tokens, 15_000);
    assert_eq!(chain.depth, 1);
}

#[test]
fn allocate_child_budget_rejects_exhausted_parent() {
    let mut g = BoundedWorkGraph::default();
    g.add_root(
        "root".into(),
        "a".into(),
        "capsule:r".into(),
        budget(100, 100),
    )
    .unwrap();
    g.consume_tokens("root", 100).unwrap();

    assert!(g.allocate_child_budget("root", "child", 0.5).is_err());
}

#[test]
fn consume_tokens_updates_node_and_chain() {
    let mut g = BoundedWorkGraph::default();
    g.add_root(
        "root".into(),
        "a".into(),
        "capsule:r".into(),
        budget(1_000, 1_000),
    )
    .unwrap();

    g.consume_tokens("root", 250).unwrap();

    assert_eq!(g.get_node("root").unwrap().budget.tokens_consumed, 250);
    assert_eq!(
        g.chain_budget_for("root").unwrap().total_consumed_tokens,
        250
    );
}

#[test]
fn parent_consumption_cannot_spend_reserved_child_budget() {
    let mut graph = BoundedWorkGraph::default();
    graph
        .add_root(
            "root".into(),
            "a".into(),
            "capsule:r".into(),
            budget(100, 100),
        )
        .unwrap();
    graph
        .queue_child(
            "root",
            "child".into(),
            "b".into(),
            "capsule:c".into(),
            budget(80, 80),
        )
        .unwrap();
    for (tokens, cost) in [(21, 0), (0, 21)] {
        let before = serde_json::to_value(&graph).unwrap();
        assert!(matches!(
            graph.consume_budget("root", tokens, cost),
            Err(WorkGraphError::InvalidBudget)
        ));
        assert_eq!(serde_json::to_value(&graph).unwrap(), before);
    }
    assert!(!graph.consume_budget("root", 20, 20).unwrap());
    graph.validate_invariants().unwrap();
    assert_eq!(graph.get_node("child").unwrap().budget.tokens_allocated, 80);
}

#[test]
fn rejected_chain_consumption_leaves_node_and_chain_unchanged() {
    let mut graph = BoundedWorkGraph::default();
    graph
        .add_root(
            "root".into(),
            "a".into(),
            "capsule:r".into(),
            budget(100, 100),
        )
        .unwrap();
    // Inject a conflicting chain ceiling to exercise the late error path.
    graph
        .chain_budgets
        .get_mut("root")
        .unwrap()
        .root_budget_tokens = 0;
    let before = serde_json::to_value(&graph).unwrap();
    assert!(matches!(
        graph.consume_budget("root", 1, 0),
        Err(WorkGraphError::InvalidBudget)
    ));
    assert_eq!(serde_json::to_value(&graph).unwrap(), before);
}

#[test]
fn parent_receipt_cannot_spend_reserved_descendant_budget() {
    let mut graph = BoundedWorkGraph::default();
    graph
        .add_root(
            "root".into(),
            "a".into(),
            "capsule:r".into(),
            budget(1000, 1000),
        )
        .unwrap();
    graph
        .delegate(
            "root",
            "child".into(),
            "b".into(),
            "capsule:c".into(),
            budget(100, 100),
        )
        .unwrap();
    graph
        .queue_child(
            "child",
            "grandchild".into(),
            "c".into(),
            "capsule:g".into(),
            budget(80, 80),
        )
        .unwrap();
    for (tokens, cost) in [(21, 0), (0, 21)] {
        let before = serde_json::to_value(&graph).unwrap();
        assert!(matches!(
            graph.record_child_receipt(receipt(
                "r1",
                "child",
                ChildOutcome::Accepted,
                "answer",
                "value",
                tokens,
                cost
            )),
            Err(WorkGraphError::ReceiptExceedsBudget(_))
        ));
        assert_eq!(serde_json::to_value(&graph).unwrap(), before);
    }
    graph.validate_invariants().unwrap();
}

#[test]
fn consume_tokens_stops_exhausted_node() {
    let mut g = BoundedWorkGraph::default();
    g.add_root(
        "root".into(),
        "a".into(),
        "capsule:r".into(),
        budget(100, 100),
    )
    .unwrap();

    g.consume_tokens("root", 100).unwrap();

    let node = g.get_node("root").unwrap();
    assert_eq!(node.status, NodeStatus::Stopped);
    assert_eq!(node.stop_reason, Some(StopReason::BudgetExhausted));
}

#[test]
fn over_budget_chains_returns_chains_above_threshold() {
    let mut g = BoundedWorkGraph::default();
    g.add_root(
        "first".into(),
        "a".into(),
        "capsule:first".into(),
        budget(1_000, 1_000),
    )
    .unwrap();
    g.add_root(
        "second".into(),
        "b".into(),
        "capsule:second".into(),
        budget(1_000, 1_000),
    )
    .unwrap();
    g.consume_tokens("first", 750).unwrap();

    let over_budget = g.over_budget_chains(70.0);

    assert_eq!(over_budget.len(), 1);
    assert_eq!(over_budget[0].chain_id, "first");
}

#[test]
fn complete_sets_outcome() {
    let mut g = BoundedWorkGraph::default();
    g.add_root(
        "root".into(),
        "a".into(),
        "capsule:r".into(),
        budget(1000, 1000),
    )
    .unwrap();
    g.complete("root", "outcome:success".into()).unwrap();
    let node = g.get_node("root").unwrap();
    assert_eq!(node.status, NodeStatus::Completed);
    assert_eq!(node.outcome_ref.as_deref(), Some("outcome:success"));
}

#[test]
fn pending_children_claim_under_global_local_limit() {
    let mut g = BoundedWorkGraph::default().with_local_concurrency(2);
    g.add_root(
        "root".into(),
        "a".into(),
        "capsule:r".into(),
        budget(1000, 1000),
    )
    .unwrap();
    for child in ["c1", "c2"] {
        g.queue_child(
            "root",
            child.into(),
            format!("agent-{child}"),
            format!("capsule:{child}"),
            budget(100, 100),
        )
        .unwrap();
    }

    assert_eq!(g.get_node("c1").unwrap().status, NodeStatus::Pending);
    g.claim_and_activate("c1").unwrap();
    assert!(matches!(
        g.claim_and_activate("c2"),
        Err(WorkGraphError::LocalConcurrencyExceeded(2))
    ));
    assert_eq!(g.get_node("c2").unwrap().status, NodeStatus::Pending);
}

fn receipt(
    receipt_id: &str,
    node_id: &str,
    outcome: ChildOutcome,
    key: &str,
    value: &str,
    tokens: u64,
    cost: u64,
) -> ChildExecutionReceipt {
    ChildExecutionReceipt {
        receipt_id: receipt_id.into(),
        node_id: node_id.into(),
        outcome_ref: format!("outcome:{receipt_id}"),
        outcome,
        tokens_consumed: tokens,
        cost_micros_consumed: cost,
        execution_fence: None,
        claims: vec![ResultClaim {
            key: key.into(),
            value: value.into(),
        }],
    }
}

#[test]
fn receipts_fuse_deterministically_and_expose_conflicts() {
    let mut g = BoundedWorkGraph::default();
    g.add_root(
        "root".into(),
        "a".into(),
        "capsule:r".into(),
        budget(1000, 1000),
    )
    .unwrap();
    for child in ["c2", "c1"] {
        g.delegate(
            "root",
            child.into(),
            format!("agent-{child}"),
            format!("capsule:{child}"),
            budget(200, 200),
        )
        .unwrap();
    }
    g.record_child_receipt(receipt(
        "r2",
        "c2",
        ChildOutcome::Accepted,
        "answer",
        "b",
        20,
        2,
    ))
    .unwrap();
    assert_eq!(
        g.evaluate_child_outcome("c2").unwrap(),
        ChildOutcome::Accepted
    );
    g.record_child_receipt(receipt(
        "r1",
        "c1",
        ChildOutcome::Accepted,
        "answer",
        "a",
        10,
        1,
    ))
    .unwrap();
    g.mark_accepted_path(&["c2".into(), "c1".into()]).unwrap();

    let fused = g.fuse_accepted_results().unwrap();
    assert!(fused.accepted.is_empty());
    assert_eq!(fused.conflicts[0].key, "answer");
    assert_eq!(fused.conflicts[0].values, ["a", "b"]);
    assert_eq!(fused.conflicts[0].node_ids, ["c1", "c2"]);
}

#[test]
fn accepted_and_waste_attribution_follow_marked_path() {
    let mut g = BoundedWorkGraph::default();
    g.add_root(
        "root".into(),
        "a".into(),
        "capsule:r".into(),
        budget(1000, 1000),
    )
    .unwrap();
    for child in ["accepted", "rejected"] {
        g.delegate(
            "root",
            child.into(),
            format!("agent-{child}"),
            format!("capsule:{child}"),
            budget(200, 200),
        )
        .unwrap();
    }
    g.record_child_receipt(receipt(
        "r1",
        "accepted",
        ChildOutcome::Accepted,
        "answer",
        "yes",
        80,
        8,
    ))
    .unwrap();
    g.record_child_receipt(receipt(
        "r2",
        "rejected",
        ChildOutcome::Rejected,
        "answer",
        "no",
        60,
        6,
    ))
    .unwrap();
    g.mark_accepted_path(&["accepted".into()]).unwrap();

    let mut serialized = serde_json::to_value(&g).unwrap();
    serialized["accepted_path"] = serde_json::json!(["root", "rejected"]);
    let forged: BoundedWorkGraph = serde_json::from_value(serialized).unwrap();
    assert!(matches!(
        forged.validate_invariants(),
        Err(WorkGraphError::InvalidGraph(message))
            if message == "accepted path lacks accepted receipt lineage"
    ));

    // A partial result cannot replace an already accepted path or earn
    // accepted-path credit merely because execution completed.
    let mut partial = g.clone();
    partial.receipts.get_mut("r2").unwrap().outcome = ChildOutcome::Partial;
    assert!(matches!(
        partial.mark_accepted_path(&["rejected".into()]),
        Err(WorkGraphError::RejectedAcceptedPath(_))
    ));
    assert!(partial.is_on_accepted_path("accepted"));
    assert!(!partial.is_on_accepted_path("rejected"));
    assert_eq!(partial.attribution().unwrap(), g.attribution().unwrap());

    let attribution = g.attribution().unwrap();
    assert_eq!(attribution.accepted_tokens, 80);
    assert_eq!(attribution.accepted_cost_micros, 8);
    assert_eq!(attribution.waste_tokens, 60);
    assert_eq!(attribution.waste_cost_micros, 6);
    let value = g.team_value_report().unwrap();
    assert_eq!(value.accepted_tokens, 80);
    assert_eq!(value.total_tokens, 140);
    assert_eq!(value.accepted_cost_micros, 8);
    assert_eq!(value.total_cost_micros, 14);
    assert_eq!(value.waste_tokens, 60);
    assert_eq!(value.waste_cost_micros, 6);
    assert_eq!(value.accepted_cost_basis_points, 5_714);
    assert!(g.is_on_accepted_path("root"));
    assert!(!g.is_on_accepted_path("rejected"));
}

#[test]
fn malformed_receipt_is_fail_closed_and_legacy_serde_defaults() {
    let mut g = BoundedWorkGraph::default();
    g.add_root(
        "root".into(),
        "a".into(),
        "capsule:r".into(),
        budget(1000, 1000),
    )
    .unwrap();
    g.delegate(
        "root",
        "child".into(),
        "agent".into(),
        "capsule:c".into(),
        budget(10, 10),
    )
    .unwrap();
    assert!(matches!(
        g.record_child_receipt(receipt(
            "receipt",
            "child",
            ChildOutcome::Accepted,
            "answer",
            "yes",
            11,
            1
        )),
        Err(WorkGraphError::ReceiptExceedsBudget(_))
    ));
    assert_eq!(g.get_node("child").unwrap().status, NodeStatus::Active);
    assert_eq!(g.get_node("child").unwrap().budget.tokens_consumed, 0);
    assert!(matches!(
        g.queue_child(
            "root",
            "x".repeat(129),
            "agent".into(),
            "capsule:c".into(),
            budget(1, 1),
        ),
        Err(WorkGraphError::InvalidIdentifier("child_node_id"))
    ));
    assert_eq!(g.total_count(), 2);

    let mut legacy = serde_json::to_value(&g).unwrap();
    let object = legacy.as_object_mut().unwrap();
    object.remove("receipts");
    object.remove("accepted_path");
    object.remove("max_local_concurrency");
    let decoded: BoundedWorkGraph = serde_json::from_value(legacy).unwrap();
    assert_eq!(decoded.total_count(), 2);
}

fn settlement_graph() -> BoundedWorkGraph {
    let mut graph = BoundedWorkGraph::default();
    graph
        .add_root(
            "root".into(),
            "root-agent".into(),
            "capsule:root".into(),
            budget(100, 100),
        )
        .unwrap();
    graph
        .queue_child(
            "root",
            "child".into(),
            "child-agent".into(),
            "capsule:child".into(),
            budget(80, 80),
        )
        .unwrap();
    graph.claim_and_activate("child").unwrap();
    graph
}

fn fenced_receipt(
    graph: &BoundedWorkGraph,
    mut value: ChildExecutionReceipt,
) -> ChildExecutionReceipt {
    value.execution_fence = graph
        .get_node(&value.node_id)
        .unwrap()
        .execution_fence
        .clone();
    assert!(value.execution_fence.is_some());
    value
}

#[test]
fn settled_child_releases_unused_budget_without_refunding_consumption() {
    let mut graph = settlement_graph();
    graph
        .record_child_receipt(fenced_receipt(
            &graph,
            receipt(
                "settled",
                "child",
                ChildOutcome::Partial,
                "result",
                "partial",
                10,
                20,
            ),
        ))
        .unwrap();
    assert_eq!(graph.reserved_child_budget("root", None).unwrap(), (10, 20));
    graph.consume_budget("root", 90, 80).unwrap();
    let chain = graph.chain_budget_for("root").unwrap();
    assert_eq!(chain.total_consumed_tokens, 100);
    assert_eq!(chain.total_consumed_cost_micros, 100);
    graph.validate_invariants().unwrap();
    let restored: BoundedWorkGraph =
        serde_json::from_value(serde_json::to_value(&graph).unwrap()).unwrap();
    restored.validate_invariants().unwrap();
    assert_eq!(
        restored.reserved_child_budget("root", None).unwrap(),
        (10, 20)
    );
}

#[test]
fn unfenced_receipt_does_not_release_unverified_budget() {
    let mut graph = BoundedWorkGraph::default();
    graph
        .add_root(
            "root".into(),
            "root-agent".into(),
            "capsule:root".into(),
            budget(100, 100),
        )
        .unwrap();
    graph
        .delegate(
            "root",
            "child".into(),
            "child-agent".into(),
            "capsule:child".into(),
            budget(80, 80),
        )
        .unwrap();
    graph
        .record_child_receipt(receipt(
            "legacy",
            "child",
            ChildOutcome::Partial,
            "result",
            "partial",
            0,
            0,
        ))
        .unwrap();
    assert_eq!(graph.reserved_child_budget("root", None).unwrap(), (80, 80));
    assert!(matches!(
        graph.consume_budget("root", 21, 1),
        Err(WorkGraphError::InvalidBudget)
    ));
    graph.validate_invariants().unwrap();
}

#[test]
fn terminal_status_without_receipt_keeps_unknown_usage_reserved() {
    for completed in [false, true] {
        let mut graph = settlement_graph();
        if completed {
            graph.complete("child", "outcome:unsettled".into()).unwrap();
        } else {
            graph.stop("child", StopReason::ManualStop).unwrap();
        }
        assert_eq!(graph.reserved_child_budget("root", None).unwrap(), (80, 80));
        let before = serde_json::to_value(&graph).unwrap();
        assert!(matches!(
            graph.consume_budget("root", 21, 1),
            Err(WorkGraphError::InvalidBudget)
        ));
        assert_eq!(serde_json::to_value(&graph).unwrap(), before);
        graph.validate_invariants().unwrap();
    }
}

#[test]
fn settled_parent_preserves_live_descendant_until_its_receipt() {
    let mut graph = settlement_graph();
    graph
        .queue_child(
            "child",
            "grandchild".into(),
            "grandchild-agent".into(),
            "capsule:grandchild".into(),
            budget(60, 40),
        )
        .unwrap();
    graph.claim_and_activate("grandchild").unwrap();
    graph
        .record_child_receipt(fenced_receipt(
            &graph,
            receipt(
                "parent-settled",
                "child",
                ChildOutcome::Partial,
                "result",
                "partial",
                10,
                20,
            ),
        ))
        .unwrap();
    assert_eq!(graph.reserved_child_budget("root", None).unwrap(), (70, 60));
    graph.consume_budget("root", 30, 40).unwrap();
    graph.validate_invariants().unwrap();
    graph
        .record_child_receipt(fenced_receipt(
            &graph,
            receipt(
                "descendant-settled",
                "grandchild",
                ChildOutcome::Rejected,
                "result",
                "rejected",
                5,
                7,
            ),
        ))
        .unwrap();
    assert_eq!(graph.reserved_child_budget("root", None).unwrap(), (15, 27));
    graph.consume_budget("root", 55, 33).unwrap();
    graph.validate_invariants().unwrap();
    assert_eq!(
        graph
            .chain_budget_for("root")
            .unwrap()
            .total_consumed_tokens,
        100
    );
    assert_eq!(
        graph
            .chain_budget_for("root")
            .unwrap()
            .total_consumed_cost_micros,
        100
    );
}

#[test]
fn settled_parent_preserves_pending_descendant_reservation() {
    let mut graph = settlement_graph();
    let pending = graph
        .allocate_child_budget("child", "not-created", 0.5)
        .unwrap();
    graph.validate_invariants().unwrap();
    assert_eq!(graph.chain_budget_for("root").unwrap().depth, 2);
    graph
        .record_child_receipt(fenced_receipt(
            &graph,
            receipt(
                "parent-settled",
                "child",
                ChildOutcome::Partial,
                "result",
                "partial",
                10,
                20,
            ),
        ))
        .unwrap();
    assert_eq!(
        graph.reserved_child_budget("root", None).unwrap(),
        (
            10 + pending.tokens_allocated,
            20 + pending.cost_micros_allocated
        )
    );
    graph.validate_invariants().unwrap();
}

#[test]
fn pending_budget_cannot_be_consumed_by_another_parent() {
    let mut graph = settlement_graph();
    let reserved = graph
        .allocate_child_budget("child", "pending", 0.5)
        .unwrap();
    let before = serde_json::to_value(&graph).unwrap();
    assert!(matches!(
        graph.queue_child(
            "root",
            "pending".into(),
            "agent".into(),
            "capsule:pending".into(),
            budget(10, 10)
        ),
        Err(WorkGraphError::InvalidGraph(_))
    ));
    assert_eq!(serde_json::to_value(&graph).unwrap(), before);
    graph
        .queue_child(
            "child",
            "pending".into(),
            "agent".into(),
            "capsule:pending".into(),
            reserved,
        )
        .unwrap();
    graph.validate_invariants().unwrap();
}

#[test]
fn incomplete_pending_reservation_is_rejected_without_mutation() {
    for missing_budget in [false, true] {
        let mut graph = settlement_graph();
        graph
            .allocate_child_budget("child", "pending", 0.5)
            .unwrap();
        if missing_budget {
            graph.pending_child_budgets.remove("pending");
        } else {
            graph.pending_child_parents.remove("pending");
        }
        let before = serde_json::to_value(&graph).unwrap();
        assert!(matches!(
            graph.queue_child(
                "child",
                "pending".into(),
                "agent".into(),
                "capsule:pending".into(),
                budget(10, 10)
            ),
            Err(WorkGraphError::InvalidGraph(_))
        ));
        assert_eq!(serde_json::to_value(&graph).unwrap(), before);
    }
}

#[test]
fn pending_budget_rejects_duplicate_ids_without_mutation() {
    let mut graph = settlement_graph();
    graph.allocate_child_budget("root", "pending", 0.5).unwrap();
    for (parent, child) in [("root", "pending"), ("child", "pending"), ("root", "child")] {
        let before = serde_json::to_value(&graph).unwrap();
        assert!(matches!(
            graph.allocate_child_budget(parent, child, 0.25),
            Err(WorkGraphError::DuplicateNode(_))
        ));
        assert_eq!(serde_json::to_value(&graph).unwrap(), before);
    }
    graph.validate_invariants().unwrap();
}

#[test]
fn pending_budget_obeys_depth_ceiling_before_reserving() {
    let mut graph = BoundedWorkGraph::new(MAX_FAN_OUT, 1);
    graph
        .add_root(
            "root".into(),
            "agent".into(),
            "capsule:root".into(),
            budget(100, 100),
        )
        .unwrap();
    graph
        .delegate(
            "root",
            "child".into(),
            "agent".into(),
            "capsule:child".into(),
            budget(80, 80),
        )
        .unwrap();
    let before = serde_json::to_value(&graph).unwrap();
    assert!(matches!(
        graph.allocate_child_budget("child", "too-deep", 0.5),
        Err(WorkGraphError::DepthExceeded(1))
    ));
    assert_eq!(serde_json::to_value(&graph).unwrap(), before);
    graph.validate_invariants().unwrap();
}

#[test]
fn sibling_allocations_cannot_exceed_parent_budget() {
    let mut graph = BoundedWorkGraph::default();
    graph
        .add_root(
            "root".into(),
            "agent".into(),
            "capsule".into(),
            budget(100, 100),
        )
        .unwrap();
    graph
        .queue_child(
            "root",
            "a".into(),
            "a".into(),
            "capsule".into(),
            budget(60, 60),
        )
        .unwrap();
    assert!(matches!(
        graph.queue_child(
            "root",
            "b".into(),
            "b".into(),
            "capsule".into(),
            budget(41, 41)
        ),
        Err(WorkGraphError::BudgetExceedsParent {
            parent_remaining: 40,
            ..
        })
    ));
}

#[test]
fn budget_overrun_is_rejected_without_mutation() {
    let mut graph = BoundedWorkGraph::default();
    graph
        .add_root(
            "root".into(),
            "agent".into(),
            "capsule".into(),
            budget(10, 10),
        )
        .unwrap();
    assert!(matches!(
        graph.consume_budget("root", 11, 1),
        Err(WorkGraphError::InvalidBudget)
    ));
    assert_eq!(graph.get_node("root").unwrap().budget.tokens_consumed, 0);
    assert_eq!(
        graph
            .chain_budget_for("root")
            .unwrap()
            .total_consumed_tokens,
        0
    );
}

#[test]
fn verified_overrun_is_persisted_as_waste_without_releasing_reservation() {
    let mut graph = BoundedWorkGraph::default().with_graph_id("graph").unwrap();
    graph
        .add_root(
            "root".into(),
            "root-agent".into(),
            "capsule:root".into(),
            budget(100, 100),
        )
        .unwrap();
    graph
        .queue_child(
            "root",
            "child".into(),
            "child-agent".into(),
            "capsule:child".into(),
            budget(20, 20),
        )
        .unwrap();
    let child = graph.claim_and_activate("child").unwrap().clone();
    let spend = MeasuredExecutionSpend::new(
        "child".into(),
        vec![format!("id:sha256:{}", "a".repeat(64))],
        25,
        15,
        child.execution_fence.unwrap(),
    );

    graph.record_failed_execution_spend(spend.clone()).unwrap();
    graph.record_failed_execution_spend(spend).unwrap();
    let child = graph.get_node("child").unwrap();
    assert_eq!(child.status, NodeStatus::Stopped);
    assert_eq!(child.stop_reason, Some(StopReason::BudgetExhausted));
    assert_eq!(child.budget.tokens_consumed, 0);
    assert!(matches!(
        graph.consume_budget("root", 81, 1),
        Err(WorkGraphError::InvalidBudget)
    ));
    graph.consume_budget("root", 75, 1).unwrap();
    let attribution = graph.attribution().unwrap();
    assert_eq!(attribution.waste_tokens, 100);
    assert_eq!(attribution.waste_cost_micros, 16);
    graph.validate_invariants().unwrap();

    let restored: BoundedWorkGraph =
        serde_json::from_value(serde_json::to_value(&graph).unwrap()).unwrap();
    assert_eq!(restored.attribution().unwrap(), attribution);
    restored.validate_invariants().unwrap();
}

#[test]
fn verified_overrun_blocks_sibling_receipt_beyond_chain_budget() {
    let mut graph = BoundedWorkGraph::default().with_graph_id("graph").unwrap();
    graph
        .add_root(
            "root".into(),
            "root-agent".into(),
            "capsule:root".into(),
            budget(100, 100),
        )
        .unwrap();
    graph
        .queue_child(
            "root",
            "overrun".into(),
            "overrun-agent".into(),
            "capsule:overrun".into(),
            budget(20, 20),
        )
        .unwrap();
    graph
        .queue_child(
            "root",
            "sibling".into(),
            "sibling-agent".into(),
            "capsule:sibling".into(),
            budget(80, 80),
        )
        .unwrap();
    let overrun = graph.claim_and_activate("overrun").unwrap().clone();
    graph.claim_and_activate("sibling").unwrap();
    graph
        .record_failed_execution_spend(MeasuredExecutionSpend::new(
            "overrun".into(),
            vec![format!("id:sha256:{}", "b".repeat(64))],
            125,
            15,
            overrun.execution_fence.unwrap(),
        ))
        .unwrap();
    let sibling_receipt = fenced_receipt(
        &graph,
        receipt(
            "sibling-receipt",
            "sibling",
            ChildOutcome::Accepted,
            "answer",
            "yes",
            80,
            10,
        ),
    );

    assert!(matches!(
        graph.record_child_receipt(sibling_receipt),
        Err(WorkGraphError::ReceiptExceedsBudget(node)) if node == "sibling"
    ));
    assert_eq!(
        graph.get_node("sibling").unwrap().status,
        NodeStatus::Active
    );
    assert_eq!(
        graph
            .chain_budget_for("root")
            .unwrap()
            .total_consumed_tokens,
        0
    );
    graph.validate_invariants().unwrap();
}

#[test]
fn receipt_accounts_only_unreported_delta() {
    let mut graph = BoundedWorkGraph::default();
    graph
        .add_root(
            "root".into(),
            "root-agent".into(),
            "capsule".into(),
            budget(100, 100),
        )
        .unwrap();
    graph
        .delegate(
            "root",
            "child".into(),
            "child-agent".into(),
            "capsule".into(),
            budget(20, 20),
        )
        .unwrap();
    graph.consume_budget("child", 3, 2).unwrap();
    graph
        .record_child_receipt(receipt(
            "receipt",
            "child",
            ChildOutcome::Accepted,
            "answer",
            "yes",
            5,
            4,
        ))
        .unwrap();
    let chain = graph.chain_budget_for("child").unwrap();
    assert_eq!(chain.total_consumed_tokens, 5);
    assert_eq!(chain.total_consumed_cost_micros, 4);
}

#[test]
fn active_claim_can_be_resumed_without_rotating_fence() {
    let mut graph = BoundedWorkGraph::default();
    graph
        .add_root(
            "root".into(),
            "root-agent".into(),
            "capsule".into(),
            budget(100, 100),
        )
        .unwrap();
    graph
        .queue_child(
            "root",
            "child".into(),
            "child-agent".into(),
            "capsule".into(),
            budget(20, 20),
        )
        .unwrap();
    let first = graph
        .claim_and_activate_until("child", Some(10))
        .unwrap()
        .execution_fence
        .clone();
    let resumed = graph.claim_or_resume_active_until("child", 20).unwrap();
    assert_eq!(resumed.execution_fence, first);
    assert_eq!(resumed.lease_expires_epoch_ms, Some(20));
    assert_eq!(resumed.claim_attempt, 1);
    assert!(resumed.execution_started);
    assert!(matches!(
        graph.claim_or_resume_active_until("child", 30),
        Err(WorkGraphError::InvalidTransition(_))
    ));
}

#[test]
fn expired_execution_stops_fail_closed_instead_of_retrying_unknown_spend() {
    let mut graph = BoundedWorkGraph::default();
    graph
        .add_root(
            "root".into(),
            "root-agent".into(),
            "capsule:root".into(),
            budget(100, 100),
        )
        .unwrap();
    graph
        .queue_child(
            "root",
            "child".into(),
            "child-agent".into(),
            "capsule:child".into(),
            budget(20, 20),
        )
        .unwrap();
    graph.claim_or_resume_active_until("child", 10).unwrap();
    graph
        .delegate(
            "child",
            "grandchild".into(),
            "grandchild-agent".into(),
            "capsule:grandchild".into(),
            budget(10, 10),
        )
        .unwrap();

    assert_eq!(
        graph.recover_expired_execution("child", 10).unwrap(),
        vec!["child", "grandchild"]
    );
    let child = graph.get_node("child").unwrap();
    assert_eq!(child.status, NodeStatus::Stopped);
    assert_eq!(child.stop_reason, Some(StopReason::LeaseLost));
    assert!(child.execution_started);
    assert_eq!(
        graph.get_node("grandchild").unwrap().stop_reason,
        Some(StopReason::ParentStopped)
    );
    assert!(matches!(
        graph.claim_or_resume_active_until("child", 20),
        Err(WorkGraphError::InvalidTransition(_))
    ));
}

#[test]
fn one_capsule_cannot_start_two_nodes() {
    let mut graph = BoundedWorkGraph::default();
    graph
        .add_root(
            "root".into(),
            "root-agent".into(),
            "capsule:root".into(),
            budget(100, 100),
        )
        .unwrap();
    for child in ["first", "second"] {
        graph
            .queue_child(
                "root",
                child.into(),
                format!("{child}-agent"),
                "capsule:shared".into(),
                budget(20, 20),
            )
            .unwrap();
    }
    graph.claim_or_resume_active_until("first", 10).unwrap();
    assert!(matches!(
        graph.claim_or_resume_active_until("second", 10),
        Err(WorkGraphError::InvalidGraph(_))
    ));
}

#[test]
fn queued_child_receipt_requires_live_execution_fence() {
    let mut graph = BoundedWorkGraph::default();
    graph
        .add_root(
            "root".into(),
            "root-agent".into(),
            "capsule".into(),
            budget(100, 100),
        )
        .unwrap();
    graph
        .queue_child(
            "root",
            "child".into(),
            "child-agent".into(),
            "capsule".into(),
            budget(20, 20),
        )
        .unwrap();
    graph.claim_and_activate("child").unwrap();
    let fence = graph
        .get_node("child")
        .unwrap()
        .execution_fence
        .clone()
        .unwrap();
    let mut stale = receipt(
        "receipt",
        "child",
        ChildOutcome::Accepted,
        "answer",
        "yes",
        5,
        4,
    );
    stale.execution_fence = Some(format!("fence:{}", "0".repeat(64)));
    assert!(matches!(
        graph.record_child_receipt(stale),
        Err(WorkGraphError::StaleExecutionFence(_))
    ));
    let mut valid = receipt(
        "receipt",
        "child",
        ChildOutcome::Accepted,
        "answer",
        "yes",
        5,
        4,
    );
    valid.execution_fence = Some(fence);
    graph.record_child_receipt(valid).unwrap();
}

#[test]
fn persisted_cycle_is_rejected() {
    let mut graph = BoundedWorkGraph::default();
    graph
        .add_root(
            "root".into(),
            "agent".into(),
            "capsule".into(),
            budget(100, 100),
        )
        .unwrap();
    graph.nodes.get_mut("root").unwrap().parent_node_id = Some("root".into());
    graph.children.insert("root".into(), vec!["root".into()]);
    assert!(matches!(
        graph.validate_invariants(),
        Err(WorkGraphError::InvalidGraph(_))
    ));
}
