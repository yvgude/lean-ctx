//! Study report: human/markdown/JSON output from experiment results.

use super::experiment::StudyExperiment;
use serde::{Deserialize, Serialize};

/// Aggregated study report across all datasets and arms.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct StudyReport {
    pub experiments: Vec<StudyExperiment>,
    pub summary: Option<StudySummary>,
}

/// High-level summary across all datasets.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct StudySummary {
    pub total_tasks: usize,
    pub control_pass_rate: f64,
    pub compressed_pass_rate: f64,
    pub quality_retained_pct: f64,
    pub control_cost_per_1k: f64,
    pub compressed_cost_per_1k: f64,
    pub cost_savings_pct: f64,
}

impl StudyReport {
    pub(crate) fn from_experiments(experiments: Vec<StudyExperiment>) -> Self {
        let summary = Self::compute_summary(&experiments);
        Self {
            experiments,
            summary,
        }
    }

    fn compute_summary(experiments: &[StudyExperiment]) -> Option<StudySummary> {
        if experiments.is_empty() {
            return None;
        }

        let mut total_tasks = 0usize;
        let mut control_passed = 0usize;
        let mut compressed_passed = 0usize;
        let mut control_cost = 0.0f64;
        let mut compressed_cost = 0.0f64;

        for exp in experiments {
            for result in &exp.results {
                match result.arm {
                    super::experiment::Arm::Control => {
                        total_tasks += result.tasks_total;
                        control_passed += result.tasks_passed;
                        control_cost += result.total_cost_usd;
                    }
                    super::experiment::Arm::CompressOnly => {
                        compressed_passed += result.tasks_passed;
                        compressed_cost += result.total_cost_usd;
                    }
                }
            }
        }

        if total_tasks == 0 {
            return None;
        }

        let control_rate = control_passed as f64 / total_tasks as f64;
        let compressed_rate = compressed_passed as f64 / total_tasks as f64;
        let quality_retained = if control_rate > 0.0 {
            compressed_rate / control_rate * 100.0
        } else {
            0.0
        };
        let ctrl_per_1k = control_cost / total_tasks as f64 * 1000.0;
        let comb_per_1k = compressed_cost / total_tasks as f64 * 1000.0;
        let savings = if ctrl_per_1k > 0.0 {
            (1.0 - comb_per_1k / ctrl_per_1k) * 100.0
        } else {
            0.0
        };

        Some(StudySummary {
            total_tasks,
            control_pass_rate: control_rate,
            compressed_pass_rate: compressed_rate,
            quality_retained_pct: quality_retained,
            control_cost_per_1k: ctrl_per_1k,
            compressed_cost_per_1k: comb_per_1k,
            cost_savings_pct: savings,
        })
    }

    pub(crate) fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }

    pub(crate) fn to_markdown(&self) -> String {
        let mut md = String::from("# Compression Savings Benchmark Study\n\n");

        if let Some(ref s) = self.summary {
            md.push_str("## Summary\n\n");
            md.push_str(&format!(
                "| Metric | Value |\n|---|---|\n\
                 | Total tasks | {} |\n\
                 | Control pass rate | {:.1}% |\n\
                 | Compressed pass rate | {:.1}% |\n\
                 | Quality retained | {:.1}% |\n\
                 | Control $/1k | ${:.2} |\n\
                 | Compressed $/1k | ${:.2} |\n\
                 | **Cost savings** | **{:.1}%** |\n\n",
                s.total_tasks,
                s.control_pass_rate * 100.0,
                s.compressed_pass_rate * 100.0,
                s.quality_retained_pct,
                s.control_cost_per_1k,
                s.compressed_cost_per_1k,
                s.cost_savings_pct,
            ));
        }

        for exp in &self.experiments {
            md.push_str(&format!("## {}\n\n", exp.dataset_name));
            md.push_str("| Arm | Pass Rate | $/1k | Tokens |\n|---|---|---|---|\n");
            for r in &exp.results {
                md.push_str(&format!(
                    "| {} | {:.1}% | ${:.2} | {} |\n",
                    r.arm.label(),
                    r.pass_rate() * 100.0,
                    r.cost_per_1k(),
                    r.total_input_tokens,
                ));
            }
            md.push('\n');
        }

        md
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_report() {
        let r = StudyReport::from_experiments(vec![]);
        assert!(r.summary.is_none());
    }
}
