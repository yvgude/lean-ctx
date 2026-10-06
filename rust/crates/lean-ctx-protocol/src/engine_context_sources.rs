// SPDX-License-Identifier: Apache-2.0

//! Explicit source-batch planning for an operator-owned Engine process.
//! Producer permission labels can restrict input, never authorize a remote caller.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    ContextDispositionV1, DataClassification, EngineContextPlanRequestV1,
    EngineContextPlanResponseV1, ProtocolReference, Sha256Digest, SourceId, UtcTimestamp,
    ValidationError,
};

pub const MAX_ENGINE_SOURCE_PLAN_REQUEST_BYTES: usize = 1024 * 1024;
pub const MAX_ENGINE_SOURCE_PLAN_SOURCES: usize = 64;
pub const MAX_ENGINE_SOURCE_CONTENT_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EngineContextSourceTypeV1 {
    Filesystem,
    IssueTracker,
    RelationalDatabase,
    Other,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EngineContextSourcePermissionV1 {
    Permitted,
    Denied,
    #[default]
    Unknown,
}

/// Factual source binding. Missing metadata is unknown, not verified/current.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineContextSourceDescriptorV1 {
    pub object_ref: ProtocolReference,
    pub source_id: SourceId,
    pub source_type: EngineContextSourceTypeV1,
    pub content_digest: Sha256Digest,
    pub revision: Option<ProtocolReference>,
    pub owner: Option<ProtocolReference>,
    pub observed_at: Option<UtcTimestamp>,
    pub valid_until: Option<UtcTimestamp>,
    pub classification: Option<DataClassification>,
    #[serde(default)]
    pub permission: EngineContextSourcePermissionV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineContextSourceV1 {
    pub descriptor: EngineContextSourceDescriptorV1,
    pub content: String,
}

/// A separate operation keeps the existing strict local-store request unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineContextSourcePlanRequestV1 {
    pub planning: EngineContextPlanRequestV1,
    pub sources: Vec<EngineContextSourceV1>,
}

impl EngineContextSourcePlanRequestV1 {
    pub fn validate_payload(&self) -> Result<(), ValidationError> {
        self.planning.validate_payload()?;
        if self.sources.len() > MAX_ENGINE_SOURCE_PLAN_SOURCES {
            return Err(ValidationError::new("too many Engine source candidates"));
        }
        let mut references = BTreeSet::new();
        for source in &self.sources {
            if source.content.len() > MAX_ENGINE_SOURCE_CONTENT_BYTES
                || source.content.trim().is_empty()
                || !references.insert(&source.descriptor.object_ref)
                || source.descriptor.content_digest != sha256_digest(source.content.as_bytes())?
                || matches!((&source.descriptor.observed_at, &source.descriptor.valid_until),
                    (Some(observed), Some(until)) if until <= observed)
            {
                return Err(ValidationError::new("invalid Engine source binding"));
            }
        }
        Ok(())
    }
}

/// Metadata for selected candidates only. No source body or execution receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineContextSourcePlanResponseV1 {
    pub result: EngineContextPlanResponseV1,
    pub source_bindings: Vec<EngineContextSourceDescriptorV1>,
    /// Integrity of the result and ordered bindings, not an authentication signature.
    pub binding_digest: Sha256Digest,
}

impl EngineContextSourcePlanResponseV1 {
    pub fn new(
        result: EngineContextPlanResponseV1,
        mut source_bindings: Vec<EngineContextSourceDescriptorV1>,
    ) -> Result<Self, ValidationError> {
        source_bindings.sort_by(|left, right| left.object_ref.cmp(&right.object_ref));
        let binding_digest = binding_digest(&result, &source_bindings)?;
        let response = Self {
            result,
            source_bindings,
            binding_digest,
        };
        response.validate_binding()?;
        Ok(response)
    }

    pub fn validate_binding(&self) -> Result<(), ValidationError> {
        self.result.plan.validate()?;
        if self.result.schema_version != 1
            || self.result.transport_version != 1
            || self.result.engine_interface_version.as_str() != "1.0.0"
            || self.result.plan.projection_digest.is_none()
            || self
                .source_bindings
                .windows(2)
                .any(|pair| pair[0].object_ref >= pair[1].object_ref)
        {
            return Err(ValidationError::new(
                "invalid Engine source response envelope",
            ));
        }
        let selected = self
            .result
            .plan
            .selections
            .iter()
            .filter(|selection| selection.disposition == ContextDispositionV1::Selected)
            .collect::<Vec<_>>();
        if selected.len() != self.source_bindings.len()
            || self.source_bindings.iter().any(|binding| {
                !selected.iter().any(|selection| {
                    selection.source_ref == binding.object_ref.as_str()
                        && selection.provider == binding.source_id.as_str()
                        && selection.sha256_digest.as_deref()
                            == Some(binding.content_digest.as_str())
                })
            })
        {
            return Err(ValidationError::new(
                "Engine source selection binding mismatch",
            ));
        }
        if binding_digest(&self.result, &self.source_bindings)? != self.binding_digest {
            return Err(ValidationError::new(
                "Engine source binding digest mismatch",
            ));
        }
        Ok(())
    }
}

fn binding_digest(
    result: &EngineContextPlanResponseV1,
    sources: &[EngineContextSourceDescriptorV1],
) -> Result<Sha256Digest, ValidationError> {
    let value = serde_json::to_value((result, sources))
        .map_err(|_| ValidationError::new("invalid Engine source binding encoding"))?;
    let bytes = serde_json::to_vec(&crate::entitlement::sort_json(value))
        .map_err(|_| ValidationError::new("invalid Engine source binding encoding"))?;
    sha256_digest(&bytes)
}

fn sha256_digest(bytes: &[u8]) -> Result<Sha256Digest, ValidationError> {
    let hex = Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Sha256Digest::new(format!("sha256:{hex}"))
}
