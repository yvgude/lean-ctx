//! Accepted-outcome contract.

use crate::common::{
    ExtensionsV1, OutcomeId, PlanId, ReceiptId, TaskId, ValidationError,
    deserialize_optional_milliunit, deserialize_schema_version, validate_milliunit,
    validate_schema_version, validate_unique_strings,
};
use crate::evidence::EvidenceRefV1;
use serde::{Deserialize, Serialize};

/// Tri-state acceptance prevents an absent observation from being treated as a rejection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AcceptanceState {
    Accepted,
    Rejected,
    Unknown,
}

/// State of an individual completion or quality signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalState {
    Passed,
    Failed,
    Unknown,
    NotRun,
}

/// Verification signals attached to an accepted-outcome observation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutcomeSignalsV1 {
    pub build: Option<SignalState>,
    pub tests: Option<SignalState>,
    pub lint: Option<SignalState>,
    pub typecheck: Option<SignalState>,
    pub completion: Option<SignalState>,
    pub pr: Option<SignalState>,
    pub correction: Option<SignalState>,
    pub rollback: Option<SignalState>,
    pub retry: Option<SignalState>,
}

/// Canonical outcome observation used for acceptance and efficiency accounting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptedOutcomeV1 {
    #[serde(deserialize_with = "deserialize_schema_version")]
    pub schema_version: u32,
    pub outcome_id: OutcomeId,
    pub task_id: TaskId,
    pub accepted: AcceptanceState,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_optional_milliunit"
    )]
    pub quality_score_milli: Option<u16>,
    pub signals: OutcomeSignalsV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contract_ref: Option<String>,
    pub evidence_refs: Vec<EvidenceRefV1>,
    pub observed_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_id: Option<PlanId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt_id: Option<ReceiptId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decision_refs: Vec<String>,
    #[serde(default, flatten)]
    pub extensions: ExtensionsV1,
}

const OUTCOME_RESERVED_FIELDS: &[&str] = &[
    "schema_version",
    "outcome_id",
    "task_id",
    "accepted",
    "quality_score_milli",
    "signals",
    "contract_ref",
    "evidence_refs",
    "observed_at",
    "plan_id",
    "receipt_id",
    "decision_refs",
];

impl AcceptedOutcomeV1 {
    /// Validate invariants that also apply to values constructed in Rust.
    pub fn validate(&self) -> Result<(), ValidationError> {
        self.extensions.validate_reserved(OUTCOME_RESERVED_FIELDS)?;
        validate_schema_version(self.schema_version)?;
        if let Some(value) = self.quality_score_milli {
            validate_milliunit(value, "quality_score_milli")?;
        }
        validate_unique_strings(&self.decision_refs, "decision_refs")?;
        if self.evidence_refs.len() > crate::MAX_PROTOCOL_ITEMS {
            return Err(ValidationError::new("evidence_refs exceeds item limit"));
        }
        if let Some(contract_ref) = &self.contract_ref {
            crate::validate_bounded_string(contract_ref, "contract_ref")?;
        }
        for evidence in &self.evidence_refs {
            evidence.validate()?;
        }
        if self.accepted != AcceptanceState::Unknown && self.evidence_refs.is_empty() {
            return Err(ValidationError::new(
                "accepted or rejected outcome requires evidence",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        <T as TryFrom<String>>::Error: std::fmt::Debug,
    {
        T::try_from(value.to_owned()).expect("identifier should be valid")
    }

    #[test]
    fn serialization_round_trip() {
        let outcome = AcceptedOutcomeV1 {
            schema_version: 1,
            outcome_id: id("outcome-1"),
            task_id: id("task-1"),
            accepted: AcceptanceState::Accepted,
            quality_score_milli: Some(950),
            signals: OutcomeSignalsV1 {
                build: Some(SignalState::Passed),
                tests: Some(SignalState::Passed),
                lint: Some(SignalState::Passed),
                typecheck: Some(SignalState::Passed),
                completion: Some(SignalState::Passed),
                pr: Some(SignalState::NotRun),
                correction: Some(SignalState::NotRun),
                rollback: Some(SignalState::NotRun),
                retry: Some(SignalState::Failed),
            },
            contract_ref: Some("contract:outcome".to_owned()),
            evidence_refs: vec![EvidenceRefV1 {
                schema_version: Some(1),
                kind: crate::EvidenceKind::QualityMeasurement,
                uri: "urn:evidence:outcome-1".to_owned(),
                digest: "a".repeat(64),
                signature_status: crate::SignatureStatus::NotSigned,
                media_type: Some("application/json".to_owned()),
                extensions: Default::default(),
            }],
            observed_at: "2026-08-09T12:00:00Z".to_owned(),
            plan_id: Some(id("plan-1")),
            receipt_id: Some(id("receipt-1")),
            decision_refs: vec!["decision:acceptance".to_owned()],
            extensions: Default::default(),
        };
        let json = serde_json::to_string(&outcome).expect("outcome should serialize");
        let decoded: AcceptedOutcomeV1 =
            serde_json::from_str(&json).expect("outcome should deserialize");
        assert_eq!(outcome, decoded);
        outcome
            .validate()
            .expect("outcome should satisfy invariants");
    }
}
