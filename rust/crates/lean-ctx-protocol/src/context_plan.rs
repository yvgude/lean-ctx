// SPDX-License-Identifier: Apache-2.0

//! Deterministic wire projection of the context kernel's semantic plan.

use crate::common::{
    ContextPlanId, ExtensionsV1, TaskId, ValidationError, deserialize_schema_version,
    validate_bounded_string, validate_schema_version,
};
use crate::entitlement::sort_json;
use crate::evidence::EvidenceRefV1;
use crate::identity::Sha256Digest;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

/// Stable disposition assigned to a context candidate by the semantic planner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextDispositionV1 {
    Selected,
    Excluded,
    Deferred,
}

/// Stable, machine-readable reason for a context selection decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextReasonCodeV1 {
    Relevant,
    Required,
    CacheHit,
    BudgetExceeded,
    LowerUtility,
    PolicyExcluded,
    DeferredForLater,
    Other,
}

/// One selected, excluded, or deferred context object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextSelectionV1 {
    pub source_ref: String,
    pub provider: String,
    pub disposition: ContextDispositionV1,
    pub token_count: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256_digest: Option<String>,
    pub reason_codes: Vec<ContextReasonCodeV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason_detail: Option<String>,
}

/// Deterministic aggregate accounting for one context provider.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextProviderStatsV1 {
    pub candidates_offered: u64,
    pub candidates_selected: u64,
    pub tokens_used: u64,
}

/// Public projection of a context plan. The context kernel remains semantic owner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextPlanProjectionV1 {
    #[serde(deserialize_with = "deserialize_schema_version")]
    pub schema_version: u32,
    pub context_plan_id: ContextPlanId,
    pub task_id: TaskId,
    /// Optional SHA-256 digest of the canonical projection without this field.
    /// Old V1 readers may omit it; new projectors always populate it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projection_digest: Option<Sha256Digest>,
    pub budget_tokens: u64,
    pub selections: Vec<ContextSelectionV1>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub provider_stats: BTreeMap<String, ContextProviderStatsV1>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub policy_decision_refs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<EvidenceRefV1>,
    #[serde(default, flatten)]
    pub extensions: ExtensionsV1,
}

const CONTEXT_PLAN_RESERVED_FIELDS: &[&str] = &[
    "schema_version",
    "context_plan_id",
    "task_id",
    "projection_digest",
    "budget_tokens",
    "selections",
    "provider_stats",
    "policy_decision_refs",
    "evidence",
];

impl ContextPlanProjectionV1 {
    /// Compute the SHA-256 digest over canonical projection content.
    ///
    /// The digest field is excluded from its own input, allowing consumers to
    /// verify a projection without a second unsigned envelope.
    pub fn compute_projection_digest(&self) -> Result<Sha256Digest, ValidationError> {
        let mut value = serde_json::to_value(self).map_err(|error| {
            ValidationError::new(format!("serialize context projection: {error}"))
        })?;
        let object = value.as_object_mut().ok_or_else(|| {
            ValidationError::new("context projection must serialize as an object")
        })?;
        object.remove("projection_digest");
        let bytes = serde_json::to_vec(&sort_json(value))
            .map_err(|error| ValidationError::new(format!("encode context projection: {error}")))?;
        let hex = Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        Sha256Digest::new(format!("sha256:{hex}"))
    }

    /// Validate projection bounds, joins, and deterministic uniqueness invariants.
    pub fn validate(&self) -> Result<(), ValidationError> {
        if let Some(actual_digest) = &self.projection_digest {
            let expected_digest = self.compute_projection_digest()?;
            if actual_digest != &expected_digest {
                return Err(ValidationError::new(
                    "projection_digest does not match canonical projection content",
                ));
            }
        }
        self.extensions
            .validate_reserved(CONTEXT_PLAN_RESERVED_FIELDS)?;
        validate_schema_version(self.schema_version)?;
        if self.selections.len() > crate::MAX_PROTOCOL_ITEMS {
            return Err(ValidationError::new("selections exceeds item limit"));
        }
        let mut sources = BTreeSet::new();
        let mut selected_tokens = 0_u64;
        for selection in &self.selections {
            validate_bounded_string(&selection.source_ref, "source_ref")?;
            validate_bounded_string(&selection.provider, "provider")?;
            if !sources.insert(selection.source_ref.as_str()) {
                return Err(ValidationError::new("duplicate context source_ref"));
            }
            if selection.reason_codes.is_empty() {
                return Err(ValidationError::new(
                    "context selection requires a reason_code",
                ));
            }
            if selection.reason_codes.len() > crate::MAX_PROTOCOL_ITEMS
                || selection.reason_codes.iter().collect::<BTreeSet<_>>().len()
                    != selection.reason_codes.len()
            {
                return Err(ValidationError::new(
                    "context reason_codes exceeds item limit or contains duplicates",
                ));
            }
            if let Some(detail) = &selection.reason_detail {
                validate_bounded_string(detail, "reason_detail")?;
            }
            if let Some(digest) = &selection.sha256_digest {
                let digest = digest.strip_prefix("sha256:").unwrap_or(digest);
                if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                    return Err(ValidationError::new("sha256_digest must be SHA-256"));
                }
            }
            if selection.disposition == ContextDispositionV1::Selected {
                selected_tokens = selected_tokens
                    .checked_add(selection.token_count)
                    .ok_or_else(|| ValidationError::new("selected token count overflow"))?;
            }
        }
        if selected_tokens > self.budget_tokens {
            return Err(ValidationError::new(
                "selected context exceeds budget_tokens",
            ));
        }
        if self.provider_stats.len() > crate::MAX_PROTOCOL_ITEMS {
            return Err(ValidationError::new("provider_stats exceeds item limit"));
        }
        for (provider, stats) in &self.provider_stats {
            validate_bounded_string(provider, "provider_stats key")?;
            if stats.candidates_selected > stats.candidates_offered {
                return Err(ValidationError::new(
                    "provider candidates_selected exceeds candidates_offered",
                ));
            }
        }
        crate::validate_unique_strings(&self.policy_decision_refs, "policy_decision_refs")?;
        if self.evidence.len() > crate::MAX_PROTOCOL_ITEMS {
            return Err(ValidationError::new("evidence exceeds item limit"));
        }
        for evidence in &self.evidence {
            evidence.validate()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::ContextPlanProjectionV1;
    use crate::Sha256Digest;

    #[test]
    fn projection_digest_is_canonical_across_serde_json_map_features() {
        let projection: ContextPlanProjectionV1 = serde_json::from_str(
            r#"{"budget_tokens":0,"context_plan_id":"plan-1","schema_version":1,"selections":[{"disposition":"deferred","provider":"provider-1","reason_codes":["deferred_for_later"],"source_ref":"source-1","token_count":0}],"task_id":"task-1"}"#,
        )
        .unwrap();

        // The golden includes the nested selection's canonical object order.
        assert_eq!(
            projection.compute_projection_digest().unwrap(),
            Sha256Digest::new(
                "sha256:4df9c96aec0ef95ac5fda9d1d1cf6a826c3ea8acbfeaa254b64d02394740e568"
                    .to_owned(),
            )
            .unwrap()
        );
    }
}
