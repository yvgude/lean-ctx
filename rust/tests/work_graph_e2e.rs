// SPDX-License-Identifier: Apache-2.0
//! Public-core end-to-end coverage for the bounded local Work Graph.
//!
//! These tests exercise the public Work Graph contract, including bounded
//! scheduling, durable lease semantics, policy inheritance, and value reporting.

use lean_ctx::core::agent_lease::{
    AGENT_LEASE_SCHEMA_VERSION, AgentLeaseAcquireV1, AgentLeaseRegistryV1, AgentLeaseRequestV1,
    AgentLeaseResourceKindV1,
};
use lean_ctx::core::work_graph::{
    BoundedWorkGraph, ChildExecutionReceipt, ChildOutcome, NodeStatus, ResultClaim, StopReason,
    WorkGraphError, WorkNodeBudget,
};

fn budget(tokens: u64, cost_micros: u64) -> WorkNodeBudget {
    WorkNodeBudget {
        tokens_allocated: tokens,
        tokens_consumed: 0,
        cost_micros_allocated: cost_micros,
        cost_micros_consumed: 0,
    }
}

fn add_root(graph: &mut BoundedWorkGraph, node_id: &str, agent_id: &str, tokens: u64, cost: u64) {
    graph
        .add_root(
            node_id.to_string(),
            agent_id.to_string(),
            format!("capsule:{node_id}"),
            budget(tokens, cost),
        )
        .expect("root must be accepted");
}

fn receipt(
    receipt_id: &str,
    node_id: &str,
    outcome: ChildOutcome,
    tokens: u64,
    cost_micros: u64,
    execution_fence: Option<String>,
    claims: Vec<ResultClaim>,
) -> ChildExecutionReceipt {
    ChildExecutionReceipt {
        receipt_id: receipt_id.to_string(),
        node_id: node_id.to_string(),
        outcome_ref: format!("outcome:{receipt_id}"),
        outcome,
        tokens_consumed: tokens,
        cost_micros_consumed: cost_micros,
        execution_fence,
        claims,
    }
}

#[test]
fn fan_out_three_preserves_parent_lineage_and_capsule() {
    let mut graph = BoundedWorkGraph::new(3, 8);
    add_root(&mut graph, "root", "root-agent", 1_000, 1_000);
    graph
        .configure_root_context("root", "task:root", "policy:workspace", "outcome:root")
        .unwrap();

    for (node_id, agent_id) in [
        ("child-1", "agent-1"),
        ("child-2", "agent-2"),
        ("child-3", "agent-3"),
    ] {
        graph
            .queue_child(
                "root",
                node_id.to_string(),
                agent_id.to_string(),
                format!("capsule:{node_id}"),
                budget(100, 100),
            )
            .expect("three bounded children must be accepted");
        graph
            .configure_child_context(
                node_id,
                &format!("task:{node_id}"),
                &format!("outcome:{node_id}"),
            )
            .unwrap();
        graph.claim_and_activate(node_id).unwrap();
    }

    assert_eq!(
        graph.children_of("root"),
        &[
            "child-1".to_string(),
            "child-2".to_string(),
            "child-3".to_string()
        ]
    );
    for (node_id, agent_id) in [
        ("child-1", "agent-1"),
        ("child-2", "agent-2"),
        ("child-3", "agent-3"),
    ] {
        let node = graph.get_node(node_id).expect("child exists");
        assert_eq!(node.agent_id, agent_id);
        assert_eq!(node.parent_node_id.as_deref(), Some("root"));
        assert_eq!(node.depth, 1);
        assert_eq!(node.capsule_ref, format!("capsule:{node_id}"));
        assert_eq!(node.status, NodeStatus::Active);
        assert_eq!(node.parent_task_ref.as_deref(), Some("task:root"));
        assert_eq!(node.policy_ref, "policy:workspace");
        assert_eq!(
            graph.root_agent_for(node_id).expect("root lineage"),
            "root-agent"
        );
    }
    assert_eq!(graph.active_count(), 4);
    graph.validate_invariants().expect("graph remains valid");
}

