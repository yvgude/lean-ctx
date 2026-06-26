use lean_ctx::core::compressor::{aggressive_compress, lightweight_cleanup, safeguard_ratio};
use lean_ctx::core::entropy::entropy_compress;
use lean_ctx::core::protocol::instruction_decoder_block;
use lean_ctx::core::signatures::extract_signatures;
use lean_ctx::core::tokens::count_tokens;
use lean_ctx::tools::CrpMode;
use lean_ctx::tools::ctx_response;

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
    let compact_overhead = (tok_compact as i64) - (tok_off as i64);
    let tdd_overhead = (tok_tdd as i64) - (tok_off as i64);
    eprintln!("  Compact overhead: {compact_overhead:+} tokens vs Off");
    eprintln!("  TDD overhead:     {tdd_overhead:+} tokens vs Off");
    eprintln!("{}", "=".repeat(70));

    assert!(
        tok_off < 2300,
        "Base instructions should be <2300 tokens, got {tok_off}"
    );
    assert!(
        tok_compact < 2450,
        "Compact instructions should be <2450 tokens, got {tok_compact}"
    );
    assert!(
        tok_tdd < 2550,
        "TDD instructions should be <2550 tokens, got {tok_tdd}"
    );

    // The <=2048 char budget governs the STATIC cold first-contact handshake
    // instructions. A live build also appends dynamic session/knowledge/gotcha
    // payload, but that is capped by INSTRUCTION_CAP_TOKENS (token budget), not
    // this char budget — and it varies with whatever session is persisted on the
    // runner. Measuring the static surface keeps the assertion deterministic
    // (#498) instead of order/state-dependent.
    let claude_code_instr = lean_ctx::server::build_claude_code_static_instructions_for_test();
    let claude_chars = claude_code_instr.len();
    let claude_tokens = count_tokens(&claude_code_instr);
    eprintln!("  Claude Code (static): {claude_tokens:>6} tokens ({claude_chars:>5} chars)");
    assert!(
        claude_chars <= 2048,
        "Claude Code static instructions MUST be <=2048 chars, got {claude_chars}"
    );
    assert!(
        compact_overhead.unsigned_abs() < 300,
        "Compact mode overhead should be <300 tokens, got {compact_overhead}"
    );
    assert!(
        tdd_overhead.unsigned_abs() < 650,
        "TDD mode overhead should be <650 tokens, got {tdd_overhead}"
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

    // Budget = the real registry tool surface (single source of truth, #141):
    // every tool with its full `McpTool::tool_def()` description — exactly what
    // the live server advertises in full mode. Set with deliberate headroom over
    // the current actual so adding a tool or two never blocks CI; only raise it
    // again on a material jump in the surface, not on routine additions (#290).
    // Raised 3000 -> 4000 for the #496 tool-profile reorg: a material jump that
    // enriches per-tool profile metadata on the full opt-in surface. The default
    // surface stays small (see `bench_lazy_default_vs_full_overhead`).
    // Raised 4000 -> 5200 for #505 (@omar-mohamed-khallaf): the power tier now
    // carries the same first-line-dense, workflow-first treatment as the other
    // tiers (actual ~4872), with headroom for a tool or two per #290.
    // Raised 5200 -> 5600 for #510 (@omar-mohamed-khallaf): the optimized tools
    // schema adds explicit WORKFLOW:/ANTIPATTERN: intent lines across the surface
    // (actual ~5498). Validated by the output-quality gate (eval A/B: NO
    // REGRESSION, Δ +0.039) — the richer intents earn their tokens. The per-
    // request cost is unchanged (core surface stays at ~2272, see
    // `core_tool_surface_stays_within_budget`); this full-surface total only
    // applies in opt-in full mode. Cutting it further is #509 (reduce tool
    // COUNT), not trimming these eval-validated descriptions.
    assert!(
        total < 5600,
        "Total tool description tokens should be <5600, got {total}"
    );

    for (name, desc) in &descriptions {
        // Every tool — including the ctx_call invoker — stays within a tight
        // per-tool description budget. ctx_call no longer embeds the full
        // non-core catalog; agents discover callable tools via ctx_discover_tools
        // (#680), so no tool is exempt from this ceiling.
        let t = count_tokens(desc);
        assert!(
            t < 160,
            "Tool '{name}' description should be <160 tokens, got {t}"
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
    eprintln!("  System instructions: {instr_tokens:>6} tokens");
    eprintln!("  Tool descriptions:   {desc_tokens:>6} tokens");
    eprintln!("  Tool schemas (JSON): {schema_tokens:>6} tokens");
    eprintln!("  {}", "-".repeat(40));
    eprintln!("  TOTAL overhead:      {total:>6} tokens");
    eprintln!(
        "  Estimated cost @$3/1M input: ${:.4}",
        total as f64 * 3.0 / 1_000_000.0
    );
    eprintln!("{}", "=".repeat(70));

    // Full tool surface (registry SSOT incl. full property schemas) — the
    // worst-case opt-in overhead. The default lazy surface is far smaller; see
    // `bench_lazy_default_vs_full_overhead` (#141). Set with deliberate headroom
    // over the current actual so routine tool additions do not trip CI (#290).
    // Raised 12000 -> 13000 for the #496 tool-profile reorg (material jump in
    // the full opt-in surface); the lazy default users actually pay is unaffected.
    // Raised 13000 -> 14000 for #510 (@omar-mohamed-khallaf): the optimized tools
    // schema enriches per-parameter descriptions + WORKFLOW/ANTIPATTERN intents
    // (actual ~13707: instr ~457 + desc ~5498 + schemas ~7752). Output-quality
    // gate confirms NO REGRESSION (eval A/B Δ +0.039). The lazy default surface
    // (bench_lazy_default_vs_full_overhead) is unaffected; cutting the full-
    // surface total is #509 (reduce tool COUNT).
    assert!(
        total < 14000,
        "Total input overhead should be <14000 tokens, got {total}"
    );
}

#[test]
fn bench_lazy_default_vs_full_overhead() {
    // This benchmark must be hermetic: instructions can inject session/memory blocks
    // unless minimal overhead is enforced and the data dir is isolated.
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    let tmp = tempfile::tempdir().expect("tempdir");

    let prev_data_dir = std::env::var("LEAN_CTX_DATA_DIR").ok();
    let prev_minimal = std::env::var("LEAN_CTX_MINIMAL").ok();

    // TODO: Audit that the environment access only happens in single-threaded code.
    unsafe { std::env::set_var("LEAN_CTX_DATA_DIR", tmp.path()) };
    // TODO: Audit that the environment access only happens in single-threaded code.
    unsafe { std::env::set_var("LEAN_CTX_MINIMAL", "1") };

    let lazy_tools = lean_ctx::tool_defs::lazy_tool_defs();
    let full_tools = lean_ctx::tool_defs::granular_tool_defs();

    let tool_tokens = |tools: &[rmcp::model::Tool]| -> (usize, usize) {
        let desc: usize = tools
            .iter()
            .map(|t| {
                t.description
                    .as_ref()
                    .map_or(0, |d| count_tokens(d.as_ref()))
            })
            .sum();
        let schema: usize = tools
            .iter()
            .map(|t| count_tokens(&serde_json::to_string(&t.input_schema).unwrap_or_default()))
            .sum();
        (desc, schema)
    };

    let (lazy_desc_tokens, lazy_schema_tokens) = tool_tokens(&lazy_tools);
    let lazy_total = lazy_desc_tokens + lazy_schema_tokens;

    let (full_desc_tokens, full_schema_tokens) = tool_tokens(&full_tools);
    let full_total = full_desc_tokens + full_schema_tokens;
    let _ = (full_desc_tokens, full_schema_tokens);

    let instructions = lean_ctx::server::build_instructions_for_test(CrpMode::Off);
    let instr_tokens = count_tokens(&instructions);

    let lazy_user_overhead = instr_tokens + lazy_total;
    let full_user_overhead = instr_tokens + full_total;
    let reduction_pct = (full_total - lazy_total) as f64 / full_total as f64 * 100.0;

    eprintln!("\n{}", "=".repeat(70));
    eprintln!("  LAZY (DEFAULT) vs FULL TOOL OVERHEAD");
    eprintln!("{}", "=".repeat(70));
    eprintln!(
        "  Lazy tools:   {:>3} tools, {:>5} tokens (desc+schema)",
        lazy_tools.len(),
        lazy_total
    );
    eprintln!(
        "  Full tools:   {:>3} tools, {:>5} tokens (desc+schema)",
        full_tools.len(),
        full_total
    );
    eprintln!("  Instructions:          {instr_tokens:>5} tokens");
    eprintln!("  {}", "-".repeat(50));
    eprintln!("  User overhead (LAZY DEFAULT):  {lazy_user_overhead:>5} tokens");
    eprintln!("  User overhead (FULL opt-in):   {full_user_overhead:>5} tokens");
    eprintln!("  Tool token reduction:          {reduction_pct:>5.1}%");
    eprintln!("{}", "=".repeat(70));

    // Lazy mode exposes exactly the curated core set. Bound it to the canonical
    // CORE_TOOL_NAMES length so it tracks the SSOT instead of a magic number
    // (#141: the prior `<=12` reflected a granular list that had drifted to omit
    // one core tool).
    assert!(
        lazy_tools.len() <= lean_ctx::tool_defs::core_tool_names().len(),
        "Lazy mode should expose <= core_tool_names() ({}), got {}",
        lean_ctx::tool_defs::core_tool_names().len(),
        lazy_tools.len()
    );
    // Real default overhead: core tools carry their full registry schemas
    // (uncompressed) plus descriptions, matching what the live server sends
    // (#141).
    assert!(
        lazy_user_overhead < 3800,
        "Lazy default overhead should be <3800 tokens, got {lazy_user_overhead}"
    );
    assert!(
        reduction_pct > 60.0,
        "Tool token reduction should be >60%, got {reduction_pct:.1}%"
    );

    match prev_data_dir {
        // TODO: Audit that the environment access only happens in single-threaded code.
        Some(v) => unsafe { std::env::set_var("LEAN_CTX_DATA_DIR", v) },
        // TODO: Audit that the environment access only happens in single-threaded code.
        None => unsafe { std::env::remove_var("LEAN_CTX_DATA_DIR") },
    }
    match prev_minimal {
        // TODO: Audit that the environment access only happens in single-threaded code.
        Some(v) => unsafe { std::env::set_var("LEAN_CTX_MINIMAL", v) },
        // TODO: Audit that the environment access only happens in single-threaded code.
        None => unsafe { std::env::remove_var("LEAN_CTX_MINIMAL") },
    }
}

#[test]
fn bench_decoder_block_token_count() {
    let block = instruction_decoder_block(false);
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
        ("git diff", generate_git_diff(15), 5.0),
        (
            "git commit -m 'feat'",
            generate_git_commit_with_hooks(30),
            0.0, // verbatim: git write-commands are never compressed (daviddatu_ fix)
        ),
        ("git push origin main", generate_git_push(), 0.0), // verbatim
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
        ("docker ps", generate_docker_ps(10), 0.0), // Verbatim via OutputPolicy
        ("docker images", generate_docker_images(15), 0.0), // Verbatim via OutputPolicy
        ("docker build -t app .", generate_docker_build(20), 40.0),
    ];

    print_compression_report("DOCKER COMMANDS", &scenarios);
}

#[test]
fn bench_npm_all_commands() {
    let scenarios = vec![
        ("npm install", generate_npm_install(30), 0.0), // Verbatim via OutputPolicy (npm install -> Passthrough)
        ("npm test", generate_npm_test_jest(), 30.0),
        ("npm ls", generate_npm_ls(20), 0.0), // Verbatim via OutputPolicy (is_package_manager_info)
    ];

    print_compression_report("NPM COMMANDS", &scenarios);
}

#[test]
fn bench_pip_commands() {
    let scenarios = vec![
        (
            "pip install -r requirements.txt",
            generate_pip_install(15),
            0.0, // Verbatim via OutputPolicy (is_package_manager_info)
        ),
        ("pip list", generate_pip_list(30), 0.0), // Verbatim via OutputPolicy (is_package_manager_info)
    ];

    print_compression_report("PIP COMMANDS", &scenarios);
}

#[test]
fn bench_kubectl_commands() {
    let scenarios = vec![
        ("kubectl get pods", generate_kubectl_pods(20), 0.0),
        ("kubectl get pods -A", generate_kubectl_pods_all(30), 0.0),
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
        eprintln!("  {cmd:<40} {orig:>8} {comp:>8} {pct:>6.1}%");
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

    let sig_output = sigs.iter().fold(String::new(), |mut s, sig| {
        use std::fmt::Write;
        let _ = writeln!(s, "{}", sig.to_compact());
        s
    });
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

    let sig_output = sigs.iter().fold(String::new(), |mut s, sig| {
        use std::fmt::Write;
        let _ = writeln!(s, "{}", sig.to_compact());
        s
    });
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
        ratio > 0.05,
        "Safeguard should prevent extreme compression on small outputs, ratio: {ratio:.2}"
    );
}

#[test]
fn bench_all_modes_comparison() {
    let content = generate_rust_file(300);
    let orig_tokens = count_tokens(&content);

    let aggressive = aggressive_compress(&content, Some("rs"));
    let entropy = entropy_compress(&content);
    let sigs = extract_signatures(&content, "rs");
    let sig_text = sigs.iter().fold(String::new(), |mut s, sig| {
        use std::fmt::Write;
        let _ = writeln!(s, "{}", sig.to_compact());
        s
    });
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
        eprintln!("  {name:<20} {tokens:>8} {pct:>6.1}%");
    }
    eprintln!("{}", "=".repeat(70));
}

