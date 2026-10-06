// SPDX-License-Identifier: Apache-2.0

//! Host-authorized capture of the existing immutable Context Autopilot handoff.

#[cfg(test)]
mod tests;

use lean_ctx_ocla::{DecisionSignerAdmissionV1, sign_decision_record};
use lean_ctx_protocol::{
    DecisionId, DecisionKind, DecisionRecordV1, DecisionStageV1, EvidenceKind, EvidenceRefV1,
    ExecutionPlanV1, SignatureStatus, TaskEnvelopeV1, UtcTimestamp,
};

use super::{
    HostReceiptAuthority, canonical_serialize, digest, now, persist_engine_artifact_content,
};
use crate::core::context_kernel::autopilot::TaskAutopilotDecision;

pub(super) fn capture(
    authority: &HostReceiptAuthority,
    task: &TaskEnvelopeV1,
    plan: &ExecutionPlanV1,
    context: &TaskAutopilotDecision,
) -> Result<DecisionRecordV1, &'static str> {
    // This check belongs to the trusted host, not an incoming record or request.
    if !authority.allow_context_decision_signing {
        return Err("host_context_decision_not_authorized");
    }
    context
        .validate_execution_plan(plan)
        .map_err(|_| "host_context_decision_plan_mismatch")?;
    crate::core::engine_interface::planning::context_binding::validate_task_binding(plan, task)
        .map_err(|_| "host_context_decision_plan_mismatch")?;
    if task.task_id != plan.task_id
        || plan
            .extensions
            .get("context_autopilot_decision_ref")
            .and_then(serde_json::Value::as_str)
            != Some(context.decision().decision_id.as_str())
    {
        return Err("host_context_decision_plan_mismatch");
    }
    // This timestamp observes the captured planning handoff, not a later outcome.
    let observed_at = now()?;
    let bytes = context
        .canonical_bytes()
        .map_err(|_| "host_context_decision_invalid")?;
    let mut record = captured_record(task, plan, context, &bytes, &observed_at)?;
    let grant = DecisionSignerAdmissionV1 {
        key_admission: authority.signer_admission.clone(),
        task: task.clone(),
        stage: DecisionStageV1::Planning,
        kind: DecisionKind::ContextSelection,
    };
    record.signature =
        sign_decision_record(&record, task, &grant, &authority.signing_key, &observed_at)
            .map_err(|_| "host_context_decision_signing_failed")?;
    for content in [&bytes, &canonical_serialize(&record)] {
        let content_digest = digest(content)?;
        drop(
            persist_engine_artifact_content(
                "execution/evidence",
                content_digest.hex(),
                "json",
                content,
            )
            .map_err(|_| "host_context_decision_evidence_unavailable")?,
        );
    }
    Ok(record)
}

fn captured_record(
    task: &TaskEnvelopeV1,
    plan: &ExecutionPlanV1,
    context: &TaskAutopilotDecision,
    bytes: &[u8],
    observed_at: &UtcTimestamp,
) -> Result<DecisionRecordV1, &'static str> {
    let handoff_digest = digest(bytes)?;
    let handoff_ref = format!("artifact://execution/evidence/{}", handoff_digest.hex());
    let task_digest = digest(&task.canonical_bytes().map_err(|_| "invalid_host_task")?)?;
    let plan_digest = digest(&canonical_serialize(plan))?;
    let projection = context.context_projection();
    projection
        .validate()
        .map_err(|_| "host_context_decision_invalid")?;
    let projection_digest = projection
        .projection_digest
        .as_ref()
        .ok_or("host_context_decision_digest_missing")?;
    Ok(DecisionRecordV1 {
        schema_version: 1,
        // Retain the executed planner ID; the signature authenticates the full payload.
        decision_id: DecisionId::new(context.decision().decision_id.clone())
            .map_err(|_| "host_context_decision_invalid")?,
        task_id: task.task_id.clone(),
        plan_id: Some(plan.plan_id.clone()),
        decision_stage: DecisionStageV1::Planning,
        decision_kind: DecisionKind::ContextSelection,
        input_refs: vec![
            task_digest.as_str().into(),
            plan_digest.as_str().into(),
            projection_digest.as_str().into(),
            handoff_ref.clone(),
        ],
        constraint_refs: plan.policy_decision_ref.iter().cloned().collect(),
        // Includes read/view policies and all selection reasons, not just wire projection.
        selected_result: serde_json::from_slice(bytes)
            .map_err(|_| "host_context_decision_invalid")?,
        rationale_code: context
            .decision()
            .reasons
            .first()
            .ok_or("host_context_decision_reason_missing")?
            .code
            .clone(),
        rationale_ref: Some(handoff_ref.clone()),
        policy_ref: plan.policy_decision_ref.clone(),
        decision_system_name: "lean-ctx-context-autopilot".into(),
        decision_system_version: env!("CARGO_PKG_VERSION").into(),
        evidence_refs: vec![EvidenceRefV1 {
            schema_version: Some(1),
            kind: EvidenceKind::RuntimeLog,
            uri: handoff_ref,
            digest: handoff_digest.as_str().into(),
            // The parent signature covers this digest; the blob is not independently signed.
            signature_status: SignatureStatus::NotSigned,
            media_type: Some("application/json".into()),
            extensions: Default::default(),
        }],
        observed_at: observed_at.as_str().into(),
        signature: String::new(),
        supersedes: None,
        extensions: Default::default(),
    })
}
