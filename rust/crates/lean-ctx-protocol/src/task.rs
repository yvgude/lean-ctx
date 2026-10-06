//! Task admission and lineage contract.

use crate::common::{
    AgentId, ExtensionsV1, ProjectId, SessionId, TaskId, TenantId, TraceId, ValidationError,
    deserialize_optional_milliunit, deserialize_schema_version, validate_milliunit,
    validate_schema_version,
};
use crate::experiment::DataClassification;
use serde::{Deserialize, Serialize};

/// Coarse task complexity used by policy and scheduling decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskComplexity {
    Unknown,
    Low,
    Medium,
    High,
    Critical,
}

/// Risk class used for safety-sensitive routing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskClass {
    Low,
    Medium,
    High,
    Critical,
}

/// Canonical task envelope for a V1 execution lineage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskEnvelopeV1 {
    #[serde(deserialize_with = "deserialize_schema_version")]
    pub schema_version: u32,
    pub task_id: TaskId,
    pub trace_id: TraceId,
    pub project_id: ProjectId,
    pub session_id: SessionId,
    pub agent_id: AgentId,
    pub complexity: TaskComplexity,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_task_id: Option<TaskId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<TenantId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_class: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risk_class: Option<RiskClass>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_optional_milliunit"
    )]
    pub quality_requirement_milli: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_budget_micros: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_budget_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_classification: Option<DataClassification>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region_policy_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_policy_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_state_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome_contract_ref: Option<String>,
    /// Unknown additive V1 fields retained for lossless forwarding.
    #[serde(default, flatten)]
    pub extensions: ExtensionsV1,
}

const TASK_RESERVED_FIELDS: &[&str] = &[
    "schema_version",
    "task_id",
    "trace_id",
    "project_id",
    "session_id",
    "agent_id",
    "complexity",
    "created_at",
    "parent_task_id",
    "tenant_id",
    "intent",
    "task_class",
    "risk_class",
    "quality_requirement_milli",
    "cost_budget_micros",
    "latency_budget_ms",
    "data_classification",
    "region_policy_ref",
    "model_policy_ref",
    "context_state_ref",
    "outcome_contract_ref",
];

impl TaskEnvelopeV1 {
    /// Schema version represented by this type.
    pub const SCHEMA_VERSION: u32 = 1;

