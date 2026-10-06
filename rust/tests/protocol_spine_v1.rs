// SPDX-License-Identifier: Apache-2.0

use std::collections::HashMap;

use lean_ctx::core::context_field::TokenBudget;
use lean_ctx::core::context_kernel::projection::project_context_plan;
use lean_ctx::core::context_kernel::types::{
    ContextPlanV1, DeferredEntry, ExcludedEntry, PlanBudget, PlanEntry, ProviderStat,
};
use lean_ctx::core::task_spine::TaskSpine;
use lean_ctx_protocol::{
    AcceptanceState, AcceptedOutcomeV1, CapabilityManifestV1, ContextPlanProjectionV1,
    DecisionRecordV1, DecisionStageV1, ExecutionPlanV1, ExecutionReceiptV1, TaskEnvelopeV1, TaskId,
};

fn fixture<T: serde::de::DeserializeOwned>(path: &str) -> T {
    let root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/ocla_contract_suite/v1");
    let body = std::fs::read_to_string(root.join(path)).expect("read fixture");
    serde_json::from_str(&body).expect("deserialize fixture")
}

#[test]
fn canonical_spine_fixtures_join_without_stringly_typed_gaps() {
    let task: TaskEnvelopeV1 = fixture("task-envelope/valid_maximal.json");
    let context: ContextPlanProjectionV1 = fixture("context-plan/valid_maximal.json");
    let plan: ExecutionPlanV1 = fixture("execution-plan/valid_maximal.json");
    let receipt: ExecutionReceiptV1 = fixture("execution-receipt/valid_maximal.json");
    let outcome: AcceptedOutcomeV1 = fixture("accepted-outcome/valid_maximal.json");
    let decision: DecisionRecordV1 = fixture("decision-record/valid_maximal.json");

    assert_eq!(context.task_id, task.task_id);
    assert_eq!(plan.task_id, task.task_id);
    assert_eq!(
        plan.context_plan_id.as_ref(),
        Some(&context.context_plan_id)
    );
    assert_eq!(plan.executor_agent_id.as_ref(), Some(&task.agent_id));
    assert_eq!(receipt.task_id, task.task_id);
    assert_eq!(receipt.plan_id, plan.plan_id);
    assert_eq!(
        receipt.context_plan_id.as_ref(),
        Some(&context.context_plan_id)
    );
    assert_eq!(receipt.executor_agent_id.as_ref(), Some(&task.agent_id));
    assert_eq!(receipt.capability_bindings, plan.capability_bindings);
    assert_eq!(outcome.task_id, task.task_id);
    assert_eq!(outcome.plan_id.as_ref(), Some(&plan.plan_id));
    assert_eq!(outcome.receipt_id.as_ref(), Some(&receipt.receipt_id));
    assert_eq!(decision.task_id, task.task_id);
    assert_eq!(decision.plan_id.as_ref(), Some(&plan.plan_id));
}

#[test]
fn task_children_keep_trace_and_receive_distinct_task_ids() {
    let session = format!("protocol-spine-test-{}", uuid::Uuid::new_v4());
    let root = TaskSpine::create_envelope("root", &session, "agent-test");
    let child = TaskSpine::create_envelope("child", &session, "agent-test");
    assert_ne!(root.task_id, child.task_id);
    assert_eq!(root.trace_id, child.trace_id);
    assert_eq!(child.parent_task_id.as_ref(), Some(&root.task_id));
    child.validate_child_of(&root).expect("valid child lineage");
}

