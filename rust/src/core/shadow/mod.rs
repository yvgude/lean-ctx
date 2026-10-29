//! Counterfactual cost and quality reporting for completed tasks.

pub mod baseline;
pub mod comparison;
pub mod persistence;
pub mod recommendation;
pub mod runtime;

pub use comparison::ShadowReport;
pub use recommendation::ShadowTask;

use baseline::simulate_baseline;
use comparison::compare;
use recommendation::measure_treatment;

/// Runs the LeanCTX treatment beside a deterministic baseline without context reduction. Each
/// task is compared with its own host-chosen model; only context changes count as savings.
#[derive(Debug, Default)]
pub struct ShadowEngine;

impl ShadowEngine {
    pub fn run_comparison(tasks: &[ShadowTask]) -> ShadowReport {
        let baselines: Vec<_> = tasks.iter().map(simulate_baseline).collect();
        let treatments: Vec<_> = tasks.iter().map(measure_treatment).collect();
        compare(&baselines, &treatments)
    }
}

pub fn run_comparison(tasks: &[ShadowTask]) -> ShadowReport {
    ShadowEngine::run_comparison(tasks)
}

#[cfg(test)]
mod runtime_tests;
#[cfg(test)]
mod tests;