#[test]
fn token_and_cost_budgets_cascade_without_double_counting() {
    let mut graph = BoundedWorkGraph::default();
    add_root(&mut graph, "root", "root-agent", 1_000, 500);

    let child_budget = graph
        .allocate_child_budget("root", "child", 0.25)
        .expect("fractional child allocation");
    assert_eq!(child_budget.tokens_allocated, 250);
    assert_eq!(child_budget.cost_micros_allocated, 125);
    graph
        .delegate(
            "root",
            "child".to_string(),
            "child-agent".to_string(),
            "capsule:child".to_string(),
            child_budget,
        )
        .expect("allocated child must be attachable");

    graph
        .consume_budget("child", 40, 20)
        .expect("child consumption");
    graph
        .consume_budget("root", 60, 30)
        .expect("root consumption");
    let chain = graph.chain_budget_for("child").expect("chain budget");
    assert_eq!(chain.total_consumed_tokens, 100);
    assert_eq!(chain.total_consumed_cost_micros, 50);
    assert_eq!(graph.get_node("child").unwrap().budget.tokens_consumed, 40);
    assert_eq!(
        graph.get_node("child").unwrap().budget.cost_micros_consumed,
        20
    );

    assert!(matches!(
        graph.consume_budget("child", 211, 106),
        Err(WorkGraphError::InvalidBudget)
    ));
    assert_eq!(graph.get_node("child").unwrap().budget.tokens_consumed, 40);
    assert_eq!(
        graph
            .chain_budget_for("root")
            .unwrap()
            .total_consumed_tokens,
        100
    );
    graph
        .validate_invariants()
        .expect("budget accounting is valid");
}

#[test]
fn depth_and_fan_out_caps_fail_closed() {
    let mut depth_graph = BoundedWorkGraph::new(16, 2);
    add_root(&mut depth_graph, "root", "root-agent", 1_000, 1_000);
    depth_graph
        .delegate(
            "root",
            "child".to_string(),
            "child-agent".to_string(),
            "capsule:child".to_string(),
            budget(400, 400),
        )
        .unwrap();
    depth_graph
        .delegate(
            "child",
            "grandchild".to_string(),
            "grand-agent".to_string(),
            "capsule:grandchild".to_string(),
            budget(200, 200),
        )
        .unwrap();
    assert!(matches!(
        depth_graph.delegate(
            "grandchild",
            "great-grandchild".to_string(),
            "great-agent".to_string(),
            "capsule:great-grandchild".to_string(),
            budget(100, 100),
        ),
        Err(WorkGraphError::DepthExceeded(2))
    ));
    assert_eq!(depth_graph.total_count(), 3);
    depth_graph.validate_invariants().unwrap();

    let mut fan_out_graph = BoundedWorkGraph::new(3, 8);
    add_root(&mut fan_out_graph, "root", "root-agent", 1_000, 1_000);
    for child in ["child-1", "child-2", "child-3"] {
        fan_out_graph
            .delegate(
                "root",
                child.to_string(),
                format!("agent-{child}"),
                format!("capsule:{child}"),
                budget(100, 100),
            )
            .unwrap();
    }
    assert!(matches!(
        fan_out_graph.delegate(
            "root",
            "child-4".to_string(),
            "agent-4".to_string(),
            "capsule:child-4".to_string(),
            budget(100, 100),
        ),
        Err(WorkGraphError::FanOutExceeded(3))
    ));
    assert_eq!(fan_out_graph.total_count(), 4);
    fan_out_graph.validate_invariants().unwrap();
}

