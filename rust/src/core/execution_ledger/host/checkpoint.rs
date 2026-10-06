// SPDX-License-Identifier: Apache-2.0
//! Canonical continuation from authenticated artifacts, not a parallel state store.

use anyhow::{Result, ensure};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use ed25519_dalek::{Signer as _, VerifyingKey};
use lean_ctx_protocol::{
    AcceptanceState, AcceptedOutcomeV1, ContextCheckpointV1, ContextCheckpointV2,
    ContextCheckpointV3, EvidenceKind, EvidenceRefV1, ExecutionPlanV1, ReceiptId, Sha256Digest,
    SignatureStatus, TaskEnvelopeV1,
};
use serde::Deserialize;

use super::{
    HostReceiptAuthority, ReceiptSignerAdmissionV1, canonical_serialize, digest, outcome, task_lock,
};
use crate::core::{
    context_checkpoint::{
        VerifiedReceiptDocumentV1, build_checkpoint_v2, verify_checkpoint_v2, verify_checkpoint_v3,
    },
    engine_interface::persist_engine_artifact_content,
    execution_ledger::ExecutionEvent,
};

/// Bound shared by local lineage and operator-transferred evidence sets.
pub(super) const MAX_CHECKPOINT_RECEIPTS: usize = 64;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HostCheckpointRequest {
    pub schema_version: u32,
    pub checkpoint_json: String,
    pub receipt_digests: Vec<Sha256Digest>,
}

impl HostReceiptAuthority {
    /// Read the just-published receipt through the existing verified lineage owner.
    /// This is not a historical lookup or a caller-supplied artifact-path API.
    pub(crate) fn published_receipt_json(
        &self,
        publication: &crate::core::execution_ledger::PublishedCanonicalReceipt,
    ) -> Result<String> {
        self.validate_current().map_err(anyhow::Error::msg)?;
        let digest = Sha256Digest::new(publication.receipt_digest.clone())?;
        // Validate the supplied publication's canonical location before the
        // common lineage reader admits its signature and ledger membership.
        let (_, original) = publication.read_canonical()?;
        self.with_checkpoint_lineage(std::slice::from_ref(&digest), |_, _, receipts, _| {
            let receipt = receipts
                .first()
                .ok_or_else(|| anyhow::anyhow!("receipt missing"))?;
            ensure!(
                receipt.canonical_bytes() == original,
                "receipt bytes changed"
            );
            self.validate_current().map_err(anyhow::Error::msg)?;
            String::from_utf8(original).map_err(Into::into)
        })
    }

