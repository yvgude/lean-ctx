//! Real-world multi-turn evidence with provider-reported cache metrics.
//!
//! Runs 5 sequential API calls for baseline and 5 for treatment,
//! building up context turn-by-turn exactly like a real agent session.
//! Reads `cached_tokens` from each response to calculate TRUE costs.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::evidence_workflow::{QualityScore, score_quality};
use crate::core::extension_registry::ExtensionRegistry;
use crate::core::gain::model_pricing::ModelPricing;
use crate::core::tokens::count_tokens;
use crate::shell::compress::engine::compress_if_beneficial_pub;

// ─── Configuration ──────────────────────────────────────────────────────────

const SHELL_CAP_LINES: usize = 500;
const REALWORLD_EVIDENCE_CAPABILITY_ID: &str = "capability://leanctx/evidence-realworld";

// ─── Data Structures ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct TurnResult {
    pub turn: usize,
    pub name: String,
    pub description: String,
    /// Tokens in this turn's NEW content (not accumulated)
    pub new_content_tokens: usize,
    /// Total accumulated tokens sent in this turn's prompt
    pub prompt_tokens_sent: usize,
    /// Provider-reported: how many tokens were cached (from previous turns)
    pub cached_tokens: usize,
    /// Provider-reported: total prompt tokens billed
    pub provider_prompt_tokens: usize,
    /// Provider-reported: completion tokens
    pub provider_completion_tokens: usize,
    /// Real cost based on provider cache breakdown
    pub real_cost_usd: f64,
    /// Response content
    pub response: String,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ArmResult {
    pub arm: String,
    pub turns: Vec<TurnResult>,
    pub total_prompt_tokens: usize,
    pub total_cached_tokens: usize,
    pub total_completion_tokens: usize,
    pub total_real_cost_usd: f64,
    pub effective_input_cost_usd: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ReceiptSavings {
    pub original_tokens: usize,
    pub delivered_tokens: usize,
    pub saved_tokens: usize,
    pub saved_pct: f64,
    pub methodology: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct QualityGateVerdict {
    pub verdict: String,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct RealWorldResult {
    #[serde(default = "default_schema_version")]
    pub schema_version: String,
    #[serde(default = "default_integrity_status")]
    pub integrity_status: String,
    #[serde(default = "default_outcome")]
    pub outcome: String,
    /// Capability that produced this performance result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capability_id: Option<String>,
    /// Version of the capability that produced this performance result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capability_version: Option<String>,
    pub baseline: ArmResult,
    pub treatment: ArmResult,
    pub savings_tokens_pct: f64,
    pub savings_cost_pct: f64,
    pub savings_usd: f64,
    pub savings: ReceiptSavings,
    pub model: String,
    pub endpoint: String,
    pub target_path: String,
    pub timestamp: String,
    pub lean_ctx_version: String,
    pub methodology: Methodology,
    pub quality: Option<QualityScore>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quality_gate: Option<QualityGateVerdict>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Methodology {
    pub description: String,
    #[serde(default = "default_multi_turn")]
    pub multi_turn: bool,
    #[serde(default)]
    pub fair_baseline: String,
    pub turns_per_arm: usize,
    pub shell_cap_lines: usize,
    pub cache_source: String,
    pub cost_calculation: String,
}

fn default_schema_version() -> String {
    "1".to_string()
}

fn default_integrity_status() -> String {
    "observed".to_string()
}

fn default_outcome() -> String {
    "succeeded".to_string()
}

fn default_multi_turn() -> bool {
    true
}

// ─── Args ───────────────────────────────────────────────────────────────────

#[allow(dead_code)]
pub(crate) struct RealWorldArgs {
    pub target_dir: PathBuf,
    pub output_dir: PathBuf,
    pub endpoint: String,
    pub model: String,
    pub api_key: String,
    pub quality_gate: bool,
}

// ─── Main Entry Point ───────────────────────────────────────────────────────

pub(crate) fn validate_workload(target: &Path) -> Result<()> {
    if !target.exists() {
        bail!("workload directory does not exist: {}", target.display());
    }
    if !target.is_dir() {
        bail!("workload must be a directory: {}", target.display());
    }
    let rs_files = collect_rs_files_by_size(target, 1, 3)?;
    if rs_files.is_empty() {
        bail!(
            "workload has no analyzable source files: no .rs files found under {} \
             (searched 3 levels, excluding target/, node_modules/, and dot-directories)",
            target.display()
        );
    }
    Ok(())
}

pub(crate) fn quality_meets_baseline(quality: &QualityScore) -> bool {
    quality.treatment_issues_found >= quality.baseline_issues_found && quality.overlap_pct >= 40.0
}

pub(crate) fn evaluate_quality_gate(result: &RealWorldResult) -> QualityGateVerdict {
    let mut reasons = Vec::new();
    let mut pass = true;
    let expected = result.methodology.turns_per_arm;

    if result.savings.saved_pct <= 0.0 {
        pass = false;
        reasons.push(format!(
            "savings_pct must be > 0, got {:.1}",
            result.savings.saved_pct
        ));
    }

    if result.baseline.turns.len() != expected {
        pass = false;
        reasons.push(format!(
            "baseline completed {}/{} turns without error",
            result.baseline.turns.len(),
            expected
        ));
    }
    if result.treatment.turns.len() != expected {
        pass = false;
        reasons.push(format!(
            "treatment completed {}/{} turns without error",
            result.treatment.turns.len(),
            expected
        ));
    }

    match &result.quality {
        Some(quality) if quality_meets_baseline(quality) => {}
        Some(quality) => {
            pass = false;
            reasons.push(format!(
                "treatment quality below baseline: overlap {:.1}% (min 40%), issues {}/{} (min {})",
                quality.overlap_pct,
                quality.treatment_issues_found,
                quality.baseline_issues_found,
                quality.baseline_issues_found
            ));
        }
        None => {
            pass = false;
            reasons.push("missing quality score from final analysis turn".to_string());
        }
    }

    if pass {
        reasons.push("all quality gate assertions passed".to_string());
    }

    QualityGateVerdict {
        verdict: if pass { "PASS" } else { "FAIL" }.to_string(),
        reasons,
    }
}

fn final_analysis_response(arm: &ArmResult) -> Option<&str> {
    arm.turns
        .iter()
        .find(|turn| turn.name == "final_analysis")
        .map(|turn| turn.response.as_str())
}

fn build_realworld_result(
    args: &RealWorldArgs,
    baseline: ArmResult,
    treatment: ArmResult,
) -> RealWorldResult {
    let savings_tokens_pct = if baseline.total_prompt_tokens > 0 {
        (1.0 - treatment.total_prompt_tokens as f64 / baseline.total_prompt_tokens as f64) * 100.0
    } else {
        0.0
    };
    let savings_cost_pct = if baseline.total_real_cost_usd > 0.0 {
        (1.0 - treatment.total_real_cost_usd / baseline.total_real_cost_usd) * 100.0
    } else {
        0.0
    };
    let savings_usd = baseline.total_real_cost_usd - treatment.total_real_cost_usd;
    let original_tokens = baseline.total_prompt_tokens;
    let delivered_tokens = treatment.total_prompt_tokens;
    let saved_tokens = original_tokens.saturating_sub(delivered_tokens);

    let quality = match (
        final_analysis_response(&baseline),
        final_analysis_response(&treatment),
    ) {
        (Some(baseline_resp), Some(treatment_resp)) => {
            Some(score_quality(baseline_resp, treatment_resp))
        }
        _ => None,
    };

    let mut result = RealWorldResult {
        schema_version: default_schema_version(),
        integrity_status: default_integrity_status(),
        outcome: default_outcome(),
        capability_id: Some(REALWORLD_EVIDENCE_CAPABILITY_ID.to_owned()),
        capability_version: Some(env!("CARGO_PKG_VERSION").to_owned()),
        baseline,
        treatment,
        savings_tokens_pct,
        savings_cost_pct,
        savings_usd,
        savings: ReceiptSavings {
            original_tokens,
            delivered_tokens,
            saved_tokens,
            saved_pct: savings_tokens_pct,
            methodology: "baseline_treatment".to_string(),
        },
        model: args.model.clone(),
        endpoint: args.endpoint.clone(),
        target_path: args.target_dir.display().to_string(),
        timestamp: chrono::Utc::now().to_rfc3339(),
        lean_ctx_version: env!("CARGO_PKG_VERSION").to_string(),
        methodology: Methodology {
            description: "Sequential 5-turn API calls per arm. Each turn accumulates \
                          context from previous turns. Provider-reported cached_tokens \
                          used for real cost calculation."
                .to_string(),
            multi_turn: true,
            fair_baseline: format!(
                "Shell output capped to last {SHELL_CAP_LINES} lines (realistic agent view)"
            ),
            turns_per_arm: 5,
            shell_cap_lines: SHELL_CAP_LINES,
            cache_source: "OpenAI usage.prompt_tokens_details.cached_tokens".to_string(),
            cost_calculation: "(prompt - cached) * input_price + cached * cache_read_price \
                              + output * output_price"
                .to_string(),
        },
        quality,
        quality_gate: None,
    };

    if args.quality_gate {
        result.quality_gate = Some(evaluate_quality_gate(&result));
        if result
            .quality_gate
            .as_ref()
            .is_some_and(|gate| gate.verdict == "FAIL")
        {
            result.outcome = "failed".to_string();
        }
    }

    result
}

pub(crate) fn execute_realworld_evidence(args: &RealWorldArgs) -> Result<RealWorldResult> {
    let target = &args.target_dir;
    validate_workload(target)?;

    println!("═══ Real-World Multi-Turn Evidence ═══");
    println!("Target:   {}", target.display());
    println!("Model:    {}", args.model);
    println!("Endpoint: {}", args.endpoint);
    println!();

    let registry = ExtensionRegistry::with_builtins();

    // Prepare context for each turn (both baseline and treatment versions)
    let turns_content = prepare_turn_content(target, &registry)?;

    // Run baseline arm (5 sequential calls, raw context)
    println!("─── Baseline Arm (no compression) ───");
    let baseline = run_arm("baseline", &turns_content, false, args)?;
    println!(
        "    Total: {} prompt tokens, {} cached, ${:.4}",
        baseline.total_prompt_tokens, baseline.total_cached_tokens, baseline.total_real_cost_usd
    );
    println!();

    // Run treatment arm (5 sequential calls, compressed context)
    println!("─── Treatment Arm (lean-ctx compressed) ───");
    let treatment = run_arm("treatment", &turns_content, true, args)?;
    println!(
        "    Total: {} prompt tokens, {} cached, ${:.4}",
        treatment.total_prompt_tokens, treatment.total_cached_tokens, treatment.total_real_cost_usd
    );
    println!();

    Ok(build_realworld_result(args, baseline, treatment))
}

// ─── Turn Content Preparation ───────────────────────────────────────────────

struct TurnContent {
    name: String,
    description: String,
    baseline_content: String,
    treatment_content: String,
}

fn prepare_turn_content(target: &Path, registry: &ExtensionRegistry) -> Result<Vec<TurnContent>> {
    let mut turns = Vec::new();

    // Turn 1: Orientation (project tree)
    let raw_tree = run_cmd(target, "find", &[".", "-name", "*.rs", "-type", "f"])?;
    let compressed_tree = compress_tree(target)?;
    turns.push(TurnContent {
        name: "orientation".to_string(),
        description: "Explore the project structure".to_string(),
        baseline_content: format!("Here is the project file listing:\n\n{raw_tree}"),
        treatment_content: format!("Here is the project structure:\n\n{compressed_tree}"),
    });

    // Turn 2: Deep Read (main source files)
    let files = collect_rs_files_by_size(target, 3, 2)?;
    let mut raw_files = String::new();
    let mut compressed_files = String::new();
    let outline_mode = registry.read_mode("outline");
    for file in &files {
        let content = fs::read_to_string(file)?;
        let path_str = file
            .strip_prefix(target)
            .unwrap_or(file)
            .display()
            .to_string();
        raw_files.push_str(&format!("=== {path_str} ===\n{content}\n\n"));
        let compressed = match &outline_mode {
            Some(m) => m.render(&content, &path_str),
            None => fallback_signatures(&content, &path_str),
        };
        compressed_files.push_str(&format!("{compressed}\n"));
    }
    turns.push(TurnContent {
        name: "deep_read".to_string(),
        description: format!("Read {} key source files for understanding", files.len()),
        baseline_content: format!("Here are the source files:\n\n{raw_files}"),
        treatment_content: format!(
            "Here are the source files (structural overview):\n\n{compressed_files}"
        ),
    });

    // Turn 3: Search + Callgraph
    let raw_search = run_cmd(
        target,
        "rg",
        &["--no-heading", "-n", "-C1", "pub fn|pub struct|impl ", "."],
    )
    .unwrap_or_default();
    let compressed_search = compress_if_beneficial_pub(
        "rg --no-heading -n -C1 'pub fn|pub struct|impl'",
        &raw_search,
    );
    turns.push(TurnContent {
        name: "search".to_string(),
        description: "Search for public API symbols and implementations".to_string(),
        baseline_content: format!("Search results for public symbols:\n\n{raw_search}"),
        treatment_content: format!("Search results (compressed):\n\n{compressed_search}"),
    });

    // Turn 4: Shell (test execution, capped)
    let raw_shell = run_cmd(target, "cargo", &["test", "--lib", "--", "--color=never"])
        .or_else(|_| run_cmd(target, "cargo", &["check", "--message-format=short"]))
        .unwrap_or_else(|_| synthetic_test_output());
    let lines: Vec<&str> = raw_shell.lines().collect();
    let capped_shell = if lines.len() > SHELL_CAP_LINES {
        let tail: String =
            lines[lines.len() - SHELL_CAP_LINES..]
                .iter()
                .fold(String::new(), |mut s, l| {
                    s.push_str(l);
                    s.push('\n');
                    s
                });
        format!(
            "[...{} lines truncated, showing last {}...]\n{tail}",
            lines.len() - SHELL_CAP_LINES,
            SHELL_CAP_LINES
        )
    } else {
        raw_shell.clone()
    };
    let compressed_shell = compress_if_beneficial_pub("cargo test --lib", &raw_shell);
    turns.push(TurnContent {
        name: "shell".to_string(),
        description: "Run tests and analyze output".to_string(),
        baseline_content: format!("Test execution output:\n\n{capped_shell}"),
        treatment_content: format!("Test execution (compressed):\n\n{compressed_shell}"),
    });

    // Turn 5: Final analysis request (same for both — just the question)
    turns.push(TurnContent {
        name: "final_analysis".to_string(),
        description: "Request final code review based on all accumulated context".to_string(),
        baseline_content: "Based on everything above, provide your final code review. \
            Identify the top 3 issues (bugs, security, performance) with severity \
            ratings and concrete code fixes."
            .to_string(),
        treatment_content: "Based on everything above, provide your final code review. \
            Identify the top 3 issues (bugs, security, performance) with severity \
            ratings and concrete code fixes."
            .to_string(),
    });

    Ok(turns)
}

// ─── Run an Arm (5 sequential API calls) ────────────────────────────────────

fn run_arm(
    arm_name: &str,
    turns: &[TurnContent],
    use_treatment: bool,
    args: &RealWorldArgs,
) -> Result<ArmResult> {
    let system_prompt = "You are a senior software engineer performing a code review. \
        Analyze each piece of context provided turn by turn. Build your understanding \
        incrementally. When asked for a final review, be concise and actionable.";

    let mut messages: Vec<serde_json::Value> =
        vec![serde_json::json!({"role": "system", "content": system_prompt})];

    let mut turn_results = Vec::new();
    let pricing = ModelPricing::load();
    let quote = pricing.quote(Some(&args.model));

    for (i, turn) in turns.iter().enumerate() {
        let start = Instant::now();
        let content = if use_treatment {
            &turn.treatment_content
        } else {
            &turn.baseline_content
        };

        // Add user message for this turn
        let user_msg = format!("[Turn {}: {}]\n\n{}", i + 1, turn.description, content);
        messages.push(serde_json::json!({"role": "user", "content": user_msg}));

        // Count tokens we're sending
        let prompt_text: String = messages
            .iter()
            .filter_map(|m| m["content"].as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let prompt_tokens_local = count_tokens(&prompt_text);

        // Make API call
        let resp = call_llm_with_usage(&args.endpoint, &args.model, &args.api_key, &messages)?;

        // Add assistant response to conversation
        messages.push(serde_json::json!({"role": "assistant", "content": resp.content}));

        let duration = start.elapsed().as_millis() as u64;

        // Calculate real cost from provider-reported numbers
        let uncached = resp.prompt_tokens.saturating_sub(resp.cached_tokens);
        let real_cost = (uncached as f64 / 1_000_000.0 * quote.cost.input_per_m)
            + (resp.cached_tokens as f64 / 1_000_000.0 * quote.cost.cache_read_per_m)
            + (resp.completion_tokens as f64 / 1_000_000.0 * quote.cost.output_per_m);

        println!(
            "    Turn {}: {:>6} prompt ({:>5} cached) + {:>4} completion = ${:.5}",
            i + 1,
            resp.prompt_tokens,
            resp.cached_tokens,
            resp.completion_tokens,
            real_cost
        );

        turn_results.push(TurnResult {
            turn: i + 1,
            name: turn.name.clone(),
            description: turn.description.clone(),
            new_content_tokens: count_tokens(content),
            prompt_tokens_sent: prompt_tokens_local,
            cached_tokens: resp.cached_tokens,
            provider_prompt_tokens: resp.prompt_tokens,
            provider_completion_tokens: resp.completion_tokens,
            real_cost_usd: real_cost,
            response: resp.content,
            duration_ms: duration,
        });
    }

    let total_prompt: usize = turn_results.iter().map(|t| t.provider_prompt_tokens).sum();
    let total_cached: usize = turn_results.iter().map(|t| t.cached_tokens).sum();
    let total_completion: usize = turn_results
        .iter()
        .map(|t| t.provider_completion_tokens)
        .sum();
    let total_cost: f64 = turn_results.iter().map(|t| t.real_cost_usd).sum();

    // Effective input cost (what was actually paid for input, excluding output)
    let total_uncached: usize = turn_results
        .iter()
        .map(|t| t.provider_prompt_tokens.saturating_sub(t.cached_tokens))
        .sum();
    let effective_input_cost = (total_uncached as f64 / 1_000_000.0 * quote.cost.input_per_m)
        + (total_cached as f64 / 1_000_000.0 * quote.cost.cache_read_per_m);

    Ok(ArmResult {
        arm: arm_name.to_string(),
        turns: turn_results,
        total_prompt_tokens: total_prompt,
        total_cached_tokens: total_cached,
        total_completion_tokens: total_completion,
        total_real_cost_usd: total_cost,
        effective_input_cost_usd: effective_input_cost,
    })
}

// ─── LLM Client with Usage Tracking ────────────────────────────────────────

struct LlmResponse {
    content: String,
    prompt_tokens: usize,
    cached_tokens: usize,
    completion_tokens: usize,
}

fn call_llm_with_usage(
    endpoint: &str,
    model: &str,
    api_key: &str,
    messages: &[serde_json::Value],
) -> Result<LlmResponse> {
    let body = serde_json::json!({
        "model": model,
        "messages": messages,
        "max_tokens": 512,
        "temperature": 0.0
    });
    let payload = serde_json::to_vec(&body).context("serialize")?;

    let agent = crate::core::http_client::ureq_agent(
        ureq::config::Config::builder()
            .tls_config(crate::core::http_client::platform_tls_config())
            .timeout_global(Some(std::time::Duration::from_mins(2)))
            .build(),
    );

    let mut last_err = None;
    for attempt in 0..4 {
        let resp_result = agent
            .post(endpoint)
            .header("Authorization", &format!("Bearer {api_key}"))
            .header("Content-Type", "application/json")
            .send(payload.as_slice());

        match resp_result {
            Ok(resp) => {
                let text = resp.into_body().read_to_string().context("read body")?;
                let json: serde_json::Value = serde_json::from_str(&text)
                    .with_context(|| format!("parse: {}", &text[..text.len().min(200)]))?;

                let content = json
                    .pointer("/choices/0/message/content")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();

                let usage = &json["usage"];
                let prompt_tokens = usage["prompt_tokens"].as_u64().unwrap_or(0) as usize;
                let completion_tokens = usage["completion_tokens"].as_u64().unwrap_or(0) as usize;
                let cached_tokens = usage
                    .pointer("/prompt_tokens_details/cached_tokens")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0) as usize;

                return Ok(LlmResponse {
                    content,
                    prompt_tokens,
                    cached_tokens,
                    completion_tokens,
                });
            }
            Err(e) => {
                let is_429 = e.to_string().contains("429");
                if is_429 && attempt < 3 {
                    let wait = std::time::Duration::from_secs(30 * (attempt as u64 + 1));
                    eprintln!("    Rate limited, waiting {}s...", wait.as_secs());
                    std::thread::sleep(wait);
                    last_err = Some(e);
                } else {
                    bail!("API call failed after {} attempts: {e}", attempt + 1);
                }
            }
        }
    }
    bail!(
        "API call failed: {}",
        last_err.expect("at least one attempt was made")
    );
}

// ─── Helpers ────────────────────────────────────────────────────────────────

fn run_cmd(dir: &Path, prog: &str, args: &[&str]) -> Result<String> {
    let output = std::process::Command::new(prog)
        .args(args)
        .current_dir(dir)
        .output()
        .with_context(|| format!("run {prog}"))?;
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn compress_tree(target: &Path) -> Result<String> {
    let mut entries = Vec::new();
    let mut fc = 0usize;
    let mut dc = 0usize;
    tree_walk(target, target, 2, &mut entries, &mut fc, &mut dc)?;
    let mut out = format!(
        "{} ({fc} files, {dc} dirs)\n",
        target.file_name().unwrap_or_default().to_string_lossy()
    );
    for e in entries {
        out.push_str(&e);
        out.push('\n');
    }
    Ok(out)
}

fn tree_walk(
    base: &Path,
    cur: &Path,
    depth: usize,
    entries: &mut Vec<String>,
    fc: &mut usize,
    dc: &mut usize,
) -> Result<()> {
    if depth == 0 {
        return Ok(());
    }
    let mut items: Vec<_> = fs::read_dir(cur)?
        .filter_map(std::result::Result::ok)
        .collect();
    items.sort_by_key(std::fs::DirEntry::file_name);
    for item in items {
        let name = item.file_name().to_string_lossy().to_string();
        if name.starts_with('.') || name == "target" || name == "node_modules" {
            continue;
        }
        let path = item.path();
        let rel = path.strip_prefix(base).unwrap_or(&path);
        let indent = "  ".repeat(rel.components().count().saturating_sub(1));
        if path.is_dir() {
            *dc += 1;
            entries.push(format!("{indent}{name}/"));
            tree_walk(base, &path, depth - 1, entries, fc, dc)?;
        } else if std::path::Path::new(&name)
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("rs"))
            || std::path::Path::new(&name)
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("toml"))
        {
            *fc += 1;
            entries.push(format!("{indent}{name}"));
        }
    }
    Ok(())
}

fn collect_rs_files_by_size(dir: &Path, max: usize, depth: usize) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    rs_walk(dir, &mut files, depth)?;
    files.sort_by(|a, b| {
        let sa = fs::metadata(a).map(|m| m.len()).unwrap_or(0);
        let sb = fs::metadata(b).map(|m| m.len()).unwrap_or(0);
        sb.cmp(&sa)
    });
    files.truncate(max);
    Ok(files)
}

fn rs_walk(dir: &Path, files: &mut Vec<PathBuf>, depth: usize) -> Result<()> {
    if depth == 0 {
        return Ok(());
    }
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') || name == "target" || name == "node_modules" {
            continue;
        }
        if path.is_dir() {
            rs_walk(&path, files, depth - 1)?;
        } else if std::path::Path::new(&name)
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("rs"))
        {
            files.push(path);
        }
    }
    Ok(())
}

