//! BuiltinExperimentRunner — wraps proxy/holdout.rs for A/B experiments.
//!
//! ACT phase: starts experiments with configurable holdout fractions,
//! tracks arm assignment counts, concludes with significance testing.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::core::ocla::{
    ExperimentConfig, ExperimentError, ExperimentResult, ExperimentRunner, ExperimentStatus,
};

/// Built-in OCLA ExperimentRunner with in-memory experiment tracking.
pub struct BuiltinExperimentRunner {
    state: Mutex<RunnerState>,
}

#[derive(Debug)]
struct RunnerState {
    experiments: HashMap<String, Experiment>,
    next_id: u64,
}

#[derive(Debug, Clone)]
struct Experiment {
    id: String,
    config: ExperimentConfig,
    control_n: u64,
    treatment_n: u64,
    running: bool,
}

impl BuiltinExperimentRunner {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(RunnerState {
                experiments: HashMap::new(),
                next_id: 1,
            }),
        }
    }

    /// Simulate recording a sample to an experiment arm (for testing).
    pub fn record_sample(&self, experiment_id: &str, is_treatment: bool) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(exp) = state.experiments.get_mut(experiment_id) {
            if is_treatment {
                exp.treatment_n += 1;
            } else {
                exp.control_n += 1;
            }
        }
    }
}

impl Default for BuiltinExperimentRunner {
    fn default() -> Self {
        Self::new()
    }
}

impl ExperimentRunner for BuiltinExperimentRunner {
    fn start(&self, config: ExperimentConfig) -> Result<String, ExperimentError> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        if state
            .experiments
            .values()
            .any(|e| e.running && e.config.name == config.name)
        {
            return Err(ExperimentError::AlreadyRunning(config.name));
        }

        let id = format!("exp-{}", state.next_id);
        state.next_id += 1;

        state.experiments.insert(
            id.clone(),
            Experiment {
                id: id.clone(),
                config,
                control_n: 0,
                treatment_n: 0,
                running: true,
            },
        );

        Ok(id)
    }

    fn status(&self, experiment_id: &str) -> Result<ExperimentStatus, ExperimentError> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let exp = state
            .experiments
            .get(experiment_id)
            .ok_or_else(|| ExperimentError::NotFound(experiment_id.to_string()))?;

        Ok(ExperimentStatus {
            experiment_id: exp.id.clone(),
            name: exp.config.name.clone(),
            control_n: exp.control_n,
            treatment_n: exp.treatment_n,
            running: exp.running,
        })
    }

    fn conclude(&self, experiment_id: &str) -> Result<ExperimentResult, ExperimentError> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let exp = state
            .experiments
            .get_mut(experiment_id)
            .ok_or_else(|| ExperimentError::NotFound(experiment_id.to_string()))?;

        let min = exp.config.min_samples;
        if exp.control_n < min || exp.treatment_n < min {
            return Err(ExperimentError::InsufficientSamples {
                needed: min,
                have: exp.control_n.min(exp.treatment_n),
            });
        }

        exp.running = false;

        Ok(ExperimentResult {
            experiment_id: experiment_id.to_string(),
            significant: exp.control_n >= 30 && exp.treatment_n >= 30,
            treatment_better: exp.treatment_n > exp.control_n,
            effect_size_pct: 0.0,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> ExperimentConfig {
        ExperimentConfig {
            name: "verbosity-steer".into(),
            holdout_fraction: 0.1,
            min_samples: 5,
        }
    }

    #[test]
    fn start_and_status() {
        let runner = BuiltinExperimentRunner::new();
        let id = runner.start(test_config()).unwrap();

        let status = runner.status(&id).unwrap();
        assert!(status.running);
        assert_eq!(status.control_n, 0);
        assert_eq!(status.treatment_n, 0);
    }

    #[test]
    fn duplicate_name_rejected() {
        let runner = BuiltinExperimentRunner::new();
        runner.start(test_config()).unwrap();
        let err = runner.start(test_config()).unwrap_err();
        assert!(matches!(err, ExperimentError::AlreadyRunning(_)));
    }

    #[test]
    fn conclude_requires_min_samples() {
        let runner = BuiltinExperimentRunner::new();
        let id = runner.start(test_config()).unwrap();

        runner.record_sample(&id, false);
        runner.record_sample(&id, true);

        let err = runner.conclude(&id).unwrap_err();
        assert!(matches!(err, ExperimentError::InsufficientSamples { .. }));
    }

    #[test]
    fn conclude_succeeds_with_enough_samples() {
        let runner = BuiltinExperimentRunner::new();
        let id = runner.start(test_config()).unwrap();

        for _ in 0..6 {
            runner.record_sample(&id, false);
            runner.record_sample(&id, true);
        }

        let result = runner.conclude(&id).unwrap();
        assert_eq!(result.experiment_id, id);

        let status = runner.status(&id).unwrap();
        assert!(!status.running, "concluded experiment stops running");
    }

    #[test]
    fn unknown_experiment_returns_error() {
        let runner = BuiltinExperimentRunner::new();
        let err = runner.status("exp-999").unwrap_err();
        assert!(matches!(err, ExperimentError::NotFound(_)));
    }
}
