// SPDX-License-Identifier: Apache-2.0
//! Fail-closed execution boundary for a claimed local Work Graph node.

use std::path::{Path, PathBuf};

use ed25519_dalek::VerifyingKey;
mod delivery_issuer;

use crate::core::agent_connector::receipt::verified_receipt_usage_for_task;
use crate::core::agent_connector::traits::{AgentConnector, TaskRequest, TaskResult};
use crate::core::context_capsule::{ContextCapsuleV1, SignedContextCapsuleV1};
use crate::core::work_graph::{
    ChildExecutionReceipt, ChildOutcome, MeasuredExecutionSpend, WorkNode,
};

pub(crate) const MAX_ATTEMPTS: u8 = 2;
const MAX_TIMEOUT_MS: u64 = 30 * 60 * 1_000;

/// Validate before acquiring a claim or provisioning execution authority.
pub(crate) fn execution_window_ms(timeout_ms: u64, max_attempts: u8) -> Result<u64, String> {
    if timeout_ms == 0 || timeout_ms > MAX_TIMEOUT_MS {
        return Err(format!("timeout_ms must be between 1 and {MAX_TIMEOUT_MS}"));
    }
    if max_attempts == 0 || max_attempts > MAX_ATTEMPTS {
        return Err(format!("max_attempts must be between 1 and {MAX_ATTEMPTS}"));
    }
    timeout_ms
        .checked_mul(u64::from(max_attempts))
        .ok_or_else(|| "execution lease duration overflow".to_string())
}

/// Node exclusivity spans attempts, but never unrelated project stores.
pub(crate) fn node_lease_resource(
    project_root: &str,
    graph_id: &str,
    node_id: &str,
) -> Result<String, String> {
    crate::core::work_graph_store::validate_id(graph_id, "graph_id")?;
    crate::core::work_graph_store::validate_id(node_id, "node_id")?;
    let scope = crate::core::project_hash::hash_project_root(project_root);
    let bytes =
        serde_json::to_vec(&(scope, graph_id, node_id)).map_err(|error| error.to_string())?;
    Ok(format!("work-graph-node:{}", blake3::hash(&bytes).to_hex()))
}

/// One execution incarnation, shared by dispatch, cancellation and receipts.
/// Length-delimited serialization prevents graph/node separator collisions.
pub(crate) fn execution_key(
    project_root: &Path,
    graph_id: &str,
    node_id: &str,
    fence: &str,
) -> Result<String, String> {
    let root = project_root
        .to_str()
        .ok_or_else(|| "project root must be valid UTF-8".to_string())?;
    execution_key_for_scope(
        &crate::core::project_hash::hash_project_root(root),
        graph_id,
        node_id,
        fence,
    )
}

/// Use the authoritative persisted store scope, including its project identity.
pub(crate) fn execution_key_for_scope(
    project_scope: &str,
    graph_id: &str,
    node_id: &str,
    fence: &str,
) -> Result<String, String> {
    if graph_id.is_empty() || node_id.is_empty() || fence.is_empty() {
        return Err("execution identity requires graph, node and fence".into());
    }
    let bytes = serde_json::to_vec(&(project_scope, graph_id, node_id, fence))
        .map_err(|error| error.to_string())?;
    Ok(format!("work-execution:{}", blake3::hash(&bytes).to_hex()))
}

#[derive(Clone, Debug)]
pub(crate) struct NodeExecutionFailure {
    pub message: String,
    pub measured_spend: Option<Box<MeasuredExecutionSpend>>,
}

impl std::fmt::Display for NodeExecutionFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl From<String> for NodeExecutionFailure {
    fn from(message: String) -> Self {
        Self {
            message,
            measured_spend: None,
        }
    }
}

impl From<&str> for NodeExecutionFailure {
    fn from(message: &str) -> Self {
        message.to_string().into()
    }
}

