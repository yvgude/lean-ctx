// SPDX-License-Identifier: Apache-2.0
//! BuiltinExperimentExecutor — executes experiment assignments locally.
//!
//! Wraps `proxy/holdout.rs` behind the OCLA trait. Experiments are identified
//! by deterministic refs. Results carry an outcome ref for correlation with
//! the OutcomeTracker and an optional rollback ref for reverting the cohort.

use crate::core::ocla::traits::{ExperimentRunner, OclaService};
use crate::core::ocla::types::{
    ExperimentOutcome, ExperimentRequest, ExperimentResult, ExperimentStopConditions,
    OclaCapability, OclaCapabilityKind, OclaResult,
};
pub use lean_ctx_protocol::{
    CurrencyCode, DataClassification, ExperimentArm, ExperimentAssignmentV1, MoneyV1,
    SideEffectPolicy,
};

/// Executes a deterministic bucketing rule from a signed assignment.
/// The runtime does NOT decide experiments — it executes assignments.
pub(crate) fn execute_bucketing_rule(seed: &str, subject: &str, holdout_pct: u8) -> bool {
    let mut hasher = blake3::Hasher::new();
    hasher.update(seed.as_bytes());
    hasher.update(subject.as_bytes());
    let hash = hasher.finalize();
    let bucket = u64::from_le_bytes(
        hash.as_bytes()[..8]
            .try_into()
            .expect("blake3 hash is at least 8 bytes"),
    ) % 100;
    bucket < u64::from(holdout_pct)
}

/// Tracks experiment state for stop-condition evaluation.
struct ExperimentState {
    samples: u64,
    started_at: std::time::Instant,
    treatment_sum: f64,
    control_sum: f64,
    treatment_count: u64,
    control_count: u64,
}

impl ExperimentState {
    fn new() -> Self {
        Self {
            samples: 0,
            started_at: std::time::Instant::now(),
            treatment_sum: 0.0,
            control_sum: 0.0,
            treatment_count: 0,
            control_count: 0,
        }
    }

    fn should_stop(&self, conditions: &ExperimentStopConditions) -> Option<String> {
        if conditions
            .max_samples
            .is_some_and(|max| self.samples >= max)
        {
            return Some("max_samples".into());
        }
        if conditions
            .max_duration_secs
            .is_some_and(|max| self.started_at.elapsed().as_secs() >= max)
        {
            return Some("max_duration_secs".into());
        }
        if let Some(min_improvement_pct) = conditions.min_improvement_pct {
            let outcome = self.outcome("");
            if self.control_count > 0
                && self.treatment_count > 0
                && outcome.improvement_pct < f64::from(min_improvement_pct)
            {
                return Some("min_improvement_pct".into());
            }
        }
        None
    }

    fn record_sample(&mut self, is_holdout: bool, metric: f64) {
        self.samples += 1;
        if is_holdout {
            self.control_sum += metric;
            self.control_count += 1;
        } else {
            self.treatment_sum += metric;
            self.treatment_count += 1;
        }
    }

    fn outcome(&self, experiment_ref: &str) -> ExperimentOutcome {
        let treatment_metric = average(self.treatment_sum, self.treatment_count);
        let control_metric = average(self.control_sum, self.control_count);
        let improvement_pct = if self.control_count == 0 || control_metric == 0.0 {
            0.0
        } else {
            (treatment_metric - control_metric) / control_metric * 100.0
        };
        ExperimentOutcome {
            experiment_ref: experiment_ref.into(),
            treatment_samples: self.treatment_count,
            control_samples: self.control_count,
            treatment_metric,
            control_metric,
            improvement_pct,
            stopped_reason: None,
            is_significant: self.treatment_count > 0
                && self.control_count > 0
                && treatment_metric != control_metric,
        }
    }
}

fn average(sum: f64, count: u64) -> f64 {
    if count == 0 { 0.0 } else { sum / count as f64 }
}

pub struct BuiltinExperimentExecutor;

impl BuiltinExperimentExecutor {
    pub fn new() -> Self {
        Self
    }

