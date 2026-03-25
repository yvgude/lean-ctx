use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use walkdir::WalkDir;

use crate::core::compressor;
use crate::core::deps;
use crate::core::entropy;
use crate::core::preservation;
use crate::core::signatures;
use crate::core::tokens::count_tokens;

const COST_PER_TOKEN: f64 = 2.50 / 1_000_000.0;
const MAX_FILE_SIZE: u64 = 100 * 1024;
const MAX_FILES: usize = 50;
const CACHE_HIT_TOKENS: usize = 13;

// ── Types ───────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ModeMeasurement {
    pub mode: String,
    pub tokens: usize,
    pub savings_pct: f64,
    pub latency_us: u64,
    pub preservation_score: f64,
}

#[derive(Debug, Clone)]
pub struct FileMeasurement {
    #[allow(dead_code)]
    pub path: String,
    pub ext: String,
    pub raw_tokens: usize,
    pub modes: Vec<ModeMeasurement>,
}

#[derive(Debug, Clone)]
pub struct LanguageStats {
    pub ext: String,
    pub count: usize,
    pub total_tokens: usize,
}

#[derive(Debug, Clone)]
pub struct ModeSummary {
    pub mode: String,
    pub total_compressed_tokens: usize,
    pub avg_savings_pct: f64,
    pub avg_latency_us: u64,
    pub avg_preservation: f64,
}

#[derive(Debug, Clone)]
pub struct SessionSimResult {
    pub raw_tokens: usize,
    pub lean_tokens: usize,
    pub lean_ccp_tokens: usize,
    pub raw_cost: f64,
    pub lean_cost: f64,
    pub ccp_cost: f64,
}

#[derive(Debug, Clone)]
pub struct ProjectBenchmark {
    pub root: String,
    pub files_scanned: usize,
    pub files_measured: usize,
    pub total_raw_tokens: usize,
    pub languages: Vec<LanguageStats>,
    pub mode_summaries: Vec<ModeSummary>,
    pub session_sim: SessionSimResult,
    #[allow(dead_code)]
    pub file_results: Vec<FileMeasurement>,
}

// ── Scanner ─────────────────────────────────────────────────

fn is_skipped_dir(name: &str) -> bool {
    matches!(
        name,
        "node_modules"
            | ".git"
            | "target"
            | "dist"
            | "build"
            | ".next"
            | ".nuxt"
            | "__pycache__"
            | ".cache"
            | "coverage"
            | "vendor"
            | ".svn"
            | ".hg"
    )
}

fn is_text_ext(ext: &str) -> bool {
    matches!(
        ext,
        "rs" | "ts" | "tsx" | "js" | "jsx" | "py" | "go" | "java"
            | "c" | "cpp" | "h" | "hpp" | "cs" | "kt" | "swift"
            | "rb" | "php" | "vue" | "svelte" | "html" | "css"
            | "scss" | "less" | "json" | "yaml" | "yml" | "toml"
            | "xml" | "md" | "txt" | "sh" | "bash" | "zsh"
            | "fish" | "sql" | "graphql" | "proto" | "ex" | "exs"
            | "zig" | "lua" | "r" | "R" | "dart" | "scala"
    )
}

fn scan_project(root: &str) -> Vec<PathBuf> {
    let mut files: Vec<(PathBuf, u64)> = Vec::new();

    for entry in WalkDir::new(root).max_depth(8).into_iter().filter_entry(|e| {
        let name = e.file_name().to_string_lossy();
        if e.file_type().is_dir() {
            if e.depth() > 0 && name.starts_with('.') {
                return false;
            }
            return !is_skipped_dir(&name);
        }
        true
    }) {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };

        if entry.file_type().is_dir() {
            continue;
        }

        let path = entry.path().to_path_buf();
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");

        if !is_text_ext(ext) {
            continue;
        }

        let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
        if size == 0 || size > MAX_FILE_SIZE {
            continue;
        }

        files.push((path, size));
    }

    files.sort_by(|a, b| b.1.cmp(&a.1));

    let mut selected = Vec::new();
    let mut ext_counts: HashMap<String, usize> = HashMap::new();

    for (path, _size) in &files {
        if selected.len() >= MAX_FILES {
            break;
        }
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_string();
        let count = ext_counts.entry(ext.clone()).or_insert(0);
        if *count < 10 {
            *count += 1;
            selected.push(path.clone());
        }
    }

    selected
}

