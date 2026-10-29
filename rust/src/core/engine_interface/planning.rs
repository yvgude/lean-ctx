// SPDX-License-Identifier: Apache-2.0

//! Host projection of the frozen scheduler contract into one actual native call.

pub(crate) mod context_binding;

use lean_ctx_protocol::{
    CapabilityBindingV1, ContextBudgetPolicyV1, EnginePolicyAdmissionV1, EnginePolicyDecisionV1,
    ExecutionPlanEstimatesV1, ExecutionPlanV1, PlanId, ProtocolReference, TaskEnvelopeV1,
};

use super::{CAPABILITY_ID, CAPABILITY_VERSION, MAX_TRANSPORT_BUDGET_TOKENS};
use crate::core::{canonical::canonical_serialize, ocla::adapters::native_context::manifest};

/// Select only the installed native operation; a recommendation is not admission.
pub(crate) fn native_plan(
    task: &TaskEnvelopeV1,
    admission: &EnginePolicyAdmissionV1,
) -> Result<ExecutionPlanV1, String> {
    let decision = native_decision(task, admission)?;
    let decision_ref = persist_evidence(&decision)?;
    let context = context_binding::NativeContextBinding::capture(task)?;
    finalize_native_plan(
        task,
        &decision,
        &format!("artifact://execution/evidence/{}", decision_ref.hex()),
        context.as_ref(),
    )
}

/// Persisted native scheduling evidence (`artifact://execution/evidence/<digest>`).
/// Names, order and values are frozen: validation recomputes the decision and
/// compares it with the one earlier releases stored.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NativeDecision {
    pub selected: ExecutionPlanV1,
    pub fallback: ExecutionPlanV1,
    pub decision_ref: String,
    pub rationale_code: String,
    pub confidence_milli: u32,
    pub candidates_evaluated: u32,
    pub candidates_excluded: u32,
}

const NATIVE_MODEL: &str = "local-native";
const FALLBACK_CAPABILITY: &str = "capability://leanctx/passthrough";
const FALLBACK_MODEL: &str = "manual";
const FALLBACK_PROVIDER: &str = "leanctx";

/// Select only the installed native operation; a recommendation is not admission.
/// The decision keeps the projection of the former reference scheduler for its
/// single local candidate: it publishes no cost, quality or latency estimate,
/// so a task that bounds any of them has no permitted candidate.
fn native_decision(
    task: &TaskEnvelopeV1,
    admission: &EnginePolicyAdmissionV1,
) -> Result<NativeDecision, String> {
    task.validate().map_err(|error| error.to_string())?;
    if admission.decision != EnginePolicyDecisionV1::Admitted {
        return Err("native plan requires actual policy admission".into());
    }
    let manifest = manifest();
    if !manifest.local
        || manifest.remote
        || task
            .data_classification
            .as_ref()
            .is_some_and(|class| !manifest.supported_classifications.contains(class))
        || task.region_policy_ref.is_some()
        || task.model_policy_ref.is_some()
    {
        return Err("native plan cannot satisfy unresolved task policy".into());
    }
    let adapter = crate::core::ocla::registry::OclaRegistry::global()
        .adapters
        .lookup(CAPABILITY_ID, CAPABILITY_VERSION)
        .ok_or("native runtime capability is not registered")?;
    if adapter.manifest() != manifest {
        return Err("registered native runtime contract differs from pinned manifest".into());
    }
    let available = adapter.health_check().map_err(|error| error.to_string())?;
    manifest
        .validate()
        .map_err(|error| format!("invalid capability manifest: {error}"))?;
    if !available
        || task.cost_budget_micros.is_some()
        || task.quality_requirement_milli.is_some()
        || task.latency_budget_ms.is_some()
    {
        return Err("native plan has no single policy-permitted candidate".into());
    }
    let mut selected = reference_plan(
        task,
        manifest.capability_id.as_str(),
        NATIVE_MODEL,
        &manifest.provider,
        None,
    )?;
    selected.context_budget_policy = Some(ContextBudgetPolicyV1::NoTokenLimit {});
    selected.context_budget_tokens = 0;
    selected.expected_cost_micros = 0;
    selected.expected_quality_milli = 0;
    selected.expected_latency_ms = 0;
    selected.estimates = Some(ExecutionPlanEstimatesV1::default());
    selected.policy_decision_ref = Some(admission.policy_ref.as_str().into());
    selected.capability_bindings = vec![CapabilityBindingV1 {
        capability_id: manifest.capability_id.clone(),
        version: manifest.version.clone(),
        manifest_digest: Some(
            super::sha256_digest(&canonical_serialize(manifest))?
                .as_str()
                .into(),
        ),
    }];
    // The manual fallback is a template that is never run; its bytes stay as
    // persisted decisions carry them.
    let mut fallback = reference_plan(
        task,
        FALLBACK_CAPABILITY,
        FALLBACK_MODEL,
        FALLBACK_PROVIDER,
        Some("fallback"),
    )?;
    fallback.estimates = None;
    let seed = format!(
        "reference-v1|permit:{}:{NATIVE_MODEL}:{}|fallback:{FALLBACK_CAPABILITY}:{FALLBACK_MODEL}:{FALLBACK_PROVIDER}",
        manifest.capability_id.as_str(),
        manifest.provider,
    );
    Ok(NativeDecision {
        selected,
        fallback,
        decision_ref: format!(
            "decision:reference-v1:{}",
            blake3::hash(seed.as_bytes()).to_hex()
        ),
        rationale_code: "reference_score_cost_latency_quality".into(),
        confidence_milli: 1000,
        candidates_evaluated: 1,
        candidates_excluded: 0,
    })
}

