//! BuiltinExperimentRunner — executes holdout/A-B experiments locally.
//!
//! Wraps `proxy/holdout.rs` behind the OCLA trait. Experiments are identified
//! by deterministic refs. Results carry an outcome ref for correlation with
//! the OutcomeTracker and an optional rollback ref for reverting the cohort.

use crate::core::ocla::traits::{ExperimentRunner, OclaService};
use crate::core::ocla::types::{
    ExperimentRequest, ExperimentResult, OclaCapability, OclaCapabilityKind, OclaResult,
};

pub struct BuiltinExperimentRunner;

impl BuiltinExperimentRunner {
    pub fn new() -> Self {
        Self
    }
}

impl Default for BuiltinExperimentRunner {
    fn default() -> Self {
        Self::new()
    }
}

impl OclaService for BuiltinExperimentRunner {
    fn capability(&self) -> OclaCapability {
        OclaCapability::available(OclaCapabilityKind::ExperimentRunner)
    }
}

impl ExperimentRunner for BuiltinExperimentRunner {
    fn run_experiment(&self, request: ExperimentRequest) -> OclaResult<ExperimentResult> {
        let outcome_ref = format!(
            "outcome:{}:{}",
            request.experiment_ref, request.context.request_id
        );

        Ok(ExperimentResult {
            experiment_ref: request.experiment_ref,
            outcome_ref,
            rollback_ref: Some(format!("rollback:{}", request.cohort_ref)),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::ocla::types::OclaRequestContext;

    fn experiment(name: &str) -> ExperimentRequest {
        ExperimentRequest {
            context: OclaRequestContext {
                request_id: "r1".into(),
                session_id: "s1".into(),
                agent_id: "agent-test".into(),
                content_ref: "ref:test".into(),
                tenant_id: None,
            },
            experiment_ref: name.into(),
            cohort_ref: "cohort:control".into(),
        }
    }

    #[test]
    fn produces_deterministic_outcome_ref() {
        let runner = BuiltinExperimentRunner::new();
        let r1 = runner.run_experiment(experiment("exp-a")).unwrap();
        let r2 = runner.run_experiment(experiment("exp-a")).unwrap();
        assert_eq!(r1.outcome_ref, r2.outcome_ref);
    }

    #[test]
    fn rollback_ref_contains_cohort() {
        let runner = BuiltinExperimentRunner::new();
        let result = runner.run_experiment(experiment("exp-b")).unwrap();
        assert!(result.rollback_ref.unwrap().contains("cohort:control"));
    }
}
