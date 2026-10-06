// SPDX-License-Identifier: Apache-2.0

//! Authenticate the genuine planning record against the actual published native run.

use lean_ctx_ocla::DecisionSignerAdmissionV1;
use lean_ctx_protocol::{
    AcceptanceState, DecisionKind, DecisionStageV1, EngineInvocationV1, EngineObservationV1,
};

use super::{HostReceiptAttempt, HostReceiptAuthority, PublishedCanonicalReceipt, digest, now};
use crate::core::{
    canonical::canonical_serialize,
    execution_protocol::RecordedExecutionProtocolV1,
    outcome::{contracts::OutcomeContractV1, evaluator::evaluate_for_task},
};

impl HostReceiptAuthority {
    /// Initial publication has no quality signal; never derive acceptance from success.
    pub(crate) fn published_context_protocol(
        &self,
        attempt: &HostReceiptAttempt,
        invocation: &EngineInvocationV1,
        observation: &EngineObservationV1,
        published: &PublishedCanonicalReceipt,
    ) -> Result<Option<RecordedExecutionProtocolV1>, String> {
        if !self.allow_context_decision_signing {
            return Ok(None);
        }
        let context = attempt
            .context
            .as_ref()
            .ok_or("host_context_decision_missing")?;
        let decision = attempt
            .context_decision
            .as_ref()
            .ok_or("host_context_decision_missing")?;
        let (receipt, _) = published
            .read_canonical()
            .map_err(|error| error.to_string())?;
        let identity = digest(&canonical_serialize(&(
            "native-unobserved-outcome-v1",
            &attempt.task,
            &receipt.receipt_id,
            &decision.decision_id,
        )))?;
        let mut outcome = evaluate_for_task(
            &OutcomeContractV1::for_task_class(context.decision().task_class),
            &[],
            format!("outcome:{}", identity.hex()),
            attempt.task.task_id.as_str(),
            now()?.as_str(),
        );
        if outcome.accepted != AcceptanceState::Unknown || outcome.quality_score_milli.is_some() {
            return Err("native publication has no accepted outcome evidence".into());
        }
        outcome.plan_id = Some(attempt.plan.plan_id.clone());
        outcome.receipt_id = Some(
            lean_ctx_protocol::ReceiptId::new(receipt.receipt_id.as_str())
                .map_err(|error| error.to_string())?,
        );
        outcome.decision_refs = vec![decision.decision_id.as_str().into()];
        let protocol = RecordedExecutionProtocolV1 {
            context_plan: context.context_projection().clone(),
            execution_plan: attempt.plan.clone(),
            invocation: invocation.clone(),
            observation: observation.clone(),
            receipt,
            published_receipt: published.clone(),
            accepted_outcome: outcome,
            decision: decision.clone(),
        };
        let grant = DecisionSignerAdmissionV1 {
            key_admission: self.signer_admission.clone(),
            task: attempt.task.clone(),
            stage: DecisionStageV1::Planning,
            kind: DecisionKind::ContextSelection,
        };
        let key = self.signing_key.verifying_key();
        protocol
            .validated_for(
                &attempt.task,
                (&self.signer_admission, &key),
                (&grant, &key),
                Some(context),
                &now()?,
            )
            .map_err(|error| error.to_string())?;
        Ok(Some(protocol))
    }
}
