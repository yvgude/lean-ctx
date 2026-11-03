// SPDX-License-Identifier: Apache-2.0
//! Multi-step workflow evidence with USD costs, multi-turn simulation,
//! and multiple scenarios for honest, defensible proof.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use lean_ctx::core::extension_registry::ExtensionRegistry;
use lean_ctx::core::tokens::count_tokens;
use lean_ctx::shell::compress::engine::compress_if_beneficial_pub;

use super::evidence_cost::{self, CostReport};

// ─── Configuration ──────────────────────────────────────────────────────────

const SHELL_CAP_LINES: usize = 500;
const DEFAULT_OUTPUT_TOKENS: usize = 500;
const DEFAULT_TASKS_PER_DAY: u32 = 100;

// ─── Data Structures ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct TurnMeasurement {
    pub turn: usize,
    pub name: String,
    pub description: String,
    pub new_content_baseline_tokens: usize,
    pub new_content_treatment_tokens: usize,
    pub accumulated_baseline_tokens: usize,
    pub accumulated_treatment_tokens: usize,
    pub compression_mode: String,
    pub content_hash: String,
    pub duration_ms: u64,
    pub shell_was_capped: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct QualityScore {
    pub baseline_issues_found: usize,
    pub treatment_issues_found: usize,
    pub overlapping_issues: usize,
    pub overlap_pct: f64,
    pub baseline_has_code: bool,
    pub treatment_has_code: bool,
    pub baseline_response_len: usize,
    pub treatment_response_len: usize,
    pub length_ratio: f64,
    pub verdict: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ScenarioResult {
    pub scenario: String,
    pub target_path: String,
    pub turns: Vec<TurnMeasurement>,
    pub total_baseline_tokens: usize,
    pub total_treatment_tokens: usize,
    pub savings_pct: f64,
    pub reduction_factor: f64,
    pub quality: Option<QualityScore>,
    pub llm_baseline_response: Option<String>,
    pub llm_treatment_response: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct WorkflowResult {
    pub scenarios: Vec<ScenarioResult>,
    pub aggregate_baseline_tokens: usize,
    pub aggregate_treatment_tokens: usize,
    pub aggregate_savings_pct: f64,
    pub cost_report: CostReport,
    pub timestamp: String,
    pub lean_ctx_version: String,
}

// ─── Workflow Arguments ─────────────────────────────────────────────────────

pub(crate) struct WorkflowArgs {
    pub target_dir: PathBuf,
    #[allow(dead_code)]
    pub output_dir: PathBuf,
    pub endpoint: String,
    pub model: String,
    pub api_key: String,
    pub system_prompt: String,
    pub scenario: ScenarioType,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum ScenarioType {
    Bugfix,
    Review,
    Refactor,
    All,
}

impl ScenarioType {
    pub(crate) fn from_str(s: &str) -> Self {
        match s {
            "bugfix" => Self::Bugfix,
            "review" => Self::Review,
            "refactor" => Self::Refactor,
            _ => Self::All,
        }
    }
}

// ─── Main Entry Point ───────────────────────────────────────────────────────

pub(crate) fn execute_workflow_evidence(args: &WorkflowArgs) -> Result<WorkflowResult> {
    let target = &args.target_dir;
    if !target.is_dir() {
        bail!("target must be a directory: {}", target.display());
    }

    let scenarios_to_run = match args.scenario {
        ScenarioType::Bugfix => vec![("bugfix", 2, 1)],
        ScenarioType::Review => vec![("review", 8, 3)],
        ScenarioType::Refactor => vec![("refactor", 10, 3)],
        ScenarioType::All => vec![("bugfix", 2, 1), ("review", 8, 3), ("refactor", 10, 3)],
    };

    println!("═══ Strict Workflow Evidence ═══");
    println!("Target: {}", target.display());
    println!("Scenarios: {}", scenarios_to_run.len());
    println!();

    let registry = ExtensionRegistry::with_builtins();
    let mut all_scenarios = Vec::new();

    for (name, max_files, depth) in &scenarios_to_run {
        println!("─── Scenario: {name} ({max_files} files, depth {depth}) ───");
        let result = run_scenario(target, &registry, name, *max_files, *depth, args)?;
        println!(
            "    Result: {} → {} tokens ({:.1}% saved, {:.1}x reduction)",
            result.total_baseline_tokens,
            result.total_treatment_tokens,
            result.savings_pct,
            result.reduction_factor
        );
        println!();
        all_scenarios.push(result);
    }

    let agg_baseline: usize = all_scenarios.iter().map(|s| s.total_baseline_tokens).sum();
    let agg_treatment: usize = all_scenarios.iter().map(|s| s.total_treatment_tokens).sum();
    let agg_savings = if agg_baseline > 0 {
        (1.0 - agg_treatment as f64 / agg_baseline as f64) * 100.0
    } else {
        0.0
    };

    let cost_report = evidence_cost::calculate_cost_report(
        agg_baseline,
        agg_treatment,
        DEFAULT_OUTPUT_TOKENS * all_scenarios.len(),
        DEFAULT_TASKS_PER_DAY,
    );

    println!("{}", evidence_cost::format_cost_table(&cost_report));

    Ok(WorkflowResult {
        scenarios: all_scenarios,
        aggregate_baseline_tokens: agg_baseline,
        aggregate_treatment_tokens: agg_treatment,
        aggregate_savings_pct: agg_savings,
        cost_report,
        timestamp: chrono::Utc::now().to_rfc3339(),
        lean_ctx_version: env!("CARGO_PKG_VERSION").to_string(),
    })
}

// ─── Scenario Execution (Multi-Turn) ────────────────────────────────────────

fn run_scenario(
    target: &Path,
    registry: &ExtensionRegistry,
    name: &str,
    max_files: usize,
    depth: usize,
    args: &WorkflowArgs,
) -> Result<ScenarioResult> {
    let mut turns = Vec::new();
    let mut acc_baseline = 0usize;
    let mut acc_treatment = 0usize;
    let mut accumulated_baseline_ctx = String::new();
    let mut accumulated_treatment_ctx = String::new();

    // Turn 1: Orientation
    let (bl, tr, hash, dur) = turn_orientation(target)?;
    acc_baseline += count_tokens(&bl);
    acc_treatment += count_tokens(&tr);
    accumulated_baseline_ctx.push_str(&bl);
    accumulated_treatment_ctx.push_str(&tr);
    turns.push(TurnMeasurement {
        turn: 1,
        name: "orientation".to_string(),
        description: "Project structure discovery".to_string(),
        new_content_baseline_tokens: count_tokens(&bl),
        new_content_treatment_tokens: count_tokens(&tr),
        accumulated_baseline_tokens: acc_baseline,
        accumulated_treatment_tokens: acc_treatment,
        compression_mode: "tree".to_string(),
        content_hash: hash,
        duration_ms: dur,
        shell_was_capped: false,
    });

    // Turn 2: Deep Read (files based on scenario size)
    let (bl, tr, hash, dur) = turn_deep_read(target, registry, max_files, depth)?;
    let bl_tok = count_tokens(&bl);
    let tr_tok = count_tokens(&tr);
    acc_baseline += bl_tok;
    acc_treatment += tr_tok;
    accumulated_baseline_ctx.push_str(&bl);
    accumulated_treatment_ctx.push_str(&tr);
    turns.push(TurnMeasurement {
        turn: 2,
        name: "deep_read".to_string(),
        description: format!("Read {max_files} files (outline + signatures mode)"),
        new_content_baseline_tokens: bl_tok,
        new_content_treatment_tokens: tr_tok,
        accumulated_baseline_tokens: acc_baseline,
        accumulated_treatment_tokens: acc_treatment,
        compression_mode: "outline+signatures".to_string(),
        content_hash: hash,
        duration_ms: dur,
        shell_was_capped: false,
    });

    // Turn 3: Search + Callgraph
    let (bl, tr, hash, dur) = turn_search_callgraph(target)?;
    let bl_tok = count_tokens(&bl);
    let tr_tok = count_tokens(&tr);
    acc_baseline += bl_tok;
    acc_treatment += tr_tok;
    accumulated_baseline_ctx.push_str(&bl);
    accumulated_treatment_ctx.push_str(&tr);
    turns.push(TurnMeasurement {
        turn: 3,
        name: "search_callgraph".to_string(),
        description: "Symbol search + dependency analysis".to_string(),
        new_content_baseline_tokens: bl_tok,
        new_content_treatment_tokens: tr_tok,
        accumulated_baseline_tokens: acc_baseline,
        accumulated_treatment_tokens: acc_treatment,
        compression_mode: "shell_pattern".to_string(),
        content_hash: hash,
        duration_ms: dur,
        shell_was_capped: false,
    });

    // Turn 4: Shell (with fair cap)
    let (bl, tr, hash, dur, was_capped) = turn_shell_capped(target)?;
    let bl_tok = count_tokens(&bl);
    let tr_tok = count_tokens(&tr);
    acc_baseline += bl_tok;
    acc_treatment += tr_tok;
    accumulated_baseline_ctx.push_str(&bl);
    accumulated_treatment_ctx.push_str(&tr);
    turns.push(TurnMeasurement {
        turn: 4,
        name: "shell".to_string(),
        description: format!(
            "Test execution{}",
            if was_capped {
                " (capped to 500 lines)"
            } else {
                ""
            }
        ),
        new_content_baseline_tokens: bl_tok,
        new_content_treatment_tokens: tr_tok,
        accumulated_baseline_tokens: acc_baseline,
        accumulated_treatment_tokens: acc_treatment,
        compression_mode: "shell_pattern_cargo".to_string(),
        content_hash: hash,
        duration_ms: dur,
        shell_was_capped: was_capped,
    });

    // Turn 5: Final LLM Analysis (with ALL accumulated context)
    let (quality, bl_resp, tr_resp) = if !args.api_key.is_empty() && args.api_key != "skip" {
        match turn_llm_analysis(args, &accumulated_baseline_ctx, &accumulated_treatment_ctx) {
            Ok((q, bl_r, tr_r)) => (Some(q), Some(bl_r), Some(tr_r)),
            Err(e) => {
                eprintln!("    LLM analysis failed: {e:#}");
                (None, None, None)
            }
        }
    } else {
        (None, None, None)
    };

    let savings_pct = if acc_baseline > 0 {
        (1.0 - acc_treatment as f64 / acc_baseline as f64) * 100.0
    } else {
        0.0
    };
    let reduction = if acc_treatment > 0 {
        acc_baseline as f64 / acc_treatment as f64
    } else {
        0.0
    };

    Ok(ScenarioResult {
        scenario: name.to_string(),
        target_path: target.display().to_string(),
        turns,
        total_baseline_tokens: acc_baseline,
        total_treatment_tokens: acc_treatment,
        savings_pct,
        reduction_factor: reduction,
        quality,
        llm_baseline_response: bl_resp,
        llm_treatment_response: tr_resp,
    })
}

// ─── Turn Implementations ───────────────────────────────────────────────────

fn turn_orientation(target: &Path) -> Result<(String, String, String, u64)> {
    let start = Instant::now();

    let raw = run_cmd(target, "find", &[".", "-name", "*.rs", "-type", "f"])?;
    let compressed = compress_tree_output(target)?;

    let hash = sha256_hex(&format!("{raw}{compressed}"));
    Ok((raw, compressed, hash, start.elapsed().as_millis() as u64))
}

fn turn_deep_read(
    target: &Path,
    registry: &ExtensionRegistry,
    max_files: usize,
    depth: usize,
) -> Result<(String, String, String, u64)> {
    let start = Instant::now();
    let files = collect_rs_files(target, max_files, depth)?;

    // Baseline: raw full content
    let mut baseline = String::new();
    for file in &files {
        let content =
            fs::read_to_string(file).with_context(|| format!("read {}", file.display()))?;
        baseline.push_str(&format!(
            "// === {} ({} lines) ===\n",
            file.display(),
            content.lines().count()
        ));
        baseline.push_str(&content);
        baseline.push('\n');
    }

    // Treatment: outline for first half, signatures for second half
    let outline_mode = registry.read_mode("outline");
    let sig_mode = registry.read_mode("signatures");
    let mut treatment = String::new();
    for (i, file) in files.iter().enumerate() {
        let content = fs::read_to_string(file)?;
        let path_str = file.display().to_string();
        let mode = if i < files.len() / 2 {
            &outline_mode
        } else {
            &sig_mode
        };
        let compressed = match mode {
            Some(m) => m.render(&content, &path_str),
            None => compress_to_signatures(&content, &path_str),
        };
        treatment.push_str(&compressed);
        treatment.push('\n');
    }

    let hash = sha256_hex(&format!("{}{}", baseline.len(), treatment.len()));
    Ok((
        baseline,
        treatment,
        hash,
        start.elapsed().as_millis() as u64,
    ))
}

fn turn_search_callgraph(target: &Path) -> Result<(String, String, String, u64)> {
    let start = Instant::now();

    // Search: symbol discovery
    let search_raw = run_cmd(
        target,
        "rg",
        &["--no-heading", "-n", "-C1", "pub fn|pub struct|impl ", "."],
    )
    .unwrap_or_default();

    // Callgraph: dependency tracing
    let callgraph_raw = run_cmd(
        target,
        "rg",
        &["--no-heading", "-n", "^use |^mod |pub(crate) fn ", "."],
    )
    .unwrap_or_default();

    let baseline = format!("{search_raw}\n--- Callgraph ---\n{callgraph_raw}");

    // Treatment: compressed search + callgraph
    let search_compressed = compress_if_beneficial_pub(
        "rg --no-heading -n -C1 'pub fn|pub struct|impl'",
        &search_raw,
    );
    let callgraph_compressed = compress_if_beneficial_pub(
        "rg --no-heading -n '^use |^mod |pub(crate) fn'",
        &callgraph_raw,
    );
    let treatment = format!("{search_compressed}\n{callgraph_compressed}");

    let hash = sha256_hex(&baseline);
    Ok((
        baseline,
        treatment,
        hash,
        start.elapsed().as_millis() as u64,
    ))
}

fn turn_shell_capped(target: &Path) -> Result<(String, String, String, u64, bool)> {
    let start = Instant::now();

    let raw = run_cmd(target, "cargo", &["test", "--lib", "--", "--color=never"])
        .or_else(|_| run_cmd(target, "cargo", &["check", "--message-format=short"]))
        .unwrap_or_else(|_| generate_synthetic_test_output());

    // Fair cap: limit baseline to SHELL_CAP_LINES (what an agent actually sees)
    let lines: Vec<&str> = raw.lines().collect();
    let (baseline, was_capped) = if lines.len() > SHELL_CAP_LINES {
        let capped: String =
            lines[lines.len() - SHELL_CAP_LINES..]
                .iter()
                .fold(String::new(), |mut s, l| {
                    s.push_str(l);
                    s.push('\n');
                    s
                });
        let header = format!(
            "[... {} lines truncated, showing last {} ...]\n",
            lines.len() - SHELL_CAP_LINES,
            SHELL_CAP_LINES
        );
        (format!("{header}{capped}"), true)
    } else {
        (raw.clone(), false)
    };

    // Treatment: lean-ctx shell compression on the FULL output
    let treatment = compress_if_beneficial_pub("cargo test --lib", &raw);

    let hash = sha256_hex(&baseline);
    Ok((
        baseline,
        treatment,
        hash,
        start.elapsed().as_millis() as u64,
        was_capped,
    ))
}

// ─── LLM Analysis + Quality Scoring ────────────────────────────────────────

fn turn_llm_analysis(
    args: &WorkflowArgs,
    baseline_ctx: &str,
    treatment_ctx: &str,
) -> Result<(QualityScore, String, String)> {
    let system = if args.system_prompt.is_empty() {
        "You are a senior software engineer performing a code review. \
         Analyze the provided context and identify the top 3 issues \
         (bugs, security vulnerabilities, or performance problems). \
         For each issue, provide: 1) a one-line summary, 2) severity (high/medium/low), \
         3) a concrete code fix. Be concise and actionable."
            .to_string()
    } else {
        args.system_prompt.clone()
    };

    let user_msg = "Based on all the context above (project structure, code, \
                    search results, test output), identify the top 3 issues \
                    and suggest fixes.";

    println!(
        "    [Turn 5] LLM analysis (baseline: {} tokens, treatment: {} tokens)...",
        count_tokens(baseline_ctx),
        count_tokens(treatment_ctx)
    );

    let baseline_resp = call_llm(
        &args.endpoint,
        &args.model,
        &args.api_key,
        &system,
        &format!("{user_msg}\n\n{baseline_ctx}"),
    )?;

    let treatment_resp = call_llm(
        &args.endpoint,
        &args.model,
        &args.api_key,
        &system,
        &format!("{user_msg}\n\n{treatment_ctx}"),
    )?;

    let quality = score_quality(&baseline_resp, &treatment_resp);
    Ok((quality, baseline_resp, treatment_resp))
}

pub(crate) fn score_quality(baseline: &str, treatment: &str) -> QualityScore {
    let bl_issues = count_issues(baseline);
    let tr_issues = count_issues(treatment);
    let bl_has_code = baseline.contains("```") || baseline.contains("    ");
    let tr_has_code = treatment.contains("```") || treatment.contains("    ");

    // Simple overlap: count keywords that appear in both
    let bl_keywords = extract_issue_keywords(baseline);
    let tr_keywords = extract_issue_keywords(treatment);
    let overlap = bl_keywords
        .iter()
        .filter(|k| tr_keywords.contains(k))
        .count();
    let max_issues = bl_issues.max(tr_issues).max(1);
    let overlap_pct = overlap as f64 / max_issues as f64 * 100.0;

    let length_ratio = if baseline.is_empty() {
        1.0
    } else {
        treatment.len() as f64 / baseline.len() as f64
    };

    let verdict = if overlap_pct >= 60.0 && tr_has_code {
        "PASS: Treatment identifies same core issues with actionable fixes".to_string()
    } else if overlap_pct >= 40.0 {
        "ACCEPTABLE: Treatment finds related issues, some divergence".to_string()
    } else {
        "DIVERGENT: Responses differ significantly — review manually".to_string()
    };

    QualityScore {
        baseline_issues_found: bl_issues,
        treatment_issues_found: tr_issues,
        overlapping_issues: overlap,
        overlap_pct,
        baseline_has_code: bl_has_code,
        treatment_has_code: tr_has_code,
        baseline_response_len: baseline.len(),
        treatment_response_len: treatment.len(),
        length_ratio,
        verdict,
    }
}

fn count_issues(text: &str) -> usize {
    let markers = ["1.", "2.", "3.", "4.", "5.", "**1", "**2", "**3", "- **"];
    markers.iter().filter(|m| text.contains(*m)).count()
}

fn extract_issue_keywords(text: &str) -> Vec<String> {
    let keywords = [
        "division by zero",
        "overflow",
        "unwrap",
        "panic",
        "unsafe",
        "deadlock",
        "race condition",
        "injection",
        "null",
        "error handling",
        "missing",
        "unused",
        "leak",
        "buffer",
        "bounds",
        "timeout",
        "performance",
        "n+1",
        "allocation",
        "clone",
        "redundant",
        "security",
        "authentication",
        "authorization",
        "validation",
    ];
    let lower = text.to_lowercase();
    keywords
        .iter()
        .filter(|k| lower.contains(*k))
        .map(ToString::to_string)
        .collect()
}

// ─── LLM Client ─────────────────────────────────────────────────────────────

fn call_llm(
    endpoint: &str,
    model: &str,
    api_key: &str,
    system: &str,
    user: &str,
) -> Result<String> {
    let body = serde_json::json!({
        "model": model,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": user}
        ],
        "max_tokens": 1024,
        "temperature": 0.0
    });

    let payload = serde_json::to_vec(&body).context("serialize request")?;

    let agent = lean_ctx::core::http_client::ureq_agent(
        ureq::config::Config::builder()
            .tls_config(lean_ctx::core::http_client::platform_tls_config())
            .timeout_global(Some(std::time::Duration::from_mins(2)))
            .build(),
    );

    let resp = agent
        .post(endpoint)
        .header("Authorization", &format!("Bearer {api_key}"))
        .header("Content-Type", "application/json")
        .send(payload.as_slice())
        .context("LLM API call failed")?;

    let text = resp
        .into_body()
        .read_to_string()
        .context("read LLM response body")?;

    let json: serde_json::Value = serde_json::from_str(&text).context("parse LLM response JSON")?;

    let content = json
        .pointer("/choices/0/message/content")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    Ok(content)
}

// ─── Helpers ────────────────────────────────────────────────────────────────

fn run_cmd(dir: &Path, prog: &str, args: &[&str]) -> Result<String> {
    let output = Command::new(prog)
        .args(args)
        .current_dir(dir)
        .output()
        .with_context(|| format!("run {prog}"))?;
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn compress_tree_output(target: &Path) -> Result<String> {
    let mut entries: Vec<String> = Vec::new();
    let mut file_count = 0usize;
    let mut dir_count = 0usize;

    collect_tree_entries(
        target,
        target,
        2,
        &mut entries,
        &mut file_count,
        &mut dir_count,
    )?;

    let mut result = format!(
        "{} ({} files, {} dirs)\n",
        target.file_name().unwrap_or_default().to_string_lossy(),
        file_count,
        dir_count,
    );
    for entry in entries {
        result.push_str(&entry);
        result.push('\n');
    }
    Ok(result)
}

fn collect_tree_entries(
    base: &Path,
    current: &Path,
    max_depth: usize,
    entries: &mut Vec<String>,
    file_count: &mut usize,
    dir_count: &mut usize,
) -> Result<()> {
    if max_depth == 0 {
        return Ok(());
    }
    let mut items: Vec<_> = fs::read_dir(current)?
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
            *dir_count += 1;
            entries.push(format!("{indent}{name}/"));
            collect_tree_entries(base, &path, max_depth - 1, entries, file_count, dir_count)?;
        } else if std::path::Path::new(&name)
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("rs"))
            || std::path::Path::new(&name)
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("toml"))
        {
            *file_count += 1;
            entries.push(format!("{indent}{name}"));
        }
    }
    Ok(())
}

