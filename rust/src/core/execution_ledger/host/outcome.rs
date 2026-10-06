// SPDX-License-Identifier: Apache-2.0
//! Explicit operator-attested outcomes, appended to an authenticated native run.

use anyhow::{Result, ensure};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use ed25519_dalek::Signer as _;
use lean_ctx_ocla::{
    DecisionSignerAdmissionV1, sign_decision_record, verify_decision_signature,
    verify_receipt_signature,
};
use lean_ctx_protocol::{
    AcceptanceState, ContextPlanProjectionV1, DecisionId, DecisionKind, DecisionRecordV1,
    DecisionStageV1, EngineInvocationV1, EngineObservationV1, EngineOutcomeBindingV1, EvidenceKind,
    EvidenceRefV1, ExecutionPlanV1, ReceiptDocumentV1, ReceiptEvidenceKindV1, ReceiptEvidenceRefV1,
    ReceiptId, ReceiptOutcomeLinkV1, Sha256Digest, SignatureStatus, TaskEnvelopeV1,
};
use serde::{Deserialize, de::DeserializeOwned};

use super::{HostReceiptAuthority, canonical_serialize, digest, now, task_lock};
use crate::core::{
    engine_artifact,
    engine_interface::{persist_engine_artifact_content, planning::context_binding},
    execution_ledger::{ExecutionEvent, PublishedCanonicalReceipt, publish_canonical_receipt},
    execution_protocol::RecordedExecutionProtocolV1,
    outcome::{
        contracts::{OutcomeContractV1, TaskClass},
        evaluator::evaluate_for_task,
        signals::{OutcomeSignal, SignalType},
    },
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HostOutcomeRequest {
    pub schema_version: u32,
    pub receipt_digest: Sha256Digest,
    pub context_decision_digest: Sha256Digest,
    pub signals: Vec<OutcomeSignal>,
    /// Former consent for local model/provider outcome learning. Accepted for
    /// wire compatibility; since v4 nothing is learned (`learning_recorded`
    /// is always false). Its scope check still applies.
    pub learn: bool,
}

pub(super) struct HostOutcomeResult {
    pub publication: PublishedCanonicalReceipt,
    pub acceptance: AcceptanceState,
    pub learning_recorded: bool,
    pub already_recorded: bool,
}

impl HostReceiptAuthority {
    pub(crate) fn observe_outcome(
        &self,
        request: &HostOutcomeRequest,
    ) -> Result<serde_json::Value> {
        let result = self.observe_outcome_bound(request, None)?;
        Ok(serde_json::json!({"schema_version":1,
            "receipt_id":result.publication.receipt_id,
            "receipt_digest":result.publication.receipt_digest,
            "acceptance":result.acceptance,"learning_recorded":result.learning_recorded,
            "already_recorded":result.already_recorded}))
    }

    pub(super) fn observe_outcome_bound(
        &self,
        request: &HostOutcomeRequest,
        binding: Option<&EngineOutcomeBindingV1>,
    ) -> Result<HostOutcomeResult> {
        self.validate_current().map_err(anyhow::Error::msg)?;
        ensure!(
            self.allow_outcome_signing,
            "host outcome signing not authorized"
        );
        ensure!(
            request.schema_version == 1
                && !request.signals.is_empty()
                && request.signals.len() <= 16,
            "invalid outcome request"
        );
        ensure!(
            request
                .signals
                .iter()
                .all(|signal| signal.evidence_ref.is_none()
                    && signal.observed_at.is_none()
                    && !matches!(
                        signal.signal_type,
                        SignalType::AgentCompletion | SignalType::RetryCount
                    )),
            "signals must be explicit operator attestations, not completion or caller evidence"
        );
        let original = publication(&request.receipt_digest)?;
        let (initial, _) = original.read_canonical()?;
        let planning_uri = format!(
            "artifact://execution/evidence/{}",
            request.context_decision_digest.hex()
        );
        // Older receipts have no planning pointer. If present, its exact signed
        // bytes must be the record being evaluated, not another valid signature.
        ensure!(
            initial
                .evidence_refs
                .iter()
                .filter(|reference| { reference.kind == ReceiptEvidenceKindV1::Runtime })
                .all(
                    |reference| reference.digest == request.context_decision_digest
                        && reference.uri.as_str() == planning_uri
                ),
            "outcome planning evidence differs from the initial receipt"
        );
        ensure!(
            binding.is_none()
                || initial
                    .evidence_refs
                    .iter()
                    .any(|reference| { reference.kind == ReceiptEvidenceKindV1::Runtime }),
            "framed outcome requires a receipt-bound planning record"
        );
        let task: TaskEnvelopeV1 = read_evidence(&initial.lineage.task_ref)?;
        let plan: ExecutionPlanV1 = read_evidence(&initial.lineage.plan_ref)?;
        let invocation: EngineInvocationV1 = read_evidence(&initial.lineage.invocation_ref)?;
        let observation_ref = initial
            .evidence_refs
            .iter()
            .find(|entry| entry.uri.as_str() == "artifact://engine/observation")
            .ok_or_else(|| anyhow::anyhow!("receipt observation missing"))?;
        let observation: EngineObservationV1 = read_evidence(&observation_ref.digest)?;
        let planning: DecisionRecordV1 = read_evidence(&request.context_decision_digest)?;
        let verified_at = now().map_err(anyhow::Error::msg)?;
        let key = self.signing_key.verifying_key();
        verify_receipt_signature(&initial, &self.signer_admission, &key, &verified_at)?;
        let mut grant = DecisionSignerAdmissionV1 {
            key_admission: self.signer_admission.clone(),
            task: task.clone(),
            stage: DecisionStageV1::Planning,
            kind: DecisionKind::ContextSelection,
        };
        verify_decision_signature(&planning, &task, &grant, &key, &verified_at)?;
        if let Some(binding) = binding {
            // These are host-supplied expectations, never authorization by
            // themselves. Compare against signed lineage before any mutation.
            ensure!(
                binding.task_id == task.task_id
                    && binding.tenant_id == task.tenant_id
                    && binding.agent_id == task.agent_id,
                "outcome task identity differs from the host binding"
            );
        }
        ensure!(
            initial.outcome.state == AcceptanceState::Unknown,
            "initial receipt must be unobserved"
        );
        ensure!(
            !request.learn
                || (task.tenant_id.is_none() && task.project_id.as_str() != "unknown-project"),
            "personal learning scope not admitted"
        );
        let handoff_ref = planning
            .rationale_ref
            .as_deref()
            .and_then(|value| value.strip_prefix("artifact://execution/evidence/"))
            .ok_or_else(|| anyhow::anyhow!("planning handoff missing"))?;
        let handoff = read_bytes(&Sha256Digest::new(format!("sha256:{handoff_ref}"))?)?;
        ensure!(
            serde_json::from_slice::<serde_json::Value>(&handoff)? == planning.selected_result,
            "planning payload mismatch"
        );
        context_binding::validate_handoff(&plan, &handoff).map_err(anyhow::Error::msg)?;
        context_binding::validate_task_binding(&plan, &task).map_err(anyhow::Error::msg)?;
        let projection: ContextPlanProjectionV1 =
            serde_json::from_value(planning.selected_result[0].clone())?;
        let class: TaskClass =
            serde_json::from_value(planning.selected_result[1]["task_class"].clone())?;
        let contract = OutcomeContractV1::for_task_class(class);
        let mut prior_outcome = evaluate_for_task(
            &contract,
            &[],
            "outcome:unobserved",
            task.task_id.as_str(),
            initial.issued_at.as_str(),
        );
        prior_outcome.plan_id = Some(plan.plan_id.clone());
        prior_outcome.receipt_id = Some(ReceiptId::new(initial.receipt_id.as_str())?);
        prior_outcome.decision_refs = vec![planning.decision_id.as_str().into()];
        let mut protocol = RecordedExecutionProtocolV1 {
            context_plan: projection,
            execution_plan: plan,
            invocation,
            observation,
            receipt: initial.clone(),
            published_receipt: original,
            accepted_outcome: prior_outcome,
            decision: planning.clone(),
        };
        // Structural/Engine checks complement both trusted signatures above. The
        // original planning bytes are restored, never a fabricated kernel decision.
        protocol.validate_for(&task)?;
        let identity = digest(&canonical_serialize(&(
            "host-outcome/v1",
            &request.receipt_digest,
            &request.context_decision_digest,
            &request.signals,
        )))
        .map_err(anyhow::Error::msg)?;
        let outcome_id = format!("outcome:{}", identity.hex());
        let evidence_bytes = serde_json::to_vec(&(&contract, &request.signals))?;
        let evidence_digest = digest(&evidence_bytes).map_err(anyhow::Error::msg)?;
        let _lock = task_lock(&task).map_err(anyhow::Error::msg)?;
        let head = self
            .ledger
            .canonical_receipt_for_task_verified(task.task_id.as_str())?
            .ok_or_else(|| anyhow::anyhow!("receipt is not recorded by this host"))?;
        let head_publication = publication(&Sha256Digest::new(head.receipt_digest)?)?;
        let (head_receipt, _) = head_publication.read_canonical()?;
        verify_receipt_signature(&head_receipt, &self.signer_admission, &key, &verified_at)?;
        let replay = head_receipt.receipt_id != initial.receipt_id;
        ensure!(
            !replay
                || (head_receipt.chain.previous_receipt_id.as_ref() == Some(&initial.receipt_id)
                    && head_receipt
                        .outcome
                        .outcome_id
                        .as_ref()
                        .map(lean_ctx_protocol::OutcomeId::as_str)
                        == Some(outcome_id.as_str())
                    && head_receipt.lineage == initial.lineage
                    && head_receipt.chain.chain_id == initial.chain.chain_id
                    && initial.chain.sequence_number.checked_add(1)
                        == Some(head_receipt.chain.sequence_number)
                    && head_receipt.chain.previous_signature_digest.as_ref()
                        == Some(
                            &digest(initial.signature.as_bytes()).map_err(anyhow::Error::msg)?
                        )),
            "conflicting or stale outcome"
        );
        let observed_at = if replay {
            head_receipt.issued_at.clone()
        } else {
            verified_at.clone()
        };
        let observed_time = chrono::DateTime::parse_from_rfc3339(observed_at.as_str())?;
        let age = observed_time.signed_duration_since(chrono::DateTime::parse_from_rfc3339(
            initial.issued_at.as_str(),
        )?);
        ensure!(
            observed_time <= chrono::DateTime::parse_from_rfc3339(verified_at.as_str())?
                && age >= chrono::Duration::zero()
                && age <= chrono::Duration::hours(i64::from(contract.expiry_window_hours)),
            "outcome is outside its contract window"
        );
        let mut outcome = evaluate_for_task(
            &contract,
            &request.signals,
            outcome_id,
            task.task_id.as_str(),
            observed_at.as_str(),
        );
        ensure!(
            outcome.accepted != AcceptanceState::Unknown,
            "contract has missing acceptance signals"
        );
        ensure!(
            outcome
                .evidence_refs
                .iter()
                .any(|entry| entry.digest == evidence_digest.as_str()),
            "evaluator evidence mismatch"
        );
        // Snapshot evaluation before adding receipt/decision links: the receipt
        // binds the result and its distinct inputs without a digest cycle.
        let outcome_bytes = canonical_serialize(&outcome);
        let outcome_digest = digest(&outcome_bytes).map_err(anyhow::Error::msg)?;
        let acceptance_digest =
            (outcome.accepted == AcceptanceState::Accepted).then_some(evidence_digest.clone());
        ensure!(
            !replay
                || (head_receipt.outcome.state == outcome.accepted
                    && head_receipt.outcome.outcome_ref.as_ref() == Some(&outcome_digest)
                    && head_receipt.outcome.acceptance_evidence_digest == acceptance_digest),
            "recorded outcome differs from evaluation"
        );
        persist(&evidence_bytes)?;
        persist(&outcome_bytes)?;
        let receipt = if replay {
            head_receipt
        } else {
            let mut next = initial.clone();
            next.chain.sequence_number = next
                .chain
                .sequence_number
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("receipt sequence exhausted"))?;
            next.chain.previous_receipt_id = Some(initial.receipt_id.clone());
            next.chain.previous_signature_digest =
                Some(digest(initial.signature.as_bytes()).map_err(anyhow::Error::msg)?);
            next.issued_at = observed_at.clone();
            next.status = if outcome.accepted == AcceptanceState::Accepted {
                lean_ctx_protocol::ReceiptTerminalStatusV1::Succeeded
            } else {
                lean_ctx_protocol::ReceiptTerminalStatusV1::Rejected
            };
            next.outcome = ReceiptOutcomeLinkV1 {
                state: outcome.accepted,
                outcome_id: Some(outcome.outcome_id.clone()),
                outcome_ref: Some(outcome_digest.clone()),
                acceptance_evidence_digest: acceptance_digest,
            };
            for (kind, hash) in [
                (ReceiptEvidenceKindV1::Outcome, &outcome_digest),
                (ReceiptEvidenceKindV1::Measurement, &evidence_digest),
            ] {
                next.evidence_refs.push(ReceiptEvidenceRefV1 {
                    kind,
                    uri: lean_ctx_protocol::ProtocolReference::new(format!(
                        "artifact://execution/evidence/{}",
                        hash.hex()
                    ))?,
                    digest: hash.clone(),
                    media_type: "application/json".into(),
                    signature_status: SignatureStatus::NotSigned,
                });
            }
            next.receipt_id = next.derived_receipt_id()?;
            next.signature =
                STANDARD.encode(self.signing_key.sign(&next.signing_bytes()?).to_bytes());
            next
        };
        receipt.validate()?;
        outcome.evidence_refs.push(EvidenceRefV1 {
            schema_version: Some(1),
            kind: EvidenceKind::QualityMeasurement,
            uri: format!("artifact://execution/evidence/{}", outcome_digest.hex()),
            digest: outcome_digest.as_str().into(),
            signature_status: SignatureStatus::NotSigned,
            media_type: Some("application/json".into()),
            extensions: Default::default(),
        });
        outcome.plan_id = Some(protocol.execution_plan.plan_id.clone());
        outcome.receipt_id = Some(ReceiptId::new(receipt.receipt_id.as_str())?);
        let mut decision = planning;
        decision.decision_id = DecisionId::new(format!("decision:{}", identity.hex()))?;
        decision.decision_stage = DecisionStageV1::Outcome;
        decision.decision_kind = DecisionKind::Stop;
        decision.input_refs.extend([
            receipt.lineage.invocation_ref.as_str().into(),
            receipt.receipt_id.as_str().into(),
        ]);
        decision.selected_result = serde_json::json!({"acceptance":outcome.accepted});
        decision.rationale_code = "operator_attested_contract_outcome".into();
        decision.rationale_ref = Some(format!(
            "artifact://execution/evidence/{}",
            evidence_digest.hex()
        ));
        decision.constraint_refs = vec![contract.reference()];
        decision.evidence_refs.clone_from(&outcome.evidence_refs);
        decision.observed_at = observed_at.as_str().into();
        grant.stage = DecisionStageV1::Outcome;
        grant.kind = DecisionKind::Stop;
        decision.signature =
            sign_decision_record(&decision, &task, &grant, &self.signing_key, &verified_at)?;
        outcome.decision_refs = vec![decision.decision_id.as_str().into()];
        // An exact replay republishes the same immutable artifact and repairs any
        // interrupted ledger projection before retrying receipt-idempotent learning.
        protocol.published_receipt = publish_canonical_receipt(
            &receipt,
            &self.ledger,
            task.trace_id.as_str(),
            &observed_at,
        )?;
        protocol.receipt = receipt;
        protocol.accepted_outcome = outcome;
        protocol.decision = decision;
        // Signature, grant and lineage validation still gates every recorded outcome.
        protocol.validated_for(
            &task,
            (&self.signer_admission, &key),
            (&grant, &key),
            None,
            &verified_at,
        )?;
        persist(&canonical_serialize(&protocol.accepted_outcome))?;
        persist(&canonical_serialize(&protocol.decision))?;
        self.ledger.append(ExecutionEvent::OutcomeRecorded {
            task_id: task.task_id.as_str().into(),
            trace_id: task.trace_id.as_str().into(),
            outcome_id: protocol.accepted_outcome.outcome_id.as_str().into(),
            receipt_id: protocol.receipt.receipt_id.as_str().into(),
            accepted: protocol.accepted_outcome.accepted,
            timestamp: observed_at.as_str().into(),
            sequence_number: 0,
            prev_hash: String::new(),
        })?;
        // Model/provider outcome learning only fed adaptive routing, which v4
        // removed; `learn` stays accepted on the wire but records nothing.
        Ok(HostOutcomeResult {
            publication: protocol.published_receipt,
            acceptance: protocol.accepted_outcome.accepted,
            learning_recorded: false,
            already_recorded: replay,
        })
    }
}