// ═══════════════════════════════════════════════════════════════════════════
// SECTION 3.5: RRF EVICTION SCORING
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn bench_rrf_eviction_vs_legacy() {
    use std::time::{Duration, Instant};

    let now = Instant::now();
    let keys: Vec<String> = (0..10).map(|i| format!("file_{i}.rs")).collect();
    let entries: Vec<lean_ctx::core::cache::CacheEntry> = (0..10)
        .map(|i| {
            let e = lean_ctx::core::cache::CacheEntry::new(
                &format!("content_{i}"),
                format!("hash_{i}"),
                i + 1,
                (i + 1) * 100,
                format!("/file_{i}.rs"),
                None,
            );
            e.set_read_count((10 - i) as u32);
            e.set_last_access(
                now.checked_sub(Duration::from_secs(i as u64))
                    .unwrap_or(now),
            );
            e
        })
        .collect();

    let entry_refs: Vec<(&String, &lean_ctx::core::cache::CacheEntry)> =
        keys.iter().zip(entries.iter()).collect();

    let rrf_scores = lean_ctx::core::cache::eviction_scores_rrf(&entry_refs, now);
    assert_eq!(rrf_scores.len(), 10);

    let mut legacy_scores: Vec<(String, f64)> = entries
        .iter()
        .map(|e| (e.path.clone(), e.eviction_score_legacy(now)))
        .collect();
    let mut rrf_sorted: Vec<(String, f64)> = rrf_scores;
    rrf_sorted.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
    legacy_scores.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());

    eprintln!("\n{}", "=".repeat(70));
    eprintln!("  RRF vs LEGACY EVICTION SCORING (10 entries)");
    eprintln!("{}", "=".repeat(70));
    eprintln!(
        "  {:>15} {:>12} {:>12}",
        "File", "RRF Score", "Legacy Score"
    );
    for i in 0..rrf_sorted.len().min(5) {
        let rrf_path = &rrf_sorted[i].0;
        let legacy_path = &legacy_scores[i].0;
        eprintln!(
            "  RRF#{}: {:>8} {:.6}  | Legacy#{}: {:>8} {:.6}",
            i + 1,
            rrf_path,
            rrf_sorted[i].1,
            i + 1,
            legacy_path,
            legacy_scores[i].1
        );
    }
    eprintln!("{}", "=".repeat(70));

    assert!(
        rrf_sorted[0].1 > rrf_sorted[9].1,
        "RRF: highest-scoring entry must rank above lowest"
    );
}

