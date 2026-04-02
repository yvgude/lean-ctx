use lean_ctx::core::compressor::{aggressive_compress, lightweight_cleanup, safeguard_ratio};
use lean_ctx::core::entropy::entropy_compress;
use lean_ctx::core::patterns;
use lean_ctx::core::protocol::instruction_decoder_block;
use lean_ctx::core::signatures::extract_signatures;
use lean_ctx::core::tokens::count_tokens;
use lean_ctx::tools::ctx_response;
use lean_ctx::tools::CrpMode;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn compression_ratio(original: usize, compressed: usize) -> f64 {
    if original == 0 {
        return 0.0;
    }
    (original - compressed) as f64 / original as f64 * 100.0
}

fn measure_pattern(command: &str, output: &str) -> (usize, usize, f64) {
    let original = count_tokens(output);
    let compressed = lean_ctx::core::patterns::compress_output(command, output)
        .unwrap_or_else(|| output.to_string());
    let comp_tokens = count_tokens(&compressed);
    (
        original,
        comp_tokens,
        compression_ratio(original, comp_tokens),
    )
}

fn measure_text(original: &str, compressed: &str) -> (usize, usize, f64) {
    let orig_t = count_tokens(original);
    let comp_t = count_tokens(compressed);
    (orig_t, comp_t, compression_ratio(orig_t, comp_t))
}

// ═══════════════════════════════════════════════════════════════════════════
// SECTION 1: INPUT TOKEN BENCHMARKS — Tool Descriptions + System Instructions
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn bench_system_instructions_token_count() {
    let instructions_off = lean_ctx::server::build_instructions_for_test(CrpMode::Off);
    let instructions_compact = lean_ctx::server::build_instructions_for_test(CrpMode::Compact);
    let instructions_tdd = lean_ctx::server::build_instructions_for_test(CrpMode::Tdd);

    let tok_off = count_tokens(&instructions_off);
    let tok_compact = count_tokens(&instructions_compact);
    let tok_tdd = count_tokens(&instructions_tdd);

    eprintln!("\n{}", "=".repeat(70));
    eprintln!("  SYSTEM INSTRUCTIONS TOKEN COUNT");
    eprintln!("{}", "=".repeat(70));
    eprintln!(
        "  CRP Off:     {:>6} tokens ({:>5} chars)",
        tok_off,
        instructions_off.len()
    );
    eprintln!(
        "  CRP Compact: {:>6} tokens ({:>5} chars)",
        tok_compact,
        instructions_compact.len()
    );
    eprintln!(
        "  CRP TDD:     {:>6} tokens ({:>5} chars)",
        tok_tdd,
        instructions_tdd.len()
    );
    eprintln!(
        "  Compact overhead: +{} tokens vs Off",
        tok_compact - tok_off
    );
    eprintln!("  TDD overhead:     +{} tokens vs Off", tok_tdd - tok_off);
    eprintln!("{}", "=".repeat(70));

    assert!(
        tok_off < 1850,
        "Base instructions should be <1850 tokens, got {tok_off}"
    );
    assert!(
        tok_compact < 2050,
        "Compact instructions should be <2050 tokens, got {tok_compact}"
    );
    assert!(
        tok_tdd < 2300,
        "TDD instructions should be <2300 tokens, got {tok_tdd}"
    );
    assert!(
        tok_compact - tok_off < 300,
        "Compact mode overhead should be <300 tokens"
    );
    assert!(
        tok_tdd - tok_off < 500,
        "TDD mode overhead should be <500 tokens"
    );
}

#[test]
fn bench_tool_descriptions_token_count() {
    let descriptions = lean_ctx::server::tool_descriptions_for_test();

    let mut total = 0usize;
    eprintln!("\n{}", "=".repeat(70));
    eprintln!("  TOOL DESCRIPTION TOKEN COUNTS");
    eprintln!("{}", "=".repeat(70));
    eprintln!("  {:<25} {:>8} {:>8}", "Tool", "Tokens", "Chars");
    eprintln!("  {}", "-".repeat(45));

    for (name, desc) in &descriptions {
        let t = count_tokens(desc);
        total += t;
        eprintln!("  {:<25} {:>8} {:>8}", name, t, desc.len());
    }

    eprintln!("  {}", "-".repeat(45));
    eprintln!("  {:<25} {:>8}", "TOTAL", total);
    eprintln!("{}", "=".repeat(70));

    assert!(
        total < 1500,
        "Total tool description tokens should be <1500, got {total}"
    );

    for (name, desc) in &descriptions {
        let t = count_tokens(desc);
        assert!(
            t < 120,
            "Tool '{name}' description should be <120 tokens, got {t}"
        );
    }
}

#[test]
fn bench_total_input_overhead() {
    let instructions = lean_ctx::server::build_instructions_for_test(CrpMode::Off);
    let descs = lean_ctx::server::tool_descriptions_for_test();
    let schemas = lean_ctx::server::tool_schemas_json_for_test();

    let instr_tokens = count_tokens(&instructions);
    let desc_tokens: usize = descs.iter().map(|(_, d)| count_tokens(d)).sum();
    let schema_tokens = count_tokens(&schemas);

    let total = instr_tokens + desc_tokens + schema_tokens;

    eprintln!("\n{}", "=".repeat(70));
    eprintln!("  TOTAL INPUT OVERHEAD (per session start)");
    eprintln!("{}", "=".repeat(70));
    eprintln!("  System instructions: {:>6} tokens", instr_tokens);
    eprintln!("  Tool descriptions:   {:>6} tokens", desc_tokens);
    eprintln!("  Tool schemas (JSON): {:>6} tokens", schema_tokens);
    eprintln!("  {}", "-".repeat(40));
    eprintln!("  TOTAL overhead:      {:>6} tokens", total);
    eprintln!(
        "  Estimated cost @$3/1M input: ${:.4}",
        total as f64 * 3.0 / 1_000_000.0
    );
    eprintln!("{}", "=".repeat(70));

    assert!(
        total < 5000,
        "Total input overhead should be <5000 tokens, got {total}"
    );
}

