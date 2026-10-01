//! Deterministic comparison of several lean-ctx treatments against one shared baseline.

use std::collections::HashSet;
use std::fmt::Write as _;

use anyhow::{Context, Result, bail};
use serde::Serialize;

use super::conditions::{Condition, assemble};
use super::model::{ModelFingerprint, ModelRunner};
use super::report::{AbReport, AbStats, PairRecord, Verdict};
use super::scorers::score_task;
use super::suite::EvalSuite;
use super::{AbRunConfig, build_request};

pub const FRONTIER_KIND: &str = "lean-ctx.eval-frontier-report";
pub const FRONTIER_SCHEMA_VERSION: u32 = 1;

/// A stable, timestamp-free summary for each strategy in a frontier run.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FrontierStrategyResult {
    pub strategy: String,
    pub quality_delta: f64,
    pub baseline_tokens: usize,
    pub strategy_tokens: usize,
    pub token_reduction_percent: f64,
    pub stats: AbStats,
    pub verdict: Verdict,
    pub records: Vec<PairRecord>,
}

/// Output deliberately omits `AbReport::created_at`, so table and JSON bytes are repeatable.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FrontierReport {
    pub schema_version: u32,
    pub kind: String,
    pub suite: String,
    pub budget_tokens: usize,
    pub model: ModelFingerprint,
    pub strategies: Vec<FrontierStrategyResult>,
}

impl FrontierReport {
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }

    pub fn render_table(&self) -> String {
        let mut out = String::new();
        let version = self
            .model
            .params
            .version
            .as_deref()
            .unwrap_or("unspecified");
        let _ = writeln!(
            out,
            "Strategy frontier: {}\nModel: {} ({}, version={})\nBudget: {} tokens / condition\n",
            self.suite, self.model.params.model, self.model.provider, version, self.budget_tokens
        );
        let _ = writeln!(
            out,
            "{:<14} {:>5} {:>12} {:>17} {:>19} {:>16}",
            "strategy", "tasks", "quality Δ", "token reduction", "wins/ties/losses", "verdict"
        );
        for result in &self.strategies {
            let _ = writeln!(
                out,
                "{:<14} {:>5} {:+.3} {:+.1}% {:>6}/{:>6}/{:<6} {:>16}",
                result.strategy,
                result.stats.n,
                result.quality_delta,
                result.token_reduction_percent,
                result.stats.wins,
                result.stats.ties,
                result.stats.losses,
                result.verdict.label()
            );
        }
        out
    }

    pub fn gate_passes(&self) -> bool {
        self.strategies
            .iter()
            .all(|result| result.verdict.gate_passes())
    }
}

/// Parse the real treatment names accepted by `eval frontier --strategies`.
pub fn parse_strategies(names: &str) -> Result<Vec<Condition>> {
    let strategies = names
        .split(',')
        .map(|name| match name.trim() {
            "lean_ctx" => Ok(Condition::LeanCtx),
            "json_crush" => Ok(Condition::JsonCrush),
            "tabular_crush" => Ok(Condition::TabularCrush),
            "yaml_crush" => Ok(Condition::YamlCrush),
            other => bail!(
                "unknown strategy {other:?}; choose lean_ctx, json_crush, tabular_crush, or yaml_crush"
            ),
        })
        .collect::<Result<Vec<_>>>()?;
    canonical_strategies(strategies)
}

fn canonical_strategies(mut strategies: Vec<Condition>) -> Result<Vec<Condition>> {
    if strategies.len() < 2 {
        bail!("frontier requires at least two strategies");
    }
    let mut seen = HashSet::with_capacity(strategies.len());
    for strategy in &strategies {
        if *strategy == Condition::Baseline {
            bail!("baseline is implicit and cannot be listed as a strategy");
        }
        if !seen.insert(strategy.label()) {
            bail!("duplicate strategy: {}", strategy.label());
        }
    }
    strategies.sort_by_key(|strategy| strategy.label());
    Ok(strategies)
}

