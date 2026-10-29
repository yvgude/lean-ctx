// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::core::context_capsule::{
    CONTEXT_CAPSULE_SCHEMA_VERSION, CapsuleSensitivityV1, ContextCapsuleBudgetV1,
    ContextCapsuleChainV1,
};
use crate::core::work_graph::{BoundedWorkGraph, NodeStatus, WorkNodeBudget};
use crate::core::work_graph_store::{ClaimNodeExecution, WorkGraphStore};
use lean_ctx_ocla::delivery_scope::DeliveryPrivacyV1;

fn plan() -> StartPlan {
    StartPlan {
        graph_id: "g1".into(),
        node: WorkNode {
            node_id: "child".into(),
            agent_id: "worker".into(),
            parent_node_id: Some("root".into()),
            capsule_ref: "capsule:child".into(),
            status: NodeStatus::Active,
            budget: WorkNodeBudget {
                tokens_allocated: 100,
                tokens_consumed: 0,
                cost_micros_allocated: 50,
                cost_micros_consumed: 0,
            },
            depth: 1,
            stop_reason: None,
            outcome_ref: None,
            claim_attempt: 1,
            delivery_attempt: None,
            execution_fence: Some(format!("fence:{}", "0".repeat(64))),
            lease_expires_epoch_ms: Some(u64::MAX),
            path_claims: Vec::new(),
            task_ref: "task:child".into(),
            parent_task_ref: Some("task:root".into()),
            policy_ref: "policy:workspace".into(),
            expected_outcome_ref: "outcome:tests-pass".into(),
            execution_started: true,
        },
        root_agent_id: "owner".into(),
        parent_capsule_ref: Some("capsule:root".into()),
        project_root: PathBuf::from("."),
        window_ms: 200,
    }
}

pub(crate) fn capsule() -> ContextCapsuleV1 {
    ContextCapsuleV1 {
        schema_version: CONTEXT_CAPSULE_SCHEMA_VERSION,
        capsule_id: "capsule:child".into(),
        request_id: "request:1".into(),
        session_id: "session:1".into(),
        agent_id: "owner".into(),
        intent_ref: "intent:test".into(),
        task_ref: "task:child".into(),
        expected_outcome_ref: "outcome:tests-pass".into(),
        acceptance_criteria_refs: Vec::new(),
        references: Vec::new(),
        finding_refs: Vec::new(),
        decision_refs: Vec::new(),
        uncertainty_refs: Vec::new(),
        negative_result_refs: Vec::new(),
        source_ref: "source:test".into(),
        policy_ref: "policy:workspace".into(),
        contract_ref: "contract:v1".into(),
        freshness_ref: "freshness:1".into(),
        sensitivity: CapsuleSensitivityV1::Internal,
        allowed_agent_ids: vec!["worker".into()],
        budget: ContextCapsuleBudgetV1 {
            tokens_used: 0,
            tokens_remaining: 100,
            cost_micros_used: 0,
            cost_micros_remaining: 50,
            latency_ms_used: 0,
            latency_ms_remaining: 200,
        },
        chain: ContextCapsuleChainV1 {
            chain_id: "graph:g1".into(),
            parent_capsule_ref: Some("capsule:root".into()),
            owner_agent_id: "owner".into(),
            attribution_ref: "attribution:1".into(),
            hop: 1,
        },
        quality_signal_refs: Vec::new(),
        recovery_refs: Vec::new(),
        delta_from: None,
    }
}

#[test]
fn capsule_scope_binds_graph_parent_outcome_window_and_signature() {
    let plan = plan();
    let valid = capsule();
    assert!(plan.validate_capsule_scope(&valid).is_ok());

    let mut wrong_graph = valid.clone();
    wrong_graph.chain.chain_id = "graph:other".into();
    assert!(plan.validate_capsule_scope(&wrong_graph).is_err());

    let mut wrong_parent = valid.clone();
    wrong_parent.chain.parent_capsule_ref = Some("capsule:other".into());
    assert!(plan.validate_capsule_scope(&wrong_parent).is_err());

    let mut wrong_outcome = valid.clone();
    wrong_outcome.expected_outcome_ref = "outcome:other".into();
    assert!(plan.validate_capsule_scope(&wrong_outcome).is_err());

    let mut short_window = valid.clone();
    short_window.budget.latency_ms_remaining = 199;
    assert!(plan.validate_capsule_scope(&short_window).is_err());

    let key = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
    let other = ed25519_dalek::SigningKey::from_bytes(&[8; 32]);
    let mut valid = valid;
    valid.assign_capsule_id().unwrap();
    let mut plan = plan;
    plan.node.capsule_ref = valid.capsule_id.clone();
    let signed = SignedContextCapsuleV1::sign(&valid, &key).unwrap();
    assert!(plan.verify_capsule(&signed, &key.verifying_key()).is_ok());
    assert!(
        plan.verify_capsule(&signed, &other.verifying_key())
            .is_err()
    );
}

