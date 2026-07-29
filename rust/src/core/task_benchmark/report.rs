//! Benchmark report generation: human-readable tables and JSON.

use serde::{Deserialize, Serialize};

use super::config::ProfileMode;
use super::runner::BenchmarkResult;

struct ReportRow {
    profile: String,
    score: String,
    savings: String,
    tokens: String,
    quality: String,
    latency: String,
}

/// Formatted benchmark report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchReport {
    pub result: BenchmarkResult,
}

impl BenchReport {
    pub fn new(result: BenchmarkResult) -> Self {
        Self { result }
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(&self.result).unwrap_or_else(|_| "{}".into())
    }

    pub fn to_human(&self) -> String {
        let mut out = String::new();
        out.push_str(&self.header());
        out.push_str(&self.summary_table());
        out.push_str(&self.per_task_table());
        if self.result.regression_detected {
            out.push_str(&self.regression_section());
        }
        out
    }

    pub fn to_markdown(&self) -> String {
        let mut out = String::new();
        out.push_str("# lean-ctx Task Benchmark Report\n\n");
        out.push_str(&format!("Repeats per config: {}\n\n", self.result.repeats));
        out.push_str(&self.md_summary_table());
        out.push_str(&self.md_per_task_table());
        if self.result.regression_detected {
            out.push_str(&self.md_regression_section());
        }
        out
    }

    fn header(&self) -> String {
        format!(
            "\n╔══════════════════════════════════════════════════════╗\n\
             ║           lean-ctx Task Benchmark Report             ║\n\
             ║         {} repeats × {} profiles                      ║\n\
             ╚══════════════════════════════════════════════════════╝\n\n",
            self.result.repeats,
            self.result.profiles.len()
        )
    }

    fn summary_table(&self) -> String {
        let mut out = String::new();
        out.push_str(
            "┌────────────────┬────────┬──────────────┬──────────┬──────────┬──────────┐\n",
        );
        out.push_str(
            "│ Configuration  │ Score  │ Token Savings│ Tokens   │ Quality  │ Latency  │\n",
        );
        out.push_str(
            "├────────────────┼────────┼──────────────┼──────────┼──────────┼──────────┤\n",
        );

        for row in self.summary_rows() {
            out.push_str(&format!(
                "│ {:<14} │ {:>6} │ {:>12} │ {:>8} │ {:>8} │ {:>8} │\n",
                row.profile, row.score, row.savings, row.tokens, row.quality, row.latency,
            ));
        }

        out.push_str(
            "└────────────────┴────────┴──────────────┴──────────┴──────────┴──────────┘\n\n",
        );
        out
    }

    fn per_task_table(&self) -> String {
        let mut out = String::new();
        out.push_str("Per-task breakdown:\n\n");

        let task_ids: Vec<String> = self
            .result
            .profiles
            .first()
            .map(|p| {
                let mut ids: Vec<String> = p.runs.iter().map(|r| r.task_id.clone()).collect();
                ids.dedup();
                ids
            })
            .unwrap_or_default();

        for task_id in &task_ids {
            out.push_str(&format!("  {task_id}:\n"));
            for profile in &self.result.profiles {
                let runs: Vec<_> = profile
                    .runs
                    .iter()
                    .filter(|r| r.task_id == *task_id)
                    .collect();
                if runs.is_empty() {
                    continue;
                }
                let avg_tokens: usize =
                    runs.iter().map(|r| r.compressed_tokens).sum::<usize>() / runs.len().max(1);
                let avg_savings: f64 =
                    runs.iter().map(|r| r.savings_pct).sum::<f64>() / runs.len() as f64;
                let all_pass = runs.iter().all(|r| r.quality.passes());
                let status = if all_pass { "✓" } else { "✗" };

                out.push_str(&format!(
                    "    {:<12} {status} {:>6} tokens ({:>5.1}% saved)\n",
                    profile.profile, avg_tokens, avg_savings,
                ));
            }
        }

        out
    }

    fn regression_section(&self) -> String {
        let mut out = String::new();
        out.push_str("\nREGRESSION DETECTED:\n");
        for detail in &self.result.regression_details {
            out.push_str(&format!("  - {detail}\n"));
        }
        out
    }

