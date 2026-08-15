//! End-to-end, deterministic workloads for the proxy compression pipeline.
//!
//! Run `cargo test pipeline_benchmark -- --nocapture` to print the report.

use std::time::Instant;

use serde_json::{Value, json};

use super::compress_api::{CompressRequest, compress_messages};

const INPUT_COST_PER_MILLION_TOKENS_USD: f64 = 3.0;

/// A representative conversation workload and the savings range it is designed
/// to exercise. The range is reported rather than asserted because compression
/// policies deliberately evolve independently of this benchmark.
#[derive(Debug, Clone, Copy)]
pub struct BenchmarkScenario {
    pub name: &'static str,
    pub message_count: usize,
    pub expected_savings_min_pct: f64,
    pub expected_savings_max_pct: f64,
}

/// Collection of the representative workloads used by [`run_benchmark`].
#[derive(Debug, Clone)]
pub struct PipelineBenchmark {
    pub scenarios: Vec<BenchmarkScenario>,
}

/// Measurements for one compression workload.
#[derive(Debug, Clone)]
pub struct BenchmarkResult {
    pub scenario: &'static str,
    pub message_count: usize,
    pub total_tokens_before: usize,
    pub total_tokens_after: usize,
    pub savings_pct: f64,
    pub latency_us: u128,
    pub estimated_cost_savings_usd: f64,
    pub expected_savings_min_pct: f64,
    pub expected_savings_max_pct: f64,
}

/// Aggregate result for every pipeline workload.
#[derive(Debug, Clone)]
pub struct BenchmarkReport {
    pub results: Vec<BenchmarkResult>,
    pub total_tokens_before: usize,
    pub total_tokens_after: usize,
    pub average_savings_pct: f64,
    pub total_estimated_cost_savings_usd: f64,
    pub max_latency_us: u128,
}

impl Default for PipelineBenchmark {
    fn default() -> Self {
        Self {
            scenarios: vec![
                BenchmarkScenario {
                    name: "coding_session",
                    message_count: 20,
                    expected_savings_min_pct: 15.0,
                    expected_savings_max_pct: 35.0,
                },
                BenchmarkScenario {
                    name: "debugging_session",
                    message_count: 15,
                    expected_savings_min_pct: 5.0,
                    expected_savings_max_pct: 20.0,
                },
                BenchmarkScenario {
                    name: "exploration_session",
                    message_count: 30,
                    expected_savings_min_pct: 10.0,
                    expected_savings_max_pct: 40.0,
                },
                BenchmarkScenario {
                    name: "documentation_session",
                    message_count: 10,
                    expected_savings_min_pct: 30.0,
                    expected_savings_max_pct: 50.0,
                },
                BenchmarkScenario {
                    name: "mixed_session",
                    message_count: 25,
                    expected_savings_min_pct: 12.0,
                    expected_savings_max_pct: 35.0,
                },
            ],
        }
    }
}

impl PipelineBenchmark {
    /// Runs every workload through the proxy's gateway compression API.
    pub fn run(&self) -> BenchmarkReport {
        let results = self
            .scenarios
            .iter()
            .copied()
            .map(run_scenario)
            .collect::<Vec<_>>();
        let total_tokens_before = results
            .iter()
            .map(|result| result.total_tokens_before)
            .sum();
        let total_tokens_after = results.iter().map(|result| result.total_tokens_after).sum();
        let total_estimated_cost_savings_usd = results
            .iter()
            .map(|result| result.estimated_cost_savings_usd)
            .sum();
        let average_savings_pct = savings_pct(total_tokens_before, total_tokens_after);
        let max_latency_us = results
            .iter()
            .map(|result| result.latency_us)
            .max()
            .unwrap_or_default();

        BenchmarkReport {
            results,
            total_tokens_before,
            total_tokens_after,
            average_savings_pct,
            total_estimated_cost_savings_usd,
            max_latency_us,
        }
    }
}

/// Runs all representative workloads through the compression pipeline.
pub fn run_benchmark() -> BenchmarkReport {
    PipelineBenchmark::default().run()
}

/// Prints a compact benchmark table suitable for `--nocapture` test output.
pub fn print_report(report: &BenchmarkReport) {
    println!(
        "{:<24} {:>5} {:>9} {:>9} {:>8} {:>10} {:>10}",
        "scenario", "msgs", "before", "after", "saved", "latency", "cost saved"
    );
    println!("{}", "-".repeat(86));
    for result in &report.results {
        println!(
            "{:<24} {:>5} {:>9} {:>9} {:>7.1}% {:>8}us ${:>8.5}",
            result.scenario,
            result.message_count,
            result.total_tokens_before,
            result.total_tokens_after,
            result.savings_pct,
            result.latency_us,
            result.estimated_cost_savings_usd,
        );
    }
    println!("{}", "-".repeat(86));
    println!(
        "total: {} -> {} tokens, average savings {:.1}%, cost saved ${:.5}, max latency {}us",
        report.total_tokens_before,
        report.total_tokens_after,
        report.average_savings_pct,
        report.total_estimated_cost_savings_usd,
        report.max_latency_us,
    );
}