/// Runs the baseline once per task, then compares each treatment to those same baseline answers.
/// Each strategy's paired statistics and verdict come from the existing `AbReport::build` path.
pub fn run_frontier(
    suite: &EvalSuite,
    suite_name: &str,
    runner: &dyn ModelRunner,
    cfg: &AbRunConfig,
    strategies: &[Condition],
) -> Result<FrontierReport> {
    suite.validate().context("validating eval suite")?;
    let strategies = canonical_strategies(strategies.to_vec())?;
    let mut records_by_strategy = vec![Vec::with_capacity(suite.tasks.len()); strategies.len()];

    for task in &suite.tasks {
        let workspace = task.resolve_workspace_path(&suite.dir)?;
        let baseline = assemble(
            Condition::Baseline,
            &workspace,
            task.query(),
            cfg.budget_tokens,
        )?;
        let baseline_response = runner.run(&build_request(&baseline.text, &task.prompt))?;
        let baseline_score = score_task(task, &baseline_response.text, &workspace)?;

        for (index, strategy) in strategies.iter().copied().enumerate() {
            let treatment = assemble(strategy, &workspace, task.query(), cfg.budget_tokens)?;
            let treatment_response = runner.run(&build_request(&treatment.text, &task.prompt))?;
            let treatment_score = score_task(task, &treatment_response.text, &workspace)?;
            records_by_strategy[index].push(PairRecord {
                task_id: task.id.clone(),
                domain: task.domain.label().to_string(),
                baseline_value: baseline_score.value,
                lean_ctx_value: treatment_score.value,
                baseline_passed: baseline_score.passed,
                lean_ctx_passed: treatment_score.passed,
                baseline_tokens: baseline.tokens,
                lean_ctx_tokens: treatment.tokens,
                baseline_context_digest: baseline.digest.clone(),
                lean_ctx_context_digest: treatment.digest,
                baseline_answer_digest: baseline_response.digest(),
                lean_ctx_answer_digest: treatment_response.digest(),
            });
        }
    }

    let mut results = Vec::with_capacity(strategies.len());
    for (strategy, records) in strategies.into_iter().zip(records_by_strategy) {
        let paired = AbReport::build(
            suite_name,
            cfg.budget_tokens,
            runner.fingerprint().clone(),
            records,
            cfg.report,
        );
        let baseline_tokens: usize = paired
            .records
            .iter()
            .map(|record| record.baseline_tokens)
            .sum();
        let strategy_tokens: usize = paired
            .records
            .iter()
            .map(|record| record.lean_ctx_tokens)
            .sum();
        let token_reduction_percent = if baseline_tokens == 0 {
            0.0
        } else {
            (baseline_tokens as f64 - strategy_tokens as f64) / baseline_tokens as f64 * 100.0
        };
        results.push(FrontierStrategyResult {
            strategy: strategy.label().to_string(),
            quality_delta: paired.stats.mean_delta,
            baseline_tokens,
            strategy_tokens,
            token_reduction_percent,
            stats: paired.stats,
            verdict: paired.verdict,
            records: paired.records,
        });
    }

    Ok(FrontierReport {
        schema_version: FRONTIER_SCHEMA_VERSION,
        kind: FRONTIER_KIND.to_string(),
        suite: suite_name.to_string(),
        budget_tokens: cfg.budget_tokens,
        model: runner.fingerprint().clone(),
        strategies: results,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::eval_ab::model::{
        ModelParams, ModelResponse, PROVIDER_RECORDED, RecordedRunner, Recording,
    };
    use std::path::PathBuf;

    #[test]
    fn strategy_order_table_and_json_are_deterministic() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("fixture.md"),
            "Service probe interval: amber-17 seconds.\n",
        )
        .unwrap();
        let raw = r#"{"id":"probe-interval","domain":"qa","prompt":"What is the service probe interval?","workspace":".","answers":["amber-17 seconds"]}"#;
        let suite = EvalSuite::parse(raw, PathBuf::from(root.path())).unwrap();
        let fingerprint = ModelFingerprint {
            provider: PROVIDER_RECORDED.into(),
            endpoint: "fixture".into(),
            params: ModelParams {
                model: "fixture-model".into(),
                ..ModelParams::default()
            },
        };
        let mut recording = Recording::new(fingerprint);
        let task = &suite.tasks[0];
        let workspace = task.resolve_workspace_path(&suite.dir).unwrap();
        for condition in [
            Condition::Baseline,
            Condition::LeanCtx,
            Condition::JsonCrush,
            Condition::TabularCrush,
            Condition::YamlCrush,
        ] {
            let context = assemble(condition, &workspace, task.query(), 4000).unwrap();
            let request = build_request(&context.text, &task.prompt);
            recording
                .entries
                .insert(request.key(), ModelResponse::new("amber-17 seconds"));
        }
        let runner = RecordedRunner::new(recording);
        let strategies = [
            Condition::YamlCrush,
            Condition::TabularCrush,
            Condition::LeanCtx,
            Condition::JsonCrush,
        ];
        let first = run_frontier(
            &suite,
            "fixture-suite",
            &runner,
            &AbRunConfig::default(),
            &strategies,
        )
        .unwrap();
        let second = run_frontier(
            &suite,
            "fixture-suite",
            &runner,
            &AbRunConfig::default(),
            &strategies,
        )
        .unwrap();

        let names: Vec<_> = first
            .strategies
            .iter()
            .map(|result| result.strategy.as_str())
            .collect();
        assert_eq!(
            names,
            ["json_crush", "lean_ctx", "tabular_crush", "yaml_crush"]
        );
        assert_eq!(first.render_table(), second.render_table());
        assert_eq!(first.to_json(), second.to_json());
        assert!(!first.to_json().contains("created_at"));
    }

    #[test]
    fn strategy_names_reject_unknown_duplicate_and_single_inputs() {
        assert!(parse_strategies("baseline,lean_ctx").is_err());
        assert!(parse_strategies("lean_ctx,lean_ctx").is_err());
        assert!(parse_strategies("lean_ctx").is_err());
        assert!(parse_strategies("lean_ctx,unknown").is_err());
    }
}