#[test]
fn parent_cancellation_stops_all_descendants() {
    let mut graph = BoundedWorkGraph::default();
    add_root(&mut graph, "root", "root-agent", 1_000, 1_000);
    graph
        .delegate(
            "root",
            "child".to_string(),
            "child-agent".to_string(),
            "capsule:child".to_string(),
            budget(300, 300),
        )
        .unwrap();
    graph
        .delegate(
            "child",
            "grandchild".to_string(),
            "grand-agent".to_string(),
            "capsule:grandchild".to_string(),
            budget(100, 100),
        )
        .unwrap();

    assert_eq!(
        graph.stop("root", StopReason::ManualStop).unwrap(),
        vec!["root", "child", "grandchild"]
    );
    assert_eq!(
        graph.get_node("root").unwrap().stop_reason,
        Some(StopReason::ManualStop)
    );
    for node_id in ["root", "child", "grandchild"] {
        assert_eq!(graph.get_node(node_id).unwrap().status, NodeStatus::Stopped);
    }
    assert_eq!(
        graph.get_node("child").unwrap().stop_reason,
        Some(StopReason::ParentStopped)
    );
    assert_eq!(
        graph.get_node("grandchild").unwrap().stop_reason,
        Some(StopReason::ParentStopped)
    );
    graph.validate_invariants().unwrap();
}

#[test]
fn stale_and_redundant_stops_are_recorded_explicitly() {
    let mut graph = BoundedWorkGraph::default();
    add_root(&mut graph, "stale-root", "stale-agent", 100, 100);
    add_root(&mut graph, "redundant-root", "redundant-agent", 100, 100);

    graph.stop("stale-root", StopReason::Stale).unwrap();
    graph.stop("redundant-root", StopReason::Redundant).unwrap();
    assert_eq!(
        graph.get_node("stale-root").unwrap().stop_reason,
        Some(StopReason::Stale)
    );
    assert_eq!(
        graph.get_node("redundant-root").unwrap().stop_reason,
        Some(StopReason::Redundant)
    );
    graph.validate_invariants().unwrap();
}

#[test]
fn graph_bound_path_lease_blocks_overlap_then_expires() {
    let mut graph = BoundedWorkGraph::default().with_local_concurrency(3);
    add_root(&mut graph, "root", "root-agent", 1_000, 1_000);
    for (node, agent) in [("first", "agent-1"), ("second", "agent-2")] {
        graph
            .queue_child(
                "root",
                node.into(),
                agent.into(),
                format!("capsule:{node}"),
                budget(100, 100),
            )
            .unwrap();
        graph
            .set_path_claims(node, vec!["path:src/shared.rs".into()])
            .unwrap();
        graph.claim_and_activate(node).unwrap();
    }
    let mut leases = AgentLeaseRegistryV1::new(4);
    let request = |id: &str, owner: &str| AgentLeaseRequestV1 {
        schema_version: AGENT_LEASE_SCHEMA_VERSION,
        lease_request_ref: format!("request:{id}"),
        resource_kind: AgentLeaseResourceKindV1::Path,
        resource_ref: graph.get_node(id).unwrap().path_claims[0].clone(),
        owner_agent_id: owner.into(),
        duration_ms: 50,
    };
    assert!(matches!(
        leases.acquire(request("first", "agent-1"), 100).unwrap(),
        AgentLeaseAcquireV1::Granted(_)
    ));
    assert!(matches!(
        leases.acquire(request("second", "agent-2"), 149).unwrap(),
        AgentLeaseAcquireV1::HeldBy { .. }
    ));
    assert!(matches!(
        leases.acquire(request("second", "agent-2"), 150).unwrap(),
        AgentLeaseAcquireV1::Granted(_)
    ));
}