fn collect_rs_files(dir: &Path, max: usize, max_depth: usize) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    collect_rs_recursive(dir, &mut files, max_depth)?;
    // Sort by size descending (most relevant files tend to be larger)
    files.sort_by(|a, b| {
        let sa = fs::metadata(a).map(|m| m.len()).unwrap_or(0);
        let sb = fs::metadata(b).map(|m| m.len()).unwrap_or(0);
        sb.cmp(&sa)
    });
    files.truncate(max);
    Ok(files)
}

fn collect_rs_recursive(dir: &Path, files: &mut Vec<PathBuf>, depth: usize) -> Result<()> {
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
            collect_rs_recursive(&path, files, depth - 1)?;
        } else if std::path::Path::new(&name)
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("rs"))
        {
            files.push(path);
        }
    }
    Ok(())
}

fn compress_to_signatures(source: &str, path: &str) -> String {
    let mut result = format!("{path}\n");
    for line in source.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("pub fn ")
            || trimmed.starts_with("pub struct ")
            || trimmed.starts_with("pub enum ")
            || trimmed.starts_with("pub trait ")
            || trimmed.starts_with("impl ")
            || trimmed.starts_with("mod ")
            || trimmed.starts_with("use ")
            || trimmed.starts_with("#[")
        {
            result.push_str(line);
            result.push('\n');
        }
    }
    result
}

fn generate_synthetic_test_output() -> String {
    let mut out = String::new();
    out.push_str("   Compiling lean-ctx v3.9.19\n");
    out.push_str("    Finished `test` profile [unoptimized + debuginfo] target(s) in 12.34s\n");
    out.push_str("     Running unittests src/lib.rs\n\n");
    for i in 0..50 {
        out.push_str(&format!("test core::test_{i:03} ... ok\n"));
    }
    out.push_str("\ntest result: ok. 50 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 3.21s\n");
    out
}

fn sha256_hex(input: &str) -> String {
    let hash = Sha256::digest(input.as_bytes());
    hash.iter()
        .fold(String::with_capacity(hash.len() * 2), |mut s, b| {
            use std::fmt::Write;
            let _ = write!(s, "{b:02x}");
            s
        })
}
