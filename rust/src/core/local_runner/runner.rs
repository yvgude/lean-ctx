use std::path::PathBuf;

use anyhow::Result;

use crate::core::agent_connector::traits::{AgentConnector, TaskRequest, TaskResult};
#[cfg(test)]
use crate::core::agent_connector::traits::{AgentInfo, TokenUsage};
use crate::core::benchmark_spec::types::{
    BenchmarkEvaluation, BenchmarkOutcome, BenchmarkResult, BenchmarkSpecV1, BenchmarkSummary,
    BenchmarkTask, EvaluationSpecV1, TaskKind,
};
use crate::core::eval_ab::scorers::{CodeScorer, Scorer, score_task};
use crate::core::eval_ab::suite::{Domain, Task as EvaluationTask};

const DEFAULT_TIMEOUT_MS: u64 = 300_000;

#[cfg(test)]
pub(crate) struct MockConnector {
    should_succeed: bool,
}

#[cfg(test)]
impl MockConnector {
    pub(crate) fn new(should_succeed: bool) -> Self {
        Self { should_succeed }
    }
}

#[cfg(test)]
impl AgentConnector for MockConnector {
    fn info(&self) -> AgentInfo {
        AgentInfo {
            name: "mock".into(),
            version: Some("1.0.0".into()),
            path: PathBuf::from("/usr/bin/mock"),
            capabilities: vec!["execute".into()],
            available: true,
        }
    }

    fn health_check(&self) -> Result<bool> {
        Ok(true)
    }

    fn execute(&self, request: &TaskRequest) -> Result<TaskResult> {
        Ok(TaskResult {
            task_id: request.id.clone(),
            agent: "mock".into(),
            model: "test-model".into(),
            success: self.should_succeed,
            exit_code: i32::from(!self.should_succeed),
            stdout: "task output".into(),
            stderr: String::new(),
            duration_ms: 1000,
            tokens_used: Some(TokenUsage {
                input_tokens: 500,
                output_tokens: 200,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
            }),
            provider_cost_micros: None,
            execution_receipt_ref: None,
        })
    }