    pub(crate) fn export_checkpoint(
        &self,
        request: &HostCheckpointRequest,
    ) -> Result<serde_json::Value> {
        self.validate_current().map_err(anyhow::Error::msg)?;
        ensure!(
            self.allow_checkpoint_signing,
            "checkpoint signing not authorized"
        );
        ensure!(
            matches!(request.schema_version, 1..=3)
                && !request.receipt_digests.is_empty()
                && request.receipt_digests.len() <= MAX_CHECKPOINT_RECEIPTS,
            "invalid checkpoint request"
        );
        ensure!(
            crate::core::secret_detection::detect_secrets(&request.checkpoint_json).is_empty(),
            "checkpoint contains credential-shaped material"
        );
        self.with_checkpoint_lineage(
            &request.receipt_digests,
            |task, plan, receipts, outcomes| {
                let (checkpoint, checkpoint_digest, payload, envelope_version, result_version) =
                    if request.schema_version == 3 {
                        let checkpoint = ContextCheckpointV3::from_canonical_bytes(
                            request.checkpoint_json.as_bytes(),
                        )?;
                        let verified =
                            verify_checkpoint_v3(checkpoint, task, Some(plan), receipts, outcomes)?;
                        (
                            serde_json::to_value(verified.checkpoint())?,
                            verified.checkpoint().digest()?,
                            verified.signing_bytes()?,
                            "leanctx.host-checkpoint/v2",
                            "leanctx.host-checkpoint-result/v2",
                        )
                    } else {
                        let verified = if request.schema_version == 1 {
                            let checkpoint = ContextCheckpointV1::from_canonical_bytes(
                                request.checkpoint_json.as_bytes(),
                            )?;
                            build_checkpoint_v2(&checkpoint, task, Some(plan), receipts, outcomes)?
                        } else {
                            // Reauthorize exact continued V2 against this host's artifacts;
                            // never demote it or treat a package signature as admission.
                            let checkpoint = ContextCheckpointV2::from_canonical_bytes(
                                request.checkpoint_json.as_bytes(),
                            )?;
                            verify_checkpoint_v2(checkpoint, task, Some(plan), receipts, outcomes)?
                        };
                        (
                            serde_json::to_value(verified.checkpoint())?,
                            verified.digest()?,
                            verified.signing_payload()?.bytes().to_vec(),
                            "leanctx.host-checkpoint/v1",
                            "leanctx.host-checkpoint-result/v1",
                        )
                    };
                self.validate_current().map_err(anyhow::Error::msg)?;
                let envelope = serde_json::json!({
                    "schema_version":envelope_version,
                    "checkpoint":checkpoint,
                    "signer":{"key_id":self.signer_admission.key_id,
                        "public_key_digest":self.signer_admission.public_key_digest},
                    "signature":STANDARD.encode(self.signing_key.sign(&payload).to_bytes()),
                });
                let bytes = canonical_serialize(&envelope);
                ensure!(
                    bytes.len() <= 1024 * 1024,
                    "checkpoint artifact exceeds read bound"
                );
                let hash = digest(&bytes).map_err(anyhow::Error::msg)?;
                drop(
                    persist_engine_artifact_content(
                        "execution/checkpoints",
                        hash.hex(),
                        "json",
                        &bytes,
                    )
                    .map_err(anyhow::Error::msg)?,
                );
                Ok(serde_json::json!({"schema_version":result_version,
                "artifact_digest":hash,"checkpoint_digest":checkpoint_digest,
                "artifact":envelope,"session_adopted":false}))
            },
        )
    }

    /// Keep one receipt/head/lineage authority and retain its lock through the consumer.
    pub(super) fn with_checkpoint_lineage<T>(
        &self,
        hashes: &[Sha256Digest],
        consume: impl FnOnce(
            &TaskEnvelopeV1,
            &ExecutionPlanV1,
            &[VerifiedReceiptDocumentV1],
            &[AcceptedOutcomeV1],
        ) -> Result<T>,
    ) -> Result<T> {
        let receipts = verify_receipts(
            hashes,
            &self.signer_admission,
            &self.signing_key.verifying_key(),
            &|hash| Ok(outcome::publication(hash)?.read_canonical()?.1),
        )?;
        let first = receipts
            .first()
            .ok_or_else(|| anyhow::anyhow!("missing receipt"))?;
        let (task, plan) = read_task_and_plan(first, &LocalCheckpointEvidence)?;
        let _lock = task_lock(&task).map_err(anyhow::Error::msg)?;
        let events = self.ledger.by_task_verified(task.task_id.as_str())?;
        for receipt in &receipts {
            ensure!(
                events.iter().any(|event| matches!(event,
                ExecutionEvent::CanonicalReceiptRecorded { receipt_id, receipt_digest, .. }
                if receipt_id == receipt.receipt_id().as_str()
                    && receipt_digest == receipt.canonical_digest().as_str())),
                "receipt not recorded by this host"
            );
        }
        let head = self
            .ledger
            .canonical_receipt_for_task_verified(task.task_id.as_str())?
            .ok_or_else(|| anyhow::anyhow!("host receipt missing"))?;
        ensure!(
            receipts
                .iter()
                .any(|receipt| receipt.canonical_digest().as_str() == head.receipt_digest),
            "checkpoint omits the current receipt"
        );
        let outcomes = reconstruct_outcomes(&plan, &receipts, &LocalCheckpointEvidence)?;
        consume(&task, &plan, &receipts, &outcomes)
    }
}

