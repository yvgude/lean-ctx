// SPDX-License-Identifier: Apache-2.0

//! Fail-closed join between checkpoints and authoritative execution artifacts.

use std::collections::BTreeSet;
use std::fmt;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use ed25519_dalek::{Signature, VerifyingKey};
use lean_ctx_protocol::{
    AcceptanceState, AcceptedOutcomeV1, CONTEXT_CHECKPOINT_V2_SIGNATURE_DOMAIN,
    ContextCheckpointArtifactLineageV1, ContextCheckpointLineageV2, ContextCheckpointV1,
    ContextCheckpointV2, ExecutionPlanV1, ReceiptDocumentV1, ReceiptId, Sha256Digest,
    TaskEnvelopeV1,
};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::core::canonical;
use crate::core::execution_ledger::{ReceiptSignerAdmissionV1, producer};

const MAX_RECEIPT_BYTES: usize = 1_048_576;

/// Error returned when checkpoint lineage cannot be proven from authoritative
/// task, plan, outcome, and signed receipt artifacts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckpointLineageError(String);

impl fmt::Display for CheckpointLineageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for CheckpointLineageError {}

/// Receipt document whose canonical bytes and Ed25519 signature were verified
/// against a server-owned signer admission.
///
/// Fields remain private so callers cannot manufacture the verified state from
/// a raw `ReceiptDocumentV1` or legacy `ExecutionReceiptV1` value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedReceiptDocumentV1 {
    document: ReceiptDocumentV1,
    canonical_bytes: Vec<u8>,
    canonical_digest: Sha256Digest,
}

/// V2 checkpoint whose artifact lineage was recomputed from authoritative task,
/// plan, signed receipt, and accepted-outcome owners.
///
/// The inner wire value is private. Only this proof-carrying wrapper exposes the
/// V2 signing payload, so structurally valid untrusted wire data cannot reach a
/// signer through the safe API.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedContextCheckpointV2 {
    checkpoint: ContextCheckpointV2,
}

impl VerifiedContextCheckpointV2 {
    /// Borrow the validated wire value for persistence or transport.
    pub fn checkpoint(&self) -> &ContextCheckpointV2 {
        &self.checkpoint
    }

    /// Produce the domain-separated bytes that a checkpoint V2 signer covers.
    pub fn signing_payload(
        &self,
    ) -> Result<VerifiedContextCheckpointSigningPayloadV2, CheckpointLineageError> {
        let canonical = self.checkpoint.canonical_bytes().map_err(protocol_error)?;
        let mut bytes =
            Vec::with_capacity(CONTEXT_CHECKPOINT_V2_SIGNATURE_DOMAIN.len() + canonical.len());
        bytes.extend_from_slice(CONTEXT_CHECKPOINT_V2_SIGNATURE_DOMAIN);
        bytes.extend_from_slice(&canonical);
        let digest = digest_bytes(&bytes)?;
        Ok(VerifiedContextCheckpointSigningPayloadV2 { bytes, digest })
    }
}

impl std::ops::Deref for VerifiedContextCheckpointV2 {
    type Target = ContextCheckpointV2;

    fn deref(&self) -> &Self::Target {
        &self.checkpoint
    }
}

/// Sealed signing material obtainable only from a verified V2 checkpoint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedContextCheckpointSigningPayloadV2 {
    bytes: Vec<u8>,
    digest: Sha256Digest,
}

impl VerifiedContextCheckpointSigningPayloadV2 {
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn digest(&self) -> &Sha256Digest {
        &self.digest
    }
}