    fn md_summary_table(&self) -> String {
        let mut out = String::new();
        out.push_str("## Summary\n\n");
        out.push_str("| Configuration | Score | Token Savings | Tokens | Quality | Latency |\n");
        out.push_str("|---|---|---|---|---|---|\n");

        for row in self.summary_rows() {
            out.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} |\n",
                row.profile, row.score, row.savings, row.tokens, row.quality, row.latency,
            ));
        }

        out.push('\n');
        out
    }

    fn md_per_task_table(&self) -> String {
        let mut out = String::new();
        out.push_str("## Per-Task Results\n\n");
        out.push_str("| Task | Profile | Tokens | Savings | Quality |\n");
        out.push_str("|---|---|---|---|---|\n");

        let task_ids: Vec<String> = self
            .result
            .profiles
            .first()
            .map(|p| {
                let mut ids: Vec<String> = p.runs.iter().map(|r| r.task_id.clone()).collect();
                ids.dedup();
                ids
            })
            .unwrap_or_default();

        for task_id in &task_ids {
            for profile in &self.result.profiles {
                let runs: Vec<_> = profile
                    .runs
                    .iter()
                    .filter(|r| r.task_id == *task_id)
                    .collect();
                if runs.is_empty() {
                    continue;
                }
                let avg_tokens: usize =
                    runs.iter().map(|r| r.compressed_tokens).sum::<usize>() / runs.len().max(1);
                let avg_savings =
                    runs.iter().map(|r| r.savings_pct).sum::<f64>() / runs.len() as f64;
                let all_pass = runs.iter().all(|r| r.quality.passes());
                let status = if all_pass { "pass" } else { "FAIL" };

                out.push_str(&format!(
                    "| {} | {} | {} | {:.1}% | {} |\n",
                    task_id, profile.profile, avg_tokens, avg_savings, status,
                ));
            }
        }

        out.push('\n');
        out
    }

    fn md_regression_section(&self) -> String {
        let mut out = String::new();
        out.push_str("## Regressions\n\n");
        for detail in &self.result.regression_details {
            out.push_str(&format!("- {detail}\n"));
        }
        out.push('\n');
        out
    }

    fn summary_rows(&self) -> Vec<ReportRow> {
        self.result
            .profiles
            .iter()
            .map(|profile| ReportRow {
                profile: profile.profile.clone(),
                score: format!("{}/{}", profile.tasks_passed, profile.tasks_total),
                savings: format_profile_savings(
                    profile.mode,
                    profile.total_raw_tokens,
                    profile.total_compressed_tokens,
                ),
                tokens: format_tokens(profile.total_compressed_tokens),
                quality: format!("{:.0}%", profile.avg_quality_score * 100.0),
                latency: format_duration(profile.avg_latency_us),
            })
            .collect()
    }
}

fn format_profile_savings(
    mode: ProfileMode,
    raw_tokens: usize,
    compressed_tokens: usize,
) -> String {
    if mode == ProfileMode::Stock {
        return "baseline".to_string();
    }
    if raw_tokens == 0 {
        return "n/a".to_string();
    }
    let savings = (1.0 - compressed_tokens as f64 / raw_tokens as f64) * 100.0;
    format!("{savings:.1}%")
}

fn format_tokens(n: usize) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        format!("{n}")
    }
}

fn format_duration(us: u64) -> String {
    if us >= 1_000_000 {
        format!("{:.2}s", us as f64 / 1_000_000.0)
    } else if us >= 1_000 {
        format!("{:.1}ms", us as f64 / 1_000.0)
    } else {
        format!("{us}μs")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::task_benchmark::{
        config::BenchConfig,
        fixtures::canonical_suite,
        run_benchmark,
        runner::{BenchmarkResult, ProfileResult},
    };

    #[test]
    fn human_report_contains_table() {
        let tasks = canonical_suite();
        let config = BenchConfig {
            repeats: 1,
            ..BenchConfig::default()
        };
        let result = run_benchmark(&tasks, &config);
        let report = BenchReport::new(result);
        let output = report.to_human();

        assert!(output.contains("Configuration"));
        assert!(output.contains("stock"));
        assert!(output.contains("standard"));
        assert!(output.contains("aggressive"));
    }

    #[test]
    fn markdown_report_has_tables() {
        let tasks = canonical_suite();
        let config = BenchConfig {
            repeats: 1,
            ..BenchConfig::default()
        };
        let result = run_benchmark(&tasks, &config);
        let report = BenchReport::new(result);
        let md = report.to_markdown();

        assert!(md.contains("# lean-ctx Task Benchmark Report"));
        assert!(md.contains("| Configuration"));
        assert!(md.contains("| Task |"));
    }

    #[test]
    fn json_report_is_valid() {
        let tasks = canonical_suite();
        let config = BenchConfig {
            repeats: 1,
            ..BenchConfig::default()
        };
        let result = run_benchmark(&tasks, &config);
        let report = BenchReport::new(result);
        let json = report.to_json();

        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(parsed.get("profiles").is_some());
        assert!(parsed.get("repeats").is_some());
    }

    #[test]
    fn format_tokens_ranges() {
        assert_eq!(format_tokens(500), "500");
        assert_eq!(format_tokens(1_500), "1.5K");
        assert_eq!(format_tokens(2_500_000), "2.5M");
    }

    #[test]
    fn summary_uses_weighted_total_token_savings() {
        let report = BenchReport::new(BenchmarkResult {
            repeats: 1,
            regression_detected: false,
            regression_details: vec![],
            profiles: vec![ProfileResult {
                profile: "standard".to_string(),
                mode: ProfileMode::Standard,
                runs: vec![],
                total_raw_tokens: 1_100,
                total_compressed_tokens: 650,
                avg_savings_pct: 25.0,
                tasks_passed: 1,
                tasks_total: 1,
                avg_quality_score: 1.0,
                avg_latency_us: 1_000,
            }],
        });

        let output = report.to_markdown();
        assert!(output.contains("40.9%"));
        assert!(!output.contains("25.0%"));
    }
}
