// SPDX-License-Identifier: Apache-2.0

//! Typed transport for operator-attested outcomes.
//!
//! This module carries bounded request/response data only.  It does not admit
//! signer keys, authenticate an actor, verify a receipt signature, or create a
//! second outcome ledger; those authorities remain in the native host.

use serde::{Deserialize, Serialize};

use crate::engine_context_source_execution::sha256_digest;
use crate::{
    AcceptanceState, AgentId, ReceiptDocumentV1, SemanticVersion, Sha256Digest, TaskId, TenantId,
    ValidationError, deserialize_schema_version,
};

/// The bounded request transport limit used by the existing Engine process
/// boundary.
pub const MAX_ENGINE_OUTCOME_REQUEST_BYTES: usize = 1024 * 1024;

/// A public request cannot carry completion/agent-lifecycle evidence or caller
/// metadata; the host turns these attestations into its existing local signal
/// values after authenticating and binding the receipt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EngineOutcomeSignalTypeV1 {
    BuildSuccess,
    TestsPassing,
    LintClean,
    TypecheckPassing,
    HumanAcceptance,
    PrMerge,
    CiPassing,
    Correction,
    Rollback,
}

/// Values preserve the existing evaluator's semantics: Boolean values pass
/// when true, Count values pass when greater than zero, and Unknown never
/// passes.  Count is u32 to match the native local signal representation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EngineOutcomeSignalValueV1 {
    Boolean(bool),
    Count(u32),
    Unknown,
}

/// One explicit operator attestation with no evidence path or caller time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineOutcomeSignalV1 {
    pub signal_type: EngineOutcomeSignalTypeV1,
    pub value: EngineOutcomeSignalValueV1,
}

/// Server-owned identity expectations checked against the signed task loaded
/// by the host.  These fields are bindings, not authentication claims.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineOutcomeBindingV1 {
    pub task_id: TaskId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<TenantId>,
    pub agent_id: AgentId,
}

/// Strict V1 request for the existing host outcome authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineOutcomeRequestV1 {
    #[serde(deserialize_with = "deserialize_schema_version")]
    pub schema_version: u32,
    pub transport_version: u32,
    pub engine_interface_version: SemanticVersion,
    pub receipt_digest: Sha256Digest,
    pub context_decision_digest: Sha256Digest,
    pub binding: EngineOutcomeBindingV1,
    pub signals: Vec<EngineOutcomeSignalV1>,
}

impl EngineOutcomeRequestV1 {
    /// Validate transport identity and bounded, metadata-free attestations.
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.schema_version != 1
            || self.transport_version != 1
            || self.engine_interface_version.as_str() != "1.0.0"
            || self.signals.is_empty()
            || self.signals.len() > 16
        {
            return Err(invalid_request());
        }
        bounded_wire_size(self, MAX_ENGINE_OUTCOME_REQUEST_BYTES)
    }
}

/// Versioned result carrying the exact canonical successor receipt bytes.
///
/// Receipt parsing below proves canonical shape, content identity, and the
/// requested outcome state only.  It intentionally does not admit the signer;
/// the existing HostReceiptAuthority performs that cryptographic/ledger check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineOutcomeResponseV1 {
    #[serde(deserialize_with = "deserialize_schema_version")]
    pub schema_version: u32,
    pub receipt_id: Sha256Digest,
    pub receipt_digest: Sha256Digest,
    /// Digest of the initial unknown receipt supplied in the request.  This is
    /// an explicit transport join, not an independent cryptographic link.
    pub original_receipt_digest: Sha256Digest,
    pub acceptance: AcceptanceState,
    pub already_recorded: bool,
    pub receipt_document_json: String,
}

