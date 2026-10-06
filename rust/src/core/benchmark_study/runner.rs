//! Study runner: orchestrates all arms across a dataset.

use std::path::PathBuf;
use std::time::Duration;

use super::datasets::{humaneval, mbpp};
use super::experiment::{Arm, ArmResult, StudyConfig, StudyExperiment, TaskResult};
use super::llm_client::{self, LlmClientConfig};
use super::report::StudyReport;
use super::sandbox;

const SYSTEM_PROMPT: &str = "You are an expert Python programmer. \
    Write a complete, correct Python function that solves the given task. \
    Return ONLY the Python code, no explanations. \
    Do not include test code or example usage.";

/// Run the full benchmark study for the given datasets.
pub(crate) fn run_study(config: &StudyConfig, dataset_names: &[&str]) -> StudyReport {
    let mut experiments = Vec::new();

    for &name in dataset_names {
        let results: Vec<ArmResult> = config
            .arms
            .iter()
            .map(|arm| {
                tracing::info!(arm = %arm, dataset = name, "running arm");
                run_arm(config, name, *arm)
            })
            .collect();

        experiments.push(StudyExperiment {
            config: config.clone(),
            dataset_name: name.to_string(),
            results,
        });
    }

    StudyReport::from_experiments(experiments)
}

fn run_arm(config: &StudyConfig, dataset: &str, arm: Arm) -> ArmResult {
    let llm_config = match build_llm_config(config, arm) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(arm = %arm, error = %e, "failed to build LLM config");
            return empty_result(arm);
        }
    };

    match dataset {
        "humaneval" => run_humaneval(config, arm, &llm_config),
        "mbpp" => run_mbpp(config, arm, &llm_config),
        other => {
            tracing::warn!(dataset = other, "unknown dataset, skipping");
            empty_result(arm)
        }
    }
}

fn build_llm_config(config: &StudyConfig, arm: Arm) -> Result<LlmClientConfig, String> {
    let model = config.reference_model.clone();

    if let Ok(key) = std::env::var("ANTHROPIC_API_KEY")
        && !key.is_empty()
    {
        return if arm.uses_compression() {
            LlmClientConfig::via_proxy_anthropic(&model)
        } else {
            LlmClientConfig::direct_anthropic(&model)
        };
    }

    Ok(LlmClientConfig::via_proxy_openai(&model))
}

fn run_humaneval(config: &StudyConfig, arm: Arm, llm_config: &LlmClientConfig) -> ArmResult {
    let data_dir = dataset_dir();
    let dataset_path = match super::datasets::download::ensure_humaneval(&data_dir) {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(error = %e, "failed to ensure HumanEval dataset");
            return empty_result(arm);
        }
    };
    let tasks = match humaneval::load_from_ndjson(&dataset_path) {
        Ok(t) if !t.is_empty() => t,
        Ok(_) => {
            tracing::error!(path = %dataset_path.display(), "HumanEval dataset empty");
            return empty_result(arm);
        }
        Err(e) => {
            tracing::error!(path = %dataset_path.display(), error = %e, "failed to load HumanEval");
            return empty_result(arm);
        }
    };

    tracing::info!(arm = %arm, tasks = tasks.len(), "running HumanEval");
    let timeout = Duration::from_secs(config.task_timeout_secs);
    let mut task_results = Vec::with_capacity(tasks.len());
    let mut total_passed = 0usize;
    let mut total_input = 0u64;
    let mut total_output = 0u64;
    let mut total_cost = 0.0f64;

    for (i, task) in tasks.iter().enumerate() {
        let prompt = format!(
            "Complete the following Python function:\n\n{}\n\n\
             The function signature and docstring are given. \
             Write ONLY the complete function implementation.",
            task.prompt
        );

        let result = run_single_task(
            &task.task_id,
            &prompt,
            llm_config,
            &config.python_bin,
            timeout,
            |solution| humaneval::build_test_script(task, solution),
        );

        if result.passed {
            total_passed += 1;
        }
        total_input += result.input_tokens;
        total_output += result.output_tokens;
        total_cost += result.cost_usd;

        tracing::info!(
            arm = %arm,
            task = %task.task_id,
            progress = format!("{}/{}", i + 1, tasks.len()),
            passed = result.passed,
            "task done"
        );

        task_results.push(result);
    }

    ArmResult {
        arm,
        tasks_total: tasks.len(),
        tasks_passed: total_passed,
        total_input_tokens: total_input,
        total_output_tokens: total_output,
        total_cost_usd: total_cost,
        task_results,
    }
}