impl VerifiedReceiptDocumentV1 {
    /// Decode canonical bytes and verify identity, signer admission, and
    /// Ed25519 signature before exposing the receipt to a lineage builder.
    pub fn from_canonical_bytes(
        bytes: &[u8],
        signer_admission: &ReceiptSignerAdmissionV1,
        verifying_key: &VerifyingKey,
    ) -> Result<Self, CheckpointLineageError> {
        if bytes.len() > MAX_RECEIPT_BYTES {
            return fail(format!(
                "verified receipt exceeds the {MAX_RECEIPT_BYTES} byte limit"
            ));
        }
        let document = ReceiptDocumentV1::from_canonical_bytes(bytes).map_err(protocol_error)?;
        if document.signer.key_id != signer_admission.key_id {
            return fail("receipt signer key_id is not the admitted key".to_owned());
        }
        producer::validate_signer_admission(signer_admission, verifying_key, &document.issued_at)
            .map_err(protocol_error)?;
        let signature_bytes = STANDARD.decode(&document.signature).map_err(|error| {
            CheckpointLineageError(format!("decode receipt signature: {error}"))
        })?;
        let signature = Signature::from_slice(&signature_bytes)
            .map_err(|error| CheckpointLineageError(format!("parse receipt signature: {error}")))?;
        verifying_key
            .verify_strict(
                &document.signature_bytes().map_err(protocol_error)?,
                &signature,
            )
            .map_err(|error| {
                CheckpointLineageError(format!("verify receipt signature: {error}"))
            })?;
        let canonical = document.canonical_bytes().map_err(protocol_error)?;
        if canonical != bytes {
            return fail("verified receipt bytes changed after canonical decoding".to_owned());
        }
        Ok(Self {
            document,
            canonical_bytes: bytes.to_vec(),
            canonical_digest: digest_bytes(bytes)?,
        })
    }

    /// Alias that makes the verification boundary explicit at call sites.
    pub fn verify_canonical_bytes(
        bytes: &[u8],
        signer_admission: &ReceiptSignerAdmissionV1,
        verifying_key: &VerifyingKey,
    ) -> Result<Self, CheckpointLineageError> {
        Self::from_canonical_bytes(bytes, signer_admission, verifying_key)
    }

    /// Borrow the verified receipt document.
    pub fn document(&self) -> &ReceiptDocumentV1 {
        &self.document
    }

    /// Borrow the exact canonical bytes whose signature was verified.
    pub fn canonical_bytes(&self) -> &[u8] {
        &self.canonical_bytes
    }

    /// Digest of the exact persisted canonical receipt bytes.
    pub fn canonical_digest(&self) -> &Sha256Digest {
        &self.canonical_digest
    }

    /// Receipt identity digest derived by `ReceiptDocumentV1`'s owner.
    pub fn receipt_id(&self) -> &Sha256Digest {
        &self.document.receipt_id
    }
}

/// Build authoritative checkpoint artifact references from validated owners.
pub fn build_checkpoint_lineage(
    task: &TaskEnvelopeV1,
    plan: Option<&ExecutionPlanV1>,
    receipts: &[VerifiedReceiptDocumentV1],
    outcomes: &[AcceptedOutcomeV1],
) -> Result<ContextCheckpointArtifactLineageV1, CheckpointLineageError> {
    task.validate().map_err(protocol_error)?;
    let task_ref = canonical_digest(task)?;
    let plan_ref = if let Some(plan) = plan {
        plan.validate().map_err(protocol_error)?;
        if plan.task_id != task.task_id {
            return fail("execution plan task_id does not match task envelope".to_owned());
        }
        Some(canonical_digest(plan)?)
    } else {
        if !receipts.is_empty() || !outcomes.is_empty() {
            return fail("receipts and outcomes require an execution plan".to_owned());
        }
        None
    };

    let mut receipt_refs = Vec::with_capacity(receipts.len());
    let mut seen_receipts = BTreeSet::new();
    for receipt in receipts {
        let document = receipt.document();
        document.validate().map_err(protocol_error)?;
        if document.lineage.task_id != task.task_id || document.lineage.task_ref != task_ref {
            return fail(
                "receipt task identity or task_ref disagrees with task envelope".to_owned(),
            );
        }
        let Some(plan) = plan else {
            return fail("receipt cannot be attached without an execution plan".to_owned());
        };
        let Some(plan_ref) = plan_ref.as_ref() else {
            return fail("execution plan reference is missing".to_owned());
        };
        if document.lineage.plan_id != plan.plan_id || document.lineage.plan_ref != *plan_ref {
            return fail(
                "receipt plan identity or plan_ref disagrees with execution plan".to_owned(),
            );
        }
        if !seen_receipts.insert(document.receipt_id.as_str().to_owned()) {
            return fail("receipt_refs contain a duplicate receipt identity".to_owned());
        }
        receipt_refs.push(receipt.canonical_digest().clone());
    }

    validate_outcome_links(task, plan, receipts, outcomes)?;
    receipt_refs.sort_unstable();
    let lineage = ContextCheckpointArtifactLineageV1 {
        task_ref,
        plan_ref,
        receipt_refs,
    };
    lineage.validate().map_err(protocol_error)?;
    Ok(lineage)
}

