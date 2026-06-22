//! Efficiency benchmark harness (Phase 0 of the efficiency epic).
//!
//! Custom harness (no criterion, matching the other `harness = false` benches):
//! prints a markdown latency + token report for `ctx_search` on a synthetic
//! corpus. From Phase 1 on it also measures the resident line-search index so
//! the walk-vs-index speedup is visible in a single run (no "before" git state
//! needed).
//!
//! Run:    cargo bench --bench efficiency
//! Tune:   `BENCH_FILES=5000` cargo bench --bench efficiency

use std::path::Path;
use std::time::{Duration, Instant};

use lean_ctx::core::protocol::CrpMode;
use lean_ctx::core::tokens::count_tokens;
use lean_ctx::tools::ctx_search;

const ITERS: usize = 50;

fn percentile(sorted: &[Duration], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = (((sorted.len() - 1) as f64) * p).round() as usize;
    sorted[idx].as_secs_f64() * 1000.0
}

/// Deterministic synthetic corpus: `n_files` Rust files spread across 20 dirs.
/// Every file contains the common token `handler`; 1-in-500 contains a rare
/// camelCase token; none contain the negative query.
fn create_corpus(root: &Path, n_files: usize) {
    use std::io::Write;
    for i in 0..n_files {
        let dir = root.join(format!("mod_{}", i % 20));
        std::fs::create_dir_all(&dir).unwrap();
        let mut f = std::fs::File::create(dir.join(format!("file_{i}.rs"))).unwrap();
        writeln!(f, "// file {i}").unwrap();
        writeln!(f, "pub fn handler_{i}(x: usize) -> usize {{").unwrap();
        for j in 0..30 {
            writeln!(f, "    let v{j} = compute({j}); // handler work").unwrap();
        }
        writeln!(f, "    x").unwrap();
        writeln!(f, "}}").unwrap();
        if i % 500 == 0 {
            writeln!(f, "fn flushPassiveEffectsRare() {{}}").unwrap();
        }
    }
}

fn measure<F: FnMut() -> String>(mut run: F) -> (f64, f64, f64, usize) {
    let _ = run(); // warm
    let mut times = Vec::with_capacity(ITERS);
    let mut tokens = 0;
    for _ in 0..ITERS {
        let t = Instant::now();
        let out = run();
        times.push(t.elapsed());
        tokens = count_tokens(&out);
    }
    times.sort_unstable();
    (
        percentile(&times, 0.5),
        percentile(&times, 0.95),
        percentile(&times, 0.99),
        tokens,
    )
}

fn bench_walk(corpus: &str, pattern: &str) -> (f64, f64, f64, usize) {
    measure(|| ctx_search::handle(pattern, corpus, None, 20, CrpMode::Off, true, false).text)
}

fn main() {
    let n_files: usize = std::env::var("BENCH_FILES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2000);
    let tmp = tempfile::tempdir().unwrap();
    let corpus = tmp.path();
    eprintln!("building synthetic corpus: {n_files} files ...");
    create_corpus(corpus, n_files);
    let corpus_str = corpus.to_string_lossy().to_string();

    let queries = [
        ("common (handler)", "handler"),
        ("rare (flushPassiveEffectsRare)", "flushPassiveEffectsRare"),
        ("negative (xyzzy_nonexistent)", "xyzzy_nonexistent"),
    ];

    println!("# ctx_search efficiency bench ({n_files} files, {ITERS} iters)\n");

    // --- Walk path (legacy): force index off so numbers are uncontaminated ---
    // TODO: Audit that the environment access only happens in single-threaded code.
    unsafe { std::env::set_var("LEAN_CTX_DISABLE_SEARCH_INDEX", "1") };
    println!("## Walk path (legacy)\n");
    println!("| query | p50 ms | p95 ms | p99 ms | resp tokens |");
    println!("|---|---|---|---|---|");
    let mut walk_out = Vec::new();
    for (label, pat) in queries {
        let (p50, p95, p99, tok) = bench_walk(&corpus_str, pat);
        println!("| {label} | {p50:.3} | {p95:.3} | {p99:.3} | {tok} |");
        walk_out.push(match_lines(&corpus_str, pat));
    }

    // --- Resident index path: warm synchronously, then measure ---
    // TODO: Audit that the environment access only happens in single-threaded code.
    unsafe { std::env::remove_var("LEAN_CTX_DISABLE_SEARCH_INDEX") };
    let warmed = lean_ctx::core::search_index::warm_blocking(&corpus_str, true, false);
    println!("\n## Resident index path (warm={warmed})\n");
    println!("| query | p50 ms | p95 ms | p99 ms | resp tokens |");
    println!("|---|---|---|---|---|");
    for (idx, (label, pat)) in queries.iter().enumerate() {
        // Recall-parity guard: index hits must equal walk hits (Jaccard 1.0).
        let index_hits = match_lines(&corpus_str, pat);
        assert_eq!(
            index_hits, walk_out[idx],
            "recall parity broken for query {label:?}"
        );
        let (p50, p95, p99, tok) = bench_walk(&corpus_str, pat);
        println!("| {label} | {p50:.3} | {p95:.3} | {p99:.3} | {tok} |");
    }
}

/// Extracts the set of `file:line` match lines from a search response so the
/// walk path and index path can be compared for recall parity.
fn match_lines(corpus: &str, pattern: &str) -> std::collections::BTreeSet<String> {
    let out = ctx_search::handle(pattern, corpus, None, 500, CrpMode::Off, true, false).text;
    out.lines()
        .filter(|l| l.contains(".rs:") || l.contains(".txt:"))
        .map(str::to_string)
        .collect()
}