#[test]
fn lifecycle_binds_host_project_and_isolates_cross_project_lineage() {
    use lean_ctx::core::execution_lifecycle::{
        ExecutionLifecycle, ProductEntitlements, RuntimeContext, ToolRequest, ToolSurface,
    };

    let lifecycle = ExecutionLifecycle::default();
    let session = format!("project-lineage-{}", uuid::Uuid::new_v4());
    let admit = |project: Option<&str>, key: &str| {
        lifecycle.begin(
            ToolRequest {
                tool_name: "ctx_read".to_owned(),
                query: None,
                session_id: session.clone(),
                agent_id: "agent-test".to_owned(),
                surface: ToolSurface::Mcp,
                idempotency_key: Some(key.to_owned()),
            },
            RuntimeContext {
                client_name: None,
                project_root: project.map(str::to_owned),
            },
            ProductEntitlements::default(),
        )
    };

    let first = admit(Some("/workspace/first"), "root").envelope;
    let other = admit(Some("/workspace/other"), "root").envelope;
    assert_eq!(first.project_id.as_str(), "/workspace/first");
    assert_eq!(other.project_id.as_str(), "/workspace/other");
    assert_ne!(first.task_id, other.task_id);
    assert_ne!(first.trace_id, other.trace_id);
    assert!(first.parent_task_id.is_none());
    assert!(other.parent_task_id.is_none());
    assert_eq!(admit(Some("/workspace/first"), "root").envelope, first);
    assert_eq!(admit(Some("/workspace/other"), "root").envelope, other);
    let child = admit(Some("/workspace/first"), "child").envelope;
    child.validate_child_of(&first).expect("same-project child");
    assert!(child.validate_child_of(&other).is_err());

    let unbound = admit(None, "root").envelope;
    assert_eq!(unbound.project_id.as_str(), "unknown-project");
    assert!(unbound.parent_task_id.is_none());
    assert_ne!(unbound.trace_id, first.trace_id);
    let invalid = admit(Some(""), "invalid").envelope;
    assert_eq!(invalid.project_id.as_str(), "unknown-project");
    assert_ne!(invalid.trace_id, other.trace_id);
}

#[test]
fn kernel_projection_is_deterministic_and_has_one_semantic_owner() {
    fn plan(reverse: bool) -> ContextPlanV1 {
        let mut selected = vec![
            PlanEntry {
                object_id: "source-b".to_owned(),
                provider: "provider-b".to_owned(),
                view: "full".to_owned(),
                tokens: 20,
                phi: 1.0,
                reason: "relevant".to_owned(),
            },
            PlanEntry {
                object_id: "source-a".to_owned(),
                provider: "provider-a".to_owned(),
                view: "full".to_owned(),
                tokens: 10,
                phi: 1.0,
                reason: "required".to_owned(),
            },
        ];
        if reverse {
            selected.reverse();
        }
        let stats = [
            (
                "provider-b".to_owned(),
                ProviderStat {
                    candidates_offered: 1,
                    candidates_selected: 1,
                    tokens_used: 20,
                },
            ),
            (
                "provider-a".to_owned(),
                ProviderStat {
                    candidates_offered: 1,
                    candidates_selected: 1,
                    tokens_used: 10,
                },
            ),
        ];
        // Legacy plans use the compatibility factory; only the kernel binds origins.
        let mut plan = ContextPlanV1::empty(
            "test",
            TokenBudget {
                total: 100,
                used: 0,
            },
        );
        plan.plan_id = "context-plan-deterministic".to_owned();
        plan.budget = PlanBudget {
            total_tokens: 100,
            used_tokens: 30,
            remaining_tokens: 70,
        };
        plan.selected = selected;
        plan.excluded = vec![ExcludedEntry {
            object_id: "source-c".to_owned(),
            provider: "provider-a".to_owned(),
            reason: "budget exceeded".to_owned(),
        }];
        plan.deferred = vec![DeferredEntry {
            object_id: "source-d".to_owned(),
            provider: "provider-b".to_owned(),
            reason: "later".to_owned(),
        }];
        plan.provider_stats = if reverse {
            stats.into_iter().rev().collect::<HashMap<_, _>>()
        } else {
            stats.into_iter().collect::<HashMap<_, _>>()
        };
        plan
    }

    let task_id = TaskId::new("task-deterministic").expect("task id");
    let left = project_context_plan(task_id.clone(), &plan(false)).expect("project left");
    let right = project_context_plan(task_id, &plan(true)).expect("project right");
    assert_eq!(left, right);
    assert_eq!(
        serde_json::to_vec(&left).expect("serialize left"),
        serde_json::to_vec(&right).expect("serialize right")
    );
}