/// Attach authoritative lineage through the explicit V1→V2 migration seam.
pub fn build_checkpoint_v2(
    checkpoint: &ContextCheckpointV1,
    task: &TaskEnvelopeV1,
    plan: Option<&ExecutionPlanV1>,
    receipts: &[VerifiedReceiptDocumentV1],
    outcomes: &[AcceptedOutcomeV1],
) -> Result<VerifiedContextCheckpointV2, CheckpointLineageError> {
    checkpoint.validate().map_err(protocol_error)?;
    if checkpoint.lineage.task_id != task.task_id
        || checkpoint.lineage.project_id != task.project_id
    {
        return fail("checkpoint task or project identity disagrees with task envelope".to_owned());
    }
    let Some(task_tenant) = task.tenant_id.as_ref() else {
        return fail("task envelope must bind a tenant identity".to_owned());
    };
    if task_tenant != &checkpoint.lineage.tenant_id {
        return fail("checkpoint tenant identity disagrees with task envelope".to_owned());
    }
    if checkpoint.lineage.plan_id.as_ref() != plan.map(|value| &value.plan_id) {
        return fail("checkpoint plan_id disagrees with supplied execution plan".to_owned());
    }
    let artifact_lineage = build_checkpoint_lineage(task, plan, receipts, outcomes)?;
    let mut expected_receipt_ids = receipts
        .iter()
        .map(|receipt| {
            ReceiptId::try_from(receipt.receipt_id().as_str().to_owned()).map_err(protocol_error)
        })
        .collect::<Result<Vec<_>, _>>()?;
    expected_receipt_ids.sort_unstable();
    if !checkpoint.lineage.receipt_ids.is_empty()
        && checkpoint.lineage.receipt_ids != expected_receipt_ids
    {
        return fail("checkpoint receipt_ids disagree with verified receipt identities".to_owned());
    }
    let mut compatibility = checkpoint.clone();
    compatibility.lineage.receipt_ids = expected_receipt_ids;
    let checkpoint = ContextCheckpointV2::from_v1_unverified(compatibility, artifact_lineage)
        .map_err(protocol_error)?;
    Ok(VerifiedContextCheckpointV2 { checkpoint })
}

/// Recompute and compare all authoritative refs against a V2 checkpoint.
pub fn validate_checkpoint_lineage(
    checkpoint: &ContextCheckpointV2,
    task: &TaskEnvelopeV1,
    plan: Option<&ExecutionPlanV1>,
    receipts: &[VerifiedReceiptDocumentV1],
    outcomes: &[AcceptedOutcomeV1],
) -> Result<(), CheckpointLineageError> {
    checkpoint.validate().map_err(protocol_error)?;
    validate_artifact_lineage(&checkpoint.lineage, task, plan, receipts, outcomes)
}

/// One artifact admission join shared by checkpoint wire versions.
pub(super) fn validate_artifact_lineage(
    lineage: &ContextCheckpointLineageV2,
    task: &TaskEnvelopeV1,
    plan: Option<&ExecutionPlanV1>,
    receipts: &[VerifiedReceiptDocumentV1],
    outcomes: &[AcceptedOutcomeV1],
) -> Result<(), CheckpointLineageError> {
    lineage.validate().map_err(protocol_error)?;
    let expected = build_checkpoint_lineage(task, plan, receipts, outcomes)?;
    if lineage.task_id != task.task_id
        || lineage.project_id != task.project_id
        || lineage.artifact_lineage != expected
    {
        return fail(
            "checkpoint artifact lineage disagrees with authoritative artifacts".to_owned(),
        );
    }
    let Some(task_tenant) = task.tenant_id.as_ref() else {
        return fail("task envelope must bind a tenant identity".to_owned());
    };
    if task_tenant != &lineage.tenant_id {
        return fail("checkpoint tenant identity disagrees with task envelope".to_owned());
    }
    if lineage.plan_id.as_ref() != plan.map(|value| &value.plan_id) {
        return fail("checkpoint plan_id disagrees with supplied execution plan".to_owned());
    }
    let mut expected_receipt_ids = receipts
        .iter()
        .map(|receipt| {
            ReceiptId::try_from(receipt.receipt_id().as_str().to_owned()).map_err(protocol_error)
        })
        .collect::<Result<Vec<_>, _>>()?;
    expected_receipt_ids.sort_unstable();
    if lineage.receipt_ids != expected_receipt_ids {
        return fail("checkpoint receipt_ids disagree with verified receipt identities".to_owned());
    }
    Ok(())
}

