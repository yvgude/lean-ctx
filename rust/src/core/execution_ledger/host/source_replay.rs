// SPDX-License-Identifier: Apache-2.0

//! Recover a completed source call through the same signed ledger and protocol.

use lean_ctx_protocol::{
    AcceptanceState, DecisionRecordV1, EngineInvocationV1, EngineObservationV1, ExecutionPlanV1,
    ReceiptEvidenceKindV1, Sha256Digest, TaskEnvelopeV1,
};

use super::{HostReceiptAttempt, HostReceiptAuthority, outcome, task_lock};
use crate::core::{
    context_kernel::autopilot::TaskAutopilotDecision,
    execution_protocol::RecordedExecutionProtocolV1,
};

impl HostReceiptAuthority {
    /// A recorded task is recoverable only from its current completed receipt.
    /// Partial intents, changed inputs and untrusted evidence remain fail-closed.
    pub(crate) fn replay_source_context(
        &self,
        task: &TaskEnvelopeV1,
        plan: &ExecutionPlanV1,
        expected_invocation: &EngineInvocationV1,
        context: &TaskAutopilotDecision,
    ) -> Result<Option<RecordedExecutionProtocolV1>, &'static str> {
        self.require_context_decision_signing()?;
        self.validate_current()?;
        let lock = task_lock(task)?;
        let events = self
            .ledger
            .by_task_verified(task.task_id.as_str())
            .map_err(|_| "host_ledger_unavailable")?;
        if events.is_empty() {
            return Ok(None);
        }
        let head = self
            .ledger
            .canonical_receipt_for_task_verified(task.task_id.as_str())
            .map_err(invalid)?
            .ok_or("host_task_already_recorded")?;
        let published =
            outcome::publication(&Sha256Digest::new(head.receipt_digest).map_err(invalid)?)
                .map_err(invalid)?;
        let (receipt, _) = published.read_canonical().map_err(invalid)?;
        if receipt.outcome.state != AcceptanceState::Unknown
            || receipt.chain.sequence_number != 1
            || published.receipt_id != head.receipt_id
            || published.receipt_ref != head.receipt_ref
        {
            return Err("host_source_replay_invalid");
        }
        let invocation: EngineInvocationV1 =
            outcome::read_evidence(&receipt.lineage.invocation_ref).map_err(invalid)?;
        if &invocation != expected_invocation {
            return Err("host_source_replay_invalid");
        }
        let observation_ref = receipt
            .evidence_refs
            .iter()
            .find(|reference| reference.uri.as_str() == "artifact://engine/observation")
            .ok_or("host_source_replay_invalid")?;
        let observation: EngineObservationV1 =
            outcome::read_evidence(&observation_ref.digest).map_err(invalid)?;
        let decision_ref = receipt
            .evidence_refs
            .iter()
            .find(|reference| reference.kind == ReceiptEvidenceKindV1::Runtime)
            .ok_or("host_source_replay_invalid")?;
        let decision: DecisionRecordV1 =
            outcome::read_evidence(&decision_ref.digest).map_err(invalid)?;
        // This is a read-only attachment to the existing attempt. The common
        // verifier checks current signer trust, exact task/plan/context digests,
        // both signatures and the native receipt/observation lineage.
        let attempt = HostReceiptAttempt {
            task: task.clone(),
            plan: plan.clone(),
            context_decision: Some(decision),
            context: Some(context.clone()),
            _task_lock: lock,
        };
        self.published_context_protocol(&attempt, &invocation, &observation, &published)
            .map_err(invalid)?
            .map(Some)
            .ok_or("host_source_replay_invalid")
    }
}

fn invalid<E>(_error: E) -> &'static str {
    "host_source_replay_invalid"
}
