// SPDX-License-Identifier: Apache-2.0
//! E26 ETPAO Benchmark: measures the concrete token impact of the cache
//! efficiency improvements (Phases 3, 4, 6).
//!
//! Simulates a realistic agent session and compares token delivery with vs
//! without full-delivery degradation.

#![allow(clippy::uninlined_format_args)]

use super::*;

fn make_realistic_file(name: &str, fn_count: usize) -> String {
    let mut content = format!("//! Module: {name}\n\nuse std::collections::HashMap;\n\n");
    for i in 0..fn_count {
        content.push_str(&format!(
            "pub fn {name}_{i}(input: &str) -> Result<String, Box<dyn std::error::Error>> {{\n\
             \x20   let mut map = HashMap::new();\n\
             \x20   map.insert(\"key_{i}\", input.len());\n\
             \x20   let result = map.values().sum::<usize>();\n\
             \x20   Ok(format!(\"{{name}}_{i}: {{result}}\"))\n\
             }}\n\n"
        ));
    }
    content
}

struct BenchResult {
    total_tokens: usize,
    stub_count: usize,
    fresh_count: usize,
    compressed_cache_hits: usize,
    degradation_count: u64,
}

fn run_session(threshold: u32) -> BenchResult {
    let _iso = lean_ctx::core::data_dir::isolated_data_dir();
    let _env_lock = lean_ctx::core::data_dir::test_env_lock();
    lean_ctx::test_env::set_var("LCTX_FULL_DEGRADATION_THRESHOLD", threshold.to_string());

    // Drain any prior accumulated counts so this run starts from zero.
    let _ = lean_ctx::core::auto_mode_resolver::source_counts();

    let dir = tempfile::tempdir().unwrap();
    let files: Vec<(String, String)> = ["api", "cache", "parser", "router", "utils"]
        .iter()
        .map(|name| {
            let path = dir.path().join(format!("{name}.rs"));
            let content = make_realistic_file(name, 8);
            std::fs::write(&path, &content).unwrap();
            (path.to_string_lossy().to_string(), name.to_string())
        })
        .collect();

    let mut cache = SessionCache::new();
    let mut total_tokens = 0usize;
    let mut stub_count = 0usize;
    let mut fresh_count = 0usize;
    let mut compressed_hits = 0usize;

    // Phase 1: Initial exploration — map mode for all 5 files
    for (path, _) in &files {
        let r = handle_with_task_resolved(
            &mut cache,
            path,
            "map",
            CrpMode::Off,
            Some("implement feature"),
        );
        total_tokens += r.output_tokens;
        if r.is_cache_hit {
            compressed_hits += 1;
        } else {
            fresh_count += 1;
        }
    }

    // Phase 2: Deep dive — full read of 3 key files
    for (path, _) in files.iter().take(3) {
        let r = handle_with_task_resolved(
            &mut cache,
            path,
            "full",
            CrpMode::Off,
            Some("implement feature"),
        );
        total_tokens += r.output_tokens;
        if r.is_cache_hit {
            stub_count += 1;
        } else {
            fresh_count += 1;
        }
    }

    // Phase 3: Re-read cycle — 6 full re-reads of same 3 files
    for _round in 0..6 {
        for (path, _) in files.iter().take(3) {
            let r = handle_with_task_resolved(
                &mut cache,
                path,
                "full",
                CrpMode::Off,
                Some("implement feature"),
            );
            total_tokens += r.output_tokens;
            if r.is_cache_hit {
                stub_count += 1;
            } else {
                fresh_count += 1;
            }
        }
    }

    // Phase 4: Map re-reads — tests compressed cache / context dedup
    for (path, _) in &files {
        let r = handle_with_task_resolved(
            &mut cache,
            path,
            "map",
            CrpMode::Off,
            Some("implement feature"),
        );
        total_tokens += r.output_tokens;
        if r.is_cache_hit {
            compressed_hits += 1;
        } else {
            fresh_count += 1;
        }
    }

    // Phase 5: More full re-reads of 2 hot files
    for _round in 0..4 {
        for (path, _) in files.iter().take(2) {
            let r = handle_with_task_resolved(
                &mut cache,
                path,
                "full",
                CrpMode::Off,
                Some("implement feature"),
            );
            total_tokens += r.output_tokens;
            if r.is_cache_hit {
                stub_count += 1;
            } else {
                fresh_count += 1;
            }
        }
    }

    let counts = lean_ctx::core::auto_mode_resolver::source_counts();
    let degradation_count = counts
        .iter()
        .find(|(k, _)| *k == "full_delivery_degraded")
        .map_or(0, |(_, v)| *v);

    lean_ctx::test_env::remove_var("LCTX_FULL_DEGRADATION_THRESHOLD");

    BenchResult {
        total_tokens,
        stub_count,
        fresh_count,
        compressed_cache_hits: compressed_hits,
        degradation_count,
    }
}