/// Re-establish the proof-carrying wrapper for a decoded or transformed V2 checkpoint.
///
/// Branching, merging, persistence, and transport boundaries must use this seam
/// before requesting signing material; structural V2 validation alone does not
/// prove that its artifact digests belong to the supplied authoritative owners.
pub fn verify_checkpoint_v2(
    checkpoint: ContextCheckpointV2,
    task: &TaskEnvelopeV1,
    plan: Option<&ExecutionPlanV1>,
    receipts: &[VerifiedReceiptDocumentV1],
    outcomes: &[AcceptedOutcomeV1],
) -> Result<VerifiedContextCheckpointV2, CheckpointLineageError> {
    validate_checkpoint_lineage(&checkpoint, task, plan, receipts, outcomes)?;
    Ok(VerifiedContextCheckpointV2 { checkpoint })
}

fn validate_outcome_links(
    task: &TaskEnvelopeV1,
    plan: Option<&ExecutionPlanV1>,
    receipts: &[VerifiedReceiptDocumentV1],
    outcomes: &[AcceptedOutcomeV1],
) -> Result<(), CheckpointLineageError> {
    if outcomes.len() > receipts.len() {
        return fail("outcome set contains entries without receipts".to_owned());
    }
    for outcome in outcomes {
        outcome.validate().map_err(protocol_error)?;
        if outcome.task_id != task.task_id {
            return fail("outcome task_id disagrees with task envelope".to_owned());
        }
        if outcome.plan_id.as_ref() != plan.map(|value| &value.plan_id) {
            return fail("outcome plan_id disagrees with execution plan".to_owned());
        }
        let Some(receipt_id) = outcome.receipt_id.as_ref() else {
            return fail("outcome must bind a receipt identity".to_owned());
        };
        let matching = receipts
            .iter()
            .filter(|receipt| receipt.receipt_id().as_str() == receipt_id.as_str())
            .collect::<Vec<_>>();
        if matching.len() != 1 {
            return fail(
                "outcome receipt_id does not identify exactly one verified receipt".to_owned(),
            );
        }
        if matching[0].document().outcome.state == AcceptanceState::Unknown {
            return fail("unknown receipt outcome cannot bind an AcceptedOutcomeV1".to_owned());
        }
    }

    let mut matched_outcomes = BTreeSet::new();
    for receipt in receipts {
        let document = receipt.document();
        let Some(outcome_id) = document.outcome.outcome_id.as_ref() else {
            if document.outcome.state == AcceptanceState::Unknown {
                continue;
            }
            return fail("terminal receipt outcome is missing outcome_id".to_owned());
        };
        let matching = outcomes
            .iter()
            .filter(|outcome| outcome.outcome_id == *outcome_id)
            .collect::<Vec<_>>();
        if matching.len() != 1 {
            return fail("receipt outcome link does not identify exactly one outcome".to_owned());
        }
        let outcome = matching[0];
        if !matched_outcomes.insert(outcome.outcome_id.as_str().to_owned()) {
            return fail("one outcome is linked by multiple receipts".to_owned());
        }
        if outcome.accepted != document.outcome.state
            || outcome.receipt_id.as_ref().map(ReceiptId::as_str)
                != Some(document.receipt_id.as_str())
        {
            return fail("receipt outcome state or receipt identity was mutated".to_owned());
        }
        let Some(outcome_ref) = document.outcome.outcome_ref.as_ref() else {
            return fail("terminal receipt outcome is missing outcome_ref".to_owned());
        };
        if !outcome
            .evidence_refs
            .iter()
            .any(|evidence| evidence.digest == outcome_ref.as_str())
        {
            return fail("outcome evidence does not bind receipt outcome_ref".to_owned());
        }
        if let Some(acceptance_ref) = document.outcome.acceptance_evidence_digest.as_ref()
            && !outcome
                .evidence_refs
                .iter()
                .any(|evidence| evidence.digest == acceptance_ref.as_str())
        {
            return fail("outcome evidence does not bind receipt acceptance evidence".to_owned());
        }
    }
    if matched_outcomes.len() != outcomes.len() {
        return fail("outcome set contains an unbound or foreign outcome".to_owned());
    }
    Ok(())
}