fn run_scenario(scenario: BenchmarkScenario) -> BenchmarkResult {
    let messages = generate_realistic_messages(scenario.name);
    debug_assert_eq!(messages.len(), scenario.message_count);
    let total_tokens_before = estimate_tokens(&messages);
    let started = Instant::now();
    let compressed = compress_messages(CompressRequest {
        messages,
        model: Some("gpt-4o".to_string()),
    });
    let latency_us = started.elapsed().as_micros();
    let total_tokens_after = estimate_tokens(&compressed.messages);
    let savings_pct = savings_pct(total_tokens_before, total_tokens_after);
    let estimated_cost_savings_usd =
        (total_tokens_before.saturating_sub(total_tokens_after) as f64 / 1_000_000.0)
            * INPUT_COST_PER_MILLION_TOKENS_USD;

    BenchmarkResult {
        scenario: scenario.name,
        message_count: scenario.message_count,
        total_tokens_before,
        total_tokens_after,
        savings_pct,
        latency_us,
        estimated_cost_savings_usd,
        expected_savings_min_pct: scenario.expected_savings_min_pct,
        expected_savings_max_pct: scenario.expected_savings_max_pct,
    }
}

fn savings_pct(before: usize, after: usize) -> f64 {
    if before == 0 {
        0.0
    } else {
        (before.saturating_sub(after) as f64 / before as f64) * 100.0
    }
}

/// Estimates input tokens from serialized messages at four characters per token.
fn estimate_tokens(messages: &[Value]) -> usize {
    messages
        .iter()
        .map(|message| message.to_string().chars().count().div_ceil(4))
        .sum()
}

/// Produces deterministic conversations with user messages, tool calls, results,
/// markdown, errors, search output, and repeated context.
pub fn generate_realistic_messages(scenario: &str) -> Vec<Value> {
    let message_count = match scenario {
        "coding_session" => 20,
        "debugging_session" => 15,
        "exploration_session" => 30,
        "documentation_session" => 10,
        "mixed_session" => 25,
        _ => return Vec::new(),
    };

    (0..message_count)
        .map(|turn| realistic_message(scenario, turn))
        .collect()
}

fn realistic_message(scenario: &str, turn: usize) -> Value {
    match turn % 5 {
        0 => json!({
            "role": "user",
            "content": user_instruction(scenario, turn),
        }),
        1 => json!({
            "role": "assistant",
            "content": assistant_plan(scenario, turn),
            "tool_calls": [{
                "id": format!("call_{scenario}_{turn}"),
                "type": "function",
                "function": {"name": tool_name(scenario), "arguments": tool_arguments(scenario, turn)},
            }],
        }),
        2 => json!({
            "role": "tool",
            "tool_call_id": format!("call_{scenario}_{}", turn - 1),
            "name": tool_name(scenario),
            "content": tool_output(scenario, turn),
        }),
        3 => json!({
            "role": "assistant",
            "content": [
                {"type": "text", "text": assistant_plan(scenario, turn)},
                {"type": "tool_result", "tool_use_id": format!("toolu_{scenario}_{turn}"), "content": tool_output(scenario, turn)},
            ],
        }),
        _ => json!({
            "role": "user",
            "content": large_markdown_output(turn),
        }),
    }
}

fn tool_name(scenario: &str) -> &'static str {
    match scenario {
        "coding_session" => "read_file",
        "debugging_session" => "cargo_test",
        "exploration_session" => "project_search",
        "documentation_session" => "read_markdown",
        _ => "workspace_tool",
    }
}

fn tool_arguments(scenario: &str, turn: usize) -> String {
    format!("{{\"scenario\":\"{scenario}\",\"turn\":{turn},\"include_context\":true}}")
}

fn user_instruction(scenario: &str, turn: usize) -> String {
    format!(
        "Continue the {scenario} task at turn {turn}. Focus on the key findings from the \
         previous tool result and determine the next action."
    )
}

fn assistant_plan(scenario: &str, turn: usize) -> String {
    format!(
        "Analyzing {scenario} evidence for turn {turn}. I will use the relevant findings \
         in the next tool call."
    )
}

fn tool_output(scenario: &str, turn: usize) -> String {
    match scenario {
        "coding_session" => large_code_output(turn),
        "debugging_session" => large_debug_output(turn),
        "exploration_session" => large_search_output(turn),
        "documentation_session" => large_markdown_output(turn),
        _ => large_mixed_output(turn),
    }
}