    /// Computes the outcome of an executed experiment arm.
    /// Called AFTER execution to report results back to the sidecar.
    pub fn compute_outcome(
        &self,
        request: &ExperimentRequest,
        metric_fn: impl Fn(&str) -> f64,
    ) -> OclaResult<ExperimentOutcome> {
        let holdout_samples = request
            .holdout
            .as_ref()
            .and_then(|holdout| holdout.max_samples);
        let stop_samples = request
            .stop_conditions
            .as_ref()
            .and_then(|conditions| conditions.max_samples);
        let sample_count = match (holdout_samples, stop_samples) {
            (Some(holdout), Some(stop)) => holdout.min(stop),
            (Some(samples), None) | (None, Some(samples)) => samples,
            (None, None) => 1,
        };
        let mut state = ExperimentState::new();
        let mut stopped_reason = None;

        for sample in 0..sample_count {
            let request_ref = format!("{}:{sample}", request.context.request_id);
            let is_holdout = request.holdout.as_ref().is_some_and(|holdout| {
                execute_bucketing_rule(&holdout.assignment_seed, &request_ref, holdout.holdout_pct)
            });
            state.record_sample(is_holdout, metric_fn(&request_ref));
            if let Some(conditions) = request.stop_conditions.as_ref()
                && let Some(reason) = state.should_stop(conditions)
            {
                stopped_reason = Some(reason);
                break;
            }
        }

        let mut outcome = state.outcome(&request.experiment_ref);
        outcome.stopped_reason = stopped_reason;
        Ok(outcome)
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
    /// The built-in runner executes signed assignments (see
    /// [`execute_assignment`]); it has no suite of its own to run. Its only
    /// suite was the model-routing evaluation, removed with automatic routing.
    fn run_experiment(&self, _request: ExperimentRequest) -> OclaResult<ExperimentResult> {
        Err(crate::core::ocla::types::OclaError::Rejected(
            OclaCapabilityKind::ExperimentRunner,
            "the built-in experiment runner has no experiment suite".into(),
        ))
    }
}

/// Backward-compatible name for callers using the previous runner API.
pub type BuiltinExperimentRunner = BuiltinExperimentExecutor;

/// Validates and executes a signed experiment assignment.
/// Returns the arm to execute, or None if the assignment is invalid/expired.
pub fn execute_assignment(
    assignment: &ExperimentAssignmentV1,
    subject_ref: &str,
    now: &str,
) -> Option<ExperimentArm> {
    if assignment.subject_id != subject_ref
        || now > assignment.expires_at.as_str()
        || assignment.kill_switch_ref == "KILLED"
    {
        return None;
    }

    Some(assignment.arm.clone())
}

#[cfg(test)]
mod tests {
    use super::{
        BuiltinExperimentExecutor, DataClassification, ExperimentArm, ExperimentAssignmentV1,
        ExperimentState, MoneyV1, SideEffectPolicy, execute_assignment, execute_bucketing_rule,
    };
    use crate::core::ocla::traits::ExperimentRunner;
    use crate::core::ocla::types::{
        ExperimentRequest, ExperimentStopConditions, HoldoutConfig, OclaRequestContext,
    };

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
    fn holdout_assignment_is_deterministic() {
        assert_eq!(
            execute_bucketing_rule("seed", "request-1", 50),
            execute_bucketing_rule("seed", "request-1", 50)
        );
    }

    #[test]
    fn distinct_seeds_produce_distinct_assignments() {
        assert!((0..100).any(|index| {
            let request_ref = format!("request-{index}");
            execute_bucketing_rule("seed-a", &request_ref, 50)
                != execute_bucketing_rule("seed-b", &request_ref, 50)
        }));
    }

    #[test]
    fn max_samples_stops_experiment() {
        let mut state = ExperimentState::new();
        state.record_sample(false, 1.0);
        let conditions = ExperimentStopConditions {
            max_samples: Some(1),
            min_improvement_pct: None,
            max_duration_secs: None,
        };
        assert_eq!(
            state.should_stop(&conditions).as_deref(),
            Some("max_samples")
        );
    }

    #[test]
    fn empty_stop_conditions_never_stop() {
        let mut state = ExperimentState::new();
        state.record_sample(false, 1.0);
        let conditions = ExperimentStopConditions {
            max_samples: None,
            min_improvement_pct: None,
            max_duration_secs: None,
        };
        assert_eq!(state.should_stop(&conditions), None);
    }

    #[test]
    fn compute_outcome_returns_holdout_metrics() {
        let runner = BuiltinExperimentExecutor::new();
        let mut request = experiment("exp-outcome");
        request.holdout = Some(HoldoutConfig {
            holdout_pct: 50,
            assignment_seed: "seed".into(),
            max_samples: Some(100),
        });
        let outcome = runner.compute_outcome(&request, |_| 10.0).unwrap();

        assert_eq!(outcome.treatment_samples + outcome.control_samples, 100);
        assert_eq!(outcome.treatment_metric, 10.0);
        assert_eq!(outcome.control_metric, 10.0);
        assert_eq!(outcome.improvement_pct, 0.0);
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

    fn assignment(arm: ExperimentArm) -> ExperimentAssignmentV1 {
        ExperimentAssignmentV1 {
            experiment_id: "exp-1".into(),
            subject_id: "subject-1".into(),
            arm,
            configuration_ref: "config-1".into(),
            expires_at: "2026-08-06T00:00:00Z".into(),
            max_incremental_cost: MoneyV1 {
                currency: "USD".into(),
                coefficient: 100,
                scale: 2,
            },
            allowed_providers: vec!["provider-1".into()],
            allowed_models: vec!["model-1".into()],
            data_classification: DataClassification::Internal,
            side_effect_policy: SideEffectPolicy::NoSideEffects,
            kill_switch_ref: "ACTIVE".into(),
            signature: "transport-verified".into(),
        }
    }

    #[test]
    fn test_execute_assignment_expired() {
        let assignment = assignment(ExperimentArm::Optimized);
        assert_eq!(
            execute_assignment(&assignment, "subject-1", "2026-08-07T00:00:00Z"),
            None
        );
    }

    #[test]
    fn test_execute_assignment_killed() {
        let mut assignment = assignment(ExperimentArm::Optimized);
        assignment.kill_switch_ref = "KILLED".into();
        assert_eq!(
            execute_assignment(&assignment, "subject-1", "2026-08-05T00:00:00Z"),
            None
        );
    }

    #[test]
    fn test_execute_assignment_valid() {
        let assignment = assignment(ExperimentArm::Shadow);
        assert_eq!(
            execute_assignment(&assignment, "subject-1", "2026-08-05T00:00:00Z"),
            Some(ExperimentArm::Shadow)
        );
    }
}