#[test]
fn zero_exit_extension_does_not_promote_unknown_outcome() {
    let mut value: serde_json::Value = serde_json::from_str(include_str!(
        "../../tests/ocla_contract_suite/v1/accepted-outcome/valid_minimal.json"
    ))
    .expect("parse fixture");
    value["future_exit_code"] = serde_json::json!(0);
    let outcome: AcceptedOutcomeV1 = serde_json::from_value(value).expect("decode outcome");
    assert_eq!(outcome.accepted, AcceptanceState::Unknown);
    assert_eq!(outcome.extensions["future_exit_code"], 0);
}

#[test]
fn admission_can_precede_plan_but_later_decisions_cannot() {
    let mut value: serde_json::Value = serde_json::from_str(include_str!(
        "../../tests/ocla_contract_suite/v1/decision-record/valid_minimal.json"
    ))
    .expect("parse fixture");
    value.as_object_mut().expect("object").remove("plan_id");
    value["decision_stage"] = serde_json::json!("admission");
    let mut decision: DecisionRecordV1 = serde_json::from_value(value).expect("decode decision");
    decision.validate().expect("admission can precede plan");
    decision.decision_stage = DecisionStageV1::Planning;
    assert!(decision.validate().is_err());
}

#[test]
fn explicit_receipt_observations_cannot_contradict_legacy_fields() {
    let mut receipt: ExecutionReceiptV1 = fixture("execution-receipt/valid_maximal.json");
    receipt.observations.latency_ms = Some(receipt.latency_ms + 1);
    assert!(receipt.validate().is_err());
}

#[test]
fn context_budget_and_remote_data_policy_fail_closed() {
    let mut context: ContextPlanProjectionV1 = fixture("context-plan/valid_maximal.json");
    context.budget_tokens = 1;
    assert!(context.validate().is_err());

    let mut capability: CapabilityManifestV1 = fixture("capability-manifest/valid_maximal.json");
    capability.remote = true;
    capability.data_movement = lean_ctx_protocol::DataMovement::Remote;
    capability.supported_classifications = vec![lean_ctx_protocol::DataClassification::Restricted];
    assert!(capability.validate().is_err());
}

#[test]
fn kernel_projection_preserves_content_digest() {
    let mut plan = ContextPlanV1::empty("digest", TokenBudget { total: 10, used: 0 });
    plan.plan_id = "context-plan-digest".to_owned();
    plan.budget = PlanBudget {
        total_tokens: 10,
        used_tokens: 2,
        remaining_tokens: 8,
    };
    plan.selected = vec![PlanEntry {
        object_id: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            .to_owned(),
        provider: "provider".to_owned(),
        view: "full".to_owned(),
        tokens: 2,
        phi: 1.0,
        reason: "required".to_owned(),
    }];
    let task_id = TaskId::new("task-digest").expect("task id");
    let projection = project_context_plan(task_id, &plan).expect("project digest");
    assert_eq!(
        projection.selections[0].sha256_digest.as_deref(),
        Some("sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
    );
    assert!(projection.projection_digest.is_some());
    assert_eq!(
        projection.projection_digest,
        Some(
            projection
                .compute_projection_digest()
                .expect("compute projection digest")
        )
    );

    let mut tampered = projection.clone();
    tampered.budget_tokens += 1;
    assert!(tampered.validate().is_err());

    let old: ContextPlanProjectionV1 = fixture("context-plan/valid_minimal.json");
    assert!(old.projection_digest.is_none());
    old.validate().expect("old V1 projection remains readable");
}

#[test]
fn context_plan_extension_cannot_shadow_projection_digest() {
    let mut context: ContextPlanProjectionV1 = fixture("context-plan/valid_minimal.json");
    context
        .extensions
        .insert("projection_digest", serde_json::json!("future"))
        .expect("extension value is bounded");
    assert!(context.validate().is_err());
}