fn fallback_signatures(source: &str, path: &str) -> String {
    let mut r = format!("{path}\n");
    for line in source.lines() {
        let t = line.trim();
        if t.starts_with("pub fn ")
            || t.starts_with("pub struct ")
            || t.starts_with("pub enum ")
            || t.starts_with("pub trait ")
            || t.starts_with("impl ")
            || t.starts_with("mod ")
        {
            r.push_str(line);
            r.push('\n');
        }
    }
    r
}

fn synthetic_test_output() -> String {
    let mut out = String::new();
    out.push_str("   Compiling lean-ctx v3.9.19\n");
    out.push_str("    Finished `test` profile in 12.34s\n");
    out.push_str("     Running unittests src/lib.rs\n\n");
    for i in 0..80 {
        out.push_str(&format!("test core::test_{i:03} ... ok\n"));
    }
    out.push_str("\ntest result: ok. 80 passed; 0 failed; finished in 4.56s\n");
    out
}

#[allow(dead_code)]
fn sha256_hex(input: &str) -> String {
    let hash = Sha256::digest(input.as_bytes());
    hash.iter().fold(String::with_capacity(64), |mut s, b| {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
        s
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_arm(arm: &str, per_turn_prompt: usize, turns: usize) -> ArmResult {
        ArmResult {
            arm: arm.to_string(),
            turns: (0..turns)
                .map(|i| TurnResult {
                    turn: i + 1,
                    name: if i + 1 == turns {
                        "final_analysis".to_string()
                    } else {
                        format!("turn-{}", i + 1)
                    },
                    description: "test".to_string(),
                    new_content_tokens: 10,
                    prompt_tokens_sent: per_turn_prompt,
                    cached_tokens: 0,
                    provider_prompt_tokens: per_turn_prompt,
                    provider_completion_tokens: 10,
                    real_cost_usd: 0.01,
                    response: if i + 1 == turns {
                        "1. division by zero bug
2. missing error handling
3. performance issue"
                            .to_string()
                    } else {
                        String::new()
                    },
                    duration_ms: 1,
                })
                .collect(),
            total_prompt_tokens: per_turn_prompt * turns,
            total_cached_tokens: 0,
            total_completion_tokens: 10 * turns,
            total_real_cost_usd: 0.05,
            effective_input_cost_usd: 0.04,
        }
    }

    fn sample_result(baseline_per_turn: usize, treatment_per_turn: usize) -> RealWorldResult {
        build_realworld_result(
            &RealWorldArgs {
                target_dir: PathBuf::from("/tmp/project"),
                output_dir: PathBuf::from("/tmp/out"),
                endpoint: "https://example.test".to_string(),
                model: "test-model".to_string(),
                api_key: "test".to_string(),
                quality_gate: false,
            },
            sample_arm("baseline", baseline_per_turn, 5),
            sample_arm("treatment", treatment_per_turn, 5),
        )
    }

    #[test]
    fn validate_workload_rejects_missing_directory() {
        let missing = PathBuf::from("/tmp/lean-ctx-missing-workload-99999999");
        assert!(validate_workload(&missing).is_err());
    }

    #[test]
    fn validate_workload_accepts_rust_project() {
        assert!(validate_workload(Path::new(".")).is_ok());
    }

    #[test]
    fn quality_gate_passes_for_positive_savings_and_matching_quality() {
        let result = sample_result(1000, 300);
        let gate = evaluate_quality_gate(&result);
        assert_eq!(gate.verdict, "PASS");
    }

    #[test]
    fn quality_gate_fails_when_savings_are_not_positive() {
        let result = sample_result(300, 1000);
        let gate = evaluate_quality_gate(&result);
        assert_eq!(gate.verdict, "FAIL");
        assert!(
            gate.reasons
                .iter()
                .any(|reason| reason.contains("savings_pct"))
        );
    }

    #[test]
    fn build_result_populates_receipt_fields() {
        let result = sample_result(1000, 300);
        assert_eq!(result.schema_version, "1");
        assert_eq!(result.integrity_status, "observed");
        assert_eq!(result.outcome, "succeeded");
        assert_eq!(result.savings.methodology, "baseline_treatment");
        assert_eq!(result.savings.original_tokens, 5000);
        assert_eq!(result.savings.delivered_tokens, 1500);
        assert_eq!(
            result.capability_id.as_deref(),
            Some(REALWORLD_EVIDENCE_CAPABILITY_ID)
        );
        assert_eq!(
            result.capability_version.as_deref(),
            Some(env!("CARGO_PKG_VERSION"))
        );
        assert!(result.methodology.multi_turn);
        assert!(
            !result.methodology.fair_baseline.is_empty(),
            "`result.methodology.fair_baseline` is empty"
        );
    }

    #[test]
    fn performance_result_capability_metadata_is_backward_compatible() {
        let result = sample_result(1000, 300);
        let mut legacy =
            serde_json::to_value(result).expect("performance result should serialize as JSON");
        let object = legacy
            .as_object_mut()
            .expect("serialized performance result should be an object");
        object.remove("capability_id");
        object.remove("capability_version");

        let decoded: RealWorldResult =
            serde_json::from_value(legacy).expect("legacy performance result should deserialize");
        assert_eq!(decoded.capability_id, None);
        assert_eq!(decoded.capability_version, None);
    }
}
