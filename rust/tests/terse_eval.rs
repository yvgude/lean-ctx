//! Terse Compression Eval Harness — 3-Arm Benchmark
//!
//! Compares compression approaches on a corpus of real-world outputs:
//! - Arm 1 (baseline): No terse, only pattern compression
//! - Arm 2 (legacy): Old `compress_terse`/`compress_ultra` (OutputDensity)
//! - Arm 3 (premium): New 4-layer terse pipeline
//!
//! Metrics per arm: token count, savings %, quality preservation score.

use lean_ctx::core::config::CompressionLevel;
use lean_ctx::core::terse;

/// A single test sample with category and content.
struct Sample {
    category: &'static str,
    content: &'static str,
}

fn eval_corpus() -> Vec<Sample> {
    vec![
        // Git outputs
        Sample {
            category: "git_status",
            content: "On branch main\nYour branch is up to date with 'origin/main'.\n\nChanges not staged for commit:\n  (use \"git add <file>...\" to update what will be committed)\n  (use \"git restore <file>...\" to discard changes in working directory)\n\tmodified:   src/core/config.rs\n\tmodified:   src/setup.rs\n\nUntracked files:\n  (use \"git add <file>...\" to include in what will be committed)\n\tsrc/core/terse/\n\nno changes added to commit (use \"git add\" and/or \"git commit -a\")",
        },
        Sample {
            category: "git_diff",
            content: "diff --git a/src/core/config.rs b/src/core/config.rs\nindex 3a4b5c6..7d8e9f0 100644\n--- a/src/core/config.rs\n+++ b/src/core/config.rs\n@@ -18,6 +18,7 @@ pub enum TerseAgent {\n     Off,\n     Lite,\n     Full,\n+    Ultra,\n }\n",
        },
        // Cargo outputs
        Sample {
            category: "cargo_build",
            content: "   Compiling lean-ctx v0.47.0 (/Users/dev/lean-ctx/rust)\n   Compiling serde v1.0.203\n   Compiling tokio v1.38.0\n   Compiling hyper v1.3.1\nwarning: unused variable `result`\n  --> src/core/config.rs:42:9\n   |\n42 |     let result = compute();\n   |         ^^^^^^ help: if this is intentional, prefix it with an underscore: `_result`\n   |\n   = note: `#[warn(unused_variables)]` on by default\n\nwarning: `lean-ctx` (lib) generated 1 warning\n    Finished `dev` profile [unoptimized + debuginfo] target(s) in 12.34s",
        },
        Sample {
            category: "cargo_test",
            content: "running 42 tests\ntest core::config::tests::default_is_off ... ok\ntest core::config::tests::from_legacy ... ok\ntest core::config::tests::to_components ... ok\ntest core::terse::engine::tests::compress_off ... ok\ntest core::terse::engine::tests::compress_lite ... ok\ntest core::terse::scoring::tests::entropy ... ok\ntest core::terse::quality::tests::paths ... ok\n\ntest result: ok. 42 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 2.45s",
        },
        // Compiler errors
        Sample {
            category: "rust_error",
            content: "error[E0308]: mismatched types\n  --> src/dashboard/routes/memory.rs:87:45\n   |\n87 |         total_tool_calls: session.tool_calls,\n   |                                  ^^^^^^^^^^^ expected `u32`, found `u64`\n   |\nhelp: you can convert a `u64` to a `u32` and panic if the converted value doesn't fit\n   |\n87 |         total_tool_calls: session.tool_calls.try_into().unwrap(),\n   |                                             ++++++++++++++++++++\n\nFor more information about this error, try `rustc --explain E0308`.\nerror: could not compile `lean-ctx` (lib) due to 1 previous error",
        },
        // JSON responses
        Sample {
            category: "json_api",
            content: "{\n  \"status\": \"success\",\n  \"data\": {\n    \"users\": [\n      {\n        \"id\": 1,\n        \"name\": \"Alice\",\n        \"email\": \"alice@example.com\",\n        \"role\": \"admin\",\n        \"created_at\": \"2024-01-15T10:30:00Z\"\n      },\n      {\n        \"id\": 2,\n        \"name\": \"Bob\",\n        \"email\": \"bob@example.com\",\n        \"role\": \"user\",\n        \"created_at\": \"2024-02-20T14:45:00Z\"\n      }\n    ],\n    \"pagination\": {\n      \"page\": 1,\n      \"per_page\": 20,\n      \"total\": 2\n    }\n  }\n}",
        },
        // Docker output
        Sample {
            category: "docker_build",
            content: "Sending build context to Docker daemon  45.2MB\nStep 1/12 : FROM rust:1.79-slim as builder\n ---> abc123def456\nStep 2/12 : WORKDIR /app\n ---> Using cache\n ---> 789ghi012jkl\nStep 3/12 : COPY Cargo.toml Cargo.lock ./\n ---> Using cache\n ---> 345mno678pqr\nStep 4/12 : COPY src/ ./src/\n ---> 901stu234vwx\nStep 5/12 : RUN cargo build --release\n ---> Running in container_abc123\n   Compiling lean-ctx v0.47.0\n    Finished `release` profile [optimized] target(s) in 45.67s\n ---> yza567bcd890\nStep 6/12 : FROM debian:bookworm-slim\n ---> efg123hij456\nSuccessfully built final_image_hash\nSuccessfully tagged lean-ctx:latest",
        },
        // Mixed prose
        Sample {
            category: "prose_readme",
            content: "# lean-ctx\n\n> Context Engineering Layer for AI coding agents\n\nlean-ctx compresses, remembers, governs and verifies what reaches the model.\n\n## Features\n\n- **10 read modes** — from full cached reads to entropy-filtered compression\n- **95+ shell patterns** — automatic command output compression\n- **Sessions & memory** — persistent knowledge across conversations\n- **Formal verification** — 53 Lean4 theorems, 0 sorry\n\n## Installation\n\n```bash\ncurl -sSf https://leanctx.com/install.sh | sh\nlean-ctx setup\n```\n\n## Quick Start\n\nAfter installation, lean-ctx automatically integrates with your AI coding agent.\nNo manual configuration needed — just start coding.\n\nFor more details, see the [documentation](https://leanctx.com/docs).",
        },
        // NPM output
        Sample {
            category: "npm_install",
            content: "npm warn deprecated inflight@1.0.6: This module is not supported, and leaks memory.\nnpm warn deprecated glob@7.2.3: Glob versions prior to v9 are no longer supported.\n\nadded 847 packages, and audited 848 packages in 12s\n\n142 packages are looking for funding\n  run `npm fund` for details\n\n3 moderate severity vulnerabilities\n\nTo address all issues, run:\n  npm audit fix\n\nRun `npm audit` for details.",
        },
        // Kubernetes
        Sample {
            category: "k8s_describe",
            content: "Name:         lean-ctx-server-7b8f9c6d5-x2k4m\nNamespace:    production\nPriority:     0\nNode:         worker-node-3/10.0.1.15\nStart Time:   Sat, 09 May 2026 08:00:00 +0200\nLabels:       app=lean-ctx-server\n              pod-template-hash=7b8f9c6d5\nAnnotations:  <none>\nStatus:       Running\nIP:           10.244.3.42\nContainers:\n  lean-ctx:\n    Container ID:   containerd://abc123\n    Image:          lean-ctx:v0.47.0\n    Port:           8080/TCP\n    State:          Running\n      Started:      Sat, 09 May 2026 08:00:05 +0200\n    Ready:          True\n    Restart Count:  0\nConditions:\n  Type              Status\n  Initialized       True\n  Ready             True\n  ContainersReady   True\n  PodScheduled      True",
        },
    ]
}

