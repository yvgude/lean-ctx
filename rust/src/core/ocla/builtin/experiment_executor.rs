// SPDX-License-Identifier: Apache-2.0
//! BuiltinExperimentExecutor — the OCLA `ExperimentRunner` capability slot.
//!
//! The engine decides and runs no experiments (founder F3: execution belongs to
//! the host). The slot stays registered so the capability catalogue and the
//! `lean-ctx-ocla` API keep their shape; every request is rejected.

use crate::core::ocla::traits::{ExperimentRunner, OclaService};
use crate::core::ocla::types::{
    ExperimentRequest, ExperimentResult, OclaCapability, OclaCapabilityKind, OclaResult,
};

pub struct BuiltinExperimentExecutor;

impl BuiltinExperimentExecutor {
    pub fn new() -> Self {
        Self
    }
}

impl Default for BuiltinExperimentExecutor {
    fn default() -> Self {
        Self::new()
    }
}

impl OclaService for BuiltinExperimentExecutor {
    fn manifest(&self) -> crate::core::ocla::OclaResult<lean_ctx_protocol::CapabilityManifestV1> {
        crate::core::ocla::capability_fabric::builtin_manifest(&self.capability())
    }

    fn capability(&self) -> OclaCapability {
        OclaCapability::available(OclaCapabilityKind::ExperimentRunner)
    }
}

impl ExperimentRunner for BuiltinExperimentExecutor {
    /// The built-in runner has no suite of its own to run. Its only suite was
    /// the model-routing evaluation, removed with automatic routing.
    fn run_experiment(&self, _request: ExperimentRequest) -> OclaResult<ExperimentResult> {
        Err(crate::core::ocla::types::OclaError::Rejected(
            OclaCapabilityKind::ExperimentRunner,
            "the built-in experiment runner has no experiment suite".into(),
        ))
    }
}

/// Backward-compatible name for callers using the previous runner API.
pub type BuiltinExperimentRunner = BuiltinExperimentExecutor;

#[cfg(test)]
mod tests {
    use super::BuiltinExperimentExecutor;
    use crate::core::ocla::traits::ExperimentRunner;
    use crate::core::ocla::types::{ExperimentRequest, OclaRequestContext};

    fn experiment(name: &str) -> ExperimentRequest {
        ExperimentRequest {
            context: OclaRequestContext {
                request_id: "r1".into(),
                session_id: "s1".into(),
                agent_id: "agent-test".into(),
                content_ref: "ref:test".into(),
                tenant_id: None,
                trace_id: "tr-unit".into(),
                task_id: None,
                parent_task_id: None,
            },
            experiment_ref: name.into(),
            cohort_ref: "cohort:control".into(),
            holdout: None,
            stop_conditions: None,
        }
    }

    #[test]
    fn invalid_request_never_returns_synthetic_refs() {
        let runner = BuiltinExperimentExecutor::new();
        let result = runner.run_experiment(experiment("exp-b"));
        assert!(result.is_err());
    }

    #[test]
    fn registry_builtins_route_experiment_requests_to_runner() {
        let registry = crate::core::ocla::registry::OclaRegistry::with_builtins();
        let result = registry
            .experiment_runner
            .run_experiment(experiment("/definitely/missing-suite.ndjson"));
        assert!(result.is_err());
    }
}