// ── Measurement ─────────────────────────────────────────────

fn measure_mode(content: &str, ext: &str, mode: &str, raw_tokens: usize) -> ModeMeasurement {
    let start = Instant::now();

    let compressed = match mode {
        "map" => {
            let sigs = signatures::extract_signatures(content, ext);
            let dep_info = deps::extract_deps(content, ext);
            let mut parts = Vec::new();
            if !dep_info.imports.is_empty() {
                parts.push(format!("deps: {}", dep_info.imports.join(", ")));
            }
            if !dep_info.exports.is_empty() {
                parts.push(format!("exports: {}", dep_info.exports.join(", ")));
            }
            let key_sigs: Vec<String> = sigs
                .iter()
                .filter(|s| s.is_exported || s.indent == 0)
                .map(|s| s.to_compact())
                .collect();
            if !key_sigs.is_empty() {
                parts.push(key_sigs.join("\n"));
            }
            parts.join("\n")
        }
        "signatures" => {
            let sigs = signatures::extract_signatures(content, ext);
            sigs.iter().map(|s| s.to_compact()).collect::<Vec<_>>().join("\n")
        }
        "aggressive" => compressor::aggressive_compress(content, Some(ext)),
        "entropy" => entropy::entropy_compress(content).output,
        "cache_hit" => "[cached] re-read ~13tok".to_string(),
        _ => content.to_string(),
    };

    let latency = start.elapsed();
    let tokens = if mode == "cache_hit" {
        CACHE_HIT_TOKENS
    } else {
        count_tokens(&compressed)
    };

    let savings_pct = if raw_tokens > 0 {
        (1.0 - tokens as f64 / raw_tokens as f64) * 100.0
    } else {
        0.0
    };

    let preservation_score = if mode == "cache_hit" {
        -1.0
    } else {
        preservation::measure(content, &compressed, ext).overall()
    };

    ModeMeasurement {
        mode: mode.to_string(),
        tokens,
        savings_pct,
        latency_us: latency.as_micros() as u64,
        preservation_score,
    }
}

fn measure_file(path: &Path, root: &str) -> Option<FileMeasurement> {
    let content = std::fs::read_to_string(path).ok()?;
    if content.is_empty() {
        return None;
    }

    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_string();

    let raw_tokens = count_tokens(&content);
    if raw_tokens == 0 {
        return None;
    }

    let modes = ["map", "signatures", "aggressive", "entropy", "cache_hit"];
    let measurements: Vec<ModeMeasurement> = modes
        .iter()
        .map(|m| measure_mode(&content, &ext, m, raw_tokens))
        .collect();

    let display_path = path
        .strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .to_string();

    Some(FileMeasurement {
        path: display_path,
        ext,
        raw_tokens,
        modes: measurements,
    })
}

// ── Aggregation ─────────────────────────────────────────────

fn aggregate_languages(files: &[FileMeasurement]) -> Vec<LanguageStats> {
    let mut map: HashMap<String, (usize, usize)> = HashMap::new();
    for f in files {
        let entry = map.entry(f.ext.clone()).or_insert((0, 0));
        entry.0 += 1;
        entry.1 += f.raw_tokens;
    }
    let mut stats: Vec<LanguageStats> = map
        .into_iter()
        .map(|(ext, (count, total_tokens))| LanguageStats { ext, count, total_tokens })
        .collect();
    stats.sort_by(|a, b| b.total_tokens.cmp(&a.total_tokens));
    stats
}

