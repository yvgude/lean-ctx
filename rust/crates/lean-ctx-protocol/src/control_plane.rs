//! Model and context-selection control-plane contracts.
//!
//! `ContextBundleV1` is retained only for historical wire compatibility. It
//! does not grant the Engine Product context-selection authority.

use crate::{CapabilityManifestV1, ContextBundleV1, TaskComplexity, TaskEnvelopeV1};
use serde::{Deserialize, Serialize};

/// Inputs supplied to a control plane for one task-routing decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControlPlaneRequest {
    pub task_envelope: TaskEnvelopeV1,
    /// Frozen legacy Product bundle; removal follows the knowledge-routing gate.
    pub context_bundle: ContextBundleV1,
    pub available_capabilities: Vec<CapabilityManifestV1>,
}

/// A selected provider, model, and resource allocation for a task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControlPlaneDecision {
    pub selected_model: String,
    pub selected_provider: String,
    pub reasoning_budget: u64,
    pub context_adjustments: Vec<String>,
    pub confidence: u16,
}

/// Extension point for Enterprise control planes such as `AdaptiveControlPlane`.
pub trait ControlPlaneContract {
    /// Select a model, provider, and deterministic execution parameters.
    fn decide(&self, request: ControlPlaneRequest) -> ControlPlaneDecision;
}

/// OSS rule-based control plane with no remote policy dependency.
#[derive(Debug, Default, Clone, Copy)]
pub struct LocalControlPlane;

impl ControlPlaneContract for LocalControlPlane {
    fn decide(&self, request: ControlPlaneRequest) -> ControlPlaneDecision {
        let selected_provider = request
            .available_capabilities
            .iter()
            .map(|capability| capability.provider.as_str())
            .min()
            .unwrap_or("local")
            .to_owned();
        let reasoning_budget = match request.task_envelope.complexity {
            TaskComplexity::Unknown | TaskComplexity::Low => 1,
            TaskComplexity::Medium => 2,
            TaskComplexity::High => 3,
            TaskComplexity::Critical => 4,
        };
        let confidence = match request.task_envelope.complexity {
            TaskComplexity::Unknown => 500,
            TaskComplexity::Low => 900,
            TaskComplexity::Medium => 800,
            TaskComplexity::High => 700,
            TaskComplexity::Critical => 600,
        };

        ControlPlaneDecision {
            selected_model: request
                .task_envelope
                .model_policy_ref
                .unwrap_or_else(|| "local/default".to_owned()),
            selected_provider,
            reasoning_budget,
            context_adjustments: Vec::new(),
            confidence,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AgentId, ProjectId, SessionId, TaskId, TraceId};

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String>,
        <T as TryFrom<String>>::Error: std::fmt::Debug,
    {
        T::try_from(value.to_owned()).expect("identifier should be valid")
    }

    fn request() -> ControlPlaneRequest {
        ControlPlaneRequest {
            task_envelope: TaskEnvelopeV1 {
                schema_version: 1,
                task_id: id::<TaskId>("task-1"),
                trace_id: id::<TraceId>("trace-1"),
                project_id: id::<ProjectId>("project-1"),
                session_id: id::<SessionId>("session-1"),
                agent_id: id::<AgentId>("agent-1"),
                complexity: TaskComplexity::Medium,
                created_at: "2026-08-12T00:00:00Z".to_owned(),
                parent_task_id: None,
                tenant_id: None,
                intent: None,
                task_class: None,
                risk_class: None,
                quality_requirement_milli: None,
                cost_budget_micros: None,
                latency_budget_ms: None,
                data_classification: None,
                region_policy_ref: None,
                model_policy_ref: None,
                context_state_ref: None,
                outcome_contract_ref: None,
            },
            context_bundle: ContextBundleV1::default(),
            available_capabilities: Vec::new(),
        }
    }

    #[test]
    fn local_control_plane_is_deterministic() {
        let control_plane = LocalControlPlane;
        assert_eq!(
            control_plane.decide(request()),
            control_plane.decide(request())
        );
    }

    #[test]
    fn contract_is_object_safe() {
        let control_plane: &dyn ControlPlaneContract = &LocalControlPlane;
        assert_eq!(control_plane.decide(request()).selected_provider, "local");
    }
}
