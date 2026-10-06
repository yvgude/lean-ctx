//! Cross-Agent Re-Read Cache Benchmark
//!
//! Measures token savings when multiple parallel agents read the same files,
//! leveraging `BuiltinDeliveryRegistry` to serve stubs instead of re-reads.
//!
//! Run:
//!   `cargo run --example bench_cross_agent_cache --features dev-tools`

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use lean_ctx::core::ocla::builtin::delivery_registry::BuiltinDeliveryRegistry;
use lean_ctx::core::ocla::traits::DeliveryRegistry;
use lean_ctx::core::ocla::types::DeliveryEntry;
use lean_ctx::core::tokens::count_tokens;

const NUM_AGENTS: usize = 4;
const TARGET_FILES: usize = 20;
const STUB_TOKENS: u64 = 13;
const COST_PER_MILLION_INPUT: f64 = 3.0;

fn main() {
    let source_dir = find_source_dir();
    let files = select_benchmark_files(&source_dir);

    println!("=== lean-ctx Cross-Agent Cache Benchmark ===\n");
    println!(
        "Scenario: {NUM_AGENTS} agents read {} shared source files\n",
        files.len()
    );

    let file_contents: Vec<FileData> = files
        .iter()
        .map(|path| {
            let content = fs::read_to_string(path).expect("failed to read source file");
            let lines = content.lines().count() as u32;
            let tokens = count_tokens(&content) as u64;
            let blake3 = compute_blake3_prefix(&content);
            let mtime = fs::metadata(path)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(1);
            FileData {
                path: path.display().to_string(),
                lines,
                tokens,
                blake3,
                mtime,
            }
        })
        .collect();

    let avg_loc: u32 =
        file_contents.iter().map(|f| f.lines).sum::<u32>() / file_contents.len() as u32;
    let total_tokens_per_agent: u64 = file_contents.iter().map(|f| f.tokens).sum();

    println!(
        "Files: {} (avg {avg_loc} LOC, {total_tokens_per_agent} tokens/agent)\n",
        files.len()
    );

    let start = Instant::now();

    // Scenario A: Without cache — each agent reads all files fresh
    let baseline_per_agent = total_tokens_per_agent;
    let baseline_total = baseline_per_agent * NUM_AGENTS as u64;

    // Scenario B: With cross-agent cache
    let registry = BuiltinDeliveryRegistry::with_config(1000, 60);
    let mut cached_per_agent: Vec<u64> = Vec::with_capacity(NUM_AGENTS);

    for agent_idx in 0..NUM_AGENTS {
        let agent_id = format!("agent-{}", agent_idx + 1);
        let conversation_id = format!("conv-{}", agent_idx + 1);
        let mut agent_tokens: u64 = 0;

        for file in &file_contents {
            let hit = registry.check_delivery(
                &file.blake3,
                file.mtime,
                &file.path,
                Some(&agent_id),
                Some(&conversation_id),
            );

            if let Some(record) = hit {
                registry.record_stub_served(&record, STUB_TOKENS);
                agent_tokens += STUB_TOKENS;
            } else {
                registry.record_delivery(DeliveryEntry {
                    access: None,
                    blake3: file.blake3,
                    path: file.path.clone(),
                    line_count: file.lines,
                    token_count: file.tokens,
                    agent_id: agent_id.clone(),
                    conversation_id: conversation_id.clone(),
                    mtime: file.mtime,
                    relay_content: None,
                    relay_mode: None,
                });
                agent_tokens += file.tokens;
            }
        }

        cached_per_agent.push(agent_tokens);
    }

    let cached_total: u64 = cached_per_agent.iter().sum();
    let elapsed = start.elapsed();

    print_results(
        baseline_per_agent,
        baseline_total,
        &cached_per_agent,
        cached_total,
        avg_loc,
        files.len(),
        elapsed,
    );
}