fn aggregate_modes(files: &[FileMeasurement]) -> Vec<ModeSummary> {
    let mode_names = ["map", "signatures", "aggressive", "entropy", "cache_hit"];
    let mut summaries = Vec::new();

    for mode_name in &mode_names {
        let mut total_tokens = 0usize;
        let mut total_savings = 0.0f64;
        let mut total_latency = 0u64;
        let mut total_preservation = 0.0f64;
        let mut preservation_count = 0usize;
        let mut count = 0usize;

        for f in files {
            if let Some(m) = f.modes.iter().find(|m| m.mode == *mode_name) {
                total_tokens += m.tokens;
                total_savings += m.savings_pct;
                total_latency += m.latency_us;
                if m.preservation_score >= 0.0 {
                    total_preservation += m.preservation_score;
                    preservation_count += 1;
                }
                count += 1;
            }
        }

        if count == 0 {
            continue;
        }

        summaries.push(ModeSummary {
            mode: mode_name.to_string(),
            total_compressed_tokens: total_tokens,
            avg_savings_pct: total_savings / count as f64,
            avg_latency_us: total_latency / count as u64,
            avg_preservation: if preservation_count > 0 {
                total_preservation / preservation_count as f64
            } else {
                -1.0
            },
        });
    }

    summaries
}

// ── Session Simulation ──────────────────────────────────────

fn simulate_session(files: &[FileMeasurement]) -> SessionSimResult {
    if files.is_empty() {
        return SessionSimResult {
            raw_tokens: 0,
            lean_tokens: 0,
            lean_ccp_tokens: 0,
            raw_cost: 0.0,
            lean_cost: 0.0,
            ccp_cost: 0.0,
        };
    }

    let file_count = files.len().min(15);
    let selected = &files[..file_count];

    let first_read_raw: usize = selected.iter().map(|f| f.raw_tokens).sum();

    let first_read_lean: usize = selected
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let mode = if i % 3 == 0 { "aggressive" } else { "map" };
            f.modes
                .iter()
                .find(|m| m.mode == mode)
                .map(|m| m.tokens)
                .unwrap_or(f.raw_tokens)
        })
        .sum();

    let cache_reread_count = 10usize.min(file_count);
    let cache_raw: usize = selected[..cache_reread_count]
        .iter()
        .map(|f| f.raw_tokens)
        .sum();
    let cache_lean: usize = cache_reread_count * CACHE_HIT_TOKENS;

    let shell_count = 8usize;
    let shell_raw = shell_count * 500;
    let shell_lean = shell_count * 200;

    let resume_raw: usize = selected.iter().map(|f| f.raw_tokens).sum();
    let resume_lean: usize = selected
        .iter()
        .map(|f| {
            f.modes
                .iter()
                .find(|m| m.mode == "map")
                .map(|m| m.tokens)
                .unwrap_or(f.raw_tokens)
        })
        .sum();
    let resume_ccp = 400usize;

    let raw_total = first_read_raw + cache_raw + shell_raw + resume_raw;
    let lean_total = first_read_lean + cache_lean + shell_lean + resume_lean;
    let ccp_total = first_read_lean + cache_lean + shell_lean + resume_ccp;

    SessionSimResult {
        raw_tokens: raw_total,
        lean_tokens: lean_total,
        lean_ccp_tokens: ccp_total,
        raw_cost: raw_total as f64 * COST_PER_TOKEN,
        lean_cost: lean_total as f64 * COST_PER_TOKEN,
        ccp_cost: ccp_total as f64 * COST_PER_TOKEN,
    }
}

// ── Public API ──────────────────────────────────────────────

pub fn run_project_benchmark(path: &str) -> ProjectBenchmark {
    let root = if path.is_empty() { "." } else { path };
    let scanned = scan_project(root);
    let files_scanned = scanned.len();

    let file_results: Vec<FileMeasurement> = scanned
        .iter()
        .filter_map(|p| measure_file(p, root))
        .collect();

    let total_raw_tokens: usize = file_results.iter().map(|f| f.raw_tokens).sum();
    let languages = aggregate_languages(&file_results);
    let mode_summaries = aggregate_modes(&file_results);
    let session_sim = simulate_session(&file_results);

    ProjectBenchmark {
        root: root.to_string(),
        files_scanned,
        files_measured: file_results.len(),
        total_raw_tokens,
        languages,
        mode_summaries,
        session_sim,
        file_results,
    }
}