/// Plan template with the identity scheme of persisted native decisions.
fn reference_plan(
    task: &TaskEnvelopeV1,
    capability_id: &str,
    model: &str,
    provider: &str,
    fallback_marker: Option<&str>,
) -> Result<ExecutionPlanV1, String> {
    let seed = format!(
        "reference-v1:{}:{capability_id}:{model}:{provider}",
        task.task_id.as_str()
    );
    let plan = ExecutionPlanV1 {
        schema_version: 1,
        plan_id: PlanId::try_from(format!("plan:{}", blake3::hash(seed.as_bytes()).to_hex()))
            .map_err(|error| format!("invalid deterministic plan id: {error}"))?,
        task_id: task.task_id.clone(),
        context_budget_tokens: 0,
        context_budget_policy: None,
        context_strategy: lean_ctx_protocol::ContextStrategy::Balanced,
        knowledge_refs: Vec::new(),
        capability_ids: vec![
            lean_ctx_protocol::CapabilityId::try_from(capability_id.to_owned())
                .map_err(|error| format!("invalid capability: {error}"))?,
        ],
        model: model.to_owned(),
        provider: provider.to_owned(),
        reasoning_allocation_milli: 0,
        max_retries: 0,
        fallback_refs: fallback_marker.map(str::to_owned).into_iter().collect(),
        stop_condition: lean_ctx_protocol::StopCondition::OnCompletion,
        expected_cost_micros: 0,
        estimates: Some(ExecutionPlanEstimatesV1 {
            cost_micros: fallback_marker.map(|_| 0_u64),
            quality_milli: None,
            latency_ms: None,
        }),
        expected_quality_milli: 0,
        expected_latency_ms: 0,
        policy_decision_ref: None,
        scheduler_decision_ref: Some("scheduler:reference-v1".to_owned()),
        executor_agent_id: Some(task.agent_id.clone()),
        context_plan_id: None,
        capability_bindings: Vec::new(),
        extensions: Default::default(),
    };
    plan.validate()
        .map_err(|error| format!("invalid deterministic execution plan: {error}"))?;
    Ok(plan)
}

// Noncyclic projection: scheduler recommendation -> evidence link -> final plan ID.
// The persisted decision intentionally cannot contain its own content hash.
fn finalize_native_plan(
    task: &TaskEnvelopeV1,
    decision: &NativeDecision,
    reference: &str,
    context: Option<&context_binding::NativeContextBinding>,
) -> Result<ExecutionPlanV1, String> {
    let mut plan = decision.selected.clone();
    plan.scheduler_decision_ref = Some(reference.into());
    if let Some(context) = context {
        context.validate_task(task)?;
        context.apply(&mut plan)?;
    }
    let identity = super::sha256_digest(&canonical_serialize(&("native-plan-v1", task, &plan)))?;
    plan.plan_id =
        PlanId::new(format!("plan:{}", identity.hex())).map_err(|error| error.to_string())?;
    plan.validate().map_err(|error| error.to_string())?;
    Ok(plan)
}

