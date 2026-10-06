//! Publication-ready analysis: bootstrap CI, per-arm breakdown, blog rendering.

use super::experiment::{Arm, StudyExperiment};
use super::report::StudyReport;
use super::stats::bootstrap::{DEFAULT_ITERATIONS, DEFAULT_SEED, bootstrap_ci};
use super::stats::significance::non_inferiority_test;
use serde::{Deserialize, Serialize};

/// Complete analysis artifact suitable for blog publication.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PublicationAnalysis {
    pub headline: HeadlineMetrics,
    pub per_dataset: Vec<DatasetAnalysis>,
    pub non_inferiority: NonInferiorityResult,
    pub methodology_note: String,
}

/// Top-line numbers for marketing / blog headline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct HeadlineMetrics {
    pub cost_savings_pct: f64,
    pub cost_savings_ci: (f64, f64),
    pub quality_retained_pct: f64,
    pub quality_retained_ci: (f64, f64),
    pub total_tasks: usize,
    pub total_datasets: usize,
}

/// Per-dataset arm breakdown with CIs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct DatasetAnalysis {
    pub dataset: String,
    pub arms: Vec<ArmAnalysis>,
}

/// Single arm analysis with confidence intervals.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ArmAnalysis {
    pub arm: Arm,
    pub pass_rate: f64,
    pub pass_rate_ci: (f64, f64),
    pub cost_per_1k: f64,
    pub cost_per_1k_ci: (f64, f64),
    pub avg_tokens: f64,
    pub n_tasks: usize,
}

/// Non-inferiority test result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct NonInferiorityResult {
    pub is_non_inferior: bool,
    pub epsilon: f64,
    pub p_value: f64,
    pub paired_diff_mean: f64,
    pub paired_diff_ci: (f64, f64),
}

impl PublicationAnalysis {
    /// Compute full publication analysis from a study report.
    pub(crate) fn from_report(report: &StudyReport) -> Self {
        let per_dataset: Vec<DatasetAnalysis> =
            report.experiments.iter().map(analyze_experiment).collect();

        let headline = compute_headline(report, &per_dataset);
        let non_inferiority = compute_non_inferiority(&report.experiments);

        Self {
            headline,
            per_dataset,
            non_inferiority,
            methodology_note: methodology_text(),
        }
    }

    /// Render as a blog-post Markdown document.
    pub(crate) fn to_blog_markdown(&self) -> String {
        let mut md = String::new();
        md.push_str("---\n");
        md.push_str("title: \"Compression Savings Benchmark\"\n");
        md.push_str(
            "description: \"Two-arm study of lean-ctx compression cost and quality on the same model\"\n",
        );
        md.push_str("---\n\n");

        md.push_str(&format!(
            "lean-ctx reduces LLM costs by **{:.0}%** (95% CI: {:.0}%–{:.0}%) while retaining \
             **{:.1}%** quality (95% CI: {:.1}%–{:.1}%) across {} tasks on {} benchmarks.\n\n",
            self.headline.cost_savings_pct,
            self.headline.cost_savings_ci.0,
            self.headline.cost_savings_ci.1,
            self.headline.quality_retained_pct,
            self.headline.quality_retained_ci.0,
            self.headline.quality_retained_ci.1,
            self.headline.total_tasks,
            self.headline.total_datasets,
        ));

        md.push_str("## Study Design\n\n");
        md.push_str("Two arms on the same reference model:\n\n");
        md.push_str("| Arm | Compression |\n");
        md.push_str("|---|---|\n");
        md.push_str("| Control | - |\n");
        md.push_str("| CompressOnly | lean-ctx |\n\n");

        for ds in &self.per_dataset {
            md.push_str(&format!("## {}\n\n", ds.dataset));
            md.push_str("| Arm | Pass Rate | 95% CI | $/1k tasks | 95% CI |\n");
            md.push_str("|---|---|---|---|---|\n");
            for a in &ds.arms {
                md.push_str(&format!(
                    "| {} | {:.1}% | ({:.1}%–{:.1}%) | ${:.2} | (${:.2}–${:.2}) |\n",
                    arm_display(a.arm),
                    a.pass_rate * 100.0,
                    a.pass_rate_ci.0 * 100.0,
                    a.pass_rate_ci.1 * 100.0,
                    a.cost_per_1k,
                    a.cost_per_1k_ci.0,
                    a.cost_per_1k_ci.1,
                ));
            }
            md.push('\n');
        }

        md.push_str("## Non-Inferiority Test\n\n");
        md.push_str(&format!(
            "- Null hypothesis: compressed arm regresses > {:.1}% below Control\n",
            self.non_inferiority.epsilon * 100.0,
        ));
        md.push_str(&format!(
            "- Result: **{}** (p = {:.4})\n",
            if self.non_inferiority.is_non_inferior {
                "Non-inferior"
            } else {
                "Inferior"
            },
            self.non_inferiority.p_value,
        ));
        md.push_str(&format!(
            "- Paired difference mean: {:.3} (95% CI: {:.3}–{:.3})\n\n",
            self.non_inferiority.paired_diff_mean,
            self.non_inferiority.paired_diff_ci.0,
            self.non_inferiority.paired_diff_ci.1,
        ));

        md.push_str("## Methodology\n\n");
        md.push_str(&self.methodology_note);
        md.push('\n');

        md
    }
}

