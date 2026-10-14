use super::{ShadowEngine, baseline::*, comparison::*, recommendation::*};
use crate::core::value_gate::{OutcomeSignal, cost_tracker::calculate_cost};

fn task(raw: u64, compressed: u64) -> ShadowTask {
    ShadowTask {
        task_id: "task-1".into(),
        query: "fix".into(),
        raw_input_tokens: raw,
        compressed_input_tokens: compressed,
        output_tokens: 1_000,
        model_used: "gpt-4o".into(),
        outcome_signals: vec![OutcomeSignal::TestsPassed],
        duration_ms: 100,
    }
}

#[test]
fn test_baseline_calculation() {
    let actual = simulate_baseline(&task(1_000, 600), &BaselineConfig::default());
    assert_eq!(
        actual.total_cost_micros,
        calculate_cost(1_000, 1_000, 0, "gpt-4o")
    );
}

#[test]
fn test_treatment_with_compression() {
    let actual = measure_treatment(&task(1_000, 600));
    assert_eq!(actual.input_tokens, 600);
    assert!(actual.total_cost_micros < calculate_cost(1_000, 1_000, 0, "gpt-4o"));
    assert_eq!(actual.optimizations_applied, ["compression"]);
}

#[test]
fn test_comparison_shows_savings() {
    let report = ShadowEngine::run_comparison(&[task(1_000, 600)]);
    assert!(report.baseline.total_cost_micros > report.treatment.total_cost_micros);
}

#[test]
fn comparison_reports_positive_token_cost_and_cpao_savings() {
    let report = ShadowEngine::run_comparison(&[task(1_000, 600)]);

    assert_eq!(report.baseline.total_tokens, 2_000);
    assert_eq!(report.treatment.total_tokens, 1_600);
    assert_eq!(report.savings.tokens_saved, 400);
    assert_eq!(report.savings.absolute_micros, 1_000);
    assert_eq!(report.savings.relative_percent, 8.0);
    assert_eq!(report.baseline.avg_cpao_micros, Some(12_500));
    assert_eq!(report.treatment.avg_cpao_micros, Some(11_500));
    assert_eq!(report.delta.cpao_micros, Some(-1_000));
}

#[test]
fn test_quality_maintained() {
    assert!(
        ShadowEngine::run_comparison(&[task(1_000, 600)])
            .savings
            .quality_maintained
    );
}

#[test]
fn test_quality_not_maintained() {
    let baseline = simulate_baseline(&task(1_000, 600), &BaselineConfig::default());
    let mut treatment = measure_treatment(&task(1_000, 600));
    treatment.outcome_accepted = false;
    assert!(
        !compare(&[baseline], &[treatment])
            .savings
            .quality_maintained
    );
}

#[test]
fn test_empty_tasks() {
    let report = ShadowEngine::run_comparison(&[]);
    assert_eq!(
        (report.tasks_count, report.baseline.total_cost_micros),
        (0, 0)
    );
}

#[test]
fn test_recommendations_generated() {
    assert!(
        !ShadowEngine::run_comparison(&[task(1_000, 600)])
            .recommendations
            .is_empty(),
        "`ShadowEngine::run_comparison(&[task(1_000, 600)]) .recommendations` is empty"
    );
}

#[test]
fn recommendations_name_model_providers() {
    let mut routed_task = task(1_000_000, 100_000);
    routed_task.model_used = "claude-sonnet".into();
    let report = ShadowEngine::run_comparison(&[routed_task]);

    let descriptions: Vec<_> = report
        .recommendations
        .iter()
        .map(|recommendation| recommendation.description.as_str())
        .collect();
    assert!(descriptions.iter().any(|description| {
        description.contains("OpenAI GPT-4o") && description.contains("Anthropic Claude Sonnet")
    }));
}

#[test]
fn test_report_format() {
    let text = format_report(&ShadowEngine::run_comparison(&[task(1_000, 600)]));
    assert!(text.contains("| Metric | Baseline | Treatment | Delta |"));
    assert!(text.contains("| Tokens | 2000 | 1600 | 400 saved |"));
    assert!(text.contains("| Avg CPAO (micros) | 12500 | 11500 | 1000 lower |"));
    // #1905: the simulated baseline must never read as a measured quality check.
    assert!(text.contains("the baseline is simulated"));
    assert!(!text.contains("Quality maintained"));
    assert!(!text.contains("maintaining quality"));
}

#[test]
fn report_format_is_deterministic() {
    let report = ShadowEngine::run_comparison(&[task(1_000, 600)]);

    assert_eq!(format_report(&report), format_report(&report));
}

#[test]
fn shadow_exit_gate_reports_savings_for_multiple_tasks_at_quality_floor() {
    let tasks = [task(4_000, 2_000), task(3_000, 1_500), task(2_000, 1_000)];
    let first = ShadowEngine::run_comparison(&tasks);
    let second = ShadowEngine::run_comparison(&tasks);

    assert_eq!(first.tasks_count, tasks.len() as u32);
    assert!(first.baseline.total_tokens > first.treatment.total_tokens);
    assert!(first.savings.relative_percent > 0.0);
    assert!(first.savings.quality_maintained);
    assert_eq!(format_report(&first), format_report(&second));
}