fn canonical_digest<T: Serialize>(value: &T) -> Result<Sha256Digest, CheckpointLineageError> {
    digest_bytes(&canonical::canonical_serialize(value))
}

fn digest_bytes(bytes: &[u8]) -> Result<Sha256Digest, CheckpointLineageError> {
    let mut value = String::from("sha256:");
    for byte in Sha256::digest(bytes) {
        use fmt::Write as _;
        write!(&mut value, "{byte:02x}").expect("writing to String cannot fail");
    }
    Sha256Digest::new(value).map_err(protocol_error)
}

pub(super) fn protocol_error(error: impl fmt::Display) -> CheckpointLineageError {
    CheckpointLineageError(error.to_string())
}

fn fail<T>(message: String) -> Result<T, CheckpointLineageError> {
    Err(CheckpointLineageError(message))
}

#[cfg(test)]
mod test_support;

#[cfg(test)]
mod tests {
    use super::*;
    use lean_ctx_protocol::{
        ContextCheckpointBranchIdV1, ContextCheckpointDeviceIdV1, ContextCheckpointIdV1,
        TaskEnvelopeV1, UtcTimestamp,
    };
    use serde_json::json;

    use super::test_support::checkpoint;

    fn make_task(task_id: &str) -> TaskEnvelopeV1 {
        serde_json::from_value(json!({
            "schema_version": 1,
            "task_id": task_id,
            "trace_id": "trace-1",
            "project_id": "project-1",
            "session_id": "session-1",
            "agent_id": "agent-1",
            "complexity": "medium",
            "created_at": "2026-08-23T11:59:00Z",
            "tenant_id": "tenant-1"
        }))
        .expect("valid task")
    }

    #[test]
    fn verified_receipt_rejects_tampered_signature() {
        let task = make_task("task-1");
        let fixture = crate::core::execution_protocol::test_support::build(&task);
        let mut tampered = fixture.protocol.receipt.clone();
        tampered.signature = base64::engine::general_purpose::STANDARD.encode([0_u8; 64]);
        let bytes = tampered
            .canonical_bytes()
            .expect("canonical tampered receipt");
        assert!(
            VerifiedReceiptDocumentV1::from_canonical_bytes(
                &bytes,
                &fixture.signer_admission,
                &fixture.verifying_key,
            )
            .is_err()
        );
    }

    #[test]
    fn builder_rejects_forged_task_plan_foreign_receipt_and_mutated_outcome() {
        let task = make_task("task-1");
        let fixture = crate::core::execution_protocol::test_support::build(&task);
        let receipt = VerifiedReceiptDocumentV1::from_canonical_bytes(
            &fixture
                .protocol
                .receipt
                .canonical_bytes()
                .expect("receipt bytes"),
            &fixture.signer_admission,
            &fixture.verifying_key,
        )
        .expect("verified receipt");
        let plan = &fixture.protocol.execution_plan;
        let outcome = fixture.protocol.accepted_outcome.clone();
        assert!(
            build_checkpoint_lineage(
                &task,
                Some(plan),
                std::slice::from_ref(&receipt),
                std::slice::from_ref(&outcome),
            )
            .is_ok()
        );

        let mut forged_task = task.clone();
        forged_task.intent = Some("different payload".to_owned());
        assert!(
            build_checkpoint_lineage(
                &forged_task,
                Some(plan),
                std::slice::from_ref(&receipt),
                std::slice::from_ref(&outcome),
            )
            .is_err()
        );

        let mut forged_plan = plan.clone();
        forged_plan.model = "different-model".to_owned();
        assert!(
            build_checkpoint_lineage(
                &task,
                Some(&forged_plan),
                std::slice::from_ref(&receipt),
                std::slice::from_ref(&outcome),
            )
            .is_err()
        );

        let foreign_task = make_task("task-foreign");
        let foreign_fixture = crate::core::execution_protocol::test_support::build(&foreign_task);
        let foreign_receipt = VerifiedReceiptDocumentV1::from_canonical_bytes(
            &foreign_fixture
                .protocol
                .receipt
                .canonical_bytes()
                .expect("foreign receipt bytes"),
            &foreign_fixture.signer_admission,
            &foreign_fixture.verifying_key,
        )
        .expect("verified foreign receipt");
        assert!(
            build_checkpoint_lineage(
                &task,
                Some(plan),
                &[foreign_receipt],
                std::slice::from_ref(&foreign_fixture.protocol.accepted_outcome),
            )
            .is_err()
        );

        let mut mutated_outcome = outcome;
        mutated_outcome.accepted = AcceptanceState::Rejected;
        assert!(
            build_checkpoint_lineage(
                &task,
                Some(plan),
                std::slice::from_ref(&receipt),
                std::slice::from_ref(&mutated_outcome),
            )
            .is_err()
        );
        assert!(build_checkpoint_lineage(&task, None, &[receipt], &[]).is_err());
    }