// ── Report: Terminal ────────────────────────────────────────

pub fn format_terminal(b: &ProjectBenchmark) -> String {
    let mut out = Vec::new();
    let sep = "\u{2550}".repeat(66);

    out.push(format!("{sep}"));
    out.push(format!("  lean-ctx Benchmark — {}", b.root));
    out.push(format!("{sep}"));

    let lang_summary: Vec<String> = b.languages.iter().take(5).map(|l| {
        format!("{} {}", l.count, l.ext)
    }).collect();
    out.push(format!("  Scanned: {} files ({})", b.files_measured, lang_summary.join(", ")));
    out.push(format!("  Total raw tokens: {}", format_num(b.total_raw_tokens)));
    out.push(String::new());

    out.push("  Mode Performance:".to_string());
    out.push(format!("  {:<14} {:>10} {:>10} {:>10} {:>10}",
        "Mode", "Tokens", "Savings", "Latency", "Quality"));
    out.push(format!("  {}", "\u{2500}".repeat(58)));

    for m in &b.mode_summaries {
        let qual = if m.avg_preservation < 0.0 {
            "N/A".to_string()
        } else {
            format!("{:.1}%", m.avg_preservation * 100.0)
        };
        let latency = if m.avg_latency_us > 1000 {
            format!("{:.1}ms", m.avg_latency_us as f64 / 1000.0)
        } else {
            format!("{}μs", m.avg_latency_us)
        };
        out.push(format!("  {:<14} {:>10} {:>9.1}% {:>10} {:>10}",
            m.mode,
            format_num(m.total_compressed_tokens),
            m.avg_savings_pct,
            latency,
            qual,
        ));
    }

    out.push(String::new());
    out.push("  Session Simulation (30-min coding):".to_string());
    out.push(format!("  {:<24} {:>10} {:>10} {:>10}",
        "Approach", "Tokens", "Cost", "Savings"));
    out.push(format!("  {}", "\u{2500}".repeat(58)));

    let s = &b.session_sim;
    out.push(format!("  {:<24} {:>10} {:>10} {:>10}",
        "Raw (no compression)",
        format_num(s.raw_tokens),
        format!("${:.3}", s.raw_cost),
        "\u{2014}",
    ));

    let lean_pct = if s.raw_tokens > 0 {
        (1.0 - s.lean_tokens as f64 / s.raw_tokens as f64) * 100.0
    } else { 0.0 };
    out.push(format!("  {:<24} {:>10} {:>10} {:>9.1}%",
        "lean-ctx (no CCP)",
        format_num(s.lean_tokens),
        format!("${:.3}", s.lean_cost),
        lean_pct,
    ));

    let ccp_pct = if s.raw_tokens > 0 {
        (1.0 - s.lean_ccp_tokens as f64 / s.raw_tokens as f64) * 100.0
    } else { 0.0 };
    out.push(format!("  {:<24} {:>10} {:>10} {:>9.1}%",
        "lean-ctx + CCP",
        format_num(s.lean_ccp_tokens),
        format!("${:.3}", s.ccp_cost),
        ccp_pct,
    ));

    out.push(format!("{sep}"));
    out.join("\n")
}

// ── Report: Markdown ────────────────────────────────────────

