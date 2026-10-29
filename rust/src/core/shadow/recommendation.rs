//! LeanCTX treatment measurement and applied-optimization evidence.

use crate::core::{
    shadow::baseline::accepted,
    value_gate::{OutcomeSignal, cost_tracker::calculate_cost},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShadowTask {
    pub task_id: String,
    pub query: String,
    pub raw_input_tokens: u64,
    pub compressed_input_tokens: u64,
    pub output_tokens: u64,
    pub model_used: String,
    pub outcome_signals: Vec<OutcomeSignal>,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreatmentMeasurement {
    pub task_id: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub model: String,
    pub total_cost_micros: u64,
    pub duration_ms: u64,
    pub outcome_accepted: bool,
    pub optimizations_applied: Vec<String>,
}

pub fn measure_treatment(task: &ShadowTask) -> TreatmentMeasurement {
    let mut optimizations = Vec::new();
    // Only context changes are LeanCTX optimizations; the model is the host's choice.
    if task.compressed_input_tokens < task.raw_input_tokens {
        optimizations.push("compression".into());
    }
    TreatmentMeasurement {
        task_id: task.task_id.clone(),
        input_tokens: task.compressed_input_tokens,
        output_tokens: task.output_tokens,
        model: task.model_used.clone(),
        total_cost_micros: calculate_cost(
            task.compressed_input_tokens,
            task.output_tokens,
            0,
            &task.model_used,
        ),
        duration_ms: task.duration_ms,
        outcome_accepted: accepted(&task.task_id, &task.outcome_signals),
        optimizations_applied: optimizations,
    }
}
