// SPDX-License-Identifier: Apache-2.0
//! Unicode checkpoint signing remains behind the existing artifact admission join.

use lean_ctx_protocol::{
    AcceptedOutcomeV1, CONTEXT_CHECKPOINT_V3_SIGNATURE_DOMAIN, ContextCheckpointV3,
    ExecutionPlanV1, TaskEnvelopeV1,
};

use super::lineage::{
    CheckpointLineageError, VerifiedReceiptDocumentV1, protocol_error, validate_artifact_lineage,
};

/// Structurally valid Unicode text does not itself authorize a checkpoint signature.
pub(crate) struct VerifiedContextCheckpointV3 {
    checkpoint: ContextCheckpointV3,
}

impl VerifiedContextCheckpointV3 {
    pub(crate) fn checkpoint(&self) -> &ContextCheckpointV3 {
        &self.checkpoint
    }

    pub(crate) fn signing_bytes(&self) -> Result<Vec<u8>, CheckpointLineageError> {
        let mut bytes = CONTEXT_CHECKPOINT_V3_SIGNATURE_DOMAIN.to_vec();
        bytes.extend(self.checkpoint.canonical_bytes().map_err(protocol_error)?);
        Ok(bytes)
    }
}

pub(crate) fn verify_checkpoint_v3(
    checkpoint: ContextCheckpointV3,
    task: &TaskEnvelopeV1,
    plan: Option<&ExecutionPlanV1>,
    receipts: &[VerifiedReceiptDocumentV1],
    outcomes: &[AcceptedOutcomeV1],
) -> Result<VerifiedContextCheckpointV3, CheckpointLineageError> {
    checkpoint.validate().map_err(protocol_error)?;
    validate_artifact_lineage(&checkpoint.lineage, task, plan, receipts, outcomes)?;
    Ok(VerifiedContextCheckpointV3 { checkpoint })
}