    #[test]
    fn checkpoint_builder_and_validator_bind_exact_receipt_ids() {
        let task = make_task("task-1");
        let fixture = crate::core::execution_protocol::test_support::build(&task);
        let receipt = VerifiedReceiptDocumentV1::from_canonical_bytes(
            &fixture
                .protocol
                .receipt
                .canonical_bytes()
                .expect("receipt bytes"),
            &fixture.signer_admission,
            &fixture.verifying_key,
        )
        .expect("verified receipt");
        let checkpoint = checkpoint(&task, &fixture.protocol.execution_plan.plan_id);
        let built = build_checkpoint_v2(
            &checkpoint,
            &task,
            Some(&fixture.protocol.execution_plan),
            std::slice::from_ref(&receipt),
            std::slice::from_ref(&fixture.protocol.accepted_outcome),
        )
        .expect("V2 checkpoint");
        assert_eq!(
            built.lineage.receipt_ids,
            vec![
                ReceiptId::try_from(receipt.receipt_id().as_str().to_owned())
                    .expect("receipt identity")
            ]
        );
        assert_eq!(
            built.lineage.artifact_lineage.receipt_refs,
            vec![receipt.canonical_digest().clone()]
        );
        validate_checkpoint_lineage(
            &built,
            &task,
            Some(&fixture.protocol.execution_plan),
            std::slice::from_ref(&receipt),
            std::slice::from_ref(&fixture.protocol.accepted_outcome),
        )
        .expect("lineage validates");

        assert!(
            !built
                .signing_payload()
                .expect("signing payload")
                .bytes()
                .is_empty()
        );
        let mut foreign = built.checkpoint().clone();
        foreign.lineage.artifact_lineage.task_ref =
            Sha256Digest::new(format!("sha256:{}", "f".repeat(64))).expect("digest");
        assert!(
            validate_checkpoint_lineage(
                &foreign,
                &task,
                Some(&fixture.protocol.execution_plan),
                std::slice::from_ref(&receipt),
                std::slice::from_ref(&fixture.protocol.accepted_outcome),
            )
            .is_err()
        );

        let mut tenantless_task = task.clone();
        tenantless_task.tenant_id = None;
        assert!(
            build_checkpoint_v2(
                &checkpoint,
                &tenantless_task,
                Some(&fixture.protocol.execution_plan),
                std::slice::from_ref(&receipt),
                std::slice::from_ref(&fixture.protocol.accepted_outcome),
            )
            .is_err()
        );
        assert!(
            validate_checkpoint_lineage(
                &built,
                &tenantless_task,
                Some(&fixture.protocol.execution_plan),
                std::slice::from_ref(&receipt),
                std::slice::from_ref(&fixture.protocol.accepted_outcome),
            )
            .is_err()
        );

        let mut wrong_plan = built.checkpoint().clone();
        let foreign_plan =
            lean_ctx_protocol::PlanId::try_from("plan-foreign".to_owned()).expect("plan id");
        wrong_plan.lineage.plan_id = Some(foreign_plan.clone());
        wrong_plan.live_state.task.plan_id = Some(foreign_plan);
        assert!(
            validate_checkpoint_lineage(
                &wrong_plan,
                &task,
                Some(&fixture.protocol.execution_plan),
                &[receipt],
                std::slice::from_ref(&fixture.protocol.accepted_outcome),
            )
            .is_err()
        );
    }

