//! Baseline simulation without LeanCTX optimizations.

use crate::core::{
    shadow::recommendation::ShadowTask,
    value_gate::{OutcomeSignal, TaskOutcome, cost_tracker::calculate_cost},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaselineMeasurement {
    pub task_id: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub model: String,
    pub total_cost_micros: u64,
    pub duration_ms: u64,
    pub outcome_accepted: bool,
}

/// The same task without context reduction: uncompressed input, priced with the model the host
/// actually chose for this task. The model is never swapped for a configured reference model —
/// LeanCTX does not choose models, so a price difference between models is not a LeanCTX saving.
pub fn simulate_baseline(task: &ShadowTask) -> BaselineMeasurement {
    let accepted = accepted(&task.task_id, &task.outcome_signals);
    BaselineMeasurement {
        task_id: task.task_id.clone(),
        input_tokens: task.raw_input_tokens,
        output_tokens: task.output_tokens,
        model: task.model_used.clone(),
        total_cost_micros: calculate_cost(
            task.raw_input_tokens,
            task.output_tokens,
            0,
            &task.model_used,
        ),
        duration_ms: task.duration_ms,
        outcome_accepted: accepted,
    }
}

pub(crate) fn accepted(task_id: &str, signals: &[OutcomeSignal]) -> bool {
    crate::core::value_gate::outcome_evaluator::evaluate(&TaskOutcome {
        task_id: task_id.into(),
        completed: true,
        signals: signals.to_vec(),
    })
}
