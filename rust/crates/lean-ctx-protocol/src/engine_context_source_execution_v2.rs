// SPDX-License-Identifier: Apache-2.0

//! Explicit v2 delivery of exact signed bytes; wire joins do not establish key trust.

use serde::{Deserialize, Serialize};

use crate::engine_context_source_execution::{canonical_sha256, sha256_digest};
use crate::{
    AcceptanceState, EngineContextSourceExecutionRequestV1, EngineContextSourceExecutionResponseV1,
    ReceiptDocumentV1, ReceiptEvidenceKindV1, ReceiptTerminalStatusV1, ValidationError,
};

/// The existing canonical receipt reader's byte limit, applied before decoding.
pub const MAX_ENGINE_SOURCE_RECEIPT_DOCUMENT_BYTES: usize = 1024 * 1024;
/// Total JSON transport bound, including escaping of the exact UTF-8 document.
pub const MAX_ENGINE_SOURCE_EXECUTION_V2_RESPONSE_BYTES: usize = 4 * 1024 * 1024;

/// Opt-in v2 response: the unchanged v1 execution and exact persisted canonical JSON.
///
/// `receipt_document_json.as_bytes()` are the signed document bytes, not a
/// re-rendered object. Consumers must independently admit the signer and verify
/// the Ed25519 signature; neither a digest nor this validator grants trust.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineContextSourceExecutionResponseV2 {
    pub schema_version: u32,
    pub execution: EngineContextSourceExecutionResponseV1,
    pub receipt_document_json: String,
}

impl EngineContextSourceExecutionResponseV2 {
    /// Validate byte identity and signed-lineage joins, not signer admission.
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.schema_version != 2
            || self.receipt_document_json.len() > MAX_ENGINE_SOURCE_RECEIPT_DOCUMENT_BYTES
        {
            return Err(invalid());
        }
        self.execution.validate()?;
        let bytes = self.receipt_document_json.as_bytes();
        let projection = &self.execution.canonical_receipt;
        if sha256_digest(bytes)? != projection.receipt_digest {
            return Err(invalid());
        }
        let document = ReceiptDocumentV1::from_canonical_bytes(bytes)?;
        let lineage = &document.lineage;
        let plan = &self.execution.execution_plan;
        let invocation = &self.execution.invocation;
        if document.receipt_id.as_str() != projection.receipt_id.as_str()
            || document.outcome.state != AcceptanceState::Unknown
            || document.status != ReceiptTerminalStatusV1::Succeeded
            || lineage.task_id != plan.task_id
            || lineage.plan_id != plan.plan_id
            || lineage.plan_ref != canonical_sha256(plan)?
            || lineage.invocation_id != invocation.invocation_id.as_str()
            || lineage.invocation_ref != canonical_sha256(invocation)?
            || !invocation.source_refs.iter().any(|reference| {
                reference.as_str() == format!("task:{}", lineage.task_ref.as_str())
            })
        {
            return Err(invalid());
        }
        let native = self
            .execution
            .observation
            .receipt_link
            .as_ref()
            .ok_or_else(invalid)?;
        let observation_digest = canonical_sha256(&self.execution.observation)?;
        for (uri, digest) in [
            (
                format!("artifact://engine/receipts/{}", native.receipt_digest.hex()),
                &native.receipt_digest,
            ),
            ("artifact://engine/observation".into(), &observation_digest),
        ] {
            if !document.evidence_refs.iter().any(|evidence| {
                evidence.uri.as_str() == uri
                    && &evidence.digest == digest
                    && evidence.kind == ReceiptEvidenceKindV1::Measurement
            }) {
                return Err(invalid());
            }
        }
        let encoded = serde_json::to_vec(self).map_err(|_| invalid())?;
        if encoded.len() > MAX_ENGINE_SOURCE_EXECUTION_V2_RESPONSE_BYTES {
            return Err(invalid());
        }
        Ok(())
    }

    /// Retain every v1 request/source/task join before admitting the document.
    pub fn validate_against(
        &self,
        request: &EngineContextSourceExecutionRequestV1,
    ) -> Result<(), ValidationError> {
        self.execution.validate_against(request)?;
        self.validate()
    }
}

fn invalid() -> ValidationError {
    ValidationError::new("source execution v2 does not bind the exact canonical receipt")
}