pub fn format_markdown(b: &ProjectBenchmark) -> String {
    let mut out = Vec::new();

    out.push("# lean-ctx Benchmark Report".to_string());
    out.push(String::new());
    out.push(format!("**Project:** `{}`", b.root));
    out.push(format!("**Files measured:** {}", b.files_measured));
    out.push(format!("**Total raw tokens:** {}", format_num(b.total_raw_tokens)));
    out.push(String::new());

    out.push("## Languages".to_string());
    out.push(String::new());
    out.push("| Extension | Files | Tokens |".to_string());
    out.push("|-----------|------:|-------:|".to_string());
    for l in &b.languages {
        out.push(format!("| {} | {} | {} |", l.ext, l.count, format_num(l.total_tokens)));
    }
    out.push(String::new());

    out.push("## Mode Performance".to_string());
    out.push(String::new());
    out.push("| Mode | Tokens | Savings | Latency | Quality |".to_string());
    out.push("|------|-------:|--------:|--------:|--------:|".to_string());
    for m in &b.mode_summaries {
        let qual = if m.avg_preservation < 0.0 {
            "N/A".to_string()
        } else {
            format!("{:.1}%", m.avg_preservation * 100.0)
        };
        let latency = if m.avg_latency_us > 1000 {
            format!("{:.1}ms", m.avg_latency_us as f64 / 1000.0)
        } else {
            format!("{}μs", m.avg_latency_us)
        };
        out.push(format!("| {} | {} | {:.1}% | {} | {} |",
            m.mode, format_num(m.total_compressed_tokens), m.avg_savings_pct, latency, qual));
    }
    out.push(String::new());

    out.push("## Session Simulation (30-min coding)".to_string());
    out.push(String::new());
    out.push("| Approach | Tokens | Cost | Savings |".to_string());
    out.push("|----------|-------:|-----:|--------:|".to_string());

    let s = &b.session_sim;
    out.push(format!("| Raw (no compression) | {} | ${:.3} | — |",
        format_num(s.raw_tokens), s.raw_cost));

    let lean_pct = if s.raw_tokens > 0 {
        (1.0 - s.lean_tokens as f64 / s.raw_tokens as f64) * 100.0
    } else { 0.0 };
    out.push(format!("| lean-ctx (no CCP) | {} | ${:.3} | {:.1}% |",
        format_num(s.lean_tokens), s.lean_cost, lean_pct));

    let ccp_pct = if s.raw_tokens > 0 {
        (1.0 - s.lean_ccp_tokens as f64 / s.raw_tokens as f64) * 100.0
    } else { 0.0 };
    out.push(format!("| lean-ctx + CCP | {} | ${:.3} | {:.1}% |",
        format_num(s.lean_ccp_tokens), s.ccp_cost, ccp_pct));

    out.push(String::new());
    out.push(format!("*Generated by lean-ctx benchmark v{} — https://leanctx.com*",
        env!("CARGO_PKG_VERSION")));

    out.join("\n")
}

// ── Report: JSON ────────────────────────────────────────────

pub fn format_json(b: &ProjectBenchmark) -> String {
    let modes: Vec<serde_json::Value> = b.mode_summaries.iter().map(|m| {
        serde_json::json!({
            "mode": m.mode,
            "total_compressed_tokens": m.total_compressed_tokens,
            "avg_savings_pct": round2(m.avg_savings_pct),
            "avg_latency_us": m.avg_latency_us,
            "avg_preservation": if m.avg_preservation < 0.0 { serde_json::Value::Null } else { serde_json::json!(round2(m.avg_preservation * 100.0)) },
        })
    }).collect();

    let languages: Vec<serde_json::Value> = b.languages.iter().map(|l| {
        serde_json::json!({
            "ext": l.ext,
            "count": l.count,
            "total_tokens": l.total_tokens,
        })
    }).collect();

    let s = &b.session_sim;
    let report = serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "root": b.root,
        "files_scanned": b.files_scanned,
        "files_measured": b.files_measured,
        "total_raw_tokens": b.total_raw_tokens,
        "languages": languages,
        "mode_summaries": modes,
        "session_simulation": {
            "raw_tokens": s.raw_tokens,
            "lean_tokens": s.lean_tokens,
            "lean_ccp_tokens": s.lean_ccp_tokens,
            "raw_cost_usd": round2(s.raw_cost),
            "lean_cost_usd": round2(s.lean_cost),
            "ccp_cost_usd": round2(s.ccp_cost),
        },
    });

    serde_json::to_string_pretty(&report).unwrap_or_else(|_| "{}".to_string())
}

// ── Helpers ─────────────────────────────────────────────────

fn format_num(n: usize) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        format!("{n}")
    }
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}
