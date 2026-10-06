//! Decision audit contract.

use crate::common::{
    DecisionId, ExtensionsV1, PlanId, TaskId, ValidationError, deserialize_schema_version,
    validate_bounded_string, validate_schema_version, validate_unique_strings,
};
use crate::evidence::EvidenceRefV1;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Kind of policy, routing, or execution decision being recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionKind {
    Routing,
    Scheduling,
    CapabilitySelection,
    ContextSelection,
    Policy,
    Fallback,
    Retry,
    Stop,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionStageV1 {
    Admission,
    Planning,
    Execution,
    Outcome,
}

/// Signed, evidence-linked record of one deterministic decision.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecisionRecordV1 {
    #[serde(deserialize_with = "deserialize_schema_version")]
    pub schema_version: u32,
    pub decision_id: DecisionId,
    pub task_id: TaskId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_id: Option<PlanId>,
    pub decision_stage: DecisionStageV1,
    pub decision_kind: DecisionKind,
    pub input_refs: Vec<String>,
    pub constraint_refs: Vec<String>,
    pub selected_result: Value,
    pub rationale_code: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rationale_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_ref: Option<String>,
    pub decision_system_name: String,
    pub decision_system_version: String,
    pub evidence_refs: Vec<EvidenceRefV1>,
    pub observed_at: String,
    pub signature: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supersedes: Option<DecisionId>,
    #[serde(default, flatten)]
    pub extensions: ExtensionsV1,
}

const DECISION_RESERVED_FIELDS: &[&str] = &[
    "schema_version",
    "decision_id",
    "task_id",
    "plan_id",
    "decision_stage",
    "decision_kind",
    "input_refs",
    "constraint_refs",
    "selected_result",
    "rationale_code",
    "rationale_ref",
    "policy_ref",
    "decision_system_name",
    "decision_system_version",
    "evidence_refs",
    "observed_at",
    "signature",
    "supersedes",
];

impl DecisionRecordV1 {
    /// Purpose/version separation for the Ed25519 decision-signature profile.
    pub const SIGNATURE_DOMAIN: &'static [u8] = b"lean-ctx:decision-record:v1\0";

    /// Compact UTF-8 JSON with recursively sorted object keys, omitting only
    /// `signature`. This is serialization, not signature or authority validation.
    /// Unknown extension fields remain covered by the signature payload.
    pub fn unsigned_canonical_bytes(&self) -> Result<Vec<u8>, ValidationError> {
        self.validate()?;
        let mut value = serde_json::to_value(self)
            .map_err(|error| ValidationError::new(format!("serialize decision record: {error}")))?;
        value
            .as_object_mut()
            .ok_or_else(|| ValidationError::new("decision record must serialize as an object"))?
            .remove("signature");
        serde_json::to_vec(&crate::receipt_document::sort_json(value))
            .map_err(|error| ValidationError::new(format!("canonicalize decision record: {error}")))
    }

    /// Versioned domain prefix followed by the unsigned canonical document.
    /// No key discovery, signing, verification, policy or I/O occurs here.
    pub fn signing_bytes(&self) -> Result<Vec<u8>, ValidationError> {
        let unsigned = self.unsigned_canonical_bytes()?;
        let mut bytes = Vec::with_capacity(Self::SIGNATURE_DOMAIN.len() + unsigned.len());
        bytes.extend_from_slice(Self::SIGNATURE_DOMAIN);
        bytes.extend_from_slice(&unsigned);
        Ok(bytes)
    }