impl EngineOutcomeResponseV1 {
    /// Validate exact canonical UTF-8 receipt bytes and response bounds.
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.schema_version != 1
            || self.acceptance == AcceptanceState::Unknown
            || self.receipt_document_json.len() > crate::MAX_ENGINE_SOURCE_RECEIPT_DOCUMENT_BYTES
        {
            return Err(invalid_response());
        }
        let bytes = self.receipt_document_json.as_bytes();
        if sha256_digest(bytes)? != self.receipt_digest {
            return Err(invalid_response());
        }
        let document = ReceiptDocumentV1::from_canonical_bytes(bytes)?;
        if document.receipt_id != self.receipt_id
            || document.outcome.state != self.acceptance
            || document.chain.previous_receipt_id.is_none()
        {
            return Err(invalid_response());
        }
        let encoded = serde_json::to_vec(self).map_err(|_| invalid_response())?;
        if encoded.len() > crate::MAX_ENGINE_SOURCE_EXECUTION_V2_RESPONSE_BYTES {
            return Err(invalid_response());
        }
        Ok(())
    }

    /// Validate the server-owned task and planning-evidence joins available on
    /// the response wire.  Signature admission and previous-receipt lineage
    /// remain host responsibilities.
    pub fn validate_against(
        &self,
        request: &EngineOutcomeRequestV1,
    ) -> Result<(), ValidationError> {
        request.validate()?;
        self.validate()?;
        if self.original_receipt_digest != request.receipt_digest {
            return Err(invalid_response());
        }
        let document =
            ReceiptDocumentV1::from_canonical_bytes(self.receipt_document_json.as_bytes())?;
        if document.lineage.task_id != request.binding.task_id {
            return Err(invalid_response());
        }
        let runtime_refs: Vec<_> = document
            .evidence_refs
            .iter()
            .filter(|entry| entry.kind == crate::ReceiptEvidenceKindV1::Runtime)
            .collect();
        if runtime_refs.is_empty()
            || !runtime_refs.iter().all(|entry| {
                entry.digest == request.context_decision_digest
                    && entry.uri.as_str()
                        == format!(
                            "artifact://execution/evidence/{}",
                            request.context_decision_digest.hex()
                        )
            })
        {
            return Err(invalid_response());
        }
        Ok(())
    }
}

fn bounded_wire_size<T: Serialize>(value: &T, limit: usize) -> Result<(), ValidationError> {
    let encoded = serde_json::to_vec(value).map_err(|_| invalid_request())?;
    if encoded.len() > limit {
        return Err(invalid_request());
    }
    Ok(())
}

fn invalid_request() -> ValidationError {
    ValidationError::new("invalid Engine outcome request")
}

fn invalid_response() -> ValidationError {
    ValidationError::new("Engine outcome response does not bind a canonical receipt")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> EngineOutcomeRequestV1 {
        EngineOutcomeRequestV1 {
            schema_version: 1,
            transport_version: 1,
            engine_interface_version: SemanticVersion::new("1.0.0").expect("version"),
            receipt_digest: Sha256Digest::new(format!("sha256:{}", "a".repeat(64)))
                .expect("digest"),
            context_decision_digest: Sha256Digest::new(format!("sha256:{}", "b".repeat(64)))
                .expect("digest"),
            binding: EngineOutcomeBindingV1 {
                task_id: TaskId::new("task-1").expect("task"),
                tenant_id: Some(TenantId::new("tenant-1").expect("tenant")),
                agent_id: AgentId::new("agent-1").expect("agent"),
            },
            signals: vec![EngineOutcomeSignalV1 {
                signal_type: EngineOutcomeSignalTypeV1::HumanAcceptance,
                value: EngineOutcomeSignalValueV1::Boolean(true),
            }],
        }
    }

    #[test]
    fn request_rejects_missing_attestation_and_unknown_authority_fields() {
        let mut value = request();
        assert!(value.validate().is_ok());
        let mut json = serde_json::to_value(&value).expect("request");
        json["learn"] = serde_json::json!(true);
        assert!(serde_json::from_value::<EngineOutcomeRequestV1>(json).is_err());
        value.signals.clear();
        assert!(value.validate().is_err());
    }

    #[test]
    fn wire_rejects_forbidden_completion_signal() {
        let json = r#"{
            "schema_version":1,
            "transport_version":1,
            "engine_interface_version":"1.0.0",
            "receipt_digest":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "context_decision_digest":"sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "binding":{"task_id":"task-1","tenant_id":"tenant-1","agent_id":"agent-1"},
            "signals":[{"signal_type":"agent_completion","value":{"boolean":true}}]
        }"#;
        assert!(serde_json::from_str::<EngineOutcomeRequestV1>(json).is_err());
    }
}