    /// Compact UTF-8 JSON with recursively sorted object keys. These bytes
    /// retain all additive fields and identify the complete task, not just its ID.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, ValidationError> {
        self.validate()?;
        let value = serde_json::to_value(self)
            .map_err(|error| ValidationError::new(format!("serialize task envelope: {error}")))?;
        serde_json::to_vec(&crate::receipt_document::sort_json(value))
            .map_err(|error| ValidationError::new(format!("canonicalize task envelope: {error}")))
    }

    /// Validate invariants that also apply to values constructed in Rust.
    pub fn validate(&self) -> Result<(), ValidationError> {
        self.extensions.validate_reserved(TASK_RESERVED_FIELDS)?;
        validate_schema_version(self.schema_version)?;
        if self.parent_task_id.as_ref() == Some(&self.task_id) {
            return Err(ValidationError::new("task cannot be its own parent"));
        }
        if let Some(value) = self.quality_requirement_milli {
            validate_milliunit(value, "quality_requirement_milli")?;
        }
        if self.parent_task_id.as_ref() == Some(&self.task_id) {
            return Err(ValidationError::new("task cannot be its own parent"));
        }
        for (value, field) in [
            (&self.intent, "intent"),
            (&self.task_class, "task_class"),
            (&self.region_policy_ref, "region_policy_ref"),
            (&self.model_policy_ref, "model_policy_ref"),
            (&self.context_state_ref, "context_state_ref"),
            (&self.outcome_contract_ref, "outcome_contract_ref"),
        ] {
            if let Some(value) = value {
                crate::validate_bounded_string(value, field)?;
            }
        }
        Ok(())
    }

    /// Validate that this envelope is a child of `parent` in the same trace.
    pub fn validate_child_of(&self, parent: &Self) -> Result<(), ValidationError> {
        self.validate()?;
        parent.validate()?;
        if self.parent_task_id.as_ref() != Some(&parent.task_id) {
            return Err(ValidationError::new(
                "child parent_task_id does not match the parent task_id",
            ));
        }
        if self.trace_id != parent.trace_id {
            return Err(ValidationError::new(
                "child task must retain the parent trace_id",
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
        let task = TaskEnvelopeV1 {
            schema_version: 1,
            task_id: id("task-1"),
            trace_id: id("trace-1"),
            project_id: id("project-1"),
            session_id: id("session-1"),
            agent_id: id("agent-1"),
            complexity: TaskComplexity::Medium,
            created_at: "2026-08-09T12:00:00Z".to_owned(),
            parent_task_id: None,
            tenant_id: Some(id("tenant-1")),
            intent: Some("implement".to_owned()),
            task_class: Some("coding".to_owned()),
            risk_class: Some(RiskClass::Low),
            quality_requirement_milli: Some(900),
            cost_budget_micros: Some(10_000),
            latency_budget_ms: Some(2_000),
            data_classification: Some(DataClassification::Internal),
            region_policy_ref: Some("policy:region".to_owned()),
            model_policy_ref: Some("policy:model".to_owned()),
            context_state_ref: Some("context:state".to_owned()),
            outcome_contract_ref: Some("contract:outcome".to_owned()),
            extensions: Default::default(),
        };
        let json = serde_json::to_string(&task).expect("task should serialize");
        let decoded: TaskEnvelopeV1 = serde_json::from_str(&json).expect("task should deserialize");
        assert_eq!(task, decoded);
        task.validate().expect("task should satisfy invariants");
    }

    #[test]
    fn quality_requirement_is_bounded() {
        let json = r#"{
            "schema_version": 1,
            "task_id": "task-1",
            "trace_id": "trace-1",
            "project_id": "project-1",
            "session_id": "session-1",
            "agent_id": "agent-1",
            "complexity": "low",
            "created_at": "2026-08-09T12:00:00Z",
            "quality_requirement_milli": 1001
        }"#;
        assert!(serde_json::from_str::<TaskEnvelopeV1>(json).is_err());
    }

    #[test]
    fn extension_reservation_is_scoped_to_this_dto() {
        let mut task: TaskEnvelopeV1 = serde_json::from_value(serde_json::json!({
            "schema_version": 1,
            "task_id": "task-1",
            "trace_id": "trace-1",
            "project_id": "project-1",
            "session_id": "session-1",
            "agent_id": "agent-1",
            "complexity": "low",
            "created_at": "2026-08-09T12:00:00Z"
        }))
        .expect("task should deserialize");
        task.extensions
            .insert("provider", serde_json::Value::from("future"))
            .expect("provider is not a task field");
        task.validate()
            .expect("cross-DTO key should remain available");
        task.extensions
            .insert("task_id", serde_json::Value::from("shadow"))
            .expect("insert is checked by the owning DTO");
        assert!(task.validate().is_err());
    }

    #[test]
    fn self_parent_is_rejected_by_task_and_child_validation() {
        let mut task: TaskEnvelopeV1 = serde_json::from_value(serde_json::json!({
            "schema_version": 1,
            "task_id": "task-1",
            "trace_id": "trace-1",
            "project_id": "project-1",
            "session_id": "session-1",
            "agent_id": "agent-1",
            "complexity": "low",
            "created_at": "2026-08-09T12:00:00Z"
        }))
        .expect("valid fixture");
        let parent = task.clone();
        task.parent_task_id = Some(task.task_id.clone());
        assert!(task.validate().is_err());
        assert!(task.validate_child_of(&parent).is_err());

        task.task_id = id("task-2");
        assert!(task.validate_child_of(&parent).is_ok());

        let mut invalid_parent = parent;
        invalid_parent.parent_task_id = Some(invalid_parent.task_id.clone());
        assert!(task.validate_child_of(&invalid_parent).is_err());
    }
}