    #[test]
    fn verified_v2_branch_preserves_authoritative_artifact_lineage() {
        let task = make_task("task-1");
        let fixture = crate::core::execution_protocol::test_support::build(&task);
        let receipt = VerifiedReceiptDocumentV1::from_canonical_bytes(
            &fixture
                .protocol
                .receipt
                .canonical_bytes()
                .expect("receipt bytes"),
            &fixture.signer_admission,
            &fixture.verifying_key,
        )
        .expect("verified receipt");
        let checkpoint = checkpoint(&task, &fixture.protocol.execution_plan.plan_id);
        let parent = build_checkpoint_v2(
            &checkpoint,
            &task,
            Some(&fixture.protocol.execution_plan),
            std::slice::from_ref(&receipt),
            std::slice::from_ref(&fixture.protocol.accepted_outcome),
        )
        .expect("V2 checkpoint");
        let spec = super::super::ContextCheckpointChildSpecV1 {
            checkpoint_id: ContextCheckpointIdV1::try_from(
                "22222222-3333-4444-8555-666666666666".to_owned(),
            )
            .expect("checkpoint id"),
            branch_id: ContextCheckpointBranchIdV1::try_from("feature".to_owned())
                .expect("branch id"),
            device_id: ContextCheckpointDeviceIdV1::try_from("device-b".to_owned())
                .expect("device id"),
            device_sequence: 1,
            created_at: UtcTimestamp::new("2026-08-23T12:01:00Z").expect("timestamp"),
        };
        let child = super::super::branch_verified_checkpoint_v2(
            &parent,
            &spec,
            &task,
            Some(&fixture.protocol.execution_plan),
            std::slice::from_ref(&receipt),
            std::slice::from_ref(&fixture.protocol.accepted_outcome),
        )
        .expect("verified V2 branch");

        assert_eq!(
            child.lineage.artifact_lineage,
            parent.lineage.artifact_lineage
        );
        assert_eq!(
            child.identity.parent_checkpoint_id.as_ref(),
            Some(&parent.identity.checkpoint_id)
        );
        assert!(
            !child
                .signing_payload()
                .expect("signing payload")
                .bytes()
                .is_empty()
        );

        let merge_spec = super::super::ContextCheckpointChildSpecV1 {
            checkpoint_id: ContextCheckpointIdV1::try_from(
                "33333333-4444-4555-8666-777777777777".to_owned(),
            )
            .expect("checkpoint id"),
            branch_id: ContextCheckpointBranchIdV1::try_from("feature".to_owned())
                .expect("branch id"),
            device_id: ContextCheckpointDeviceIdV1::try_from("device-b".to_owned())
                .expect("device id"),
            device_sequence: 2,
            created_at: UtcTimestamp::new("2026-08-23T12:02:00Z").expect("timestamp"),
        };
        let merged = super::super::merge_verified_checkpoints_v2(
            &parent,
            &[],
            &child,
            &[],
            &parent,
            &merge_spec,
            super::super::ContextCheckpointConflictPolicyV1::Fail,
            &task,
            Some(&fixture.protocol.execution_plan),
            &[receipt],
            std::slice::from_ref(&fixture.protocol.accepted_outcome),
        )
        .expect("verified V2 merge");
        assert_eq!(
            merged.checkpoint().lineage.artifact_lineage,
            parent.lineage.artifact_lineage
        );
        assert_eq!(
            merged.receipt().right_digest(),
            &parent.digest().expect("parent digest")
        );
        assert!(
            merged
                .receipt()
                .signing_payload()
                .starts_with(b"leanctx/context-checkpoint-merge/v2\0")
        );
        assert!(
            !merged
                .receipt()
                .signing_payload()
                .starts_with(b"leanctx/context-checkpoint-merge/v1\0")
        );
    }
}
