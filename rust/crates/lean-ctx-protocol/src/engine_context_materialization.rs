// SPDX-License-Identifier: Apache-2.0

//! Bounded materialization of an already-planned explicit source batch.
//!
//! The request carries the original source bodies because the planning
//! response intentionally contains descriptors only.  The binding digest is
//! an integrity join to that prior response, not an authorization grant or a
//! receipt.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt::Write as _;

use crate::{
    EngineContextSourcePlanRequestV1, EngineContextSourcePlanResponseV1, SemanticVersion,
    Sha256Digest, UtcTimestamp, ValidationError,
};

/// Keep the materialized context within the existing explicit-source bound.
pub const MAX_ENGINE_SOURCE_MATERIALIZED_CONTEXT_BYTES: usize =
    super::MAX_ENGINE_SOURCE_PLAN_REQUEST_BYTES;

/// Re-submit the bounded original source batch with the digest of the plan to
/// which the caller intends to bind materialization.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineContextSourceMaterializationRequestV1 {
    pub source_plan: EngineContextSourcePlanRequestV1,
    pub expected_binding_digest: Sha256Digest,
    /// Server-emitted identity time from a retention-enabled source plan.
    /// This is an unsigned replay hint, never authorization or provenance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub planning_evaluation_time: Option<UtcTimestamp>,
}

impl EngineContextSourceMaterializationRequestV1 {
    /// Validate the original source request and its serialized size before any
    /// policy planning or body joining occurs.
    pub fn validate_payload(&self) -> Result<(), ValidationError> {
        self.source_plan.validate_payload()?;
        let encoded = serde_json::to_vec(self)
            .map_err(|_| ValidationError::new("invalid Engine materialization request"))?;
        if encoded.len() > super::MAX_ENGINE_SOURCE_PLAN_REQUEST_BYTES {
            return Err(ValidationError::new(
                "Engine materialization source request exceeds size bound",
            ));
        }
        Ok(())
    }
}

/// Digest-bound context bytes selected by the canonical source planner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineContextSourceMaterializationResponseV1 {
    pub schema_version: u32,
    pub transport_version: u32,
    pub engine_interface_version: SemanticVersion,
    pub plan: EngineContextSourcePlanResponseV1,
    pub materialized_digest: Sha256Digest,
    pub materialized_token_count: u64,
    pub content: String,
}

impl EngineContextSourceMaterializationResponseV1 {
    /// Validate the additive response envelope and the digest of its final
    /// policy-enforced materialized bytes.
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.schema_version != 1
            || self.transport_version != 1
            || self.engine_interface_version.as_str() != "1.0.0"
            || self.content.len() > MAX_ENGINE_SOURCE_MATERIALIZED_CONTEXT_BYTES
            || self.materialized_token_count > self.plan.result.plan.budget_tokens
        {
            return Err(ValidationError::new(
                "invalid Engine materialization response envelope",
            ));
        }
        self.plan.validate_binding()?;
        let actual = sha256_digest(self.content.as_bytes())?;
        if actual != self.materialized_digest {
            return Err(ValidationError::new(
                "Engine materialized content digest mismatch",
            ));
        }
        Ok(())
    }
}

fn sha256_digest(bytes: &[u8]) -> Result<Sha256Digest, ValidationError> {
    let mut digest = String::from("sha256:");
    for byte in Sha256::digest(bytes).iter() {
        write!(&mut digest, "{byte:02x}")
            .map_err(|_| ValidationError::new("Engine materialized digest encoding failed"))?;
    }
    Sha256Digest::new(digest)
}
