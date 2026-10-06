// SPDX-License-Identifier: Apache-2.0

//! Execute an admitted source snapshot through the existing native and receipt authorities.

use std::path::Path;

use lean_ctx_protocol::{
    EngineContextSourceMaterializationRequestV1, EngineContextSourcePlanResponseV1,
    EngineInvocationIdV1, EngineInvocationV1, EngineObservationV1, EnginePolicyAdmissionV1,
    EnginePolicyDecisionV1, ExecutionPlanV1, ProtocolReference, TaskEnvelopeV1,
};

use super::{
    ENGINE_TRANSPORT_POLICY_REF, EngineTransportView, NativeContextEngine,
    NativeContextEngineRequest, bind_transport_root, context_plan, planning, sha256_digest,
    verified_output_view,
};
use crate::core::{
    canonical::canonical_serialize,
    execution_ledger::{PublishedCanonicalReceipt, host::HostReceiptAuthority},
};

pub(crate) struct SourceExecutionResult {
    pub(crate) source_plan: EngineContextSourcePlanResponseV1,
    pub(crate) execution_plan: ExecutionPlanV1,
    pub(crate) invocation: EngineInvocationV1,
    pub(crate) observation: EngineObservationV1,
    pub(crate) view: EngineTransportView,
    pub(crate) receipt: PublishedCanonicalReceipt,
}

/// A caller declaration is not a scheduler recommendation or source authorization.
/// The immutable kernel decision supplies the context binding in this process.
pub(crate) fn execute(
    root: &Path,
    task: &TaskEnvelopeV1,
    declared_plan: &ExecutionPlanV1,
    request: &EngineContextSourceMaterializationRequestV1,
    authority: &HostReceiptAuthority,
) -> Result<SourceExecutionResult, &'static str> {
    authority.require_context_decision_signing()?;
    let root = bind_transport_root(root).map_err(|_| "unsafe_root")?;
    let admission = EnginePolicyAdmissionV1 {
        policy_ref: reference(ENGINE_TRANSPORT_POLICY_REF)?,
        decision: EnginePolicyDecisionV1::Admitted,
    };
    planning::validate_native_plan(task, declared_plan, &admission)
        .map_err(|_| "invalid_host_plan")?;
    if declared_plan.provider != "local-native"
        || declared_plan.context_plan_id.is_some()
        || task.region_policy_ref.is_some()
        || task.model_policy_ref.is_some()
        || request.source_plan.planning.task_id != task.task_id
        || declared_plan.context_token_limit().is_none()
    {
        return Err("invalid_host_source_plan");
    }
    let (snapshot, decision) = context_plan::materialize_sources_with_decision(&root, request)?;
    if decision.context_projection() != &snapshot.plan.result.plan
        || declared_plan.context_token_limit() != Some(snapshot.plan.result.plan.budget_tokens)
    {
        return Err("host_source_decision_mismatch");
    }
    let mut plan = declared_plan.clone();
    plan.context_plan_id = Some(snapshot.plan.result.plan.context_plan_id.clone());
    let plan = decision
        .bind_execution_plan(plan)
        .map_err(|_| "host_source_decision_mismatch")?;
    planning::validate_native_plan(task, &plan, &admission)
        .map_err(|_| "invalid_host_source_plan")?;
    let engine = NativeContextEngine::with_root(&root).map_err(|_| "unsafe_root")?;
    let source_plan_digest = sha256_digest(&canonical_serialize(&snapshot.plan))
        .map_err(|_| "host_source_evidence_unavailable")?;
    let input_ref = reference(&format!(
        "input:source-materialization-sha256:{}",
        snapshot.materialized_digest.hex()
    ))?;
    let mut source_refs = vec![
        input_ref.clone(),
        reference(&format!(
            "artifact://execution/evidence/{}",
            source_plan_digest.hex()
        ))?,
    ];
    // Keep invocation lineage bounded: the persisted descriptor-only source
    // plan above binds every selected source without one ref per source.
    source_refs
        .extend(planning::binding_refs(task, &plan).map_err(|_| "invalid_host_source_plan")?);
    let identity = sha256_digest(&canonical_serialize(&(
        "source-execution-v1",
        &plan,
        &source_refs,
        &snapshot.materialized_digest,
    )))
    .map_err(|_| "host_source_evidence_unavailable")?;
    let native = NativeContextEngineRequest {
        invocation_id: EngineInvocationIdV1::new(format!(
            "engine-invocation-{}",
            &identity.hex()[..32]
        ))
        .map_err(|_| "invalid_host_source_plan")?,
        input_ref,
        input_digest: snapshot.materialized_digest.clone(),
        source_refs,
        policy_admission: admission,
        // No invented filesystem identity: the adapter consumes the bound bytes directly.
        paths: vec![],
        mode: "aggressive".into(),
        budget_tokens: plan.context_token_limit(),
        timeout_ms: task.latency_budget_ms.unwrap_or(30_000).min(30_000),
    };
    // Validate all record bounds before durable intent or capability execution.
    let expected_invocation = engine
        .invocation_for(&native)
        .map_err(|_| "invalid_host_source_plan")?;
    if let Some(recorded) =
        authority.replay_source_context(task, &plan, &expected_invocation, &decision)?
    {
        let view = verified_output_view(&recorded.invocation, &recorded.observation)
            .map_err(|_| "host_engine_evidence_unavailable")?;
        // A stored signature is not a current source/policy authorization lease.
        if context_plan::materialize(&root, request)? != snapshot {
            return Err("source_plan_changed");
        }
        return Ok(SourceExecutionResult {
            source_plan: snapshot.plan,
            execution_plan: plan,
            invocation: recorded.invocation,
            observation: recorded.observation,
            view,
            receipt: recorded.published_receipt,
        });
    }
    let attempt = authority.begin_with_context(task, &plan, Some(&decision))?;
    planning::persist_evidence(task).map_err(|_| "host_source_evidence_unavailable")?;
    planning::persist_evidence(&plan).map_err(|_| "host_source_evidence_unavailable")?;
    planning::persist_evidence(&snapshot.plan).map_err(|_| "host_source_evidence_unavailable")?;
    let (invocation, observation) = engine
        .execute_materialized(native, &snapshot.content)
        .map_err(|_| "host_engine_evidence_unavailable")?;
    let view = verified_output_view(&invocation, &observation)
        .map_err(|_| "host_engine_evidence_unavailable")?;
    // Replay is not an authorization lease. Reuse the same policy/materialization
    // authority immediately before disclosure; a changed snapshot cannot publish.
    if context_plan::materialize(&root, request)? != snapshot {
        return Err("source_plan_changed");
    }
    let receipt = authority.publish(&attempt, &invocation, &observation, &view.text)?;
    Ok(SourceExecutionResult {
        source_plan: snapshot.plan,
        execution_plan: plan,
        invocation,
        observation,
        view,
        receipt,
    })
}

fn reference(value: &str) -> Result<ProtocolReference, &'static str> {
    ProtocolReference::new(value).map_err(|_| "invalid_host_source_plan")
}