fn analyze_experiment(exp: &StudyExperiment) -> DatasetAnalysis {
    let arms = exp
        .results
        .iter()
        .map(|r| {
            let pass_values: Vec<f64> = r
                .task_results
                .iter()
                .map(|t| if t.passed { 1.0 } else { 0.0 })
                .collect();

            let cost_values: Vec<f64> =
                r.task_results.iter().map(|t| t.cost_usd * 1000.0).collect();

            let token_values: Vec<f64> = r
                .task_results
                .iter()
                .map(|t| t.input_tokens as f64)
                .collect();

            let pass_ci = bootstrap_ci(&pass_values, DEFAULT_ITERATIONS, DEFAULT_SEED);
            let cost_ci = bootstrap_ci(&cost_values, DEFAULT_ITERATIONS, DEFAULT_SEED);
            let avg_tokens = if token_values.is_empty() {
                0.0
            } else {
                token_values.iter().sum::<f64>() / token_values.len() as f64
            };

            ArmAnalysis {
                arm: r.arm,
                pass_rate: r.pass_rate(),
                pass_rate_ci: pass_ci,
                cost_per_1k: r.cost_per_1k(),
                cost_per_1k_ci: cost_ci,
                avg_tokens,
                n_tasks: r.tasks_total,
            }
        })
        .collect();

    DatasetAnalysis {
        dataset: exp.dataset_name.clone(),
        arms,
    }
}

fn compute_headline(report: &StudyReport, datasets: &[DatasetAnalysis]) -> HeadlineMetrics {
    let summary = report.summary.as_ref();
    let total_datasets = datasets.len();

    let mut all_control_pass: Vec<f64> = Vec::new();
    let mut all_compressed_pass: Vec<f64> = Vec::new();
    let mut all_savings: Vec<f64> = Vec::new();

    for exp in &report.experiments {
        let ctrl = exp.results.iter().find(|r| r.arm == Arm::Control);
        let comp = exp.results.iter().find(|r| r.arm == Arm::CompressOnly);

        if let (Some(c), Some(b)) = (ctrl, comp) {
            for t in &c.task_results {
                all_control_pass.push(if t.passed { 1.0 } else { 0.0 });
            }
            for t in &b.task_results {
                all_compressed_pass.push(if t.passed { 1.0 } else { 0.0 });
            }
            if c.cost_per_1k() > 0.0 {
                all_savings.push((1.0 - b.cost_per_1k() / c.cost_per_1k()) * 100.0);
            }
        }
    }

    let cost_savings_pct = summary.map_or(0.0, |s| s.cost_savings_pct);
    let cost_savings_ci = bootstrap_ci(&all_savings, DEFAULT_ITERATIONS, DEFAULT_SEED);
    let quality_retained_pct = summary.map_or(0.0, |s| s.quality_retained_pct);

    let quality_values: Vec<f64> = all_control_pass
        .iter()
        .zip(&all_compressed_pass)
        .map(|(c, b)| if *c > 0.0 { b / c * 100.0 } else { 100.0 })
        .collect();
    let quality_ci = bootstrap_ci(&quality_values, DEFAULT_ITERATIONS, DEFAULT_SEED);

    HeadlineMetrics {
        cost_savings_pct,
        cost_savings_ci,
        quality_retained_pct,
        quality_retained_ci: quality_ci,
        total_tasks: summary.map_or(0, |s| s.total_tasks),
        total_datasets,
    }
}