#[derive(Clone, Debug)]
pub(crate) struct NodeExecutionPlan {
    pub graph_id: String,
    pub node: WorkNode,
    pub root_agent_id: String,
    pub parent_capsule_ref: Option<String>,
    pub project_root: PathBuf,
    pub timeout_ms: u64,
    pub model: Option<String>,
    pub max_attempts: u8,
}

impl NodeExecutionPlan {
    fn validate(&self) -> Result<&str, String> {
        if self.graph_id.is_empty() || self.graph_id.len() > 128 {
            return Err("invalid graph_id".into());
        }
        execution_window_ms(self.timeout_ms, self.max_attempts)?;
        let fence = self
            .node
            .execution_fence
            .as_deref()
            .ok_or_else(|| "node must carry a live execution fence".to_string())?;
        if self.node.task_ref.is_empty() || self.node.policy_ref.is_empty() {
            return Err("node task/policy context is not configured".into());
        }
        let canonical_root = self
            .project_root
            .canonicalize()
            .map_err(|error| format!("invalid project root: {error}"))?;
        if !canonical_root.is_dir() {
            return Err("project root is not a directory".into());
        }
        Ok(fence)
    }
}

pub(crate) fn execute_claimed_node(
    plan: &NodeExecutionPlan,
    signed_capsule: &SignedContextCapsuleV1,
    pinned_key: &VerifyingKey,
    connector: &dyn AgentConnector,
) -> Result<ChildExecutionReceipt, NodeExecutionFailure> {
    let issuer = delivery_issuer::DeliveryIssuerConfig::from_environment()?;
    execute_claimed_node_with_hooks(
        plan,
        signed_capsule,
        pinned_key,
        connector,
        |reference, task_id| {
            verified_receipt_usage_for_task(reference, task_id).map_err(|error| error.to_string())
        },
        |attempt, task_id| {
            let root = plan
                .project_root
                .to_str()
                .ok_or("project root must be UTF-8")?;
            let fence = plan
                .node
                .execution_fence
                .as_deref()
                .ok_or("missing execution fence")?;
            crate::core::work_graph_store::WorkGraphStore::mutate(root, |store| {
                store
                    .graph_mut(&plan.graph_id)?
                    .begin_delivery_attempt(&plan.node.node_id, fence, attempt)
                    .map_err(|error| error.to_string())
            })?;
            issuer
                .as_ref()
                .map(|issuer| {
                    issuer.issue(plan, attempt, task_id, signed_capsule.capsule.sensitivity)
                })
                .transpose()
        },
    )
}

#[cfg(test)]
fn execute_claimed_node_with_verifier<F>(
    plan: &NodeExecutionPlan,
    signed_capsule: &SignedContextCapsuleV1,
    pinned_key: &VerifyingKey,
    connector: &dyn AgentConnector,
    verify_receipt: F,
) -> Result<ChildExecutionReceipt, NodeExecutionFailure>
where
    F: Fn(&str, &str) -> Result<(u64, u64), String>,
{
    execute_claimed_node_with_hooks(
        plan,
        signed_capsule,
        pinned_key,
        connector,
        verify_receipt,
        |_, _| Ok(None),
    )
}

