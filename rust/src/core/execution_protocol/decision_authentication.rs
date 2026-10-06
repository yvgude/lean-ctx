// SPDX-License-Identifier: Apache-2.0

//! Stage-aware checks on an authenticated decision, never a replacement for receipts.

use lean_ctx_protocol::{DecisionKind, DecisionStageV1, TaskEnvelopeV1, UtcTimestamp};

use super::{
    ExecutionProtocolError, RecordedExecutionProtocolV1, fail, protocol_error, sha256_digest,
};
use crate::core::{
    canonical::canonical_serialize, context_kernel::autopilot::TaskAutopilotDecision,
    engine_artifact,
};

pub(super) fn validate(
    protocol: &RecordedExecutionProtocolV1,
    task: &TaskEnvelopeV1,
    actual_context: Option<&TaskAutopilotDecision>,
) -> Result<(), ExecutionProtocolError> {
    if protocol.decision.decision_stage != DecisionStageV1::Planning {
        // Execution/Outcome retain all existing invocation/receipt/outcome gates.
        return Ok(());
    }
    if protocol.decision.decision_kind != DecisionKind::ContextSelection {
        return Err(fail("unsupported authenticated planning decision kind"));
    }
    let context =
        actual_context.ok_or_else(|| fail("planning requires the actual context handoff"))?;
    context
        .validate_execution_plan(&protocol.execution_plan)
        .map_err(protocol_error)?;
    let decision = &protocol.decision;
    if context.context_projection() != &protocol.context_plan
        || context.context_projection().task_id != task.task_id
        || decision.decision_id.as_str() != context.decision().decision_id
        || protocol
            .execution_plan
            .extensions
            .get("context_autopilot_decision_ref")
            .and_then(serde_json::Value::as_str)
            != Some(context.decision().decision_id.as_str())
    {
        return Err(fail(
            "planning decision differs from the executed context handoff",
        ));
    }
    let bytes = context.canonical_bytes().map_err(protocol_error)?;
    let selected: serde_json::Value = serde_json::from_slice(&bytes).map_err(protocol_error)?;
    if decision.selected_result != selected
        || decision.policy_ref != protocol.execution_plan.policy_decision_ref
        || context
            .decision()
            .reasons
            .first()
            .map(|reason| reason.code.as_str())
            != Some(decision.rationale_code.as_str())
    {
        return Err(fail(
            "signed planning payload does not match actual selected policies",
        ));
    }
    let observed_at = UtcTimestamp::new(decision.observed_at.clone()).map_err(protocol_error)?;
    if observed_at.as_str() > protocol.receipt.issued_at.as_str() {
        return Err(fail("planning observation follows receipt issuance"));
    }
    let digest = sha256_digest(&bytes);
    let reference = format!("artifact://execution/evidence/{}", &digest[7..]);
    if decision.rationale_ref.as_deref() != Some(reference.as_str())
        || !decision.input_refs.contains(&reference)
        || !decision
            .evidence_refs
            .iter()
            .any(|evidence| evidence.uri == reference && evidence.digest == digest)
    {
        return Err(fail("planning decision omits actual handoff evidence"));
    }
    verify_persisted(&bytes)?;
    // The signed record itself must be the producer's persisted artifact, too.
    verify_persisted(&canonical_serialize(decision))
}

fn verify_persisted(bytes: &[u8]) -> Result<(), ExecutionProtocolError> {
    let digest = sha256_digest(bytes);
    let stored = engine_artifact::read_content("execution/evidence", &digest[7..], "json")
        .map_err(protocol_error)?;
    if stored != bytes {
        return Err(fail(
            "planning evidence differs from captured canonical bytes",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::core::execution_protocol::test_support;

    #[test]
    fn receipt_trust_alone_never_authenticates_an_unsigned_decision() {
        let task: lean_ctx_protocol::TaskEnvelopeV1 = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../tests/ocla_contract_suite/v1/task-envelope/valid_minimal.json"
        )))
        .unwrap();
        let mut fixture = test_support::build(&task);
        fixture.validated_for(&task).unwrap();
        fixture.protocol.decision.signature = "test-signature".into();
        fixture.protocol.validate_for(&task).unwrap();
        assert!(fixture.validated_for(&task).is_err());
    }
}