#[test]
fn receipts_require_the_claim_fence_and_aggregate_deltas() {
    let mut graph = BoundedWorkGraph::default();
    add_root(&mut graph, "root", "root-agent", 1_000, 1_000);
    graph
        .queue_child(
            "root",
            "child".to_string(),
            "child-agent".to_string(),
            "capsule:child".to_string(),
            budget(100, 100),
        )
        .unwrap();
    graph.claim_and_activate("child").unwrap();
    let fence = graph
        .get_node("child")
        .unwrap()
        .execution_fence
        .clone()
        .expect("claim fence");

    graph.consume_budget("child", 5, 3).unwrap();
    let wrong_fence = format!("fence:{}", "0".repeat(64));
    assert!(matches!(
        graph.record_child_receipt(receipt(
            "receipt-1",
            "child",
            ChildOutcome::Accepted,
            10,
            8,
            Some(wrong_fence),
            Vec::new(),
        )),
        Err(WorkGraphError::StaleExecutionFence(_))
    ));
    assert_eq!(graph.get_node("child").unwrap().status, NodeStatus::Active);
    assert_eq!(graph.get_node("child").unwrap().budget.tokens_consumed, 5);

    let valid_receipt = receipt(
        "receipt-1",
        "child",
        ChildOutcome::Accepted,
        10,
        8,
        Some(fence),
        Vec::new(),
    );
    graph
        .record_child_receipt(valid_receipt.clone())
        .expect("valid receipt");
    assert_eq!(
        graph
            .chain_budget_for("root")
            .unwrap()
            .total_consumed_tokens,
        10
    );
    assert_eq!(
        graph
            .chain_budget_for("root")
            .unwrap()
            .total_consumed_cost_micros,
        8
    );
    assert_eq!(
        graph.evaluate_child_outcome("child").unwrap(),
        ChildOutcome::Accepted
    );
    assert!(matches!(
        graph.record_child_receipt(valid_receipt),
        Err(WorkGraphError::DuplicateReceipt(_))
    ));
    graph.validate_invariants().unwrap();
}

#[test]
fn child_outcomes_are_evaluated_independently() {
    let mut graph = BoundedWorkGraph::default();
    add_root(&mut graph, "root", "root-agent", 1_000, 1_000);
    for child in ["accepted", "rejected"] {
        graph
            .queue_child(
                "root",
                child.to_string(),
                format!("agent-{child}"),
                format!("capsule:{child}"),
                budget(100, 100),
            )
            .unwrap();
        graph.claim_and_activate(child).unwrap();
    }
    let accepted_fence = graph.get_node("accepted").unwrap().execution_fence.clone();
    let rejected_fence = graph.get_node("rejected").unwrap().execution_fence.clone();
    graph
        .record_child_receipt(receipt(
            "accepted-receipt",
            "accepted",
            ChildOutcome::Accepted,
            20,
            10,
            accepted_fence,
            Vec::new(),
        ))
        .unwrap();
    graph
        .record_child_receipt(receipt(
            "rejected-receipt",
            "rejected",
            ChildOutcome::Rejected,
            15,
            7,
            rejected_fence,
            Vec::new(),
        ))
        .unwrap();

    assert_eq!(
        graph.evaluate_child_outcome("accepted").unwrap(),
        ChildOutcome::Accepted
    );
    assert_eq!(
        graph.evaluate_child_outcome("rejected").unwrap(),
        ChildOutcome::Rejected
    );
    assert_eq!(
        graph.get_node("accepted").unwrap().status,
        NodeStatus::Completed
    );
    assert_eq!(
        graph.get_node("rejected").unwrap().status,
        NodeStatus::Failed
    );
    graph.validate_invariants().unwrap();
}

#[test]
fn contradictory_accepted_claims_fuse_deterministically() {
    let mut graph = BoundedWorkGraph::default();
    add_root(&mut graph, "root", "root-agent", 1_000, 1_000);
    for child in ["left", "right"] {
        graph
            .queue_child(
                "root",
                child.to_string(),
                format!("agent-{child}"),
                format!("capsule:{child}"),
                budget(100, 100),
            )
            .unwrap();
        graph.claim_and_activate(child).unwrap();
    }
    for (child, value) in [("left", "one"), ("right", "two")] {
        let fence = graph.get_node(child).unwrap().execution_fence.clone();
        graph
            .record_child_receipt(receipt(
                &format!("{child}-receipt"),
                child,
                ChildOutcome::Accepted,
                10,
                10,
                fence,
                vec![ResultClaim {
                    key: "answer".to_string(),
                    value: value.to_string(),
                }],
            ))
            .unwrap();
    }
    graph
        .mark_accepted_path(&["left".to_string(), "right".to_string()])
        .unwrap();

    let fused = graph.fuse_accepted_results().unwrap();
    assert!(fused.accepted.is_empty());
    assert_eq!(fused.conflicts.len(), 1);
    assert_eq!(fused.conflicts[0].key, "answer");
    assert_eq!(fused.conflicts[0].values, vec!["one", "two"]);
    assert_eq!(fused.conflicts[0].node_ids, vec!["left", "right"]);
    assert_eq!(fused, graph.fuse_accepted_results().unwrap());
    graph.validate_invariants().unwrap();
}