fn execute_claimed_node_with_hooks<F, B>(
    plan: &NodeExecutionPlan,
    signed_capsule: &SignedContextCapsuleV1,
    pinned_key: &VerifyingKey,
    connector: &dyn AgentConnector,
    verify_receipt: F,
    before_attempt: B,
) -> Result<ChildExecutionReceipt, NodeExecutionFailure>
where
    F: Fn(&str, &str) -> Result<(u64, u64), String>,
    B: Fn(
        u8,
        &str,
    ) -> Result<
        Option<Box<crate::core::agent_connector::traits::ChildDeliveryProfileV1>>,
        String,
    >,
{
    let fence = plan.validate()?.to_string();
    signed_capsule
        .verify(pinned_key)
        .map_err(|error| format!("capsule verification failed: {error}"))?;
    let capsule = &signed_capsule.capsule;
    validate_capsule_scope(plan, capsule)?;
    if !connector
        .health_check()
        .map_err(|error| format!("connector health check failed: {error}"))?
    {
        return Err("connector is unavailable".into());
    }
    if connector.name() != "codex" {
        return Err("connector lacks an enforced read-only execution sandbox".into());
    }

    let request = TaskRequest {
        id: execution_key(
            &plan.project_root,
            &plan.graph_id,
            &plan.node.node_id,
            &fence,
        )?,
        prompt: execution_prompt(plan, signed_capsule),
        working_dir: canonical_directory(&plan.project_root)?,
        timeout_ms: plan.timeout_ms,
        model: plan.model.clone(),
        max_turns: None,
        profile_name: Some("work-graph".into()),
        profile_hash: None,
        delivery_profile: None,
    };
    let mut attempts = Vec::new();
    for attempt in 1..=plan.max_attempts {
        let mut attempt_request = request.clone();
        attempt_request.id = format!("{}:attempt-{attempt}", request.id);
        // Publish the retry fence before allowing the process to use authority.
        // Persistence failure must never launch an untracked child.
        attempt_request.delivery_profile = before_attempt(attempt, &attempt_request.id)?;
        match connector.execute(&attempt_request) {
            Ok(result) => {
                if result.task_id != attempt_request.id {
                    return Err("connector result does not match dispatched attempt".into());
                }
                if result.execution_receipt_ref.is_none() {
                    return Err(
                        "connector attempt completed without canonical usage receipt".into(),
                    );
                }
                let terminal = result.success;
                attempts.push(result);
                // Verify measured usage before authorizing another paid attempt.
                let receipt =
                    receipt_from_attempts(plan, fence.clone(), &attempts, &verify_receipt)?;
                if receipt.tokens_consumed > plan.node.budget.tokens_allocated
                    || receipt.cost_micros_consumed > plan.node.budget.cost_micros_allocated
                {
                    let outcome_refs = attempts
                        .iter()
                        .filter_map(|attempt| attempt.execution_receipt_ref.clone())
                        .collect::<Vec<_>>();
                    return Err(NodeExecutionFailure {
                        message: "connector result exceeds node budget".into(),
                        measured_spend: Some(Box::new(MeasuredExecutionSpend::new(
                            plan.node.node_id.clone(),
                            outcome_refs,
                            receipt.tokens_consumed,
                            receipt.cost_micros_consumed,
                            fence,
                        ))),
                    });
                }
                if terminal
                    || attempt == plan.max_attempts
                    // The CLI connector cannot enforce a provider-side token or
                    // cost ceiling for the next attempt.  Once an attempt has
                    // incurred measured spend, fail closed instead of risking a
                    // cumulative retry overrun.  A completed attempt whose
                    // canonical receipt measures zero usage may still use the
                    // bounded retry slot.
                    || receipt.tokens_consumed > 0
                    || receipt.cost_micros_consumed > 0
                {
                    return Ok(receipt);
                }
            }
            Err(error) => {
                return Err(format!(
                    "connector attempt failed without canonical usage receipt: {error}"
                )
                .into());
            }
        }
    }
    Err("connector produced no attempt".into())
}

fn validate_capsule_scope(
    plan: &NodeExecutionPlan,
    capsule: &ContextCapsuleV1,
) -> Result<(), String> {
    let required_latency_ms = plan
        .timeout_ms
        .checked_mul(u64::from(plan.max_attempts))
        .ok_or_else(|| "execution latency budget overflow".to_string())?;
    if capsule.capsule_id != plan.node.capsule_ref
        || capsule.task_ref != plan.node.task_ref
        || capsule.policy_ref != plan.node.policy_ref
        || capsule.expected_outcome_ref != plan.node.expected_outcome_ref
        || capsule.chain.chain_id != format!("graph:{}", plan.graph_id)
        || capsule.chain.parent_capsule_ref != plan.parent_capsule_ref
        || capsule.chain.hop != plan.node.depth
        || capsule.chain.owner_agent_id != plan.root_agent_id
        || !capsule
            .allowed_agent_ids
            .iter()
            .any(|agent| agent == &plan.node.agent_id)
        || capsule.budget.tokens_remaining < plan.node.budget.tokens_allocated
        || capsule.budget.cost_micros_remaining < plan.node.budget.cost_micros_allocated
        || capsule.budget.latency_ms_remaining < required_latency_ms
    {
        return Err("capsule does not authorize this node or budget".into());
    }
    Ok(())
}