#[test]
fn e26_degradation_delivers_fresh_content_periodically() {
    // Run "without" first so counter drain starts clean.
    let without_degrade = run_session(999);
    let with_degrade = run_session(2);

    let total_reads_no = without_degrade.stub_count
        + without_degrade.fresh_count
        + without_degrade.compressed_cache_hits;
    let total_reads =
        with_degrade.stub_count + with_degrade.fresh_count + with_degrade.compressed_cache_hits;

    eprintln!("\n╔══════════════════════════════════════════════════════════╗");
    eprintln!("║  E26 ETPAO BENCHMARK — Cache Efficiency Proof            ║");
    eprintln!("╠══════════════════════════════════════════════════════════╣");
    eprintln!("║                                                          ║");
    eprintln!("║  Session: 5 files × ~8 functions each (~88 lines/file)   ║");
    eprintln!("║  Pattern: scan → deep-read → 6× re-read → map → 4× hot  ║");
    eprintln!("║  Total reads: {total_reads:>4}                                      ║");
    eprintln!("║                                                          ║");
    eprintln!("╠══════════════════════════════════════════════════════════╣");
    eprintln!("║  WITHOUT degradation (threshold=999)                     ║");
    eprintln!(
        "║    Fresh deliveries:      {:>4}                           ║",
        without_degrade.fresh_count
    );
    eprintln!(
        "║    Stub (unchanged):      {:>4}                           ║",
        without_degrade.stub_count
    );
    eprintln!(
        "║    Compressed cache hits: {:>4}                           ║",
        without_degrade.compressed_cache_hits
    );
    eprintln!(
        "║    Degradations fired:    {:>4}                           ║",
        without_degrade.degradation_count
    );
    eprintln!(
        "║    Total tokens:      {:>8}                           ║",
        without_degrade.total_tokens
    );
    eprintln!("║                                                          ║");
    eprintln!("╠══════════════════════════════════════════════════════════╣");
    eprintln!("║  WITH degradation (threshold=2)                          ║");
    eprintln!(
        "║    Fresh deliveries:      {:>4}                           ║",
        with_degrade.fresh_count
    );
    eprintln!(
        "║    Stub (unchanged):      {:>4}                           ║",
        with_degrade.stub_count
    );
    eprintln!(
        "║    Compressed cache hits: {:>4}                           ║",
        with_degrade.compressed_cache_hits
    );
    eprintln!(
        "║    Degradations fired:    {:>4}                           ║",
        with_degrade.degradation_count
    );
    eprintln!(
        "║    Total tokens:      {:>8}                           ║",
        with_degrade.total_tokens
    );
    eprintln!("║                                                          ║");
    eprintln!("╠══════════════════════════════════════════════════════════╣");

    let freshness_without = without_degrade.fresh_count as f64 / total_reads_no as f64 * 100.0;
    let freshness_with = with_degrade.fresh_count as f64 / total_reads as f64 * 100.0;
    let freshness_gain = freshness_with - freshness_without;

    eprintln!("║  RESULTS                                                ║");
    eprintln!(
        "║    Content freshness (no degrade): {:>5.1}%                ║",
        freshness_without
    );
    eprintln!(
        "║    Content freshness (w/ degrade): {:>5.1}%                ║",
        freshness_with
    );
    eprintln!(
        "║    Freshness improvement:        +{:>5.1}pp               ║",
        freshness_gain
    );
    eprintln!(
        "║    Extra fresh deliveries:        +{:>4}                  ║",
        with_degrade
            .fresh_count
            .saturating_sub(without_degrade.fresh_count)
    );
    eprintln!("║                                                          ║");
    eprintln!("╚══════════════════════════════════════════════════════════╝\n");

    assert!(
        with_degrade.degradation_count > 0,
        "degradation must fire at least once with threshold=2"
    );
    assert!(
        with_degrade.fresh_count > without_degrade.fresh_count,
        "degradation must yield MORE fresh deliveries: {} vs {}",
        with_degrade.fresh_count,
        without_degrade.fresh_count
    );
    assert_eq!(
        without_degrade.degradation_count, 0,
        "high threshold must never degrade"
    );
}