pub(crate) fn validate_native_plan(
    task: &TaskEnvelopeV1,
    plan: &ExecutionPlanV1,
    admission: &EnginePolicyAdmissionV1,
) -> Result<(), String> {
    task.validate().map_err(|error| error.to_string())?;
    plan.validate().map_err(|error| error.to_string())?;
    if task.task_id != plan.task_id
        || plan
            .executor_agent_id
            .as_ref()
            .is_some_and(|agent| agent != &task.agent_id)
        || plan.capability_ids.len() != 1
        || plan.capability_ids[0].as_str() != CAPABILITY_ID
        || plan.model != "local-native"
        || ![manifest().provider.as_str(), "local-native"].contains(&plan.provider.as_str())
        || plan.capability_bindings.len() != 1
        || plan
            .capability_bindings
            .iter()
            .any(|binding| binding.version != CAPABILITY_VERSION)
        || plan.policy_decision_ref.as_deref() != Some(admission.policy_ref.as_str())
        || admission.decision != EnginePolicyDecisionV1::Admitted
        || plan
            .context_token_limit()
            .is_some_and(|limit| limit == 0 || limit > MAX_TRANSPORT_BUDGET_TOKENS)
    {
        return Err("native execution plan does not bind admitted task and operation".into());
    }
    if plan.provider == manifest().provider {
        let reference = plan
            .scheduler_decision_ref
            .as_deref()
            .ok_or("native scheduler evidence is required")?;
        let digest = reference
            .strip_prefix("artifact://execution/evidence/")
            .ok_or("native scheduler evidence reference is invalid")?;
        let bytes = super::artifact_store::read_content("execution/evidence", digest, "json")?;
        let decision: NativeDecision =
            serde_json::from_slice(&bytes).map_err(|_| "native scheduler evidence is invalid")?;
        let context = context_binding::NativeContextBinding::from_plan(plan)?;
        if decision != native_decision(task, admission)?
            || &finalize_native_plan(task, &decision, reference, context.as_ref())? != plan
        {
            return Err("native plan differs from its verified scheduler recommendation".into());
        }
    } else {
        // Explicit caller plans are declarations, not native scheduler evidence.
        // Context-plan references are checked by the owning lifecycle; the limited
        // standalone host rejects them before this shared execution boundary.
        if plan.scheduler_decision_ref.is_some()
            || !plan.knowledge_refs.is_empty()
            || plan.capability_bindings[0]
                .manifest_digest
                .as_deref()
                .is_some_and(|digest| {
                    !super::sha256_digest(&canonical_serialize(manifest()))
                        .is_ok_and(|actual| actual.as_str() == digest)
                })
        {
            return Err("host plan carries unsupported native planning evidence".into());
        }
    }
    Ok(())
}

pub(crate) fn binding_refs(
    task: &TaskEnvelopeV1,
    plan: &ExecutionPlanV1,
) -> Result<[ProtocolReference; 2], String> {
    let task_ref = super::sha256_digest(&canonical_serialize(task))?;
    let plan_ref = super::sha256_digest(&canonical_serialize(plan))?;
    Ok([
        ProtocolReference::new(format!("task:{}", task_ref.as_str()))
            .map_err(|error| error.to_string())?,
        ProtocolReference::new(format!("plan:{}", plan_ref.as_str()))
            .map_err(|error| error.to_string())?,
    ])
}