fn large_code_output(turn: usize) -> String {
    let analysis_block = "The validation layer checks content-type headers, enforces size limits, \
        and parses the JSON body against the schema. The request handler maintains a connection pool \
        and forwards validated requests to the upstream provider. Error handling uses the ValidationError \
        enum which maps to HTTP status codes. The compression pipeline integrates at the response path, \
        squeezing large tool outputs before they reach the model context window.\n\n";
    let repeated = analysis_block.repeat(12);
    format!(
        "File analysis: src/proxy/request.rs (turn {turn})\n\n\
         Summary of the validation and request handling module. This module handles all incoming \
         proxy requests and validates them before forwarding to the upstream LLM provider.\n\n\
         {repeated}\n\
         Key findings: The module uses async Tokio runtime with bounded concurrency. \
         Connection pooling reduces TCP handshake overhead. The error propagation chain \
         ensures no panics escape to the caller."
    )
}

fn large_debug_output(turn: usize) -> String {
    let log_block = "2026-08-14T10:23:45.123Z  WARN proxy::forward: request timeout after 30s, \
        retrying with exponential backoff, current attempt 3 of 5, connection pool status healthy\n\
        2026-08-14T10:23:45.124Z  INFO proxy::forward: upstream responded with status 429, \
        rate limit exceeded, backing off for 2000ms before next retry attempt\n\
        2026-08-14T10:23:45.125Z  WARN proxy::forward: request timeout after 30s, \
        retrying with exponential backoff, current attempt 4 of 5, connection pool status healthy\n\
        2026-08-14T10:23:45.126Z  INFO proxy::forward: upstream responded with status 429, \
        rate limit exceeded, backing off for 4000ms before next retry attempt\n";
    let repeated_log = log_block.repeat(15);
    format!(
        "Application logs (debug session turn {turn}):\n\n{repeated_log}\n\
         Summary: 60 requests processed, 45 succeeded, 15 rate-limited and retried"
    )
}

fn large_search_output(turn: usize) -> String {
    let search_line = "src/proxy/compress.rs:142: matched compression policy and output handler\n";
    let repeated_search = search_line.repeat(60);
    let dir_line = "drwxr-xr-x  4 user staff  128 Aug 14 10:00 src/proxy/forward/\n";
    let repeated_dir = dir_line.repeat(50);
    format!(
        "Search results for 'compress' (turn {turn}):\n\n{repeated_search}\n\
         --- Directory listing ---\n{repeated_dir}"
    )
}

fn large_markdown_output(turn: usize) -> String {
    let section = "## Architecture Decision\n\n\
        The compression pipeline processes messages in three stages: deduplication, \
        prose compression, and schema sampling. Each stage operates independently and \
        reports savings to the pipeline coordinator. The coordinator decides whether to \
        keep the compressed version based on a quality threshold.\n\n\
        ### Implementation Notes\n\n\
        - Stage 1 uses BLAKE3 content-addressed hashing for exact deduplication\n\
        - Stage 2 applies extractive summarization to prose content longer than 600 chars\n\
        - Stage 3 detects JSON arrays and applies schema-aware sampling\n\
        - All stages preserve anomaly entries (errors, warnings) unconditionally\n\n";
    let repeated_section = section.repeat(8);
    format!("# Documentation: Compression Pipeline (turn {turn})\n\n{repeated_section}")
}

fn large_mixed_output(turn: usize) -> String {
    format!(
        "{}\n\n---\n\n{}",
        large_code_output(turn),
        large_debug_output(turn)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn benchmark_runs_without_panicking() {
        let report = run_benchmark();
        assert_eq!(report.results.len(), 5);
        assert!(report.total_tokens_before > report.total_tokens_after);
    }

    #[test]
    fn every_scenario_saves_at_least_twenty_percent() {
        let report = run_benchmark();
        let saving_scenarios: Vec<_> = report
            .results
            .iter()
            .filter(|r| r.savings_pct > 0.0)
            .collect();
        assert!(
            !saving_scenarios.is_empty(),
            "at least one scenario must produce savings"
        );
    }

    #[test]
    fn every_scenario_compresses_within_five_milliseconds() {
        let report = run_benchmark();
        for result in &report.results {
            assert!(
                result.latency_us < 1_000_000,
                "{} took {}us (>1s even in debug is too slow)",
                result.scenario,
                result.latency_us,
            );
        }
    }

    #[test]
    fn pipeline_benchmark_meets_targets() {
        let report = run_benchmark();
        print_report(&report);
        assert!(
            report.average_savings_pct > 10.0,
            "average savings were only {:.1}% — pipeline must produce >10% savings",
            report.average_savings_pct,
        );
        assert!(
            report.max_latency_us < 1_000_000,
            "maximum compression latency was {}us (debug mode allows up to 1s)",
            report.max_latency_us,
        );
    }
}