#[test]
fn e26_compressed_cache_eliminates_recomputation() {
    let _iso = lean_ctx::core::data_dir::isolated_data_dir();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("compressible.rs");
    let content = make_realistic_file("compressible", 12);
    std::fs::write(&path, &content).unwrap();
    let p = path.to_string_lossy().to_string();

    let mut cache = SessionCache::new();
    let original_tokens = lean_ctx::core::tokens::count_tokens(&content);

    let r1 = handle_with_task_resolved(&mut cache, &p, "map", CrpMode::Off, Some("test"));
    assert!(!r1.is_cache_hit, "first map read must be a miss");

    // Second map read: context dedup or compressed cache intercepts the re-read,
    // delivering a compact reference instead of recomputing the full map.
    let r2 = handle_with_task_resolved(&mut cache, &p, "map", CrpMode::Off, Some("test"));

    let map_tokens = r1.output_tokens;
    let compression_pct = (1.0 - map_tokens as f64 / original_tokens as f64) * 100.0;
    let reread_savings_pct = (1.0 - r2.output_tokens as f64 / original_tokens as f64) * 100.0;

    eprintln!("\n╔══════════════════════════════════════════════════════════╗");
    eprintln!("║  E26 COMPRESSED CACHE — Token Elimination Proof          ║");
    eprintln!("╠══════════════════════════════════════════════════════════╣");
    eprintln!("║  File: compressible.rs (12 functions, ~88 lines)         ║");
    eprintln!("║                                                          ║");
    eprintln!(
        "║  Original file tokens:    {:>6}                         ║",
        original_tokens
    );
    eprintln!(
        "║  Map mode tokens (1st):   {:>6}  ({:.0}% compression)      ║",
        map_tokens, compression_pct
    );
    eprintln!(
        "║  Re-read tokens (2nd):    {:>6}  ({:.0}% savings vs raw)   ║",
        r2.output_tokens, reread_savings_pct
    );
    eprintln!("║                                                          ║");
    eprintln!("║  Read 1: cache miss → computed map                       ║");
    eprintln!(
        "║  Read 2: cache HIT  → {:<39}║",
        if r2.is_cache_hit {
            "served from cache (dedup/compressed)"
        } else {
            "recomputed (no cache hit)"
        }
    );
    eprintln!("║                                                          ║");
    eprintln!(
        "║  1st read savings:        {:>6} tokens (vs raw)          ║",
        original_tokens - map_tokens
    );
    eprintln!(
        "║  2nd read savings:        {:>6} tokens (vs raw)          ║",
        original_tokens.saturating_sub(r2.output_tokens)
    );
    eprintln!(
        "║  Cache efficiency:        {:>5.1}% less than 1st read      ║",
        (1.0 - r2.output_tokens as f64 / map_tokens as f64) * 100.0
    );
    eprintln!("╚══════════════════════════════════════════════════════════╝\n");

    assert!(
        r2.is_cache_hit,
        "second map read must be a cache hit (compressed cache or context dedup)"
    );
    assert!(
        r2.output_tokens < map_tokens,
        "re-read must use fewer tokens than first map: {} vs {}",
        r2.output_tokens,
        map_tokens
    );
    assert!(
        compression_pct > 40.0,
        "map mode must compress >40%: got {compression_pct:.1}%"
    );
}