pub(super) fn read_bytes(expected: &Sha256Digest) -> Result<Vec<u8>> {
    let bytes = engine_artifact::read_content("execution/evidence", expected.hex(), "json")
        .map_err(anyhow::Error::msg)?;
    ensure!(
        digest(&bytes).map_err(anyhow::Error::msg)? == *expected,
        "evidence digest mismatch"
    );
    Ok(bytes)
}

pub(super) fn read_evidence<T: DeserializeOwned>(expected: &Sha256Digest) -> Result<T> {
    Ok(serde_json::from_slice(&read_bytes(expected)?)?)
}

fn persist(bytes: &[u8]) -> Result<()> {
    let hash = digest(bytes).map_err(anyhow::Error::msg)?;
    drop(
        persist_engine_artifact_content("execution/evidence", hash.hex(), "json", bytes)
            .map_err(anyhow::Error::msg)?,
    );
    Ok(())
}

pub(super) fn publication(expected: &Sha256Digest) -> Result<PublishedCanonicalReceipt> {
    let bytes = engine_artifact::read_content("execution/receipts", expected.hex(), "json")
        .map_err(anyhow::Error::msg)?;
    ensure!(
        digest(&bytes).map_err(anyhow::Error::msg)? == *expected,
        "receipt digest mismatch"
    );
    let receipt = ReceiptDocumentV1::from_canonical_bytes(&bytes)?;
    Ok(PublishedCanonicalReceipt {
        receipt_id: receipt.receipt_id.as_str().into(),
        receipt_ref: format!("id:{}", expected.as_str()),
        receipt_digest: expected.as_str().into(),
        path: crate::core::data_dir::resolve_data_dir()
            .map_err(anyhow::Error::msg)?
            .join("execution/receipts")
            .join(format!("{}.json", expected.hex())),
    })
}