#[test]
fn accepted_path_marks_ancestors_and_rejects_failed_nodes() {
    let mut graph = BoundedWorkGraph::default();
    add_root(&mut graph, "root", "root-agent", 1_000, 1_000);
    for child in ["accepted", "rejected"] {
        graph
            .queue_child(
                "root",
                child.to_string(),
                format!("agent-{child}"),
                format!("capsule:{child}"),
                budget(100, 100),
            )
            .unwrap();
        graph.claim_and_activate(child).unwrap();
    }
    for (child, outcome) in [
        ("accepted", ChildOutcome::Accepted),
        ("rejected", ChildOutcome::Rejected),
    ] {
        let fence = graph.get_node(child).unwrap().execution_fence.clone();
        graph
            .record_child_receipt(receipt(
                &format!("{child}-receipt"),
                child,
                outcome,
                10,
                10,
                fence,
                Vec::new(),
            ))
            .unwrap();
    }

    graph.mark_accepted_path(&["accepted".to_string()]).unwrap();
    assert!(graph.is_on_accepted_path("root"));
    assert!(graph.is_on_accepted_path("accepted"));
    assert!(!graph.is_on_accepted_path("rejected"));
    assert!(matches!(
        graph.mark_accepted_path(&["rejected".to_string()]),
        Err(WorkGraphError::RejectedAcceptedPath(_))
    ));
    graph.validate_invariants().unwrap();
}

#[test]
fn rejected_branch_cost_is_attributed_as_waste() {
    let mut graph = BoundedWorkGraph::default();
    add_root(&mut graph, "root", "root-agent", 1_000, 1_000);
    for child in ["accepted", "rejected"] {
        graph
            .queue_child(
                "root",
                child.to_string(),
                format!("agent-{child}"),
                format!("capsule:{child}"),
                budget(100, 100),
            )
            .unwrap();
        graph.claim_and_activate(child).unwrap();
    }
    for (child, outcome, tokens, cost) in [
        ("accepted", ChildOutcome::Accepted, 20, 10),
        ("rejected", ChildOutcome::Rejected, 7, 5),
    ] {
        let fence = graph.get_node(child).unwrap().execution_fence.clone();
        graph
            .record_child_receipt(receipt(
                &format!("{child}-receipt"),
                child,
                outcome,
                tokens,
                cost,
                fence,
                Vec::new(),
            ))
            .unwrap();
    }
    graph.mark_accepted_path(&["accepted".to_string()]).unwrap();

    let attribution = graph.attribution().unwrap();
    assert_eq!(attribution.accepted_tokens, 20);
    assert_eq!(attribution.accepted_cost_micros, 10);
    assert_eq!(attribution.waste_tokens, 7);
    assert_eq!(attribution.waste_cost_micros, 5);
    let value = graph.team_value_report().unwrap();
    assert_eq!(value.accepted_tokens, 20);
    assert_eq!(value.total_tokens, 27);
    assert_eq!(value.accepted_cost_micros, 10);
    assert_eq!(value.total_cost_micros, 15);
    assert_eq!(value.waste_tokens, 7);
    assert_eq!(value.waste_cost_micros, 5);
    assert_eq!(value.accepted_cost_basis_points, 6_666);
    graph.validate_invariants().unwrap();
}