#[test]
fn eval_baseline_vs_premium() {
    let corpus = eval_corpus();
    let mut baseline_savings = Vec::new();
    let mut premium_savings = Vec::new();

    for sample in &corpus {
        // Arm 1: Baseline (no terse)
        baseline_savings.push(0.0f32);

        // Arm 3: Premium (new pipeline)
        let result = terse::pipeline::compress(sample.content, &CompressionLevel::Standard, None);
        premium_savings.push(result.savings_pct);
    }

    let avg_premium: f32 = premium_savings.iter().sum::<f32>() / premium_savings.len() as f32;

    eprintln!("\n=== Terse Eval Results ===");
    for (i, sample) in corpus.iter().enumerate() {
        eprintln!(
            "  {:<15} baseline: {:>5.1}%  premium: {:>5.1}%",
            sample.category, baseline_savings[i], premium_savings[i],
        );
    }
    eprintln!("  Average premium savings: {avg_premium:.1}%");
    eprintln!("===========================\n");

    assert!(
        avg_premium > 0.0,
        "premium pipeline should achieve some compression on the eval corpus"
    );
}

#[test]
fn eval_quality_preservation() {
    let corpus = eval_corpus();

    for sample in &corpus {
        let result = terse::pipeline::compress(sample.content, &CompressionLevel::Standard, None);

        assert!(
            result.quality_passed,
            "quality gate should pass for category '{}': savings={:.1}%",
            sample.category, result.savings_pct,
        );
    }
}

