// SPDX-License-Identifier: Apache-2.0
//! Decision Loop scenarios with a deterministic in-process mock provider.

use std::sync::Arc;

use anyhow::{Context, Result, bail, ensure};
use clap::{Args, Parser};
use serde::Serialize;

use lean_ctx::core::{
    decision_loop::{DecisionLoop, protocol_profile},
    knowledge_router::{KnowledgeRouter, PatternReferenceResolver, builtin_manifests},
    shadow::{ShadowEngine, ShadowTask},
    triage::{
        TaskAnalysisInput, TriageEngine, rules::RuleTriageBackend,
        semantic_analyzer::SemanticAnalyzer,
    },
    value_gate::{ExecutionCost, OutcomeSignal, cost_tracker::calculate_cost},
};

#[derive(Args, Debug, Clone)]
pub(crate) struct ScenarioArgs {
    /// Which scenario to run (all, triage, knowledge, shadow, value-gate, full-loop)
    #[arg(default_value = "all")]
    pub scenario: String,

    /// Output format
    #[arg(long, default_value = "text", value_parser = ["text", "json"])]
    pub format: String,
}

#[derive(Parser, Debug)]
#[command(name = "scenario", disable_help_subcommand = true)]
struct ScenarioCli {
    #[command(flatten)]
    args: ScenarioArgs,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct ScenarioResult {
    scenario: &'static str,
    passed: bool,
    evidence: Vec<String>,
}

#[derive(Debug, Clone, Copy)]
struct MockProvider {
    model: &'static str,
    provider: &'static str,
}

impl MockProvider {
    fn execute(self, input_tokens: u64) -> (ExecutionCost, Vec<OutcomeSignal>) {
        let output_tokens = 400;
        (
            ExecutionCost {
                input_tokens,
                output_tokens,
                cache_read_tokens: 0,
                model: self.model.into(),
                provider: self.provider.into(),
                estimated_cost_micros: calculate_cost(input_tokens, output_tokens, 0, self.model),
            },
            vec![
                OutcomeSignal::BuildSucceeded,
                OutcomeSignal::TestsPassed,
                OutcomeSignal::UserAccepted,
            ],
        )
    }
}

const MOCK_PROVIDER: MockProvider = MockProvider {
    model: "gpt-4o",
    provider: "scenario-mock",
};

/// Runs one or all Decision Loop scenarios and prints auditable evidence.
pub(crate) fn cmd_scenario(args: &ScenarioArgs) -> Result<()> {
    let results = match args.scenario.as_str() {
        "all" => run_all_scenarios(),
        "triage" => Ok(vec![run_triage_scenario()?]),
        "knowledge" => Ok(vec![run_knowledge_scenario()?]),
        "shadow" => Ok(vec![run_shadow_scenario()?]),
        "value-gate" => Ok(vec![run_value_gate_scenario()?]),
        "full-loop" => Ok(vec![run_full_loop_scenario()?]),
        other => bail!("Unknown scenario: {other}"),
    }?;
    print!("{}", render(&args.format, &results)?);
    Ok(())
}

pub(crate) fn cmd_scenario_from_cli(args: &[String]) -> Result<()> {
    let mut cli_args = Vec::with_capacity(args.len() + 1);
    cli_args.push("scenario".to_owned());
    cli_args.extend(args.iter().cloned());
    let cli = ScenarioCli::try_parse_from(cli_args)
        .map_err(|error| anyhow::anyhow!(error.to_string().trim_end().to_owned()))?;
    cmd_scenario(&cli.args)
}

fn run_all_scenarios() -> Result<Vec<ScenarioResult>> {
    Ok(vec![
        run_triage_scenario()?,
        run_knowledge_scenario()?,
        run_shadow_scenario()?,
        run_value_gate_scenario()?,
        run_full_loop_scenario()?,
    ])
}

fn run_triage_scenario() -> Result<ScenarioResult> {
    let data_dir = lean_ctx::core::data_dir::lean_ctx_data_dir()
        .unwrap_or_else(|_| std::path::PathBuf::from("."));
    let engine = TriageEngine::new(vec![
        Box::new(RuleTriageBackend),
        Box::new(SemanticAnalyzer::from_data_dir(&data_dir)),
    ]);
    let tasks = [
        "fix the authentication bug in src/auth.rs",
        "review PR #42 for the release branch",
        "refactor the knowledge router across modules",
        "add tests for the value gate",
        "explain the shadow comparison report",
    ];
    let mut evidence = Vec::with_capacity(tasks.len());

    for task in tasks {
        let hypothesis = engine
            .analyze(&TaskAnalysisInput {
                query: task.into(),
                ..Default::default()
            })
            .with_context(|| format!("triage task '{task}'"))?;
        let profile = hypothesis.profile;
        ensure!(
            !profile.task_class.is_empty(),
            "triage task '{task}' has no task class"
        );
        ensure!(
            matches!(profile.complexity.as_str(), "low" | "medium" | "high"),
            "triage task '{task}' has invalid complexity '{}'",
            profile.complexity
        );
        ensure!(
            (1..=1_000).contains(&profile.confidence_milli),
            "triage task '{task}' has invalid confidence {}",
            profile.confidence_milli
        );
        evidence.push(format!(
            "{task}: class={}, complexity={}, confidence={}",
            profile.task_class, profile.complexity, profile.confidence_milli
        ));
    }

    Ok(ScenarioResult {
        scenario: "triage",
        passed: true,
        evidence,
    })
}

fn run_knowledge_scenario() -> Result<ScenarioResult> {
    let query = "review LEAN-42 and #17 in src/core/decision_loop.rs";
    let profile = triage(query)?;
    let router = KnowledgeRouter {
        manifests: builtin_manifests(),
        resolvers: vec![Arc::new(PatternReferenceResolver)],
    };
    let result = router.route(
        "scenario-knowledge",
        query,
        &protocol_profile(&profile),
        &[],
        None,
    );

    ensure!(
        !result.bundle.candidates.is_empty(),
        "knowledge router did not materialize a context bundle"
    );
    ensure!(
        result
            .receipt
            .sources_used
            .iter()
            .any(|source| source == "jira"),
        "knowledge router did not include the expected Jira source"
    );
    ensure!(
        result
            .receipt
            .sources_used
            .iter()
            .any(|source| source == "github"),
        "knowledge router did not include the expected GitHub source"
    );

    Ok(ScenarioResult {
        scenario: "knowledge",
        passed: true,
        evidence: vec![
            format!("bundle={}", result.bundle.bundle_id),
            format!("sources={}", result.receipt.sources_used.join(",")),
            format!("selected={}", result.receipt.candidates_selected),
            format!("tokens={}", result.bundle.total_tokens),
        ],
    })
}

fn run_shadow_scenario() -> Result<ScenarioResult> {
    let report = ShadowEngine::run_comparison(&[ShadowTask {
        task_id: "scenario-shadow".into(),
        query: "summarize decision loop evidence".into(),
        raw_input_tokens: 4_000,
        compressed_input_tokens: 1_000,
        output_tokens: 400,
        model_used: MOCK_PROVIDER.model.into(),
        outcome_signals: vec![OutcomeSignal::BuildSucceeded, OutcomeSignal::TestsPassed],
        duration_ms: 120,
    }]);

    ensure!(
        report.baseline.total_cost_micros > report.treatment.total_cost_micros,
        "shadow treatment did not cost less than the baseline"
    );
    ensure!(
        report.savings.absolute_micros > 0,
        "shadow report has no savings estimate"
    );
    ensure!(
        report.savings.quality_maintained,
        "shadow treatment did not maintain outcome quality"
    );

    Ok(ScenarioResult {
        scenario: "shadow",
        passed: true,
        evidence: vec![
            format!("baseline_cost_micros={}", report.baseline.total_cost_micros),
            format!(
                "treatment_cost_micros={}",
                report.treatment.total_cost_micros
            ),
            format!("savings_micros={}", report.savings.absolute_micros),
            format!("savings_percent={:.1}", report.savings.relative_percent),
        ],
    })
}

fn run_value_gate_scenario() -> Result<ScenarioResult> {
    let loop_ = DecisionLoop::default();
    let mut result = loop_.execute_task(
        "add a value gate regression test",
        "scenario-session",
        "scenario-agent",
    );
    let (cost, signals) = MOCK_PROVIDER.execute(1_000);
    let expected_cpao = cost.estimated_cost_micros;
    loop_.complete_task(&mut result, cost, signals);
    let assessment = result
        .assessment
        .as_ref()
        .context("value gate did not create an assessment")?;

    ensure!(
        assessment.outcome_accepted,
        "value gate rejected the mock outcome"
    );
    ensure!(
        assessment.cpao_micros == Some(expected_cpao),
        "value gate CPAO {:?} did not equal expected {expected_cpao}",
        assessment.cpao_micros
    );
    ensure!(
        assessment
            .evidence
            .iter()
            .any(|item| item == "signal=TestsPassed"),
        "value gate assessment omitted outcome evidence"
    );

    Ok(ScenarioResult {
        scenario: "value-gate",
        passed: true,
        evidence: vec![
            format!("model={}", assessment.model),
            format!("cost_micros={}", assessment.cost_micros),
            format!("cpao_micros={expected_cpao}"),
            "outcome_accepted=true".into(),
        ],
    })
}

fn run_full_loop_scenario() -> Result<ScenarioResult> {
    let query = "review LEAN-42 and #17 in src/core/decision_loop.rs";
    let loop_ = DecisionLoop::default();
    let mut result = loop_.execute_task(query, "scenario-session", "scenario-agent");
    ensure!(
        result.envelope_created,
        "decision loop did not create a task envelope"
    );
    ensure!(
        !result.profile.task_class.is_empty(),
        "decision loop did not enrich the task profile"
    );

    let router = KnowledgeRouter {
        manifests: builtin_manifests(),
        resolvers: vec![Arc::new(PatternReferenceResolver)],
    };
    let routing = router.route(
        &result.task_id,
        query,
        &protocol_profile(&result.profile),
        &[],
        None,
    );
    ensure!(
        !routing.bundle.candidates.is_empty(),
        "full loop did not create a context bundle"
    );

    let (cost, signals) = MOCK_PROVIDER.execute(routing.bundle.total_tokens);
    loop_.complete_task(&mut result, cost, signals);
    let assessment = result
        .assessment
        .as_ref()
        .context("full loop did not create a value assessment")?;
    ensure!(
        assessment.outcome_accepted,
        "full loop rejected the mock outcome"
    );
    ensure!(
        assessment.cpao_micros.is_some(),
        "full loop did not calculate CPAO"
    );
    ensure!(
        assessment
            .evidence
            .iter()
            .any(|item| item == "signal=BuildSucceeded"),
        "full loop assessment omitted execution evidence"
    );

    Ok(ScenarioResult {
        scenario: "full-loop",
        passed: true,
        evidence: vec![
            format!(
                "triage=class:{},complexity:{},confidence:{}",
                result.profile.task_class,
                result.profile.complexity,
                result.profile.confidence_milli
            ),
            format!(
                "knowledge=selected:{},sources:{}",
                routing.receipt.candidates_selected,
                routing.receipt.sources_used.join(",")
            ),
            format!(
                "execution=provider:{},model:{},tokens:{}",
                MOCK_PROVIDER.provider, assessment.model, assessment.total_tokens
            ),
            format!(
                "value-gate=cpao_micros:{}",
                assessment.cpao_micros.unwrap_or(0)
            ),
            "evidence=BuildSucceeded,TestsPassed,UserAccepted".into(),
        ],
    })
}

fn triage(query: &str) -> Result<lean_ctx::core::triage::profile::TaskProfileLocal> {
    Ok(TriageEngine::default()
        .analyze(&TaskAnalysisInput {
            query: query.into(),
            ..Default::default()
        })
        .context("triage knowledge scenario")?
        .profile)
}

fn render(format: &str, results: &[ScenarioResult]) -> Result<String> {
    match format {
        "json" => Ok(format!("{}\n", serde_json::to_string_pretty(results)?)),
        "text" => Ok(results
            .iter()
            .map(|result| {
                format!(
                    "Scenario: {}\nTesting: {} decision-loop path\nResult: {}\nEvidence:\n{}\n",
                    result.scenario,
                    result.scenario,
                    if result.passed { "PASS" } else { "FAIL" },
                    result
                        .evidence
                        .iter()
                        .map(|item| format!("- {item}"))
                        .collect::<Vec<_>>()
                        .join("\n")
                )
            })
            .collect::<Vec<_>>()
            .join("\n")),
        other => bail!("Unknown output format: {other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn triage_scenario_runs() {
        assert!(run_triage_scenario().unwrap().passed);
    }

    #[test]
    fn knowledge_scenario_runs() {
        assert!(run_knowledge_scenario().unwrap().passed);
    }

    #[test]
    fn shadow_scenario_runs() {
        assert!(run_shadow_scenario().unwrap().passed);
    }

    #[test]
    fn value_gate_scenario_runs() {
        assert!(run_value_gate_scenario().unwrap().passed);
    }

    #[test]
    fn full_loop_produces_complete_evidence() {
        let result = run_full_loop_scenario().unwrap();
        assert!(result.passed);
        assert!(
            result
                .evidence
                .iter()
                .any(|item| item.starts_with("triage="))
        );
        assert!(
            result
                .evidence
                .iter()
                .any(|item| item.starts_with("knowledge="))
        );
        assert!(
            result
                .evidence
                .iter()
                .any(|item| item.starts_with("execution="))
        );
        assert!(
            result
                .evidence
                .iter()
                .any(|item| item.starts_with("value-gate="))
        );
    }

    #[test]
    fn all_scenarios_run_without_panic() {
        assert_eq!(run_all_scenarios().unwrap().len(), 5);
    }

    #[test]
    fn json_output_format_works() {
        let result = run_full_loop_scenario().unwrap();
        let output = render("json", &[result]).unwrap();
        let json: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(json[0]["scenario"], "full-loop");
        assert_eq!(json[0]["passed"], true);
    }

    #[test]
    fn cli_accepts_scenario_and_json_format() {
        let args = ["full-loop".into(), "--format".into(), "json".into()];
        assert!(cmd_scenario_from_cli(&args).is_ok());
    }
}