#[test]
fn node_leases_isolate_projects_but_reject_competing_attempts() {
    use crate::core::agent_lease::{
        AGENT_LEASE_SCHEMA_VERSION, AgentLeaseAcquireV1, AgentLeaseRegistryV1, AgentLeaseRequestV1,
        AgentLeaseResourceKindV1,
    };
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let resource_a = node_lease_resource(first.path().to_str().unwrap(), "graph", "node").unwrap();
    let resource_b = node_lease_resource(second.path().to_str().unwrap(), "graph", "node").unwrap();
    assert_ne!(resource_a, resource_b);
    let request = |resource: String, owner: &str, attempt: &str| AgentLeaseRequestV1 {
        schema_version: AGENT_LEASE_SCHEMA_VERSION,
        lease_request_ref: format!("attempt:{attempt}"),
        resource_kind: AgentLeaseResourceKindV1::WorkGraphNode,
        resource_ref: resource,
        owner_agent_id: owner.into(),
        duration_ms: 100,
    };
    let mut registry = AgentLeaseRegistryV1::new(4);
    assert!(matches!(
        registry
            .acquire(request(resource_a.clone(), "worker-a", "attempt-a"), 1)
            .unwrap(),
        AgentLeaseAcquireV1::Granted(_)
    ));
    assert!(matches!(
        registry
            .acquire(request(resource_b, "worker-b", "attempt-b"), 1)
            .unwrap(),
        AgentLeaseAcquireV1::Granted(_)
    ));
    assert!(matches!(
        registry
            .acquire(request(resource_a, "worker-c", "attempt-c"), 1)
            .unwrap(),
        AgentLeaseAcquireV1::HeldBy { .. }
    ));
}