fn canonical_directory(path: &Path) -> Result<PathBuf, String> {
    let canonical = path
        .canonicalize()
        .map_err(|error| format!("invalid working directory: {error}"))?;
    canonical
        .is_dir()
        .then_some(canonical)
        .ok_or_else(|| "working directory is not a directory".into())
}

fn execution_prompt(plan: &NodeExecutionPlan, signed: &SignedContextCapsuleV1) -> String {
    format!(
        "Execute bounded Work Graph node {}. Context capsule {} is verified; policy_ref={}; task_ref={}; expected_outcome_ref={}. Stay within {} tokens and {} cost micros. Return a concise result.",
        plan.node.node_id,
        signed.capsule.capsule_id,
        signed.capsule.policy_ref,
        signed.capsule.task_ref,
        signed.capsule.expected_outcome_ref,
        plan.node.budget.tokens_allocated,
        plan.node.budget.cost_micros_allocated,
    )
}

fn receipt_from_attempts<F>(
    plan: &NodeExecutionPlan,
    fence: String,
    attempts: &[TaskResult],
    verify_receipt: F,
) -> Result<ChildExecutionReceipt, String>
where
    F: Fn(&str, &str) -> Result<(u64, u64), String>,
{
    let result = attempts
        .last()
        .ok_or_else(|| "connector produced no result".to_string())?;
    let reference = result
        .execution_receipt_ref
        .as_deref()
        .ok_or_else(|| "connector result lacks canonical execution receipt".to_string())?;
    let mut tokens = 0_u64;
    let mut cost = 0_u64;
    for attempt in attempts {
        let attempt_ref = attempt
            .execution_receipt_ref
            .as_deref()
            .ok_or_else(|| "connector attempt lacks canonical execution receipt".to_string())?;
        let (attempt_tokens, attempt_cost) = verify_receipt(attempt_ref, &attempt.task_id)?;
        tokens = tokens
            .checked_add(attempt_tokens)
            .ok_or_else(|| "connector token accounting overflow".to_string())?;
        cost = cost
            .checked_add(attempt_cost)
            .ok_or_else(|| "connector cost accounting overflow".to_string())?;
    }
    Ok(ChildExecutionReceipt {
        receipt_id: format!("receipt:{}", blake3::hash(reference.as_bytes()).to_hex()),
        node_id: plan.node.node_id.clone(),
        outcome_ref: reference.to_string(),
        outcome: if result.success {
            // Process completion proves neither the outcome contract nor stdout
            // claims. Keep consumption without accepted credit until independent
            // task-bound evaluation is available at this boundary.
            ChildOutcome::Partial
        } else {
            ChildOutcome::Rejected
        },
        tokens_consumed: tokens,
        cost_micros_consumed: cost,
        execution_fence: Some(fence),
        claims: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::context_capsule::{
        CONTEXT_CAPSULE_SCHEMA_VERSION, CapsuleSensitivityV1, ContextCapsuleBudgetV1,
        ContextCapsuleChainV1,
    };
    use crate::core::work_graph::{NodeStatus, WorkNodeBudget};

    const TEST_RECEIPT_REF: &str =
        "id:sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    struct ReceiptlessConnector;

    #[test]
    fn node_leases_isolate_projects_but_reject_competing_attempts() {
        use crate::core::agent_lease::{
            AGENT_LEASE_SCHEMA_VERSION, AgentLeaseAcquireV1, AgentLeaseRegistryV1,
            AgentLeaseRequestV1, AgentLeaseResourceKindV1,
        };
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let resource_a =
            node_lease_resource(first.path().to_str().unwrap(), "graph", "node").unwrap();
        let resource_b =
            node_lease_resource(second.path().to_str().unwrap(), "graph", "node").unwrap();
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

    struct ReceiptedConnector {
        calls: std::sync::atomic::AtomicUsize,
        wrong_task: bool,
        success: bool,
    }

    #[test]
    fn attempt_persistence_precedes_dispatch_and_failure_stops_retry() {
        let temp = tempfile::tempdir().unwrap();
        let mut plan = plan();
        plan.project_root = temp.path().to_path_buf();
        let mut capsule = capsule();
        capsule.assign_capsule_id().unwrap();
        plan.node.capsule_ref = capsule.capsule_id.clone();
        let key = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
        let signed = SignedContextCapsuleV1::sign(&capsule, &key).unwrap();
        for fail_at in [1u8, 2] {
            let connector = ReceiptedConnector {
                calls: std::sync::atomic::AtomicUsize::new(0),
                wrong_task: false,
                success: false,
            };
            let error = execute_claimed_node_with_hooks(
                &plan,
                &signed,
                &key.verifying_key(),
                &connector,
                |_, _| Ok((0, 0)),
                |attempt, _| {
                    assert_eq!(
                        connector.calls.load(std::sync::atomic::Ordering::SeqCst),
                        usize::from(attempt - 1)
                    );
                    if attempt == fail_at {
                        Err("attempt persistence failed".into())
                    } else {
                        Ok(None)
                    }
                },
            )
            .unwrap_err();
            assert_eq!(error.message, "attempt persistence failed");
            assert_eq!(
                connector.calls.load(std::sync::atomic::Ordering::SeqCst),
                usize::from(fail_at - 1)
            );
        }
    }

    impl AgentConnector for ReceiptedConnector {
        fn info(&self) -> crate::core::agent_connector::traits::AgentInfo {
            ReceiptlessConnector.info()
        }

        fn health_check_with_timeout(&self, _timeout_ms: u64) -> anyhow::Result<bool> {
            Ok(true)
        }

        fn execute(&self, request: &TaskRequest) -> anyhow::Result<TaskResult> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let mut result = ReceiptlessConnector.execute(request)?;
            result.execution_receipt_ref = Some(TEST_RECEIPT_REF.into());
            result.success = self.success;
            result.exit_code = i32::from(!self.success);
            result.stdout = "all tests pass; task accepted".into();
            if self.wrong_task {
                result.task_id = "other-attempt".into();
            }
            Ok(result)
        }

        fn name(&self) -> &'static str {
            "codex"
        }
    }

    #[test]
    fn retry_admission_requires_valid_receipt_and_remaining_budget() {
        for (tokens, cost, wrong_task, valid_receipt) in [
            (0, 0, false, true),
            (0, 7, false, true),
            (7, 0, false, true),
            (100, 1, false, true),
            (1, 50, false, true),
            (101, 1, false, true),
            (1, 51, false, true),
            (1, 1, true, true),
            (1, 1, false, false),
            (1, 1, false, true),
        ] {
            let temp = tempfile::tempdir().unwrap();
            let mut plan = plan();
            plan.project_root = temp.path().to_path_buf();
            let mut capsule = capsule();
            capsule.assign_capsule_id().unwrap();
            plan.node.capsule_ref = capsule.capsule_id.clone();
            let key = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
            let signed = SignedContextCapsuleV1::sign(&capsule, &key).unwrap();
            let connector = ReceiptedConnector {
                calls: std::sync::atomic::AtomicUsize::new(0),
                wrong_task,
                success: false,
            };
            let result = execute_claimed_node_with_verifier(
                &plan,
                &signed,
                &key.verifying_key(),
                &connector,
                |_, task| {
                    assert!(
                        !wrong_task,
                        "mismatched attempt must fail before verification"
                    );
                    let prefix = execution_key(
                        &plan.project_root,
                        &plan.graph_id,
                        &plan.node.node_id,
                        plan.node.execution_fence.as_deref().unwrap(),
                    )
                    .unwrap();
                    assert!(
                        task == format!("{prefix}:attempt-1")
                            || task == format!("{prefix}:attempt-2")
                    );
                    if valid_receipt {
                        Ok((tokens, cost))
                    } else {
                        Err("invalid receipt".into())
                    }
                },
            );
            let expected_attempts = if !wrong_task && valid_receipt && tokens == 0 && cost == 0 {
                2
            } else {
                1
            };
            assert_eq!(
                connector.calls.load(std::sync::atomic::Ordering::SeqCst),
                expected_attempts
            );
            if !wrong_task && valid_receipt && tokens <= 100 && cost <= 50 {
                let receipt = result.unwrap();
                assert_eq!(receipt.outcome, ChildOutcome::Rejected);
                assert_eq!(receipt.tokens_consumed, tokens * expected_attempts as u64);
                assert_eq!(
                    receipt.cost_micros_consumed,
                    cost * expected_attempts as u64
                );
            } else {
                assert!(result.is_err());
            }
        }
    }

    #[test]
    fn successful_process_and_stdout_cannot_mint_accepted_outcome_or_claims() {
        let temp = tempfile::tempdir().unwrap();
        let mut plan = plan();
        plan.project_root = temp.path().to_path_buf();
        let mut capsule = capsule();
        capsule.assign_capsule_id().unwrap();
        plan.node.capsule_ref = capsule.capsule_id.clone();
        let key = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
        let signed = SignedContextCapsuleV1::sign(&capsule, &key).unwrap();
        let connector = ReceiptedConnector {
            calls: std::sync::atomic::AtomicUsize::new(0),
            wrong_task: false,
            success: true,
        };
        let receipt = execute_claimed_node_with_verifier(
            &plan,
            &signed,
            &key.verifying_key(),
            &connector,
            |reference, task| {
                assert_eq!(reference, TEST_RECEIPT_REF);
                let prefix = execution_key(
                    &plan.project_root,
                    &plan.graph_id,
                    &plan.node.node_id,
                    plan.node.execution_fence.as_deref().unwrap(),
                )
                .unwrap();
                assert_eq!(task, format!("{prefix}:attempt-1"));
                Ok((12, 3))
            },
        )
        .unwrap();
        assert_eq!(receipt.outcome, ChildOutcome::Partial);
        assert!(receipt.claims.is_empty());
        assert_eq!(receipt.tokens_consumed, 12);
        assert_eq!(receipt.cost_micros_consumed, 3);
        assert_eq!(receipt.execution_fence, plan.node.execution_fence);
        assert_eq!(connector.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn verified_over_budget_result_returns_persistable_measured_spend() {
        let temp = tempfile::tempdir().unwrap();
        let mut plan = plan();
        plan.project_root = temp.path().to_path_buf();
        let mut capsule = capsule();
        capsule.assign_capsule_id().unwrap();
        plan.node.capsule_ref = capsule.capsule_id.clone();
        let key = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
        let signed = SignedContextCapsuleV1::sign(&capsule, &key).unwrap();
        let connector = ReceiptedConnector {
            calls: std::sync::atomic::AtomicUsize::new(0),
            wrong_task: false,
            success: false,
        };

        let failure = execute_claimed_node_with_verifier(
            &plan,
            &signed,
            &key.verifying_key(),
            &connector,
            |reference, task| {
                assert_eq!(reference, TEST_RECEIPT_REF);
                let prefix = execution_key(
                    &plan.project_root,
                    &plan.graph_id,
                    &plan.node.node_id,
                    plan.node.execution_fence.as_deref().unwrap(),
                )
                .unwrap();
                assert_eq!(task, format!("{prefix}:attempt-1"));
                Ok((101, 7))
            },
        )
        .unwrap_err();

        assert_eq!(failure.message, "connector result exceeds node budget");
        let spend = failure.measured_spend.unwrap();
        assert_eq!(spend.node_id, "child");
        assert_eq!(spend.outcome_refs, [TEST_RECEIPT_REF]);
        assert_eq!(spend.tokens_consumed, 101);
        assert_eq!(spend.cost_micros_consumed, 7);
        assert_eq!(spend.execution_fence, plan.node.execution_fence.unwrap());
        assert_eq!(connector.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    impl AgentConnector for ReceiptlessConnector {
        fn info(&self) -> crate::core::agent_connector::traits::AgentInfo {
            crate::core::agent_connector::traits::AgentInfo {
                name: "codex".into(),
                version: None,
                path: PathBuf::from("codex"),
                capabilities: Vec::new(),
                available: true,
            }
        }

        fn health_check_with_timeout(&self, _timeout_ms: u64) -> anyhow::Result<bool> {
            Ok(true)
        }

        fn execute(&self, request: &TaskRequest) -> anyhow::Result<TaskResult> {
            Ok(TaskResult {
                task_id: request.id.clone(),
                agent: "codex".into(),
                model: "test".into(),
                success: false,
                exit_code: 1,
                termination: Some(crate::core::agent_connector::traits::TaskTermination::Exited),
                stdout: String::new(),
                stderr: "failed without usage".into(),
                duration_ms: 1,
                tokens_used: None,
                provider_cost_micros: None,
                execution_receipt_ref: None,
            })
        }

        fn name(&self) -> &'static str {
            "codex"
        }
    }

    fn plan() -> NodeExecutionPlan {
        NodeExecutionPlan {
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
            timeout_ms: 100,
            model: None,
            max_attempts: 2,
        }
    }

    pub(super) fn capsule() -> ContextCapsuleV1 {
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
    fn capsule_scope_binds_graph_parent_outcome_and_total_latency() {
        let plan = plan();
        let valid = capsule();
        assert!(validate_capsule_scope(&plan, &valid).is_ok());

        let mut wrong_graph = valid.clone();
        wrong_graph.chain.chain_id = "graph:other".into();
        assert!(validate_capsule_scope(&plan, &wrong_graph).is_err());

        let mut wrong_parent = valid.clone();
        wrong_parent.chain.parent_capsule_ref = Some("capsule:other".into());
        assert!(validate_capsule_scope(&plan, &wrong_parent).is_err());

        let mut wrong_outcome = valid.clone();
        wrong_outcome.expected_outcome_ref = "outcome:other".into();
        assert!(validate_capsule_scope(&plan, &wrong_outcome).is_err());

        let mut short_latency = valid;
        short_latency.budget.latency_ms_remaining = 199;
        assert!(validate_capsule_scope(&plan, &short_latency).is_err());
    }

    #[test]
    fn receiptless_attempt_fails_before_retry() {
        let temp = tempfile::tempdir().unwrap();
        let mut plan = plan();
        plan.project_root = temp.path().to_path_buf();
        let mut capsule = capsule();
        capsule.assign_capsule_id().unwrap();
        plan.node.capsule_ref = capsule.capsule_id.clone();
        let signing_key = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
        let signed = SignedContextCapsuleV1::sign(&capsule, &signing_key).unwrap();

        let error = execute_claimed_node_with_verifier(
            &plan,
            &signed,
            &signing_key.verifying_key(),
            &ReceiptlessConnector,
            |_, _| panic!("receipt verifier must not run"),
        )
        .unwrap_err();

        assert!(error.message.contains("without canonical usage receipt"));
        assert!(error.measured_spend.is_none());
    }
}