fn print_results(
    baseline_per_agent: u64,
    baseline_total: u64,
    cached_per_agent: &[u64],
    cached_total: u64,
    avg_loc: u32,
    file_count: usize,
    elapsed: std::time::Duration,
) {
    println!(
        "Scenario: {NUM_AGENTS} agents read {file_count} shared source files (avg {avg_loc} LOC)\n"
    );
    println!("| Agent            | Without Cache  | With Cache     | Savings |");
    println!("|------------------|----------------|----------------|---------|");

    for (i, &agent_tok) in cached_per_agent.iter().enumerate() {
        let label = if i == 0 {
            format!("Agent {} (cold)", i + 1)
        } else {
            format!("Agent {}", i + 1)
        };
        let savings_pct = if baseline_per_agent > 0 {
            (1.0 - agent_tok as f64 / baseline_per_agent as f64) * 100.0
        } else {
            0.0
        };
        println!(
            "| {label:<16} | {:>10} tok | {:>10} tok | {savings_pct:>5.1}% |",
            format_number(baseline_per_agent),
            format_number(agent_tok),
        );
    }

    let total_savings_pct = if baseline_total > 0 {
        (1.0 - cached_total as f64 / baseline_total as f64) * 100.0
    } else {
        0.0
    };
    println!(
        "| {:<16} | {:>10} tok | {:>10} tok | {total_savings_pct:>5.1}% |",
        "**TOTAL**",
        format_number(baseline_total),
        format_number(cached_total),
    );

    let cost_without = baseline_total as f64 / 1_000_000.0 * COST_PER_MILLION_INPUT;
    let cost_with = cached_total as f64 / 1_000_000.0 * COST_PER_MILLION_INPUT;
    let cost_saved = cost_without - cost_with;

    println!("\nCost at ${COST_PER_MILLION_INPUT:.0}/1M input tokens:");
    println!("  Without cache: ${cost_without:.2} per shared-read round");
    println!("  With cache:    ${cost_with:.2} per shared-read round");
    println!("  Saved:         ${cost_saved:.2} ({total_savings_pct:.1}%)");

    println!("\n---");
    println!(
        "Benchmark completed in {:.1}ms",
        elapsed.as_secs_f64() * 1000.0
    );
    println!(
        "Reddit title: \"{total_savings_pct:.0}% fewer tokens when {NUM_AGENTS} agents share a codebase\""
    );
}

fn format_number(n: u64) -> String {
    let s = n.to_string();
    let mut result = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            result.push(',');
        }
        result.push(ch);
    }
    result.chars().rev().collect()
}

fn compute_blake3_prefix(content: &str) -> [u8; 12] {
    let hash = blake3::hash(content.as_bytes());
    let bytes = hash.as_bytes();
    let mut prefix = [0u8; 12];
    prefix.copy_from_slice(&bytes[..12]);
    prefix
}

fn find_source_dir() -> PathBuf {
    let candidates = [
        PathBuf::from("rust/src"),
        PathBuf::from("src"),
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src"),
    ];
    for c in &candidates {
        if c.is_dir() {
            return c.clone();
        }
    }
    panic!("Cannot find source directory. Run from repo root or rust/ directory.");
}

fn select_benchmark_files(source_dir: &Path) -> Vec<PathBuf> {
    let mut rs_files: Vec<(PathBuf, u64)> = Vec::new();
    collect_rs_files(source_dir, &mut rs_files);

    rs_files.sort_by_key(|(_path, size)| std::cmp::Reverse(*size));

    let mut selected: Vec<PathBuf> = Vec::new();
    for (path, _size) in &rs_files {
        if selected.len() >= TARGET_FILES {
            break;
        }
        let Ok(content) = fs::read_to_string(path) else {
            continue;
        };
        let lines = content.lines().count();
        if (200..=800).contains(&lines) {
            selected.push(path.clone());
        }
    }

    // Fall back to any files >= 100 lines if not enough in range
    if selected.len() < TARGET_FILES {
        for (path, _size) in &rs_files {
            if selected.len() >= TARGET_FILES {
                break;
            }
            if !selected.contains(path) {
                let Ok(content) = fs::read_to_string(path) else {
                    continue;
                };
                let lines = content.lines().count();
                if lines >= 100 {
                    selected.push(path.clone());
                }
            }
        }
    }

    assert!(
        !selected.is_empty(),
        "No suitable .rs files found in {}",
        source_dir.display()
    );

    selected
}

fn collect_rs_files(dir: &Path, results: &mut Vec<(PathBuf, u64)>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path
                .file_name()
                .is_some_and(|n| n == "target" || n == "vendor")
            {
                continue;
            }
            collect_rs_files(&path, results);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            results.push((path, size));
        }
    }
}

struct FileData {
    path: String,
    lines: u32,
    tokens: u64,
    blake3: [u8; 12],
    mtime: u64,
}