#[test]
fn bench_decoder_block_token_count() {
    let block = instruction_decoder_block();
    let tokens = count_tokens(&block);

    eprintln!(
        "\n  CEP Decoder Block: {} tokens ({} chars)",
        tokens,
        block.len()
    );
    assert!(
        tokens < 300,
        "Decoder block should be <300 tokens, got {tokens}"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// SECTION 2: PATTERN COMPRESSION BENCHMARKS — Shell Output Compression
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn bench_git_all_commands() {
    let scenarios = vec![
        ("git log -p -10", generate_git_log_patch(10), 70.0),
        ("git log -p -50", generate_git_log_patch(50), 80.0),
        ("git log --stat -20", generate_git_log_stat(20), 50.0),
        ("git log -30", generate_git_log_standard(30), 60.0),
        ("git status", generate_git_status(), 30.0),
        ("git diff", generate_git_diff(15), 40.0),
        (
            "git commit -m 'feat'",
            generate_git_commit_with_hooks(30),
            50.0,
        ),
        ("git push origin main", generate_git_push(), 20.0),
    ];

    print_compression_report("GIT COMMANDS", &scenarios);
}

#[test]
fn bench_cargo_all_commands() {
    let scenarios = vec![
        ("cargo build", generate_cargo_build_success(), 30.0),
        ("cargo build", generate_cargo_build_with_warnings(20), 40.0),
        ("cargo test", generate_cargo_test(50, 2), 40.0),
        ("cargo clippy", generate_cargo_clippy(10), 30.0),
    ];

    print_compression_report("CARGO COMMANDS", &scenarios);
}

#[test]
fn bench_docker_all_commands() {
    let scenarios = vec![
        ("docker ps", generate_docker_ps(10), 30.0),
        ("docker images", generate_docker_images(15), 30.0),
        ("docker build -t app .", generate_docker_build(20), 40.0),
    ];

    print_compression_report("DOCKER COMMANDS", &scenarios);
}

#[test]
fn bench_npm_all_commands() {
    let scenarios = vec![
        ("npm install", generate_npm_install(30), 30.0),
        ("npm test", generate_npm_test_jest(), 30.0),
        ("npm ls", generate_npm_ls(20), 20.0),
    ];

    print_compression_report("NPM COMMANDS", &scenarios);
}

#[test]
fn bench_pip_commands() {
    let scenarios = vec![
        (
            "pip install -r requirements.txt",
            generate_pip_install(15),
            20.0,
        ),
        ("pip list", generate_pip_list(30), 5.0),
    ];

    print_compression_report("PIP COMMANDS", &scenarios);
}

#[test]
fn bench_kubectl_commands() {
    let scenarios = vec![
        ("kubectl get pods", generate_kubectl_pods(20), 10.0),
        ("kubectl get pods -A", generate_kubectl_pods_all(30), 10.0),
    ];

    print_compression_report("KUBECTL COMMANDS", &scenarios);
}

#[test]
fn bench_pattern_coverage_comprehensive() {
    let all = vec![
        ("git log -p -10", generate_git_log_patch(10)),
        ("git log --stat -20", generate_git_log_stat(20)),
        ("git status", generate_git_status()),
        ("git diff", generate_git_diff(15)),
        ("git commit -m 'feat'", generate_git_commit_with_hooks(30)),
        ("cargo build", generate_cargo_build_with_warnings(20)),
        ("cargo test", generate_cargo_test(50, 2)),
        ("cargo clippy", generate_cargo_clippy(10)),
        ("docker ps", generate_docker_ps(10)),
        ("docker build -t app .", generate_docker_build(20)),
        ("npm install", generate_npm_install(30)),
        ("npm test", generate_npm_test_jest()),
        ("pip install -r requirements.txt", generate_pip_install(15)),
        ("kubectl get pods", generate_kubectl_pods(20)),
    ];

    let mut total_orig = 0usize;
    let mut total_comp = 0usize;

    eprintln!("\n{}", "=".repeat(80));
    eprintln!("  COMPREHENSIVE PATTERN COVERAGE — ALL COMMANDS");
    eprintln!("{}", "=".repeat(80));
    eprintln!(
        "  {:<40} {:>8} {:>8} {:>7}",
        "Command", "Original", "Compr.", "Saved%"
    );
    eprintln!("  {}", "-".repeat(65));

    for (cmd, output) in &all {
        let (orig, comp, pct) = measure_pattern(cmd, output);
        total_orig += orig;
        total_comp += comp;
        eprintln!("  {:<40} {:>8} {:>8} {:>6.1}%", cmd, orig, comp, pct);
    }

    let total_pct = compression_ratio(total_orig, total_comp);
    eprintln!("  {}", "-".repeat(65));
    eprintln!(
        "  {:<40} {:>8} {:>8} {:>6.1}%",
        "TOTAL", total_orig, total_comp, total_pct
    );
    eprintln!("{}", "=".repeat(80));

    assert!(
        total_pct > 40.0,
        "Overall pattern savings should be >40%, got {total_pct:.1}%"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// SECTION 3: COMPRESSION MODE BENCHMARKS — ctx_read modes on real-like files
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn bench_aggressive_mode_rust() {
    let content = generate_rust_file(200);
    let compressed = aggressive_compress(&content, Some("rs"));
    let (orig, comp, pct) = measure_text(&content, &compressed);

    eprintln!("\n  [aggressive mode, Rust 200L] {orig} → {comp} tokens ({pct:.1}% saved)");
    assert!(
        pct > 5.0,
        "Aggressive on Rust should save >5%, got {pct:.1}%"
    );
}

#[test]
fn bench_aggressive_mode_python() {
    let content = generate_python_file(200);
    let compressed = aggressive_compress(&content, Some("py"));
    let (orig, comp, pct) = measure_text(&content, &compressed);

    eprintln!("\n  [aggressive mode, Python 200L] {orig} → {comp} tokens ({pct:.1}% saved)");
    assert!(
        pct > 5.0,
        "Aggressive on Python should save >5%, got {pct:.1}%"
    );
}

#[test]
fn bench_aggressive_mode_typescript() {
    let content = generate_typescript_file(200);
    let compressed = aggressive_compress(&content, Some("ts"));
    let (orig, comp, pct) = measure_text(&content, &compressed);

    eprintln!("\n  [aggressive mode, TypeScript 200L] {orig} → {comp} tokens ({pct:.1}% saved)");
    assert!(
        pct > 15.0,
        "Aggressive on TS should save >15%, got {pct:.1}%"
    );
}

#[test]
fn bench_entropy_mode_rust() {
    let content = generate_rust_file(200);
    let result = entropy_compress(&content);
    let (orig, comp, pct) = measure_text(&content, &result.output);

    eprintln!("\n  [entropy mode, Rust 200L] {orig} → {comp} tokens ({pct:.1}% saved)");
    eprintln!("  Techniques: {:?}", result.techniques);
    assert!(pct > 3.0, "Entropy on Rust should save >3%, got {pct:.1}%");
}

#[test]
fn bench_signatures_mode_rust() {
    let content = generate_rust_file(200);
    let sigs = extract_signatures(&content, "rs");

    let sig_output: String = sigs
        .iter()
        .map(|s| format!("{}\n", s.to_compact()))
        .collect();
    let (orig, comp, pct) = measure_text(&content, &sig_output);

    eprintln!(
        "\n  [signatures mode, Rust 200L] {orig} → {comp} tokens ({pct:.1}% saved), {} signatures",
        sigs.len()
    );
    assert!(
        pct > 50.0,
        "Signatures on Rust should save >50%, got {pct:.1}%"
    );
    assert!(
        sigs.len() > 5,
        "Should extract >5 signatures, got {}",
        sigs.len()
    );
}

#[test]
fn bench_signatures_mode_typescript() {
    let content = generate_typescript_file(200);
    let sigs = extract_signatures(&content, "ts");

    let sig_output: String = sigs
        .iter()
        .map(|s| format!("{}\n", s.to_compact()))
        .collect();
    let (orig, comp, pct) = measure_text(&content, &sig_output);

    eprintln!(
        "\n  [signatures mode, TS 200L] {orig} → {comp} tokens ({pct:.1}% saved), {} signatures",
        sigs.len()
    );
    assert!(
        pct > 40.0,
        "Signatures on TS should save >40%, got {pct:.1}%"
    );
}

#[test]
fn bench_lightweight_cleanup() {
    let content = generate_rust_file(200);
    let cleaned = lightweight_cleanup(&content);
    let (orig, comp, pct) = measure_text(&content, &cleaned);

    eprintln!("\n  [lightweight cleanup, Rust 200L] {orig} → {comp} tokens ({pct:.1}% saved)");
    assert!(comp <= orig, "Cleanup should never increase token count");
}

#[test]
fn bench_safeguard_prevents_over_compression() {
    let original = generate_rust_file(100);
    let over_compressed = "fn main() {}";
    let result = safeguard_ratio(&original, over_compressed);

    let orig_tokens = count_tokens(&original);
    let result_tokens = count_tokens(&result);
    let ratio = result_tokens as f64 / orig_tokens as f64;

    eprintln!(
        "\n  [safeguard] Original: {} tokens, Over-compressed: {} tokens, Result: {} tokens, Ratio: {:.2}",
        orig_tokens,
        count_tokens(over_compressed),
        result_tokens,
        ratio
    );
    assert!(
        ratio > 0.15,
        "Safeguard should prevent compression below 15%, ratio: {ratio:.2}"
    );
}

#[test]
fn bench_all_modes_comparison() {
    let content = generate_rust_file(300);
    let orig_tokens = count_tokens(&content);

    let aggressive = aggressive_compress(&content, Some("rs"));
    let entropy = entropy_compress(&content);
    let sigs = extract_signatures(&content, "rs");
    let sig_text: String = sigs
        .iter()
        .map(|s| format!("{}\n", s.to_compact()))
        .collect();
    let cleaned = lightweight_cleanup(&content);

    let modes = vec![
        ("full (baseline)", orig_tokens),
        ("aggressive", count_tokens(&aggressive)),
        ("entropy", count_tokens(&entropy.output)),
        ("signatures", count_tokens(&sig_text)),
        ("lightweight", count_tokens(&cleaned)),
    ];

    eprintln!("\n{}", "=".repeat(70));
    eprintln!("  ALL COMPRESSION MODES — Rust File (300 lines)");
    eprintln!("{}", "=".repeat(70));
    eprintln!("  {:<20} {:>8} {:>7}", "Mode", "Tokens", "Saved%");
    eprintln!("  {}", "-".repeat(40));

    for (name, tokens) in &modes {
        let pct = compression_ratio(orig_tokens, *tokens);
        eprintln!("  {:<20} {:>8} {:>6.1}%", name, tokens, pct);
    }
    eprintln!("{}", "=".repeat(70));
}

// ═══════════════════════════════════════════════════════════════════════════
// SECTION 4: OUTPUT TOKEN BENCHMARKS — Response Compression
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn bench_response_compression_off() {
    let verbose_response = generate_verbose_llm_response();
    let compressed = ctx_response::handle(&verbose_response, CrpMode::Off);
    let (orig, comp, pct) = measure_text(&verbose_response, &compressed);

    eprintln!("\n  [ctx_response CRP Off] {orig} → {comp} tokens ({pct:.1}% saved)");
    assert!(
        pct > 5.0,
        "Even Off mode should strip some filler, got {pct:.1}%"
    );
}

#[test]
fn bench_response_compression_compact() {
    let verbose_response = generate_verbose_llm_response();
    let compressed = ctx_response::handle(&verbose_response, CrpMode::Compact);
    let (orig, comp, pct) = measure_text(&verbose_response, &compressed);

    eprintln!("\n  [ctx_response CRP Compact] {orig} → {comp} tokens ({pct:.1}% saved)");
    assert!(pct > 10.0, "Compact mode should save >10%, got {pct:.1}%");
}

#[test]
fn bench_response_compression_tdd() {
    let verbose_response = generate_verbose_llm_response();
    let compressed = ctx_response::handle(&verbose_response, CrpMode::Tdd);
    let (orig, comp, pct) = measure_text(&verbose_response, &compressed);

    eprintln!("\n  [ctx_response CRP TDD] {orig} → {comp} tokens ({pct:.1}% saved)");
    assert!(pct > 15.0, "TDD mode should save >15%, got {pct:.1}%");
}

#[test]
fn bench_response_filler_removal_accuracy() {
    let response_with_fillers = "\
Sure, I'd be happy to help you with that! Let me take a look at this for you.\n\n\
Great question! Here's what I found after analyzing the codebase thoroughly:\n\n\
I'll now walk you through the changes step by step so you can understand each modification.\n\n\
First, let me explain how the authentication flow works in the current implementation.\n\n\
The function `process_data` in `src/handlers.rs` has a critical bug on line 42 where the \
validation logic doesn't properly handle edge cases with expired tokens.\n\n\
```rust\npub fn process_data(input: &str) -> Result<Output, Error> {\n    let validated = validate(input)?;\n    Ok(Output::new(validated))\n}\n```\n\n\
As you can see, the function currently doesn't check for token expiration before processing.\n\n\
I've also updated the tests to cover the new edge cases. The test file now includes comprehensive \
coverage for both valid and invalid token scenarios.\n\n\
```rust\n#[test]\nfn test_process_data_valid() {\n    let result = process_data(\"hello\");\n    assert!(result.is_ok());\n}\n\n\
#[test]\nfn test_process_data_expired() {\n    let result = process_data(\"expired_token\");\n    assert!(result.is_err());\n}\n```\n\n\
Moving on to the next part, I also refactored the configuration module to improve maintainability.\n\n\
I hope this helps! Let me know if you have any other questions or if you'd like me to make any \
additional changes to the implementation. Feel free to ask if anything is unclear.\n\n\
Don't hesitate to reach out if you need further assistance with this or any other issues.";

    let result_off = ctx_response::handle(response_with_fillers, CrpMode::Off);
    let result_tdd = ctx_response::handle(response_with_fillers, CrpMode::Tdd);

    let orig_tokens = count_tokens(response_with_fillers);
    let off_tokens = count_tokens(&result_off);
    let tdd_tokens = count_tokens(&result_tdd);

    let off_savings = compression_ratio(orig_tokens, off_tokens);
    let tdd_savings = compression_ratio(orig_tokens, tdd_tokens);

    eprintln!("\n  [filler removal in context]");
    eprintln!("    Original:    {} tokens", orig_tokens);
    eprintln!(
        "    CRP Off:     {} tokens ({:.1}% saved)",
        off_tokens, off_savings
    );
    eprintln!(
        "    CRP TDD:     {} tokens ({:.1}% saved)",
        tdd_tokens, tdd_savings
    );

    assert!(
        off_savings > 5.0,
        "Filler removal (Off mode) should save >5%, got {off_savings:.1}%"
    );
    assert!(
        tdd_tokens <= off_tokens,
        "TDD should compress at least as much as Off mode"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// SECTION 5: THINKING TOKEN BENCHMARKS — CRP/TDD Instruction Effectiveness
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn bench_thinking_reduction_cues_present() {
    let compact = lean_ctx::server::build_instructions_for_test(CrpMode::Compact);
    let tdd = lean_ctx::server::build_instructions_for_test(CrpMode::Tdd);

    assert!(
        compact.contains("THINK LESS") || compact.contains("Trust these summaries"),
        "Compact mode must contain thinking-reduction cue"
    );
    assert!(
        compact.contains("<=200 tokens") || compact.contains("≤200 tokens"),
        "Compact mode must contain token budget"
    );
    assert!(
        tdd.contains("THINK LESS") || tdd.contains("Trust compressed outputs"),
        "TDD mode must contain thinking-reduction cue"
    );
    assert!(
        tdd.contains("<=150 tokens") || tdd.contains("≤150 tokens"),
        "TDD mode must contain strict token budget"
    );
    assert!(
        tdd.contains("ZERO NARRATION"),
        "TDD mode must contain zero-narration rule"
    );

    eprintln!("\n  [thinking reduction] All cues present in CRP Compact and TDD modes ✓");
}

#[test]
fn bench_crp_mode_token_budgets() {
    let compact = lean_ctx::server::build_instructions_for_test(CrpMode::Compact);
    let tdd = lean_ctx::server::build_instructions_for_test(CrpMode::Tdd);

    let compact_budget_present =
        compact.contains("TARGET: <=200 tokens") || compact.contains("TARGET: ≤200 tokens");
    let tdd_budget_present =
        tdd.contains("TOKEN BUDGET: <=150 tokens") || tdd.contains("TOKEN BUDGET: ≤150 tokens");

    eprintln!("\n  [token budgets]");
    eprintln!(
        "    Compact ≤200 tokens budget: {}",
        if compact_budget_present { "✓" } else { "✗" }
    );
    eprintln!(
        "    TDD ≤150 tokens budget:     {}",
        if tdd_budget_present { "✓" } else { "✗" }
    );

    assert!(
        compact_budget_present,
        "Compact mode must specify ≤200 token budget"
    );
    assert!(
        tdd_budget_present,
        "TDD mode must specify ≤150 token budget"
    );
}

#[test]
fn bench_tdd_symbols_token_efficiency() {
    let prose = "I added a new function called process_data to the module handlers in file server.rs at line 42. \
                 Then I removed lines 10 through 15 from the file because they were no longer needed. \
                 I also modified the function validate_token and renamed it to verify_jwt for clarity. \
                 The operation completed successfully with no warnings or errors detected.";

    let tdd = "+F1:42 fn process_data(handlers)\n\
               -F1:10-15\n\
               ~F1:42 validate_token -> verify_jwt\n\
               ok 0w";

    let prose_tokens = count_tokens(prose);
    let tdd_tokens = count_tokens(tdd);
    let savings_pct = compression_ratio(prose_tokens, tdd_tokens);

    eprintln!(
        "\n  [TDD symbols] Prose: {} tokens → TDD: {} tokens ({:.1}% saved)",
        prose_tokens, tdd_tokens, savings_pct
    );
    eprintln!(
        "  Semantic density: {:.1}x more meaning per token",
        prose_tokens as f64 / tdd_tokens as f64
    );
    assert!(
        savings_pct > 40.0,
        "TDD notation should save >40% vs prose, got {savings_pct:.1}%"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// SECTION 6: PERFORMANCE BENCHMARKS — Latency and Throughput
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn bench_tokenizer_throughput() {
    let content = generate_rust_file(1000);

    let start = std::time::Instant::now();
    let iterations = 100;
    for _ in 0..iterations {
        let _ = count_tokens(&content);
    }
    let elapsed = start.elapsed();

    let per_call = elapsed / iterations;
    let tokens = count_tokens(&content);

    eprintln!(
        "\n  [tokenizer] {} tokens counted in {:?}/call ({} calls)",
        tokens, per_call, iterations
    );
    assert!(
        per_call.as_millis() < 50,
        "Tokenizer should be <50ms/call, got {:?}",
        per_call
    );
}

#[test]
fn bench_tokenizer_cache_hit() {
    let content = "fn main() { println!(\"Hello, world!\"); }";

    let _ = count_tokens(content);

    let start = std::time::Instant::now();
    let iterations = 10_000;
    for _ in 0..iterations {
        let _ = count_tokens(content);
    }
    let elapsed = start.elapsed();

    let per_call_ns = elapsed.as_nanos() / iterations as u128;
    eprintln!(
        "\n  [tokenizer cache] Cache hit: {}ns/call ({} calls)",
        per_call_ns, iterations
    );
    assert!(
        per_call_ns < 5_000,
        "Cache hit should be <5μs, got {}ns",
        per_call_ns
    );
}

#[test]
fn bench_pattern_compression_throughput() {
    let output = generate_git_log_patch(20);

    let start = std::time::Instant::now();
    let iterations = 50;
    for _ in 0..iterations {
        let _ = patterns::compress_output("git log -p", &output);
    }
    let elapsed = start.elapsed();

    let per_call = elapsed / iterations;
    eprintln!(
        "\n  [git pattern] {} bytes compressed in {:?}/call ({} calls)",
        output.len(),
        per_call,
        iterations
    );
    assert!(
        per_call.as_millis() < 20,
        "Git pattern compression should be <20ms/call, got {:?}",
        per_call
    );
}

#[test]
fn bench_aggressive_compression_throughput() {
    let content = generate_rust_file(500);

    let start = std::time::Instant::now();
    let iterations = 100;
    for _ in 0..iterations {
        let _ = aggressive_compress(&content, Some("rs"));
    }
    let elapsed = start.elapsed();

    let per_call = elapsed / iterations;
    eprintln!(
        "\n  [aggressive] 500-line Rust file in {:?}/call ({} calls)",
        per_call, iterations
    );
    assert!(
        per_call.as_millis() < 10,
        "Aggressive compression should be <10ms/call, got {:?}",
        per_call
    );
}

#[test]
fn bench_entropy_compression_throughput() {
    let content = generate_rust_file(200);

    let start = std::time::Instant::now();
    let iterations = 20;
    for _ in 0..iterations {
        let _ = entropy_compress(&content);
    }
    let elapsed = start.elapsed();

    let per_call = elapsed / iterations;
    eprintln!(
        "\n  [entropy] 200-line Rust file in {:?}/call ({} calls)",
        per_call, iterations
    );
    assert!(
        per_call.as_millis() < 100,
        "Entropy compression should be <100ms/call, got {:?}",
        per_call
    );
}

#[test]
fn bench_signatures_extraction_throughput() {
    let content = generate_rust_file(500);

    let start = std::time::Instant::now();
    let iterations = 50;
    for _ in 0..iterations {
        let _ = extract_signatures(&content, "rs");
    }
    let elapsed = start.elapsed();

    let per_call = elapsed / iterations;
    eprintln!(
        "\n  [signatures] 500-line Rust extraction in {:?}/call ({} calls)",
        per_call, iterations
    );
    assert!(
        per_call.as_millis() < 30,
        "Signature extraction should be <30ms/call, got {:?}",
        per_call
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// SECTION 7: REGRESSION GUARDS — Ensure no performance degradation
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn guard_no_inflation_aggressive() {
    for size in [50, 100, 200, 500] {
        let content = generate_rust_file(size);
        let compressed = aggressive_compress(&content, Some("rs"));
        let orig = count_tokens(&content);
        let comp = count_tokens(&compressed);
        assert!(
            comp <= orig,
            "Aggressive should never inflate! Size {size}: {orig} → {comp}"
        );
    }
    eprintln!("\n  [guard] Aggressive mode never inflates output ✓");
}

#[test]
fn guard_no_inflation_lightweight() {
    for size in [50, 100, 200, 500] {
        let content = generate_rust_file(size);
        let cleaned = lightweight_cleanup(&content);
        let orig = count_tokens(&content);
        let comp = count_tokens(&cleaned);
        assert!(
            comp <= orig,
            "Lightweight should never inflate! Size {size}: {orig} → {comp}"
        );
    }
    eprintln!("\n  [guard] Lightweight cleanup never inflates output ✓");
}

#[test]
fn guard_pattern_never_inflates() {
    let commands: Vec<(&str, String)> = vec![
        ("git log -p -5", generate_git_log_patch(5)),
        ("git status", generate_git_status()),
        ("cargo build", generate_cargo_build_success()),
        ("docker ps", generate_docker_ps(5)),
        ("npm install", generate_npm_install(10)),
    ];

    for (cmd, output) in &commands {
        if let Some(compressed) = lean_ctx::core::patterns::compress_output(cmd, output) {
            let orig = count_tokens(output);
            let comp = count_tokens(&compressed);
            assert!(
                comp <= orig,
                "Pattern '{cmd}' should never inflate: {orig} → {comp}"
            );
        }
    }
    eprintln!("\n  [guard] Pattern compression never inflates output ✓");
}

#[test]
fn guard_tool_descriptions_not_empty() {
    let descs = lean_ctx::server::tool_descriptions_for_test();
    for (name, desc) in &descs {
        assert!(
            !desc.is_empty(),
            "Tool '{name}' must have a non-empty description"
        );
        assert!(
            desc.len() > 10,
            "Tool '{name}' description too short ({} chars): '{desc}'",
            desc.len()
        );
    }
    eprintln!(
        "\n  [guard] All {} tools have meaningful descriptions ✓",
        descs.len()
    );
}

#[test]
fn guard_essential_instructions_present() {
    let instr = lean_ctx::server::build_instructions_for_test(CrpMode::Off);

    let required = vec![
        "NEVER use native Read",
        "ctx_read",
        "ctx_shell",
        "ctx_search",
        "ctx_tree",
        "CEP v1",
        "ACT FIRST",
        "DELTA ONLY",
        "ctx_overview",
        "ctx_compress",
        "ctx_session",
    ];

    for keyword in &required {
        assert!(
            instr.contains(keyword),
            "System instructions must contain '{keyword}'"
        );
    }
    eprintln!(
        "\n  [guard] All {} essential instruction keywords present ✓",
        required.len()
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// DATA GENERATORS
// ═══════════════════════════════════════════════════════════════════════════

fn print_compression_report(title: &str, scenarios: &[(&str, String, f64)]) {
    let mut total_orig = 0usize;
    let mut total_comp = 0usize;

    eprintln!("\n{}", "=".repeat(70));
    eprintln!("  {title}");
    eprintln!("{}", "=".repeat(70));
    eprintln!(
        "  {:<35} {:>8} {:>8} {:>7}",
        "Command", "Original", "Compr.", "Saved%"
    );
    eprintln!("  {}", "-".repeat(60));

    for (cmd, output, min_savings) in scenarios {
        let (orig, comp, pct) = measure_pattern(cmd, output);
        total_orig += orig;
        total_comp += comp;
        let status = if pct >= *min_savings { "✓" } else { "✗" };
        eprintln!(
            "  {:<35} {:>8} {:>8} {:>6.1}% {status}",
            cmd, orig, comp, pct
        );
        assert!(
            pct >= *min_savings,
            "'{cmd}' should save ≥{min_savings}%, got {pct:.1}%"
        );
    }

    let total_pct = compression_ratio(total_orig, total_comp);
    eprintln!("  {}", "-".repeat(60));
    eprintln!(
        "  {:<35} {:>8} {:>8} {:>6.1}%",
        "TOTAL", total_orig, total_comp, total_pct
    );
    eprintln!("{}", "=".repeat(70));
}

fn generate_rust_file(lines: usize) -> String {
    let mut s = String::new();
    s.push_str("use std::collections::HashMap;\nuse std::sync::Arc;\n\n");
    s.push_str("/// Main application struct that handles all processing\n");
    s.push_str("pub struct Application {\n    config: Config,\n    state: Arc<State>,\n}\n\n");

    let mut line = 9;
    while line < lines {
        let fn_idx = line / 15;
        s.push_str(&format!(
            "/// Process item {} with validation and error handling\n\
             pub fn process_item_{}(input: &str, config: &Config) -> Result<Output, Error> {{\n\
             \tlet validated = validate_input(input)?;\n\
             \tlet transformed = transform_data(&validated, config);\n\
             \t// Apply business rules\n\
             \tif transformed.is_empty() {{\n\
             \t\treturn Err(Error::EmptyInput);\n\
             \t}}\n\
             \tlet result = compute_result(&transformed)?;\n\
             \tOk(Output::new(result))\n\
             }}\n\n",
            fn_idx, fn_idx
        ));
        line += 13;
    }
    s
}

fn generate_python_file(lines: usize) -> String {
    let mut s = String::new();
    s.push_str("import os\nimport sys\nfrom typing import Dict, List, Optional\n\n");
    s.push_str("# Main service class for data processing\n");
    s.push_str("class DataService:\n    \"\"\"Service for processing data pipelines.\"\"\"\n\n");

    let mut line = 8;
    while line < lines {
        let fn_idx = line / 12;
        s.push_str(&format!(
            "    def process_batch_{fn_idx}(self, items: List[Dict], config: Optional[Dict] = None) -> List[Dict]:\n\
             \t\t\"\"\"Process batch {fn_idx} with validation.\"\"\"\n\
             \t\t# Validate inputs\n\
             \t\tvalidated = [self._validate(item) for item in items]\n\
             \t\tresults = []\n\
             \t\tfor item in validated:\n\
             \t\t\tif item is not None:\n\
             \t\t\t\tresults.append(self._transform(item))\n\
             \t\treturn results\n\n"
        ));
        line += 12;
    }
    s
}

fn generate_typescript_file(lines: usize) -> String {
    let mut s = String::new();
    s.push_str(
        "import { Request, Response } from 'express';\nimport { Database } from './db';\n\n",
    );
    s.push_str("// API controller for user management\n");
    s.push_str("export interface UserPayload {\n  name: string;\n  email: string;\n  role: 'admin' | 'user';\n}\n\n");

    let mut line = 9;
    while line < lines {
        let fn_idx = line / 14;
        s.push_str(&format!(
            "/**\n * Handle user request {fn_idx}\n */\n\
             export async function handleUser{fn_idx}(req: Request, res: Response): Promise<void> {{\n\
             \tconst {{ name, email }} = req.body as UserPayload;\n\
             \t// Validate the payload\n\
             \tif (!name || !email) {{\n\
             \t\tres.status(400).json({{ error: 'Missing fields' }});\n\
             \t\treturn;\n\
             \t}}\n\
             \tconst result = await Database.upsert({{ name, email }});\n\
             \tres.json({{ success: true, data: result }});\n\
             }}\n\n"
        ));
        line += 14;
    }
    s
}

fn generate_verbose_llm_response() -> String {
    "Sure, I'd be happy to help you with that! Let me take a look at the code.\n\n\
     Great question! Here's what I found after analyzing the codebase:\n\n\
     I'll now walk you through the changes step by step.\n\n\
     First, let me read the relevant files to understand the current state of things.\n\n\
     The function `process_data` in `src/handlers.rs` needs to be updated. Here are the changes:\n\n\
     ```rust\n\
     pub fn process_data(input: &str) -> Result<Output, Error> {\n\
         let validated = validate(input)?;\n\
         Ok(Output::new(validated))\n\
     }\n\
     ```\n\n\
     I've also updated the tests to cover the new edge cases. The test file now includes:\n\n\
     ```rust\n\
     #[test]\n\
     fn test_process_data() {\n\
         let result = process_data(\"hello\");\n\
         assert!(result.is_ok());\n\
     }\n\
     ```\n\n\
     I hope this helps! Let me know if you have any other questions or if you'd like me to make any additional changes.\n\n\
     Is there anything else I can assist you with?"
        .to_string()
}

fn generate_git_status() -> String {
    let mut s = String::from("On branch feature/new-auth\nYour branch is ahead of 'origin/feature/new-auth' by 3 commits.\n  (use \"git push\" to publish your local commits)\n\nChanges to be committed:\n  (use \"git restore --staged <file>...\" to unstage)\n");
    for i in 0..8 {
        s.push_str(&format!("\tmodified:   src/auth/handler_{i}.rs\n"));
    }
    s.push_str("\tnew file:   src/auth/middleware.rs\n\trenamed:    src/old.rs -> src/new.rs\n");
    s.push_str("\nChanges not staged for commit:\n  (use \"git add <file>...\" to update what will be committed)\n");
    for i in 0..5 {
        s.push_str(&format!("\tmodified:   tests/test_{i}.rs\n"));
    }
    s.push_str("\nUntracked files:\n  (use \"git add <file>...\" to include in what will be committed)\n\ttmp/debug.log\n\tnotes.md\n");
    s
}

fn generate_git_diff(files: usize) -> String {
    let mut s = String::new();
    for i in 0..files {
        s.push_str(&format!(
            "diff --git a/src/module_{i}.rs b/src/module_{i}.rs\n\
             index abc1234..def5678 100644\n\
             --- a/src/module_{i}.rs\n\
             +++ b/src/module_{i}.rs\n\
             @@ -10,8 +10,12 @@ fn existing_function() {{\n\
              \tlet x = 1;\n\
              \tlet y = 2;\n\
             -\tlet old_value = compute_old(x, y);\n\
             -\treturn old_value;\n\
             +\tlet new_value = compute_new(x, y);\n\
             +\t// Added validation\n\
             +\tif new_value > 0 {{\n\
             +\t\treturn Ok(new_value);\n\
             +\t}}\n\
             +\tErr(Error::Invalid)\n\
              }}\n\n"
        ));
    }
    s
}

fn generate_git_commit_with_hooks(hook_count: usize) -> String {
    let mut s = String::new();
    for i in 0..hook_count {
        s.push_str(&format!("check-{i:02}...........................passed\n"));
    }
    s.push_str("[feature/auth abc1234] feat: implement JWT authentication\n");
    s.push_str(" 8 files changed, 234 insertions(+), 45 deletions(-)\n");
    s.push_str(" create mode 100644 src/auth/jwt.rs\n");
    s.push_str(" create mode 100644 src/auth/middleware.rs\n");
    s
}

fn generate_git_push() -> String {
    "Enumerating objects: 15, done.\n\
     Counting objects: 100% (15/15), done.\n\
     Delta compression using up to 10 threads\n\
     Compressing objects: 100% (8/8), done.\n\
     Writing objects: 100% (9/9), 2.34 KiB | 2.34 MiB/s, done.\n\
     Total 9 (delta 5), reused 0 (delta 0), pack-reused 0\n\
     remote: Resolving deltas: 100% (5/5), completed with 3 local objects.\n\
     To github.com:user/repo.git\n\
        abc1234..def5678  main -> main\n"
        .to_string()
}

fn generate_cargo_build_success() -> String {
    "   Compiling serde v1.0.197\n\
     Compiling serde_json v1.0.115\n\
     Compiling tokio v1.37.0\n\
     Compiling my-app v0.1.0 (/home/user/project)\n\
     Finished `dev` profile [unoptimized + debuginfo] target(s) in 12.34s\n"
        .to_string()
}

fn generate_cargo_build_with_warnings(count: usize) -> String {
    let mut s = String::from("   Compiling my-app v0.1.0 (/home/user/project)\n");
    for i in 0..count {
        s.push_str(&format!(
            "warning: unused variable: `temp_{i}`\n\
              --> src/handlers/mod.rs:{}:9\n\
               |\n\
             {} |     let temp_{i} = calculate();\n\
               |         ^^^^^^^ help: if this is intentional, prefix it with an underscore: `_temp_{i}`\n\
               |\n",
            100 + i * 5,
            100 + i * 5
        ));
    }
    s.push_str(&format!(
        "warning: `my-app` (bin \"my-app\") generated {count} warnings\n\
         Finished `dev` profile [unoptimized + debuginfo] target(s) in 3.45s\n"
    ));
    s
}

fn generate_cargo_test(pass: usize, fail: usize) -> String {
    let mut s = String::from("\nrunning tests\n");
    for i in 0..pass {
        s.push_str(&format!("test tests::test_case_{i} ... ok\n"));
    }
    for i in 0..fail {
        s.push_str(&format!("test tests::test_failing_{i} ... FAILED\n"));
    }
    s.push_str(&format!(
        "\ntest result: {}. {} passed; {} failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.23s\n",
        if fail > 0 { "FAILED" } else { "ok" },
        pass,
        fail
    ));
    s
}

fn generate_cargo_clippy(count: usize) -> String {
    let mut s = String::from("    Checking my-app v0.1.0\n");
    for i in 0..count {
        s.push_str(&format!(
            "warning: redundant clone\n\
              --> src/handler_{i}.rs:{}:14\n\
               |\n\
             {} |     data.clone()\n\
               |          ^^^^^^^^ help: remove this\n\
               = note: `#[warn(clippy::redundant_clone)]` on by default\n\n",
            20 + i * 3,
            20 + i * 3
        ));
    }
    s.push_str(&format!(
        "warning: `my-app` (bin \"my-app\") generated {count} warnings\n"
    ));
    s
}

fn generate_docker_ps(count: usize) -> String {
    let mut s = String::from(
        "CONTAINER ID   IMAGE                    COMMAND                  CREATED        STATUS        PORTS                    NAMES\n"
    );
    for i in 0..count {
        s.push_str(&format!(
            "abc{i:04}def   nginx:1.25-alpine        \"nginx -g 'daemon of…\"   {} hours ago   Up {} hours   0.0.0.0:80{}->80/tcp     web-{i}\n",
            2 + i,
            2 + i,
            80 + i
        ));
    }
    s
}

fn generate_docker_images(count: usize) -> String {
    let mut s = String::from("REPOSITORY          TAG       IMAGE ID       CREATED        SIZE\n");
    for i in 0..count {
        s.push_str(&format!(
            "myapp-{i}            latest    sha256:abc{i:04}   {} days ago    {}MB\n",
            1 + i,
            50 + i * 30
        ));
    }
    s
}

fn generate_docker_build(steps: usize) -> String {
    let mut s = String::new();
    for i in 1..=steps {
        s.push_str(&format!(
            "[{i}/{steps}] RUN apt-get install -y package-{i} && rm -rf /var/lib/apt/lists/*\n"
        ));
        s.push_str(&format!(
            " ---> Running in abc{i:04}def\n\
              ---> sha256:abc{i:05}def\n"
        ));
    }
    s.push_str("Successfully built sha256:final123456\nSuccessfully tagged myapp:latest\n");
    s
}

fn generate_npm_install(count: usize) -> String {
    let mut s = String::new();
    for i in 0..count {
        s.push_str(&format!(
            "npm warn deprecated package-{i}@1.0.{i}: Use package-{i}@2 instead\n"
        ));
    }
    s.push_str(&format!(
        "\nadded {} packages, and audited {} packages in 5s\n\n\
         {} packages are looking for funding\n  run `npm fund` for details\n\n\
         found 0 vulnerabilities\n",
        count + 50,
        count + 100,
        count / 3
    ));
    s
}

fn generate_npm_test_jest() -> String {
    "PASS src/components/Button.test.tsx\n\
     PASS src/components/Form.test.tsx\n\
     PASS src/utils/api.test.ts\n\
     FAIL src/hooks/useAuth.test.ts\n\
       ● useAuth › should handle token refresh\n\
     \n\
     Test Suites: 1 failed, 3 passed, 4 total\n\
     Tests:       1 failed, 15 passed, 16 total\n\
     Snapshots:   0 total\n\
     Time:        3.456 s\n\
     Ran all test suites.\n"
        .to_string()
}

fn generate_npm_ls(count: usize) -> String {
    let mut s = String::from("my-app@1.0.0 /home/user/project\n");
    for i in 0..count {
        s.push_str(&format!("├── package-{i}@{}.{}.0\n", 1 + i % 5, i % 10));
    }
    s.push_str("└── last-package@1.0.0\n");
    s
}

fn generate_pip_install(count: usize) -> String {
    let mut s = String::new();
    for i in 0..count {
        s.push_str(&format!(
            "Collecting package-{i}>=1.0\n\
             \tDownloading package_{i}-1.{i}.0-cp311-cp311-manylinux_2_17_x86_64.whl ({}kB)\n",
            500 + i * 100
        ));
    }
    s.push_str(&format!(
        "Successfully installed {}\n",
        (0..count)
            .map(|i| format!("package-{i}-1.{i}.0"))
            .collect::<Vec<_>>()
            .join(" ")
    ));
    s
}

fn generate_pip_list(count: usize) -> String {
    let mut s = String::from("Package          Version\n---------------- -------\n");
    for i in 0..count {
        s.push_str(&format!("package-{i:<8} {}.{}.0\n", 1 + i % 5, i % 10));
    }
    s
}

fn generate_kubectl_pods(count: usize) -> String {
    let mut s =
        String::from("NAME                              READY   STATUS    RESTARTS   AGE\n");
    for i in 0..count {
        let status = match i % 5 {
            0 => "Running",
            1 => "Running",
            2 => "Running",
            3 => "Pending",
            _ => "CrashLoopBackOff",
        };
        s.push_str(&format!(
            "app-{i}-deployment-abc{i:04}def   1/1     {:<20} {}          {}d\n",
            status,
            i % 3,
            1 + i
        ));
    }
    s
}

fn generate_kubectl_pods_all(count: usize) -> String {
    let mut s = String::from(
        "NAMESPACE     NAME                              READY   STATUS    RESTARTS   AGE\n",
    );
    let namespaces = ["default", "kube-system", "monitoring", "app-prod"];
    for i in 0..count {
        let ns = namespaces[i % namespaces.len()];
        s.push_str(&format!(
            "{:<13} pod-{i}-abc{i:04}def                1/1     Running   0          {}d\n",
            ns,
            1 + i
        ));
    }
    s
}

fn generate_git_log_patch(n: usize) -> String {
    let mut output = String::new();
    for i in 0..n {
        let msg = format!("implement feature number {i} with improvements");
        output.push_str(&format!(
            "commit {i:07}abc1234567890abcdef1234567890\n\
             Author: Developer <dev@example.com>\n\
             Date:   Mon Mar {d} 10:{h:02}:00 2026 +0100\n\
             \n\
             {ty}: {msg}\n\
             \n\
             diff --git a/src/module_{i}.rs b/src/module_{i}.rs\n\
             index abc1234..def5678 100644\n\
             --- a/src/module_{i}.rs\n\
             +++ b/src/module_{i}.rs\n\
             @@ -1,{ctx} +1,{ctx2} @@\n",
            d = 10 + (i % 20),
            h = i % 24,
            ty = ["feat", "fix", "refactor", "docs", "test"][i % 5],
            ctx = 10 + i,
            ctx2 = 12 + i,
        ));
        for j in 0..(5 + i % 10) {
            output.push_str(&format!(" fn existing_function_{j}() {{}}\n"));
        }
        for j in 0..(3 + i % 5) {
            output.push_str(&format!("+fn new_function_{i}_{j}() {{ todo!() }}\n"));
        }
        for j in 0..(2 + i % 3) {
            output.push_str(&format!(
                "-fn old_function_{i}_{j}() {{ unimplemented!() }}\n"
            ));
        }
        output.push('\n');
    }
    output
}

fn generate_git_log_standard(n: usize) -> String {
    let mut output = String::new();
    for i in 0..n {
        let msg = format!("update component {i} with better error handling");
        output.push_str(&format!(
            "commit {i:07}abc1234567890abcdef1234567890\n\
             Author: Developer <dev@example.com>\n\
             Date:   Mon Mar {d} 10:{h:02}:00 2026 +0100\n\
             \n\
             {ty}: {msg}\n\
             \n\
             This is a longer commit message that describes the changes made.\n\
             It spans multiple lines to be more realistic.\n\
             \n",
            d = 10 + (i % 20),
            h = i % 24,
            ty = ["feat", "fix", "refactor", "docs", "test"][i % 5],
        ));
    }
    output
}

fn generate_git_log_stat(n: usize) -> String {
    let mut output = String::new();
    for i in 0..n {
        let msg = format!("update module {i}");
        output.push_str(&format!(
            "commit {i:07}abc1234567890abcdef1234567890\n\
             Author: Developer <dev@example.com>\n\
             Date:   Mon Mar {d} 10:{h:02}:00 2026 +0100\n\
             \n\
             {ty}: {msg}\n\
             \n",
            d = 10 + (i % 20),
            h = i % 24,
            ty = ["feat", "fix", "refactor", "docs", "test"][i % 5],
        ));
        for j in 0..(2 + i % 4) {
            let plus = 3 + j;
            let minus = 1 + j;
            let bar: String = "+".repeat(plus) + &"-".repeat(minus);
            output.push_str(&format!(
                " src/mod_{i}_{j}.rs | {total} {bar}\n",
                total = plus + minus,
            ));
        }
        let total_files = 2 + i % 4;
        output.push_str(&format!(
            " {total_files} files changed, {} insertions(+), {} deletions(-)\n\n",
            10 + i * 2,
            5 + i,
        ));
    }
    output
}