fn run_mbpp(config: &StudyConfig, arm: Arm, llm_config: &LlmClientConfig) -> ArmResult {
    let data_dir = dataset_dir();
    let dataset_path = match super::datasets::download::ensure_mbpp(&data_dir) {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(error = %e, "failed to ensure MBPP dataset");
            return empty_result(arm);
        }
    };
    let tasks = match mbpp::load_from_ndjson(&dataset_path) {
        Ok(t) if !t.is_empty() => t,
        Ok(_) => {
            tracing::error!(path = %dataset_path.display(), "MBPP dataset empty");
            return empty_result(arm);
        }
        Err(e) => {
            tracing::error!(path = %dataset_path.display(), error = %e, "failed to load MBPP");
            return empty_result(arm);
        }
    };

    tracing::info!(arm = %arm, tasks = tasks.len(), "running MBPP");
    let timeout = Duration::from_secs(config.task_timeout_secs);
    let mut task_results = Vec::with_capacity(tasks.len());
    let mut total_passed = 0usize;
    let mut total_input = 0u64;
    let mut total_output = 0u64;
    let mut total_cost = 0.0f64;

    for (i, task) in tasks.iter().enumerate() {
        let prompt = format!(
            "Write a Python function that solves this task:\n\n{}\n\n\
             Write ONLY the complete function, no test code.",
            task.text
        );

        let task_id = format!("MBPP/{}", task.task_id);
        let result = run_single_task(
            &task_id,
            &prompt,
            llm_config,
            &config.python_bin,
            timeout,
            |solution| mbpp::build_test_script(task, solution),
        );

        if result.passed {
            total_passed += 1;
        }
        total_input += result.input_tokens;
        total_output += result.output_tokens;
        total_cost += result.cost_usd;

        tracing::info!(
            arm = %arm,
            task = %task_id,
            progress = format!("{}/{}", i + 1, tasks.len()),
            passed = result.passed,
            "task done"
        );

        task_results.push(result);
    }

    ArmResult {
        arm,
        tasks_total: tasks.len(),
        tasks_passed: total_passed,
        total_input_tokens: total_input,
        total_output_tokens: total_output,
        total_cost_usd: total_cost,
        task_results,
    }
}

fn run_single_task<F>(
    task_id: &str,
    prompt: &str,
    llm_config: &LlmClientConfig,
    python_bin: &str,
    timeout: Duration,
    build_test: F,
) -> TaskResult
where
    F: Fn(&str) -> String,
{
    let completion = match llm_client::complete(llm_config, SYSTEM_PROMPT, prompt) {
        Ok(c) => c,
        Err(e) => {
            return TaskResult {
                task_id: task_id.into(),
                passed: false,
                input_tokens: 0,
                output_tokens: 0,
                cost_usd: 0.0,
                model_used: llm_config.model.clone(),
                compressed_tokens: None,
                latency_ms: 0,
                error: Some(e),
            };
        }
    };

    let code = llm_client::extract_code(&completion.content);
    let test_script = build_test(&code);
    let sandbox_result = sandbox::execute_python(python_bin, &test_script, timeout);

    let cost = llm_client::cost_for_tokens(
        &completion.model_used,
        completion.input_tokens,
        completion.output_tokens,
    );

    TaskResult {
        task_id: task_id.into(),
        passed: sandbox_result.passed,
        input_tokens: completion.input_tokens,
        output_tokens: completion.output_tokens,
        cost_usd: cost,
        model_used: completion.model_used,
        compressed_tokens: None,
        latency_ms: completion.latency_ms,
        error: if sandbox_result.passed {
            None
        } else {
            Some(sandbox_result.stderr)
        },
    }
}

fn dataset_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("LEAN_CTX_BENCHMARK_DATA") {
        return PathBuf::from(dir);
    }
    let data_dir = dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("lean-ctx")
        .join("benchmark-data");
    std::fs::create_dir_all(&data_dir).ok();
    data_dir
}

fn empty_result(arm: Arm) -> ArmResult {
    ArmResult {
        arm,
        tasks_total: 0,
        tasks_passed: 0,
        total_input_tokens: 0,
        total_output_tokens: 0,
        total_cost_usd: 0.0,
        task_results: vec![],
    }
}
