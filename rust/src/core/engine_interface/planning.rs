// SPDX-License-Identifier: Apache-2.0

//! Host projection of the frozen scheduler contract into one actual native call.

pub(crate) mod context_binding;

use lean_ctx_protocol::{
    CapabilityBindingV1, ContextBudgetPolicyV1, EnginePolicyAdmissionV1, EnginePolicyDecisionV1,
    ExecutionPlanEstimatesV1, ExecutionPlanV1, PlanId, ProtocolReference, TaskEnvelopeV1,
};

use super::{CAPABILITY_ID, CAPABILITY_VERSION, MAX_TRANSPORT_BUDGET_TOKENS};
use crate::core::{
    canonical::canonical_serialize,
    ocla::{
        adapters::native_context::manifest,
        catalogue::{CatalogueEntry, ProviderEntry, TechnicalCatalogue},
        policy_constraints::PolicyConstraints,
        reference_scheduler::ReferenceScheduler,
        scheduler_service::{ExecutionCandidate, SchedulerService},
    },
};

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

fn native_decision(
    task: &TaskEnvelopeV1,
    admission: &EnginePolicyAdmissionV1,
) -> Result<crate::core::ocla::scheduler_service::SchedulerDecision, String> {
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
    let mut catalogue = TechnicalCatalogue {
        capabilities: vec![CatalogueEntry {
            capability_id: manifest.capability_id.as_str().into(),
            version: manifest.version.clone(),
            manifest: manifest.clone(),
            available,
        }],
        ..Default::default()
    };
    catalogue.providers.push(ProviderEntry {
        provider_id: manifest.provider.clone(),
        // This is the installed non-model execution profile, not an LLM call.
        models_available: vec!["local-native".into()],
        regions: vec![],
    });
    let scheduler = ReferenceScheduler::new();
    let mut candidates = scheduler
        .generate_candidates(task, std::slice::from_ref(manifest), &catalogue)
        .map_err(|error| error.to_string())?;
    for candidate in &mut candidates {
        let plan = &mut candidate.plan;
        plan.context_budget_policy = Some(ContextBudgetPolicyV1::NoTokenLimit {});
        plan.context_budget_tokens = 0;
        plan.expected_cost_micros = 0;
        plan.expected_quality_milli = 0;
        plan.expected_latency_ms = 0;
        plan.estimates = Some(ExecutionPlanEstimatesV1::default());
        plan.policy_decision_ref = Some(admission.policy_ref.as_str().into());
        plan.capability_bindings = vec![CapabilityBindingV1 {
            capability_id: manifest.capability_id.clone(),
            version: manifest.version.clone(),
            manifest_digest: Some(
                super::sha256_digest(&canonical_serialize(manifest))?
                    .as_str()
                    .into(),
            ),
        }];
        // A requested quality/deadline is not an estimate that proves compliance.
        candidate.expected_cost_micros = None;
        candidate.expected_quality_milli = None;
        candidate.expected_latency_ms = None;
    }
    let filtered = scheduler.filter_candidates(
        candidates,
        &PolicyConstraints {
            allowed_providers: Some(vec![manifest.provider.clone()]),
            max_cost_micros: task.cost_budget_micros,
            min_quality_milli: task.quality_requirement_milli.map(u32::from),
            max_latency_ms: task.latency_budget_ms,
            ..Default::default()
        },
    );
    if filtered.len() != 1 {
        return Err("native plan has no single policy-permitted candidate".into());
    }
    let mut fallback_plan = scheduler
        .fallback_plan(task)
        .map_err(|error| error.to_string())?;
    // Keep the unselectable fallback's historical evidence bytes. Native plans
    // admit exactly one real candidate above; this manual template is never run.
    // Changing its projection would invalidate already persisted native decisions.
    fallback_plan.estimates = None;
    let fallback = ExecutionCandidate::new(
        fallback_plan.clone(),
        fallback_plan.capability_ids[0].as_str(),
        fallback_plan.model.clone(),
        fallback_plan.provider.clone(),
        None,
        None,
        None,
    );
    let decision = scheduler.select_plan(&filtered, &fallback);
    let plan = &decision.selected;
    if plan.capability_ids != [manifest.capability_id.clone()]
        || plan.model != "local-native"
        || plan.provider != manifest.provider
    {
        return Err("scheduler recommendation is not the native operation".into());
    }
    Ok(decision)
}

// Noncyclic projection: scheduler recommendation -> evidence link -> final plan ID.
// The persisted decision intentionally cannot contain its own content hash.
fn finalize_native_plan(
    task: &TaskEnvelopeV1,
    decision: &crate::core::ocla::scheduler_service::SchedulerDecision,
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
        let decision: crate::core::ocla::scheduler_service::SchedulerDecision =
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
        let decision: crate::core::ocla::scheduler_service::SchedulerDecision =
            serde_json::from_slice(&bytes).unwrap();
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