    fn name(&self) -> &'static str {
        "mock"
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub(crate) struct RunConfig {
    pub agent_name: String,
    pub profile_name: String,
    pub suite_name: Option<String>,
    pub timeout_override_ms: Option<u64>,
    pub working_dir: PathBuf,
    pub repeats: u32,
}

impl Default for RunConfig {
    fn default() -> Self {
        Self {
            agent_name: String::new(),
            profile_name: "coder".into(),
            suite_name: None,
            timeout_override_ms: None,
            working_dir: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            repeats: 1,
        }
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub(crate) enum RunProgress {
    Starting,
    DetectingAgents,
    LoadingProfile,
    RunningTask {
        task_index: usize,
        total: usize,
        task_id: String,
    },
    TaskComplete {
        task_id: String,
        passed: bool,
    },
    AllComplete,
}

#[allow(dead_code)]
pub(crate) struct LocalRunner {
    config: RunConfig,
    connector: Box<dyn AgentConnector>,
}

#[allow(dead_code)]
impl LocalRunner {
    pub(crate) fn new(config: RunConfig, connector: Box<dyn AgentConnector>) -> Self {
        Self { config, connector }
    }

    pub(crate) fn run(&self, spec: &BenchmarkSpecV1) -> Result<BenchmarkResult> {
        self.run_with_profile(spec, &self.config.profile_name)
    }

    pub(crate) fn run_with_profile(
        &self,
        spec: &BenchmarkSpecV1,
        profile_name: &str,
    ) -> Result<BenchmarkResult> {
        self.run_with_profile_progress(spec, profile_name, |_| {})
    }

    pub(crate) fn run_with_progress<F>(
        &self,
        spec: &BenchmarkSpecV1,
        on_progress: F,
    ) -> Result<BenchmarkResult>
    where
        F: Fn(RunProgress),
    {
        self.run_with_profile_progress(spec, &self.config.profile_name, on_progress)
    }

    fn run_with_profile_progress<F>(
        &self,
        spec: &BenchmarkSpecV1,
        profile_name: &str,
        on_progress: F,
    ) -> Result<BenchmarkResult>
    where
        F: Fn(RunProgress),
    {
        spec.validate()
            .map_err(|error| anyhow::anyhow!("invalid benchmark spec: {error}"))?;
        on_progress(RunProgress::Starting);
        let total = spec.suite.tasks.len();
        let repeats = spec.configuration.repeats;
        let mut outcomes = Vec::with_capacity(total * repeats as usize);

        for repeat in 0..repeats {
            for (idx, task) in spec.suite.tasks.iter().enumerate() {
                on_progress(RunProgress::RunningTask {
                    task_index: idx + (repeat as usize * total),
                    total: total * repeats as usize,
                    task_id: task.id.clone(),
                });
                let request = self.task_request_for_profile(spec, task, profile_name);
                let result = self.connector.execute(&request)?;
                let outcome = outcome_from_task_result(task, &result, &self.config.working_dir)?;
                let passed = outcome.passed;
                on_progress(RunProgress::TaskComplete {
                    task_id: task.id.clone(),
                    passed,
                });
                outcomes.push(outcome);
            }
        }

        let summary = BenchmarkSummary::from_outcomes(&outcomes, spec.configuration.quality_floor);
        on_progress(RunProgress::AllComplete);

        Ok(BenchmarkResult {
            spec_id: spec.id.clone(),
            spec_version: spec.version.clone(),
            profile_hash: spec.configuration.profile_hash.clone().unwrap_or_default(),
            agent: self.connector.name().to_owned(),
            model: spec
                .configuration
                .model
                .clone()
                .unwrap_or_else(|| "unknown".to_owned()),
            runtime_version: spec.configuration.runtime_version.clone(),
            outcomes,
            summary,
            completed_at: current_unix_timestamp(),
        })
    }

    fn task_request(&self, spec: &BenchmarkSpecV1, task: &BenchmarkTask) -> TaskRequest {
        self.task_request_for_profile(spec, task, &self.config.profile_name)
    }

    fn task_request_for_profile(
        &self,
        spec: &BenchmarkSpecV1,
        task: &BenchmarkTask,
        profile_name: &str,
    ) -> TaskRequest {
        TaskRequest {
            id: task.id.clone(),
            prompt: task_to_prompt(task),
            working_dir: self.config.working_dir.clone(),
            timeout_ms: self
                .config
                .timeout_override_ms
                .or(task.timeout_ms)
                .unwrap_or(DEFAULT_TIMEOUT_MS),
            model: spec.configuration.model.clone(),
            max_turns: None,
            profile_name: Some(profile_name.to_owned()),
            profile_hash: spec.configuration.profile_hash.clone(),
        }
    }
}

#[allow(dead_code)]
pub(crate) fn task_to_prompt(task: &BenchmarkTask) -> String {
    let kind_instruction = match task.kind {
        TaskKind::Explore => "Explore and understand the repository structure.",
        TaskKind::LocateRegression => "Find the source of a regression in the codebase.",
        TaskKind::FixBug => "Diagnose and fix the reported bug.",
        TaskKind::RunTests => "Run the project's test suite and report results.",
        TaskKind::ExplainArchitecture => "Produce a clear architectural summary.",
        TaskKind::Custom => "Complete the following task as described.",
    };
    format!("{kind_instruction}\n\n{}", task.description)
}

fn outcome_from_task_result(
    task: &BenchmarkTask,
    result: &TaskResult,
    working_dir: &std::path::Path,
) -> Result<BenchmarkOutcome> {
    let tokens_in = result.tokens_used.as_ref().map_or(0, |t| t.input_tokens);
    let tokens_out = result.tokens_used.as_ref().map_or(0, |t| t.output_tokens);
    let cost = result
        .provider_cost_micros
        .map_or(0.0, |micros| micros as f64 / 1_000_000.0);
    let evaluation = task
        .evaluation
        .as_ref()
        .map(|spec| evaluate_task(task, spec, &result.stdout, working_dir))
        .transpose()?;
    let (passed, quality_score) = evaluation.as_ref().map_or((false, 0.0), |evaluation| {
        (evaluation.passed, evaluation.score)
    });
    let error = match (result.stderr.trim(), result.success) {
        (stderr, _) if !stderr.is_empty() => Some(stderr.to_owned()),
        (_, false) => Some(format!("agent exited with code {}", result.exit_code)),
        _ => None,
    };
    Ok(BenchmarkOutcome {
        task_id: task.id.clone(),
        passed,
        cost_usd: cost,
        quality_score,
        latency_ms: result.duration_ms,
        tokens_input: tokens_in,
        tokens_output: tokens_out,
        error,
        evaluation,
        execution_receipt_ref: result.execution_receipt_ref.clone(),
    })
}

fn evaluate_task(
    task: &BenchmarkTask,
    spec: &EvaluationSpecV1,
    output: &str,
    working_dir: &std::path::Path,
) -> Result<BenchmarkEvaluation> {
    spec.validate().map_err(anyhow::Error::msg)?;
    match spec {
        EvaluationSpecV1::Qa {
            answers,
            minimum_f1,
        } => {
            let evaluation_task = EvaluationTask {
                id: task.id.clone(),
                domain: Domain::Qa,
                task_class: None,
                prompt: task.description.clone(),
                workspace: working_dir.display().to_string(),
                retrieval_query: None,
                answers: answers.clone(),
                target_file: None,
                test_cmd: None,
            };
            let score = score_task(&evaluation_task, output, working_dir)?;
            Ok(BenchmarkEvaluation {
                evaluator_id: spec.id().to_owned(),
                metric: score.metric,
                score: score.value,
                passed: score.passed && score.value >= *minimum_f1,
                detail: score.detail,
                output_digest: blake3::hash(output.as_bytes()).to_hex().to_string(),
            })
        }
        EvaluationSpecV1::Code {
            target_file,
            test_cmd,
        } => {
            let evaluation_task = EvaluationTask {
                id: task.id.clone(),
                domain: Domain::Code,
                task_class: None,
                prompt: task.description.clone(),
                workspace: working_dir.display().to_string(),
                retrieval_query: None,
                answers: Vec::new(),
                target_file: Some(target_file.clone()),
                test_cmd: Some(test_cmd.clone()),
            };
            let score = CodeScorer::isolated().score(&evaluation_task, output, working_dir)?;
            Ok(BenchmarkEvaluation {
                evaluator_id: spec.id().to_owned(),
                metric: score.metric,
                score: score.value,
                passed: score.passed,
                detail: score.detail,
                output_digest: blake3::hash(output.as_bytes()).to_hex().to_string(),
            })
        }
    }
}

fn current_unix_timestamp() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn test_spec() -> BenchmarkSpecV1 {
        use crate::core::benchmark_spec::types::{
            BenchmarkConfiguration, BenchmarkKind, BenchmarkSuite,
        };
        BenchmarkSpecV1 {
            id: "test-spec".into(),
            version: "1.0.0".into(),
            name: "Test".into(),
            description: "Test benchmark".into(),
            suite: BenchmarkSuite {
                kind: BenchmarkKind::TaskScore,
                tasks: vec![
                    BenchmarkTask {
                        id: "t1".into(),
                        name: "Task 1".into(),
                        description: "Do task 1".into(),
                        kind: TaskKind::Explore,
                        timeout_ms: Some(60_000),
                        evaluation: Some(EvaluationSpecV1::Qa {
                            answers: vec!["task output".into()],
                            minimum_f1: 1.0,
                        }),
                    },
                    BenchmarkTask {
                        id: "t2".into(),
                        name: "Task 2".into(),
                        description: "Do task 2".into(),
                        kind: TaskKind::FixBug,
                        timeout_ms: Some(120_000),
                        evaluation: Some(EvaluationSpecV1::Qa {
                            answers: vec!["task output".into()],
                            minimum_f1: 1.0,
                        }),
                    },
                ],
            },
            configuration: BenchmarkConfiguration {
                profile_hash: Some("abc123".into()),
                agent: None,
                model: None,
                runtime_version: "test".into(),
                repeats: 1,
                quality_floor: 0.95,
            },
            created_at: "0".into(),
        }
    }

    #[test]
    fn new_runner_sets_config() {
        let runner = LocalRunner::new(RunConfig::default(), Box::new(MockConnector::new(true)));
        assert_eq!(runner.config.profile_name, "coder");
    }

    #[test]
    fn run_with_mock_connector() {
        let runner = LocalRunner::new(RunConfig::default(), Box::new(MockConnector::new(true)));
        let result = runner.run(&test_spec()).unwrap();
        assert_eq!(result.outcomes.len(), 2);
        assert!(result.outcomes.iter().all(|o| o.passed));
        assert_eq!(result.summary.passed_tasks, 2);
        assert!(result.summary.quality_evaluated);
        assert!(!result.summary.receipt_evidence_complete);
    }

    #[test]
    fn evaluator_not_exit_code_decides_quality_and_preserves_receipt_link() {
        let mut spec = test_spec();
        let task = spec.suite.tasks.remove(0);
        let result = TaskResult {
            task_id: task.id.clone(),
            agent: "mock".into(),
            model: "test-model".into(),
            success: false,
            exit_code: 1,
            stdout: "task output".into(),
            stderr: String::new(),
            duration_ms: 1000,
            tokens_used: None,
            provider_cost_micros: None,
            execution_receipt_ref: Some("receipt:task-1".into()),
        };

        let outcome = outcome_from_task_result(&task, &result, std::path::Path::new("."))
            .expect("declared evaluator should score output");

        assert!(outcome.passed);
        assert_eq!(outcome.quality_score, 1.0);
        assert_eq!(
            outcome.execution_receipt_ref.as_deref(),
            Some("receipt:task-1")
        );
        assert_eq!(outcome.error.as_deref(), Some("agent exited with code 1"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn isolated_code_evaluator_scores_a_fixture_without_touching_the_workspace() {
        let workspace = tempfile::tempdir().expect("temporary fixture workspace");
        std::fs::write(
            workspace.path().join("solution.sh"),
            "add() { printf '%s\\n' 0; }\n",
        )
        .expect("write broken fixture");
        std::fs::write(
            workspace.path().join("test.sh"),
            ". ./solution.sh\n[ \"$(add 2 3)\" = \"5\" ]\n",
        )
        .expect("write deterministic test");
        let task = BenchmarkTask {
            id: "repair".into(),
            name: "Repair".into(),
            description: "Return the repaired solution.".into(),
            kind: TaskKind::FixBug,
            timeout_ms: None,
            evaluation: Some(EvaluationSpecV1::Code {
                target_file: "solution.sh".into(),
                test_cmd: "sh test.sh".into(),
            }),
        };

        let evaluation = evaluate_task(
            &task,
            task.evaluation.as_ref().expect("evaluator"),
            "add() { printf '%s\\n' \"$(( $1 + $2 ))\"; }",
            workspace.path(),
        )
        .expect("isolated evaluator must run");

        assert_eq!(evaluation.evaluator_id, "code-unit-test-v1");
        assert_eq!(evaluation.metric, "unit_test");
        assert!(evaluation.passed);
        assert_eq!(
            std::fs::read_to_string(workspace.path().join("solution.sh"))
                .expect("original fixture remains readable"),
            "add() { printf '%s\\n' 0; }\n"
        );
    }

    #[test]
    fn outcome_uses_only_explicit_provider_cost() {
        let mut spec = test_spec();
        let task = spec.suite.tasks.remove(0);
        let result = TaskResult {
            task_id: task.id.clone(),
            agent: "mock".into(),
            model: "test-model".into(),
            success: true,
            exit_code: 0,
            stdout: "task output".into(),
            stderr: String::new(),
            duration_ms: 1_000,
            tokens_used: Some(TokenUsage {
                input_tokens: 1_000,
                output_tokens: 500,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
            }),
            provider_cost_micros: Some(42),
            execution_receipt_ref: Some("receipt:task-1".into()),
        };

        let outcome = outcome_from_task_result(&task, &result, std::path::Path::new("."))
            .expect("declared evaluator should score output");
        assert!((outcome.cost_usd - 0.000_042).abs() < f64::EPSILON);

        let unpriced = TaskResult {
            provider_cost_micros: None,
            ..result
        };
        let outcome = outcome_from_task_result(&task, &unpriced, std::path::Path::new("."))
            .expect("declared evaluator should score output");
        assert_eq!(outcome.cost_usd, 0.0);
    }

    #[test]
    fn missing_evaluator_blocks_quality_floor() {
        let mut spec = test_spec();
        spec.suite.tasks[0].evaluation = None;
        let runner = LocalRunner::new(RunConfig::default(), Box::new(MockConnector::new(true)));

        let result = runner
            .run(&spec)
            .expect("runner should preserve observed output");

        assert!(!result.summary.quality_evaluated);
        assert!(!result.summary.quality_floor_met);
        assert!(!result.outcomes[0].passed);
    }

    #[test]
    fn task_request_uses_task_timeout_then_default() {
        let runner = LocalRunner::new(RunConfig::default(), Box::new(MockConnector::new(true)));
        let spec = test_spec();

        assert_eq!(
            runner.task_request(&spec, &spec.suite.tasks[0]).timeout_ms,
            60_000
        );

        let task_without_timeout = BenchmarkTask {
            id: "default-timeout".into(),
            name: "Default timeout".into(),
            description: "Use the runner default".into(),
            kind: TaskKind::Custom,
            timeout_ms: None,
            evaluation: None,
        };
        assert_eq!(
            runner.task_request(&spec, &task_without_timeout).timeout_ms,
            DEFAULT_TIMEOUT_MS
        );
    }

    #[test]
    fn task_request_uses_explicit_run_profile() {
        let runner = LocalRunner::new(RunConfig::default(), Box::new(MockConnector::new(true)));
        let spec = test_spec();
        let request = runner.task_request_for_profile(&spec, &spec.suite.tasks[0], "benchmark-a");

        assert_eq!(request.profile_name.as_deref(), Some("benchmark-a"));
    }

    #[test]
    fn progress_callback_fires() {
        let counter = AtomicUsize::new(0);
        let runner = LocalRunner::new(RunConfig::default(), Box::new(MockConnector::new(true)));
        runner
            .run_with_progress(&test_spec(), |_| {
                counter.fetch_add(1, Ordering::Relaxed);
            })
            .unwrap();
        assert_eq!(counter.load(Ordering::Relaxed), 6);
    }

    #[test]
    fn task_to_prompt_covers_all_kinds() {
        for kind in &[
            TaskKind::Explore,
            TaskKind::LocateRegression,
            TaskKind::FixBug,
            TaskKind::RunTests,
            TaskKind::ExplainArchitecture,
            TaskKind::Custom,
        ] {
            let task = BenchmarkTask {
                id: "test".into(),
                name: "Test".into(),
                description: "Test task".into(),
                kind: *kind,
                timeout_ms: None,
                evaluation: None,
            };
            let prompt = task_to_prompt(&task);
            assert!(!prompt.is_empty());
            assert!(prompt.contains(&task.description));
        }
    }
}
