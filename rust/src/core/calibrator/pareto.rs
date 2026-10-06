use super::candidate::CandidateProfile;
use crate::core::benchmark_spec::types::BenchmarkResult;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CalibratedResult {
    pub candidate: CandidateProfile,
    pub cost_per_task: f64,
    pub mean_quality: f64,
    pub mean_latency_ms: f64,
    pub pass_rate: f64,
    pub quality_floor_met: bool,
    /// True only when every outcome has an explicit deterministic evaluation.
    pub quality_evaluated: bool,
    /// True only when every outcome links an instrumented execution receipt.
    /// This is evidence linkage, not an independent verification claim.
    pub receipt_evidence_complete: bool,
}

impl CalibratedResult {
    pub(crate) fn from_benchmark_result(
        candidate: CandidateProfile,
        benchmark: &BenchmarkResult,
    ) -> Self {
        Self {
            candidate,
            cost_per_task: benchmark.summary.cost_per_task_usd,
            mean_quality: benchmark.summary.mean_quality,
            mean_latency_ms: benchmark.summary.mean_latency_ms,
            pass_rate: benchmark.summary.pass_rate,
            quality_floor_met: benchmark.summary.quality_floor_met,
            quality_evaluated: benchmark.summary.quality_evaluated,
            receipt_evidence_complete: benchmark.summary.receipt_evidence_complete,
        }
    }
}

pub(crate) fn compute_pareto_frontier(
    results: &[CalibratedResult],
    quality_floor: f64,
) -> Vec<CalibratedResult> {
    let mut eligible: Vec<_> = results
        .iter()
        .filter(|r| {
            r.cost_per_task.is_finite()
                && r.mean_quality.is_finite()
                && r.quality_evaluated
                && r.receipt_evidence_complete
                && r.mean_quality >= quality_floor
        })
        .cloned()
        .collect();
    eligible.sort_by(|a, b| {
        a.cost_per_task
            .total_cmp(&b.cost_per_task)
            .then_with(|| b.mean_quality.total_cmp(&a.mean_quality))
            .then_with(|| a.candidate.id.cmp(&b.candidate.id))
    });
    let mut frontier = Vec::new();
    let mut best_quality = f64::NEG_INFINITY;
    for r in &eligible {
        if r.mean_quality > best_quality {
            best_quality = r.mean_quality;
            frontier.push(r.clone());
        }
    }
    frontier
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::benchmark_spec::types::{
        BenchmarkEvaluation, BenchmarkOutcome, BenchmarkResult, BenchmarkSummary,
    };

    fn result(id: &str, cost: f64, quality: f64) -> CalibratedResult {
        CalibratedResult {
            candidate: CandidateProfile {
                id: id.into(),
                label: id.into(),
                budget_tokens: 32_000,
                compression: "balanced".into(),
                reuse_threshold: 0.85,
                capability_variant: "leanctx".into(),
            },
            cost_per_task: cost,
            mean_quality: quality,
            mean_latency_ms: 100.0,
            pass_rate: 1.0,
            quality_floor_met: quality >= 0.95,
            quality_evaluated: true,
            receipt_evidence_complete: true,
        }
    }

    #[test]
    fn pareto_filters_below_floor() {
        let results = vec![
            result("a", 0.50, 0.98),
            result("b", 0.30, 0.90),
            result("c", 0.40, 0.96),
        ];
        let frontier = compute_pareto_frontier(&results, 0.95);
        assert!(frontier.iter().all(|r| r.mean_quality >= 0.95));
    }

    #[test]
    fn empty_input() {
        assert!(compute_pareto_frontier(&[], 0.95).is_empty());
    }

    #[test]
    fn all_below_floor() {
        let results = vec![result("a", 0.10, 0.80), result("b", 0.20, 0.85)];
        assert!(compute_pareto_frontier(&results, 0.95).is_empty());
    }

    #[test]
    fn observed_or_incompletely_receipted_results_cannot_reach_frontier() {
        let mut incomplete = result("incomplete", 0.10, 0.99);
        incomplete.receipt_evidence_complete = false;

        assert!(compute_pareto_frontier(&[incomplete], 0.95).is_empty());
    }

    #[test]
    fn converts_benchmark_summary_to_calibrated_result() {
        let outcomes = vec![
            BenchmarkOutcome {
                task_id: "passed".into(),
                passed: true,
                cost_usd: 0.03,
                quality_score: 1.0,
                latency_ms: 100,
                tokens_input: 100,
                tokens_output: 10,
                error: None,
                evaluation: Some(BenchmarkEvaluation {
                    evaluator_id: "qa-f1-v1".into(),
                    metric: "f1".into(),
                    score: 1.0,
                    passed: true,
                    detail: String::new(),
                    output_digest: "passed".into(),
                }),
                execution_receipt_ref: None,
                capability_observation: None,
            },
            BenchmarkOutcome {
                task_id: "failed".into(),
                passed: false,
                cost_usd: 0.01,
                quality_score: 0.5,
                latency_ms: 300,
                tokens_input: 100,
                tokens_output: 10,
                error: Some("failed".into()),
                evaluation: Some(BenchmarkEvaluation {
                    evaluator_id: "qa-f1-v1".into(),
                    metric: "f1".into(),
                    score: 0.5,
                    passed: false,
                    detail: "failed".into(),
                    output_digest: "failed".into(),
                }),
                execution_receipt_ref: None,
                capability_observation: None,
            },
        ];
        let benchmark = BenchmarkResult {
            spec_id: "spec".into(),
            spec_version: "1.0.0".into(),
            profile_hash: "hash".into(),
            agent: "mock".into(),
            model: "test".into(),
            runtime_version: "test".into(),
            summary: BenchmarkSummary::from_outcomes(&outcomes, 0.70),
            outcomes,
            completed_at: "0".into(),
        };

        let calibrated = CalibratedResult::from_benchmark_result(
            result("candidate", 0.0, 0.0).candidate,
            &benchmark,
        );

        assert!((calibrated.cost_per_task - 0.02).abs() < f64::EPSILON);
        assert!((calibrated.mean_quality - 0.75).abs() < f64::EPSILON);
        assert!((calibrated.mean_latency_ms - 200.0).abs() < f64::EPSILON);
        assert!((calibrated.pass_rate - 0.5).abs() < f64::EPSILON);
        assert!(calibrated.quality_floor_met);
    }

    #[test]
    fn keeps_every_non_dominated_candidate() {
        let results = vec![
            result("low", 0.20, 0.95),
            result("medium", 0.40, 0.97),
            result("high", 0.60, 0.99),
        ];
        let frontier = compute_pareto_frontier(&results, 0.95);
        let ids: Vec<_> = frontier.iter().map(|r| r.candidate.id.as_str()).collect();
        assert_eq!(ids, ["low", "medium", "high"]);
    }

    #[test]
    fn equal_cost_prefers_higher_quality() {
        let results = vec![
            result("lower-quality", 0.40, 0.96),
            result("higher-quality", 0.40, 0.98),
        ];
        let frontier = compute_pareto_frontier(&results, 0.95);
        assert_eq!(frontier.len(), 1);
        assert_eq!(frontier[0].candidate.id, "higher-quality");
    }

    #[test]
    fn identical_objectives_keep_a_deterministic_representative() {
        let results = vec![
            result("duplicate-b", 0.40, 0.96),
            result("duplicate-a", 0.40, 0.96),
        ];
        let frontier = compute_pareto_frontier(&results, 0.95);
        assert_eq!(frontier.len(), 1);
        assert_eq!(frontier[0].candidate.id, "duplicate-a");
    }
}