#[test]
fn bench_rrf_eviction_handles_single_entry() {
    use std::time::Instant;

    let now = Instant::now();
    let key = "solo.rs".to_string();
    let entry = lean_ctx::core::cache::CacheEntry::new(
        "single",
        "h".to_string(),
        1,
        50,
        "/solo.rs".to_string(),
        None,
    );

    let refs: Vec<(&String, &lean_ctx::core::cache::CacheEntry)> = vec![(&key, &entry)];
    let scores = lean_ctx::core::cache::eviction_scores_rrf(&refs, now);
    assert_eq!(scores.len(), 1);
    assert!(
        scores[0].1 > 0.0,
        "single entry must have positive RRF score"
    );
}

#[test]
fn bench_rrf_eviction_empty() {
    use std::time::Instant;
    let now = Instant::now();
    let scores = lean_ctx::core::cache::eviction_scores_rrf(&[], now);
    assert!(scores.is_empty());
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
    eprintln!("    Original:    {orig_tokens} tokens");
    eprintln!("    CRP Off:     {off_tokens} tokens ({off_savings:.1}% saved)");
    eprintln!("    CRP TDD:     {tdd_tokens} tokens ({tdd_savings:.1}% saved)");

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

    // #579 condensed the instruction skeleton: efficiency cues now live in the
    // one-line `CRP MODE:` suffix (lowercase), not a standalone block.
    let compact_lc = compact.to_lowercase();
    let tdd_lc = tdd.to_lowercase();
    assert!(
        compact_lc.contains("trust tool outputs") || compact_lc.contains("output efficiency"),
        "Compact mode must contain output efficiency cue"
    );
    assert!(
        compact.contains("<=200 tok")
            || compact.contains("<=200 tokens")
            || compact.contains("≤200"),
        "Compact mode must contain token budget"
    );
    assert!(
        tdd_lc.contains("max density") || tdd_lc.contains("output efficiency"),
        "TDD mode must contain output efficiency cue"
    );
    assert!(
        tdd.contains("<=150 tok") || tdd.contains("<=150 tokens") || tdd.contains("≤150"),
        "TDD mode must contain strict token budget"
    );
    assert!(
        tdd_lc.contains("zero narration"),
        "TDD mode must contain zero-narration rule"
    );

    eprintln!("\n  [output efficiency] All cues present in CRP Compact and TDD modes ✓");
}