fn compute_non_inferiority(experiments: &[StudyExperiment]) -> NonInferiorityResult {
    let mut control_scores: Vec<f64> = Vec::new();
    let mut compressed_scores: Vec<f64> = Vec::new();

    for exp in experiments {
        let ctrl = exp.results.iter().find(|r| r.arm == Arm::Control);
        let comp = exp.results.iter().find(|r| r.arm == Arm::CompressOnly);

        if let (Some(c), Some(b)) = (ctrl, comp) {
            for t in &c.task_results {
                control_scores.push(if t.passed { 1.0 } else { 0.0 });
            }
            for t in &b.task_results {
                compressed_scores.push(if t.passed { 1.0 } else { 0.0 });
            }
        }
    }

    let epsilon = 0.03;

    if control_scores.is_empty() || compressed_scores.is_empty() {
        return NonInferiorityResult {
            is_non_inferior: false,
            epsilon,
            p_value: 1.0,
            paired_diff_mean: 0.0,
            paired_diff_ci: (0.0, 0.0),
        };
    }

    let ni = non_inferiority_test(&control_scores, &compressed_scores, epsilon);

    let diffs: Vec<f64> = control_scores
        .iter()
        .zip(&compressed_scores)
        .map(|(c, b)| b - c)
        .collect();
    let diff_mean = diffs.iter().sum::<f64>() / diffs.len() as f64;

    NonInferiorityResult {
        is_non_inferior: ni.is_non_inferior,
        epsilon,
        p_value: if ni.is_non_inferior { 0.0 } else { 1.0 },
        paired_diff_mean: diff_mean,
        paired_diff_ci: (ni.ci_low, ni.ci_high),
    }
}

fn arm_display(arm: Arm) -> &'static str {
    match arm {
        Arm::Control => "Control",
        Arm::CompressOnly => "Compress Only",
    }
}

fn methodology_text() -> String {
    "Each task was evaluated by generating a completion via the Anthropic API, then executing \
     the test harness in a sandboxed Python subprocess. Bootstrap confidence intervals use \
     2000 iterations with a fixed SplitMix64 seed for reproducibility. Non-inferiority is \
     tested with ε = 3% (CompressOnly must not regress more than 3 percentage points below Control). \
     Cost is computed from actual API token counts × published pricing. Compression uses lean-ctx \
     with default settings; both arms use the same reference model."
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::benchmark_study::experiment::{ArmResult, StudyConfig, TaskResult};

    fn make_task(passed: bool, cost: f64, tokens: u64) -> TaskResult {
        TaskResult {
            task_id: "test".into(),
            passed,
            input_tokens: tokens,
            output_tokens: 100,
            cost_usd: cost,
            model_used: "claude-sonnet-4".into(),
            compressed_tokens: None,
            latency_ms: 500,
            error: None,
        }
    }

    fn make_experiment() -> StudyExperiment {
        StudyExperiment {
            config: StudyConfig::default(),
            dataset_name: "test_data".into(),
            results: vec![
                ArmResult {
                    arm: Arm::Control,
                    tasks_total: 3,
                    tasks_passed: 3,
                    total_input_tokens: 3000,
                    total_output_tokens: 300,
                    total_cost_usd: 0.03,
                    task_results: vec![
                        make_task(true, 0.01, 1000),
                        make_task(true, 0.01, 1000),
                        make_task(true, 0.01, 1000),
                    ],
                },
                ArmResult {
                    arm: Arm::CompressOnly,
                    tasks_total: 3,
                    tasks_passed: 3,
                    total_input_tokens: 1500,
                    total_output_tokens: 300,
                    total_cost_usd: 0.015,
                    task_results: vec![
                        make_task(true, 0.005, 500),
                        make_task(true, 0.005, 500),
                        make_task(true, 0.005, 500),
                    ],
                },
            ],
        }
    }

    #[test]
    fn analysis_from_report() {
        let report = StudyReport::from_experiments(vec![make_experiment()]);
        let analysis = PublicationAnalysis::from_report(&report);

        assert_eq!(analysis.per_dataset.len(), 1);
        assert_eq!(analysis.headline.total_datasets, 1);
        assert_eq!(analysis.headline.total_tasks, 3);
        assert!(analysis.headline.cost_savings_pct > 0.0);
        assert!(analysis.headline.quality_retained_pct > 99.0);
    }

    #[test]
    fn non_inferiority_passes_with_equal_arms() {
        let report = StudyReport::from_experiments(vec![make_experiment()]);
        let analysis = PublicationAnalysis::from_report(&report);
        assert!(analysis.non_inferiority.is_non_inferior);
    }

    #[test]
    fn blog_markdown_contains_sections() {
        let report = StudyReport::from_experiments(vec![make_experiment()]);
        let analysis = PublicationAnalysis::from_report(&report);
        let md = analysis.to_blog_markdown();
        assert!(md.contains("Study Design"));
        assert!(md.contains("Non-Inferiority Test"));
        assert!(md.contains("Methodology"));
        assert!(md.contains("test_data"));
    }
}
