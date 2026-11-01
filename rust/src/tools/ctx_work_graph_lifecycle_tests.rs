// SPDX-License-Identifier: Apache-2.0

use super::test_api::{node, request};
use super::*;

fn create_graph(project: &str) {
    let budget = WorkNodeBudget {
        tokens_allocated: 100,
        tokens_consumed: 0,
        cost_micros_allocated: 100,
        cost_micros_consumed: 0,
    };
    let mut graph = BoundedWorkGraph::default();
    graph
        .add_root(
            "root".into(),
            "lead".into(),
            "capsule:root".into(),
            budget.clone(),
        )
        .unwrap();
    graph
        .queue_child(
            "root",
            "child".into(),
            "worker".into(),
            "capsule:child".into(),
            WorkNodeBudget {
                tokens_allocated: 40,
                cost_micros_allocated: 40,
                ..budget
            },
        )
        .unwrap();
    WorkGraphStore::mutate(project, |store| store.create("graph", graph)).unwrap();
}

#[test]
fn durable_stop_revokes_and_frees_the_slot_only_after_it_persists() {
    let isolated = crate::core::data_dir::isolated_data_dir();
    let project = isolated.path().to_str().unwrap();
    create_graph(project);
    WorkGraphStore::mutate(project, |store| {
        let graph = store.graph_mut("graph")?;
        graph
            .queue_child(
                "root",
                "sibling".into(),
                "peer".into(),
                "capsule:sibling".into(),
                WorkNodeBudget {
                    tokens_allocated: 10,
                    tokens_consumed: 0,
                    cost_micros_allocated: 10,
                    cost_micros_consumed: 0,
                },
            )
            .map_err(|error| error.to_string())?;
        for id in ["child", "sibling"] {
            graph
                .set_path_claims(id, vec!["path:src/lib.rs".into()])
                .map_err(|error| error.to_string())?;
        }
        store.claim_node_execution("graph", "child", 1, 1_000)?;
        Ok(())
    })
    .unwrap();
    let cancel_request = request("cancel", project, "worker");
    let error =
        crate::core::work_graph_store::with_save_failure(|| cancel(&cancel_request)).unwrap_err();
    assert!(error.contains("injected work graph persistence failure"));
    assert_eq!(
        node(project).status,
        crate::core::work_graph::NodeStatus::Active
    );
    // The delegating host may stop its child; a stranger may not.
    assert!(cancel(&request("cancel", project, "stranger")).is_err());
    cancel(&request("cancel", project, "lead")).unwrap();
    let store = WorkGraphStore::load(project).unwrap();
    let graph = store.graph("graph").unwrap();
    // Only the root stays active; the stopped child no longer holds a slot.
    assert_eq!(graph.active_count(), 1);
    assert_eq!(
        graph.observation()["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|node| node["node_id"] == "child")
            .unwrap()["process_exit"],
        "confirmed"
    );
    // The stopped child may still be running: its paths stay exclusive
    // until its window ends.
    let claim_sibling = |now| {
        WorkGraphStore::mutate(project, |store| {
            store
                .claim_node_execution("graph", "sibling", now, 5_000)
                .map(|_| ())
        })
    };
    assert!(
        claim_sibling(999)
            .unwrap_err()
            .contains("path claim conflicts")
    );
    claim_sibling(1_000).unwrap();
}

/// AP07-G1 across tools: a node's path claim and a `ctx_agent` lease of
/// the same file are one lease resource, and a claim canonicalized for
/// another project never leases a path of this one.
#[test]
fn path_claims_and_agent_leases_name_one_resource() {
    let project = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let root = project.path().to_str().unwrap();
    let claim = canonical_path_claim(root, "path:src").unwrap();
    assert_eq!(
        crate::core::work_graph_delegation::path_lease_resource(root, &claim).unwrap(),
        crate::core::agent_lease::project_path_target_resource(root, "src").unwrap()
    );
    let foreign = canonical_path_claim(other.path().to_str().unwrap(), "path:src").unwrap();
    assert!(crate::core::work_graph_delegation::path_lease_resource(root, &foreign).is_err());
}

/// AP07-G6: the store's compare-and-mutate was unreachable from the tool,
/// so a write planned on an observed state silently overwrote a newer one.
#[test]
fn guarded_write_refuses_a_graph_changed_since_observe() {
    let isolated = crate::core::data_dir::isolated_data_dir();
    let project = isolated.path().to_str().unwrap();
    create_graph(project);
    let write_revision = || -> String {
        let observed: serde_json::Value =
            serde_json::from_str(&observe(&request("observe", project, "lead")).unwrap()).unwrap();
        observed["write_revision"].as_str().unwrap().to_string()
    };
    let observed = write_revision();
    WorkGraphStore::mutate(project, |store| {
        store
            .graph_mut("graph")?
            .set_path_claims("child", vec!["path:src/lib.rs".into()])
            .map_err(|error| error.to_string())
    })
    .unwrap();
    let stale = Request {
        if_revision: Some(&observed),
        ..request("cancel", project, "lead")
    };
    let before = node(project).status;
    assert!(cancel(&stale).unwrap_err().contains("revision conflict"));
    assert_eq!(node(project).status, before);
    let fresh = write_revision();
    cancel(&Request {
        if_revision: Some(&fresh),
        ..request("cancel", project, "lead")
    })
    .unwrap();
    assert_eq!(
        node(project).status,
        crate::core::work_graph::NodeStatus::Stopped
    );
}

#[test]
fn launcher_action_and_unbounded_windows_are_refused_before_any_claim() {
    let isolated = crate::core::data_dir::isolated_data_dir();
    let project = isolated.path().to_str().unwrap();
    create_graph(project);
    let before = WorkGraphStore::load(project)
        .unwrap()
        .graph_revision("graph");
    let execute = request("execute", project, "worker");
    assert_eq!(handle(execute).unwrap_err(), EXECUTE_REMOVED);
    for timeout in [0, 1_800_001] {
        let invalid = Request {
            timeout_ms: Some(timeout),
            ..request("start", project, "worker")
        };
        assert!(start(&invalid).unwrap_err().contains("timeout_ms"));
    }
    assert_eq!(
        before,
        WorkGraphStore::load(project)
            .unwrap()
            .graph_revision("graph")
    );
    // Every missing field of an action is named in one answer.
    let create = Request {
        capsule_ref: Some(""),
        tokens: Some(10),
        node_id: None,
        ..request("create", project, "lead")
    };
    assert_eq!(
        check_required_fields(&create).unwrap_err(),
        "create requires node_id, capsule_ref, task_ref, policy_ref, \
             expected_outcome_ref, tokens, cost_micros; missing: node_id, capsule_ref, task_ref, \
             policy_ref, expected_outcome_ref, cost_micros"
    );
}

/// Regression: a claimed child carries a fence, and the receipt path used to
/// demand a canonical measured receipt for task `graph:node` that nothing
/// produces, so claimed children could never finish.
#[test]
fn claimed_child_finishes_with_a_fenced_reported_receipt() {
    let isolated = crate::core::data_dir::isolated_data_dir();
    let project = isolated.path().to_str().unwrap();
    create_graph(project);
    claim(&request("claim", project, "worker")).unwrap();
    let fence = node(project).execution_fence.unwrap();
    let receipt_request = |agent: &'static str, fence: Option<&'static str>| Request {
        receipt_id: Some("receipt-child"),
        outcome_ref: Some("outcome:child"),
        outcome: Some("accepted"),
        tokens: Some(12),
        cost_micros: Some(3),
        execution_fence: fence,
        ..request("receipt", project, agent)
    };
    assert!(
        receipt(&receipt_request("worker", None))
            .unwrap_err()
            .contains("execution_fence")
    );
    let fence: &'static str = Box::leak(fence.into_boxed_str());
    assert!(receipt(&receipt_request("stranger", Some(fence))).is_err());
    receipt(&receipt_request("lead", Some(fence))).unwrap();
    let finished = node(project);
    assert_eq!(
        finished.status,
        crate::core::work_graph::NodeStatus::Completed
    );
    assert_eq!(finished.budget.tokens_consumed, 12);
    let graph: serde_json::Value =
        serde_json::from_str(&get(&request("get", project, "lead")).unwrap()).unwrap();
    assert_eq!(
        graph["receipts"]["receipt-child"]["usage_source"],
        "reported"
    );
    // Reported figures never free the allocation: the root's 100 minus the
    // child's full 40 leaves 60, not 100 - 12.
    let queue_next = |tokens: u64| {
        WorkGraphStore::mutate(project, |store| {
            store
                .graph_mut("graph")?
                .queue_child(
                    "root",
                    format!("next-{tokens}"),
                    "worker".into(),
                    format!("capsule:next-{tokens}"),
                    WorkNodeBudget {
                        tokens_allocated: tokens,
                        tokens_consumed: 0,
                        cost_micros_allocated: 1,
                        cost_micros_consumed: 0,
                    },
                )
                .map(|_| ())
                .map_err(|error| error.to_string())
        })
    };
    assert!(queue_next(61).is_err());
    queue_next(60).unwrap();
}

/// Regression: budget exhaustion stopped a started child without ending its
/// incarnation, so it held a concurrency slot and its paths forever.
#[test]
fn budget_exhaustion_ends_the_started_incarnation() {
    let isolated = crate::core::data_dir::isolated_data_dir();
    let project = isolated.path().to_str().unwrap();
    create_graph(project);
    WorkGraphStore::mutate(project, |store| {
        store.claim_node_execution("graph", "child", 1, u64::MAX)?;
        Ok(())
    })
    .unwrap();
    consume(&Request {
        tokens: Some(40),
        cost_micros: Some(40),
        ..request("consume", project, "worker")
    })
    .unwrap();
    let store = WorkGraphStore::load(project).unwrap();
    let graph = store.graph("graph").unwrap();
    assert_eq!(
        graph.get_node("child").unwrap().stop_reason,
        Some(StopReason::BudgetExhausted)
    );
    assert_eq!(graph.active_count(), 1);
}