    /// Validate schema invariants for a decision record.
    pub fn validate(&self) -> Result<(), ValidationError> {
        self.extensions
            .validate_reserved(DECISION_RESERVED_FIELDS)?;
        validate_schema_version(self.schema_version)?;
        validate_unique_strings(&self.input_refs, "input_refs")?;
        validate_unique_strings(&self.constraint_refs, "constraint_refs")?;
        validate_bounded_string(&self.rationale_code, "rationale_code")?;
        validate_bounded_string(&self.decision_system_name, "decision_system_name")?;
        validate_bounded_string(&self.decision_system_version, "decision_system_version")?;
        if self.evidence_refs.len() > crate::MAX_PROTOCOL_ITEMS {
            return Err(ValidationError::new("evidence_refs exceeds item limit"));
        }
        for (value, field) in [
            (&self.rationale_ref, "rationale_ref"),
            (&self.policy_ref, "policy_ref"),
        ] {
            if let Some(value) = value {
                validate_bounded_string(value, field)?;
            }
        }
        if self.decision_stage != DecisionStageV1::Admission && self.plan_id.is_none() {
            return Err(ValidationError::new(
                "post-admission decision requires plan_id",
            ));
        }
        for evidence in &self.evidence_refs {
            evidence.validate()?;
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

    fn decision() -> DecisionRecordV1 {
        DecisionRecordV1 {
            schema_version: 1,
            decision_id: id("decision-1"),
            task_id: id("task-1"),
            plan_id: Some(id("plan-1")),
            decision_stage: DecisionStageV1::Planning,
            decision_kind: DecisionKind::Routing,
            input_refs: vec!["input:task".to_owned()],
            constraint_refs: vec!["constraint:budget".to_owned()],
            selected_result: serde_json::json!({ "model": "model-1" }),
            rationale_code: "lowest_cost_within_slo".to_owned(),
            rationale_ref: Some("rationale:1".to_owned()),
            policy_ref: Some("policy:model".to_owned()),
            decision_system_name: "scheduler".to_owned(),
            decision_system_version: "1.0.0".to_owned(),
            evidence_refs: vec![],
            observed_at: "2026-08-09T12:00:00Z".to_owned(),
            signature: "signature".to_owned(),
            supersedes: None,
            extensions: Default::default(),
        }
    }

    #[test]
    fn serialization_round_trip() {
        let decision = decision();
        let json = serde_json::to_string(&decision).expect("decision should serialize");
        let decoded: DecisionRecordV1 =
            serde_json::from_str(&json).expect("decision should deserialize");
        assert_eq!(decision, decoded);
        decision
            .validate()
            .expect("decision should satisfy invariants");
    }

    #[test]
    fn signing_payload_is_domain_separated_and_omits_only_signature() {
        let mut record = decision();
        let unsigned = record.unsigned_canonical_bytes().unwrap();
        let payload = record.signing_bytes().unwrap();
        assert_eq!(
            payload,
            [DecisionRecordV1::SIGNATURE_DOMAIN, &unsigned].concat()
        );
        let value: Value = serde_json::from_slice(&unsigned).unwrap();
        assert!(value.get("signature").is_none());
        let mut expected = serde_json::to_value(&record).unwrap();
        expected.as_object_mut().unwrap().remove("signature");
        assert_eq!(value, expected);
        record.signature = "a different signature".into();
        assert_eq!(record.signing_bytes().unwrap(), payload);
        record.selected_result = serde_json::json!({"model":"different"});
        assert_ne!(record.signing_bytes().unwrap(), payload);
    }

    #[test]
    fn signing_payload_covers_extensions_and_recursively_sorts_objects() {
        let mut record = decision();
        record.selected_result = serde_json::from_str(r#"{"z":{"b":2,"a":1},"a":0}"#).unwrap();
        let before = record.signing_bytes().unwrap();
        record.selected_result = serde_json::from_str(r#"{"a":0,"z":{"a":1,"b":2}}"#).unwrap();
        assert_eq!(before, record.signing_bytes().unwrap());
        let encoded = String::from_utf8(record.unsigned_canonical_bytes().unwrap()).unwrap();
        assert!(encoded.contains(r#""selected_result":{"a":0,"z":{"a":1,"b":2}}"#));
        record
            .extensions
            .insert("audit_extra", serde_json::json!({"x":1}))
            .unwrap();
        assert_ne!(before, record.signing_bytes().unwrap());
    }
}