#[test]
fn issuance_requires_host_grant_and_current_persisted_attempt() {
    let _isolated = crate::core::data_dir::isolated_data_dir();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_str().unwrap();
    let (_, mut policy, _) = crate::core::a2a::task::delivery_delegation::tests::fixture();
    let host_key = crate::core::agent_identity::get_or_create_keypair("host").unwrap();
    policy.peers[0].public_key =
        crate::core::agent_identity::hex_encode(host_key.verifying_key().as_bytes());
    let authority_file = dir.path().join("authority.json");
    let budget = || WorkNodeBudget {
        tokens_allocated: 100,
        tokens_consumed: 0,
        cost_micros_allocated: 100,
        cost_micros_consumed: 0,
    };
    let mut node = WorkGraphStore::mutate(root, |store| {
        let mut graph = BoundedWorkGraph::default();
        graph
            .add_root(
                "root".into(),
                "host".into(),
                "root-capsule".into(),
                WorkNodeBudget {
                    tokens_allocated: 200,
                    cost_micros_allocated: 200,
                    ..budget()
                },
            )
            .map_err(|e| e.to_string())?;
        graph
            .queue_child(
                "root",
                "node".into(),
                "child".into(),
                "child-capsule".into(),
                budget(),
            )
            .map_err(|e| e.to_string())?;
        store.create("graph", graph)?;
        let now = chrono::Utc::now().timestamp_millis() as u64;
        let ClaimNodeExecution::Claimed(node) =
            store.claim_node_execution("graph", "node", now, now + 600_000)?
        else {
            return Err("unexpected recovery".into());
        };
        store
            .graph_mut("graph")?
            .begin_delivery_attempt("node", node.execution_fence.as_deref().unwrap(), 1)
            .map_err(|e| e.to_string())?;
        Ok(*node)
    })
    .unwrap();
    node.task_ref = "task:child".into();
    node.policy_ref = "policy:test".into();
    let plan = StartPlan {
        graph_id: "graph".into(),
        node,
        root_agent_id: "host".into(),
        parent_capsule_ref: None,
        project_root: dir.path().to_owned(),
        window_ms: 60_000,
    };
    let mut config: DeliveryIssuerConfig = serde_json::from_value(serde_json::json!({
        "schema_version": 1,
        "project_root": dir.path(),
        "authority_file": authority_file,
        "host_agent": "host",
        "host_key_id": "host-key",
        "host_grant_id": "delegate",
        "recipient": "daemon",
        "tenant_id": "account",
        "project_id": "project",
        "privacy": DeliveryPrivacyV1::Private,
        "allow_write": false,
    }))
    .unwrap();
    let fence = plan.node.execution_fence.as_deref().unwrap();
    let base = execution_key(dir.path(), "graph", "node", fence).unwrap();
    let task = attempt_task_id(&base, 1);
    policy.grants[0].revoked = true;
    std::fs::write(&authority_file, serde_json::to_vec(&policy).unwrap()).unwrap();
    assert!(
        config
            .issue(&plan, 1, &task, CapsuleSensitivityV1::Internal)
            .is_err()
    );
    // A refused issuance never provisions a child key.
    assert!(crate::core::agent_identity::get_stored_public_key("child").is_err());
    policy.grants[0].revoked = false;
    std::fs::write(&authority_file, serde_json::to_vec(&policy).unwrap()).unwrap();
    let profile = config
        .issue(&plan, 1, &task, CapsuleSensitivityV1::Internal)
        .unwrap();
    assert_eq!(profile.task_id, task);
    assert_eq!(profile.profile.write_grant_id, "no-write-grant");
    let delegation = profile.profile.delegation.as_ref().unwrap();
    assert!(delegation.write_grant_id.is_none());
    assert_eq!(delegation.execution.attempt, 1);
    assert!(profile.launch_value(&task, dir.path()).is_some());
    assert!(profile.launch_value("other-task", dir.path()).is_none());
    assert!(
        profile
            .launch_value(&task, tempfile::tempdir().unwrap().path())
            .is_none()
    );
    assert!(
        config
            .issue(&plan, 1, "wrong-task", CapsuleSensitivityV1::Internal)
            .is_err()
    );
    // A later persisted attempt revokes issuance for the earlier one.
    WorkGraphStore::mutate(root, |store| {
        store
            .graph_mut("graph")?
            .begin_delivery_attempt("node", fence, 2)
            .map_err(|e| e.to_string())
    })
    .unwrap();
    assert!(
        config
            .issue(&plan, 1, &task, CapsuleSensitivityV1::Internal)
            .is_err()
    );
    let second_task = attempt_task_id(&base, 2);
    assert_eq!(
        config
            .issue(&plan, 2, &second_task, CapsuleSensitivityV1::Internal)
            .unwrap()
            .profile
            .delegation
            .unwrap()
            .execution
            .attempt,
        2
    );
    config.privacy = DeliveryPrivacyV1::Project;
    assert!(
        config
            .issue(&plan, 2, &second_task, CapsuleSensitivityV1::Restricted)
            .is_err()
    );
    config.privacy = DeliveryPrivacyV1::Private;
    config.allow_write = true;
    let writable = config
        .issue(&plan, 2, &second_task, CapsuleSensitivityV1::Restricted)
        .unwrap();
    assert_eq!(
        writable
            .profile
            .delegation
            .as_ref()
            .unwrap()
            .write_grant_id
            .as_deref(),
        Some("delegated-write")
    );
    assert_eq!(writable.profile.write_grant_id, "delegated-write");
    config.allow_write = false;
    let other = tempfile::tempdir().unwrap();
    config.project_root = other.path().to_owned();
    assert!(
        config
            .issue(&plan, 2, &second_task, CapsuleSensitivityV1::Internal)
            .is_err()
    );
    config.project_root = dir.path().to_owned();
    // A durable stop ends issuance for the incarnation.
    WorkGraphStore::mutate(root, |store| {
        store
            .graph_mut("graph")?
            .stop("node", crate::core::work_graph::StopReason::ManualStop)
            .map_err(|error| error.to_string())
    })
    .unwrap();
    assert!(
        config
            .issue(&plan, 2, &second_task, CapsuleSensitivityV1::Internal)
            .is_err()
    );
}

#[test]
fn issuer_environment_requires_bounded_complete_json() {
    let _guard = crate::core::data_dir::test_env_lock();
    let previous = std::env::var_os(ISSUER_ENV);
    let rejected: Vec<_> = [
        String::new(),
        "{}".into(),
        "null".into(),
        "x".repeat(16_385),
    ]
    .into_iter()
    .map(|raw| {
        crate::test_env::set_var(ISSUER_ENV, raw);
        DeliveryIssuerConfig::from_environment().is_err()
    })
    .collect();
    crate::test_env::remove_var(ISSUER_ENV);
    let absent = DeliveryIssuerConfig::from_environment().unwrap().is_none();
    if let Some(previous) = previous {
        crate::test_env::set_var(ISSUER_ENV, previous);
    }
    assert!(rejected.into_iter().all(|rejected| rejected));
    assert!(absent);
}