/// Digest-addressed evidence the checkpoint verifier requires.
///
/// One rule set serves this host's artifact store and an operator-transferred
/// package, so a transferred package can never satisfy a weaker evidence
/// requirement than a local resume.
pub(super) trait CheckpointEvidence {
    fn canonical_bytes(&self, expected: &Sha256Digest) -> Result<Vec<u8>>;
}

/// This host's already admitted evidence artifacts.
pub(super) struct LocalCheckpointEvidence;

impl CheckpointEvidence for LocalCheckpointEvidence {
    fn canonical_bytes(&self, expected: &Sha256Digest) -> Result<Vec<u8>> {
        outcome::read_bytes(expected)
    }
}

fn read_evidence_value<T: serde::de::DeserializeOwned>(
    evidence: &dyn CheckpointEvidence,
    expected: &Sha256Digest,
) -> Result<T> {
    Ok(serde_json::from_slice(
        &evidence.canonical_bytes(expected)?,
    )?)
}

/// Verify every receipt in the requested set against one admitted signer.
/// The reader supplies bytes; the signature, identity and admission checks are
/// the protocol verifier's, never the reader's.
pub(super) fn verify_receipts(
    hashes: &[Sha256Digest],
    admission: &ReceiptSignerAdmissionV1,
    verifying_key: &VerifyingKey,
    read: &dyn Fn(&Sha256Digest) -> Result<Vec<u8>>,
) -> Result<Vec<VerifiedReceiptDocumentV1>> {
    ensure!(
        !hashes.is_empty() && hashes.len() <= MAX_CHECKPOINT_RECEIPTS,
        "invalid receipt set"
    );
    let mut receipts = Vec::with_capacity(hashes.len());
    for hash in hashes {
        receipts.push(VerifiedReceiptDocumentV1::from_canonical_bytes(
            &read(hash)?,
            admission,
            verifying_key,
        )?);
    }
    Ok(receipts)
}

/// Read the exact task and plan the first verified receipt binds.
pub(super) fn read_task_and_plan(
    first: &VerifiedReceiptDocumentV1,
    evidence: &dyn CheckpointEvidence,
) -> Result<(TaskEnvelopeV1, ExecutionPlanV1)> {
    let task: TaskEnvelopeV1 = read_evidence_value(evidence, &first.document().lineage.task_ref)?;
    let plan: ExecutionPlanV1 = read_evidence_value(evidence, &first.document().lineage.plan_ref)?;
    Ok((task, plan))
}

/// Reconstruct the immutable outcome snapshots the signed receipts bind.
pub(super) fn reconstruct_outcomes(
    plan: &ExecutionPlanV1,
    receipts: &[VerifiedReceiptDocumentV1],
    evidence: &dyn CheckpointEvidence,
) -> Result<Vec<AcceptedOutcomeV1>> {
    let mut outcomes = Vec::new();
    for receipt in receipts {
        let document = receipt.document();
        if document.outcome.state == AcceptanceState::Unknown {
            continue;
        }
        let reference = document
            .outcome
            .outcome_ref
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("outcome reference missing"))?;
        // The signed receipt binds this immutable evaluation snapshot. Only
        // cyclic links added by its owner after signing are reconstructed.
        let mut value: AcceptedOutcomeV1 = read_evidence_value(evidence, reference)?;
        ensure!(
            value.outcome_id.as_str()
                == document
                    .outcome
                    .outcome_id
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("outcome identity missing"))?
                    .as_str()
                && value.accepted == document.outcome.state
                && value.receipt_id.is_none()
                && value.plan_id.is_none(),
            "unsupported outcome snapshot"
        );
        value.plan_id = Some(plan.plan_id.clone());
        value.receipt_id = Some(ReceiptId::new(document.receipt_id.as_str())?);
        value.evidence_refs.push(EvidenceRefV1 {
            schema_version: Some(1),
            kind: EvidenceKind::QualityMeasurement,
            uri: format!("artifact://execution/evidence/{}", reference.hex()),
            digest: reference.as_str().into(),
            signature_status: SignatureStatus::NotSigned,
            media_type: Some("application/json".into()),
            extensions: Default::default(),
        });
        outcomes.push(value);
    }
    Ok(outcomes)
}