#[test]
fn bench_crp_mode_token_budgets() {
    let compact = lean_ctx::server::build_instructions_for_test(CrpMode::Compact);
    let tdd = lean_ctx::server::build_instructions_for_test(CrpMode::Tdd);

    let compact_budget_present = compact.contains("<=200 tok")
        || compact.contains("TARGET: <=200 tokens")
        || compact.contains("≤200");
    let tdd_budget_present = tdd.contains("<=150 tok")
        || tdd.contains("TOKEN BUDGET: <=150 tokens")
        || tdd.contains("BUDGET: <=150");

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
        "\n  [TDD symbols] Prose: {prose_tokens} tokens → TDD: {tdd_tokens} tokens ({savings_pct:.1}% saved)"
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

// SECTION 6: PERFORMANCE BENCHMARKS removed — machine-dependent timing
// thresholds are unsuitable for OSS CI. Use `lean-ctx benchmark` CLI instead.

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

    // #579: the static skeleton is capped at ≤400 tokens — workflow tools
    // (ctx_overview, ctx_compress) moved to the on-demand LEAN-CTX.md doc.
    // #496: the per-session skeleton (Bare profile) stays lean — it anchors the
    // mandatory tool mapping + the intent playbook. The CEP protocol, AUTO hints
    // and the LEAN-CTX.md reference now live in the dedicated rule file /
    // CLAUDE.md (FULL profile) that the agent loads alongside, keeping the
    // worst-case per-session MCP instructions under the 2048-char Claude cap.
    let required = vec![
        "ALWAYS use lean-ctx tools",
        "ctx_read",
        "ctx_shell",
        "ctx_search",
        "ctx_tree",
        "ctx_compose",
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

// AUTONOMY TOKEN IMPACT removed — writes to ~/.lean-ctx/ (not hermetic for CI)

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
        eprintln!("  {cmd:<35} {orig:>8} {comp:>8} {pct:>6.1}% {status}");
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
            "/// Process item {fn_idx} with validation and error handling\n\
             pub fn process_item_{fn_idx}(input: &str, config: &Config) -> Result<Output, Error> {{\n\
             \tlet validated = validate_input(input)?;\n\
             \tlet transformed = transform_data(&validated, config);\n\
             \t// Apply business rules\n\
             \tif transformed.is_empty() {{\n\
             \t\treturn Err(Error::EmptyInput);\n\
             \t}}\n\
             \tlet result = compute_result(&transformed)?;\n\
             \tOk(Output::new(result))\n\
             }}\n\n"
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
    let mut s = String::from(
        "On branch feature/new-auth\nYour branch is ahead of 'origin/feature/new-auth' by 3 commits.\n  (use \"git push\" to publish your local commits)\n\nChanges to be committed:\n  (use \"git restore --staged <file>...\" to unstage)\n",
    );
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
        "CONTAINER ID   IMAGE                    COMMAND                  CREATED        STATUS        PORTS                    NAMES\n",
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
            0..=2 => "Running",
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

// ---------------------------------------------------------------------------
// v3.4.1 Optimization benchmarks: Token overhead + Latency
// ---------------------------------------------------------------------------

#[test]
fn bench_minimal_overhead_suppresses_all_meta_strings() {
    // LEAN_CTX_MINIMAL is process-global; serialize against the other instruction
    // benchmarks (cf. bench_lazy_default_vs_full_overhead) so a parallel test can't
    // clear it between set_var and build_instructions_for_test.
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    // TODO: Audit that the environment access only happens in single-threaded code.
    unsafe { std::env::set_var("LEAN_CTX_MINIMAL", "1") };

    let instructions = lean_ctx::server::build_instructions_for_test(CrpMode::Tdd);
    let instr_tokens = count_tokens(&instructions);

    eprintln!("\n{}", "=".repeat(70));
    eprintln!("  MINIMAL OVERHEAD: Instructions Token Count");
    eprintln!("{}", "=".repeat(70));
    eprintln!("  Instructions (minimal):  {instr_tokens:>5} tokens");

    assert!(
        !instructions.contains("--- ACTIVE SESSION"),
        "minimal_overhead should suppress session block"
    );

    // TODO: Audit that the environment access only happens in single-threaded code.
    unsafe { std::env::remove_var("LEAN_CTX_MINIMAL") };

    let full_instructions = lean_ctx::server::build_instructions_for_test(CrpMode::Tdd);
    let full_tokens = count_tokens(&full_instructions);

    eprintln!("  Instructions (full):     {full_tokens:>5} tokens");
    let saved = full_tokens.saturating_sub(instr_tokens);
    eprintln!("  Tokens saved by minimal: {saved:>5}");
    eprintln!("{}", "=".repeat(70));

    assert!(
        instr_tokens <= full_tokens,
        "minimal instructions ({instr_tokens}) should be <= full ({full_tokens})"
    );
}

#[test]
fn bench_hash_fast_vs_full_correctness() {
    use lean_ctx::core::hasher::hash_str;
    use lean_ctx::server::helpers::hash_fast;

    let small = "a".repeat(1000);
    assert_eq!(
        hash_str(&small),
        hash_fast(&small),
        "hash_fast must match hash_str for small strings"
    );

    let exactly_16k = "x".repeat(16 * 1024);
    assert_eq!(
        hash_str(&exactly_16k),
        hash_fast(&exactly_16k),
        "hash_fast must match hash_str at the 16KB boundary"
    );

    let large = "b".repeat(100_000);
    let fast_hash = hash_fast(&large);
    assert_eq!(
        fast_hash.len(),
        64,
        "hash_fast should produce valid BLAKE3 hex"
    );
    assert_ne!(
        fast_hash,
        hash_fast(&"c".repeat(100_000)),
        "different large strings should produce different hashes"
    );

    eprintln!("\n{}", "=".repeat(70));
    eprintln!("  BLAKE3 FAST FINGERPRINT");
    eprintln!("{}", "=".repeat(70));
    let start_full = std::time::Instant::now();
    for _ in 0..100 {
        let _ = hash_str(&large);
    }
    let full_us = start_full.elapsed().as_micros();

    let start_fast = std::time::Instant::now();
    for _ in 0..100 {
        let _ = hash_fast(&large);
    }
    let fast_us = start_fast.elapsed().as_micros();

    let speedup = full_us as f64 / fast_us.max(1) as f64;
    eprintln!("  100x hash_str(100KB):     {full_us:>6} us");
    eprintln!("  100x hash_fast(100KB):    {fast_us:>6} us");
    eprintln!("  Speedup:                  {speedup:>6.1}x");
    eprintln!("{}", "=".repeat(70));

    assert!(
        speedup > 1.5,
        "hash_fast should be faster for 100KB, got {speedup:.1}x"
    );
}

#[test]
fn bench_terse_pipeline_compression() {
    use lean_ctx::core::config::CompressionLevel;
    use lean_ctx::core::terse;

    let input = "fn main() {\n    println!(\"hello\");\n}\n".repeat(500);

    let start_standard = std::time::Instant::now();
    for _ in 0..100 {
        let _ = terse::pipeline::compress(&input, &CompressionLevel::Standard, None);
    }
    let standard_us = start_standard.elapsed().as_micros();

    let start_off = std::time::Instant::now();
    for _ in 0..100 {
        let result = terse::pipeline::compress(&input, &CompressionLevel::Off, None);
        assert_eq!(result.output.len(), input.len());
    }
    let off_us = start_off.elapsed().as_micros();

    eprintln!("\n{}", "=".repeat(70));
    eprintln!("  TERSE PIPELINE BENCHMARK (100 iters, ~20KB input)");
    eprintln!("{}", "=".repeat(70));
    eprintln!("  Off (passthrough):  {off_us:>6} us");
    eprintln!("  Standard (4-layer): {standard_us:>6} us");
    eprintln!("{}", "=".repeat(70));
}

#[test]
fn bench_count_tokens_cache_effectiveness() {
    let text = "The quick brown fox jumps over the lazy dog. ".repeat(200);

    let start_cold = std::time::Instant::now();
    let t1 = count_tokens(&text);
    let cold_us = start_cold.elapsed().as_micros();

    let start_cached = std::time::Instant::now();
    let t2 = count_tokens(&text);
    let cached_us = start_cached.elapsed().as_micros();

    assert_eq!(t1, t2, "cached result must match");

    let slightly_different = format!("{text}X");
    let start_miss = std::time::Instant::now();
    let t3 = count_tokens(&slightly_different);
    let miss_us = start_miss.elapsed().as_micros();

    eprintln!("\n{}", "=".repeat(70));
    eprintln!("  COUNT_TOKENS CACHE BENCHMARK (~9KB input)");
    eprintln!("{}", "=".repeat(70));
    eprintln!("  Cold (first call):  {cold_us:>6} us  -> {t1} tokens");
    eprintln!("  Cached (same text): {cached_us:>6} us  -> {t2} tokens");
    eprintln!("  Miss (diff text):   {miss_us:>6} us  -> {t3} tokens");
    if cold_us > 0 {
        let speedup = cold_us as f64 / cached_us.max(1) as f64;
        eprintln!("  Cache speedup:      {speedup:>6.0}x");
    }
    eprintln!("{}", "=".repeat(70));

    assert!(
        cached_us <= cold_us + 5,
        "cached call should be faster than cold call"
    );
}

#[test]
fn bench_session_prepare_save_is_cpu_only() {
    use lean_ctx::core::session::SessionState;

    let mut session = SessionState::new();
    session.set_task("benchmark task for save split", None);
    for _ in 0..20 {
        session.record_tool_call(100, 200);
        session.stats.unsaved_changes = 0;
    }

    let start = std::time::Instant::now();
    for _ in 0..100 {
        session.stats.unsaved_changes = 5;
        if let Ok(prepared) = session.prepare_save() {
            std::mem::drop(prepared);
        }
    }
    let prepare_us = start.elapsed().as_micros();

    eprintln!("\n{}", "=".repeat(70));
    eprintln!("  SESSION PREPARE_SAVE BENCHMARK (100 iters)");
    eprintln!("{}", "=".repeat(70));
    eprintln!("  100x prepare_save (serialize only): {prepare_us:>6} us");
    eprintln!(
        "  Per call:                           {:>6} us",
        prepare_us / 100
    );
    eprintln!("  Note: write_to_disk() I/O runs in background thread.");
    eprintln!("{}", "=".repeat(70));

    assert!(
        prepare_us / 100 < 5000,
        "prepare_save should take <5ms per call, got {} us",
        prepare_us / 100
    );

    assert_eq!(
        session.stats.unsaved_changes, 0,
        "prepare_save must reset unsaved_changes"
    );
}

#[test]
fn bench_session_save_semantics() {
    use lean_ctx::core::session::SessionState;

    let mut session = SessionState::new();
    for _ in 0..6 {
        session.increment();
    }
    assert!(session.should_save(), "should_save after 6 mutations");

    let prepared = session.prepare_save().expect("prepare_save");
    assert_eq!(
        session.stats.unsaved_changes, 0,
        "prepare_save must reset counter immediately (for async path)"
    );
    assert!(
        !session.should_save(),
        "should_save must be false after prepare_save"
    );

    let write_result = prepared.write_to_disk();
    assert!(
        write_result.is_ok(),
        "write_to_disk to valid dir should succeed"
    );

    for _ in 0..6 {
        session.increment();
    }
    assert!(session.should_save());

    let result = session.save();
    assert!(result.is_ok(), "synchronous save should succeed");
    assert_eq!(
        session.stats.unsaved_changes, 0,
        "save must reset counter on success"
    );
    assert!(
        !session.should_save(),
        "should_save must be false after save"
    );
}

#[test]
fn bench_full_per_call_overhead_budget() {
    // Serialize the LEAN_CTX_MINIMAL toggle below against sibling env-mutating tests.
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    let lazy_tools = lean_ctx::tool_defs::lazy_tool_defs();
    let full_tools = lean_ctx::tool_defs::granular_tool_defs();

    let instructions_minimal = {
        // TODO: Audit that the environment access only happens in single-threaded code.
        unsafe { std::env::set_var("LEAN_CTX_MINIMAL", "1") };
        let i = lean_ctx::server::build_instructions_for_test(CrpMode::Tdd);
        // TODO: Audit that the environment access only happens in single-threaded code.
        unsafe { std::env::remove_var("LEAN_CTX_MINIMAL") };
        i
    };
    let instructions_full = lean_ctx::server::build_instructions_for_test(CrpMode::Tdd);

    let lazy_tool_tokens: usize = lazy_tools
        .iter()
        .map(|t| {
            let desc = t.description.as_ref().map_or(0, |d| d.as_ref().len());
            let schema = serde_json::to_string(&t.input_schema)
                .unwrap_or_default()
                .len();
            (desc + schema) / 4
        })
        .sum();

    let full_tool_tokens: usize = full_tools
        .iter()
        .map(|t| {
            let desc = t.description.as_ref().map_or(0, |d| d.as_ref().len());
            let schema = serde_json::to_string(&t.input_schema)
                .unwrap_or_default()
                .len();
            (desc + schema) / 4
        })
        .sum();

    let minimal_instr_tokens = count_tokens(&instructions_minimal);
    let full_instr_tokens = count_tokens(&instructions_full);

    let best_case = minimal_instr_tokens + lazy_tool_tokens;
    let worst_case = full_instr_tokens + full_tool_tokens;

    eprintln!("\n{}", "=".repeat(70));
    eprintln!("  TOTAL PER-SESSION OVERHEAD BUDGET");
    eprintln!("{}", "=".repeat(70));
    eprintln!(
        "  Best case  (lazy+minimal):  {best_case:>5} tok  ({minimal_instr_tokens} instr + {lazy_tool_tokens} tools)"
    );
    eprintln!(
        "  Worst case (full+verbose):  {worst_case:>5} tok  ({full_instr_tokens} instr + {full_tool_tokens} tools)"
    );
    let reduction = (worst_case - best_case) as f64 / worst_case as f64 * 100.0;
    eprintln!("  Total reduction potential:   {reduction:>5.1}%");
    eprintln!("{}", "=".repeat(70));

    // Best case = lazy core tools (full registry schemas) + minimal
    // instructions, matching the live server's default surface (#141 SSOT).
    assert!(
        best_case < 4200,
        "Best-case overhead should be <4200 tokens, got {best_case}"
    );
    assert!(
        reduction > 50.0,
        "Reduction potential should be >50%, got {reduction:.1}%"
    );
}