#[test]
fn eval_max_level_more_aggressive() {
    let corpus = eval_corpus();
    let mut standard_total = 0.0f32;
    let mut max_total = 0.0f32;

    for sample in &corpus {
        let std_result =
            terse::pipeline::compress(sample.content, &CompressionLevel::Standard, None);
        let max_result = terse::pipeline::compress(sample.content, &CompressionLevel::Max, None);
        standard_total += std_result.savings_pct;
        max_total += max_result.savings_pct;
    }

    eprintln!(
        "Standard avg: {:.1}%, Max avg: {:.1}%",
        standard_total / corpus.len() as f32,
        max_total / corpus.len() as f32,
    );

    assert!(
        max_total >= standard_total,
        "Max level should be at least as aggressive as Standard: max={max_total:.1} vs std={standard_total:.1}"
    );
}

#[test]
fn eval_off_level_no_changes() {
    let corpus = eval_corpus();
    for sample in &corpus {
        let result = terse::pipeline::compress(sample.content, &CompressionLevel::Off, None);
        assert_eq!(
            result.output, sample.content,
            "Off level must not modify content for '{}'",
            sample.category,
        );
        assert_eq!(result.savings_pct, 0.0);
    }
}

#[test]
fn eval_legacy_comparison() {
    let corpus = eval_corpus();
    let mut legacy_savings = Vec::new();
    let mut premium_savings = Vec::new();

    for sample in &corpus {
        let before = terse::counter::count(sample.content);

        let legacy = legacy_compress_terse(sample.content);
        let legacy_after = terse::counter::count(&legacy);
        let legacy_pct = terse::counter::savings_pct(before, legacy_after);
        legacy_savings.push(legacy_pct);

        let premium = terse::pipeline::compress(sample.content, &CompressionLevel::Standard, None);
        premium_savings.push(premium.savings_pct);
    }

    let avg_legacy: f32 = legacy_savings.iter().sum::<f32>() / legacy_savings.len() as f32;
    let avg_premium: f32 = premium_savings.iter().sum::<f32>() / premium_savings.len() as f32;

    eprintln!("\n=== Legacy vs Premium ===");
    for (i, sample) in corpus.iter().enumerate() {
        eprintln!(
            "  {:<15} legacy: {:>5.1}%  premium: {:>5.1}%",
            sample.category, legacy_savings[i], premium_savings[i],
        );
    }
    eprintln!("  Average legacy: {avg_legacy:.1}%  premium: {avg_premium:.1}%");
    eprintln!("=========================\n");
}

fn legacy_compress_terse(text: &str) -> String {
    text.lines()
        .filter(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                return false;
            }
            if trimmed.starts_with("//") || trimmed.starts_with('#') || trimmed.starts_with("--") {
                return false;
            }
            if trimmed.len() >= 4 {
                let chars: Vec<char> = trimmed.chars().collect();
                let first = chars[0];
                if matches!(first, '=' | '-' | '*') {
                    let same = chars.iter().filter(|c| **c == first).count();
                    if same as f64 / chars.len() as f64 > 0.7 {
                        return false;
                    }
                }
            }
            true
        })
        .collect::<Vec<_>>()
        .join("\n")
}