pub(crate) fn persist_evidence<T: serde::Serialize>(
    value: &T,
) -> Result<lean_ctx_protocol::Sha256Digest, String> {
    let bytes = canonical_serialize(value);
    let digest = super::sha256_digest(&bytes)?;
    drop(super::persist_engine_artifact_content(
        "execution/evidence",
        digest.hex(),
        "json",
        &bytes,
    )?);
    Ok(digest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::engine_interface::{NativeContextEngine, verified_output_view};

    fn task() -> TaskEnvelopeV1 {
        serde_json::from_value(serde_json::json!({
            "schema_version":1, "task_id":"native-plan-task", "trace_id":"native-plan-trace",
            "project_id":"native-plan-project", "session_id":"native-plan-session",
            "agent_id":"native-plan-agent", "complexity":"medium",
            "created_at":"2026-08-23T11:59:00Z"
        }))
        .unwrap()
    }

    fn admission() -> EnginePolicyAdmissionV1 {
        EnginePolicyAdmissionV1 {
            policy_ref: ProtocolReference::new("policy:native-plan-test").unwrap(),
            decision: EnginePolicyDecisionV1::Admitted,
        }
    }

    #[test]
    fn native_plan_rejects_missing_descriptor_only_and_unhealthy_runtime_bindings() {
        use crate::core::ocla::{
            adapters::{AdapterRegistry, NativeContextAdapter},
            registry::{OclaRegistry, with_test_registry},
        };
        let missing_root = tempfile::tempdir().unwrap();
        for mode in 0..3 {
            let mut registry = OclaRegistry::with_builtins();
            registry.adapters = AdapterRegistry::new();
            match mode {
                0 => {}
                1 => registry
                    .adapters
                    .register_descriptor(manifest().clone())
                    .unwrap(),
                _ => registry
                    .adapters
                    .register(NativeContextAdapter::with_root(
                        missing_root.path().join("not-created"),
                    ))
                    .unwrap(),
            }
            let _registry = with_test_registry(registry);
            assert!(
                native_decision(&task(), &admission()).is_err(),
                "mode={mode}"
            );
        }
    }

    /// Stored native plans are revalidated by recomputing this decision: its
    /// bytes must never change (they were proven equal to the former
    /// reference scheduler's before it was removed, 2026-10-16).
    #[test]
    fn native_decision_bytes_are_frozen() {
        let decision = native_decision(&task(), &admission()).unwrap();
        assert_eq!(
            super::super::sha256_digest(&canonical_serialize(&decision))
                .unwrap()
                .as_str(),
            "sha256:b320c7a5da703c1bd531058499c1816a63397c93ec6758c77edd6c55aac55847"
        );
    }

    #[test]
    fn native_plan_is_deterministic_manifest_bound_and_honest_about_unknowns() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let task = task();
        let plan = native_plan(&task, &admission()).unwrap();
        assert_eq!(plan, native_plan(&task, &admission()).unwrap());
        assert_eq!(plan.context_token_limit(), None);
        assert_eq!(plan.estimates, Some(ExecutionPlanEstimatesV1::default()));
        assert_eq!(plan.provider, manifest().provider);
        assert_eq!(plan.capability_bindings[0].version, CAPABILITY_VERSION);
        assert!(plan.capability_bindings[0].manifest_digest.is_some());
        assert_eq!(
            plan.policy_decision_ref.as_deref(),
            Some(admission().policy_ref.as_str())
        );
        let decision_ref = plan.scheduler_decision_ref.as_ref().unwrap();
        let digest = decision_ref
            .strip_prefix("artifact://execution/evidence/")
            .unwrap();
        let bytes = crate::core::engine_interface::artifact_store::read_content(
            "execution/evidence",
            digest,
            "json",
        )
        .unwrap();
        let decision: NativeDecision = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            decision.selected.capability_bindings,
            plan.capability_bindings
        );
        assert_eq!(decision.selected.estimates, plan.estimates);
        assert_eq!(
            decision.selected.policy_decision_ref,
            plan.policy_decision_ref
        );
    }

    #[test]
    fn native_plan_rejects_unproved_constraints_and_rejected_admission() {
        let _data = crate::core::data_dir::isolated_data_dir();
        for kind in 0..5 {
            let mut task = task();
            match kind {
                0 => task.quality_requirement_milli = Some(500),
                1 => task.cost_budget_micros = Some(100),
                2 => task.latency_budget_ms = Some(100),
                3 => task.region_policy_ref = Some("policy:region".into()),
                _ => task.model_policy_ref = Some("policy:model".into()),
            }
            assert!(native_plan(&task, &admission()).is_err());
        }
        let mut rejected = admission();
        rejected.decision = EnginePolicyDecisionV1::Rejected;
        assert!(native_plan(&task(), &rejected).is_err());
        assert!(
            !crate::core::data_dir::lean_ctx_data_dir()
                .unwrap()
                .join("engine-interface/v1/outputs")
                .exists()
        );
    }

    #[test]
    fn historical_native_decision_with_legacy_fallback_still_revalidates() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let task = task();
        let admission = admission();
        let mut historical = native_decision(&task, &admission).unwrap();
        // The historical fallback predates explicit nullable estimate projections.
        historical.fallback.estimates = None;
        let digest = persist_evidence(&historical).unwrap();
        let reference = format!("artifact://execution/evidence/{}", digest.hex());
        let plan = finalize_native_plan(&task, &historical, &reference, None).unwrap();
        assert_eq!(plan.estimates, Some(ExecutionPlanEstimatesV1::default()));
        validate_native_plan(&task, &plan, &admission).unwrap();
    }

    #[test]
    fn actual_invocation_binds_exact_documents_and_preserves_native_unlimited_output() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let root = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(root.path()).unwrap();
        let path = root.join("snapshot.rs");
        let source = "fn bound_snapshot() {}\n".repeat(40);
        let engine = NativeContextEngine::with_root(&root).unwrap();
        let task = task();
        let plan = native_plan(&task, &admission()).unwrap();
        let (invocation, observation) = engine
            .execute_ctx_read_rooted_snapshot_with_plan(
                path.to_str().unwrap(),
                &source,
                admission(),
                &task,
                &plan,
            )
            .unwrap();
        assert!(
            binding_refs(&task, &plan)
                .unwrap()
                .iter()
                .all(|r| invocation.source_refs.contains(r))
        );
        assert_eq!(invocation.source_refs, observation.source_lineage);
        let view = verified_output_view(&invocation, &observation).unwrap();
        let (legacy_invocation, legacy_observation) = engine
            .execute_ctx_read_rooted_snapshot(path.to_str().unwrap(), &source, admission())
            .unwrap();
        assert_ne!(invocation.invocation_id, legacy_invocation.invocation_id);
        assert_eq!(
            view.text,
            verified_output_view(&legacy_invocation, &legacy_observation)
                .unwrap()
                .text
        );
        let mut changed = task.clone();
        changed.intent = Some("same task id, different intent".into());
        assert!(validate_native_plan(&changed, &plan, &admission()).is_err());
        let changed_plan = native_plan(&changed, &admission()).unwrap();
        let (changed_invocation, _) = engine
            .execute_ctx_read_rooted_snapshot_with_plan(
                path.to_str().unwrap(),
                &source,
                admission(),
                &changed,
                &changed_plan,
            )
            .unwrap();
        assert_ne!(changed_invocation.invocation_id, invocation.invocation_id);
        let mut bad_plan = plan.clone();
        bad_plan.task_id = lean_ctx_protocol::TaskId::new("wrong-task").unwrap();
        assert!(
            engine
                .execute_ctx_read_rooted_snapshot_with_plan(
                    path.to_str().unwrap(),
                    &source,
                    admission(),
                    &task,
                    &bad_plan,
                )
                .is_err()
        );
        bad_plan = plan;
        bad_plan.context_budget_policy = None;
        assert!(validate_native_plan(&task, &bad_plan, &admission()).is_err());
    }

    #[test]
    fn generated_plan_rejects_mutation_missing_proof_and_corrupted_scheduler_artifact() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let task = task();
        let plan = native_plan(&task, &admission()).unwrap();
        for kind in 0..7 {
            let mut changed = plan.clone();
            match kind {
                0 => changed.scheduler_decision_ref = None,
                1 => changed.capability_bindings.clear(),
                2 => changed.capability_bindings[0].manifest_digest = None,
                3 => changed.knowledge_refs.push("knowledge:foreign".into()),
                4 => changed.max_retries = 1,
                5 => changed.stop_condition = lean_ctx_protocol::StopCondition::OnAcceptance,
                _ => {
                    changed.context_budget_tokens = 50;
                    changed.context_budget_policy = None;
                }
            }
            assert!(validate_native_plan(&task, &changed, &admission()).is_err());
        }
        let digest = plan
            .scheduler_decision_ref
            .as_ref()
            .unwrap()
            .strip_prefix("artifact://execution/evidence/")
            .unwrap();
        let path = crate::core::data_dir::lean_ctx_data_dir()
            .unwrap()
            .join("execution/evidence")
            .join(format!("{digest}.json"));
        std::fs::write(path, b"{}").unwrap();
        assert!(validate_native_plan(&task, &plan, &admission()).is_err());
    }
}
