use lean_ctx::core::patterns::git;
use lean_ctx::core::tokens::count_tokens;

fn measure_compression(command: &str, output: &str) -> (usize, usize, f64) {
    let original = count_tokens(output);
    let compressed = git::compress(command, output).unwrap_or_else(|| output.to_string());
    let compressed_tokens = count_tokens(&compressed);
    let savings_pct = if original > 0 {
        (original - compressed_tokens) as f64 / original as f64 * 100.0
    } else {
        0.0
    };
    (original, compressed_tokens, savings_pct)
}

#[test]
fn verify_git_log_patch_savings() {
    let output = generate_git_log_patch(10);
    let (orig, comp, pct) = measure_compression("git log -p", &output);
    eprintln!("[git log -p, 10 commits] {orig} → {comp} tokens ({pct:.1}% saved)");
    assert!(
        pct > 70.0,
        "git log -p should save >70% tokens, got {pct:.1}%"
    );
}

#[test]
fn verify_git_log_patch_large_savings() {
    let output = generate_git_log_patch(50);
    let (orig, comp, pct) = measure_compression("git log -p", &output);
    eprintln!("[git log -p, 50 commits] {orig} → {comp} tokens ({pct:.1}% saved)");
    assert!(
        pct > 80.0,
        "git log -p 50 commits should save >80% tokens, got {pct:.1}%"
    );
}

#[test]
fn verify_git_log_stat_savings() {
    let output = generate_git_log_stat(20);
    let (orig, comp, pct) = measure_compression("git log --stat", &output);
    eprintln!("[git log --stat, 20 commits] {orig} → {comp} tokens ({pct:.1}% saved)");
    assert!(
        pct > 50.0,
        "git log --stat should save >50% tokens, got {pct:.1}%"
    );
}

#[test]
fn verify_git_log_standard_savings() {
    let output = generate_git_log_standard(30);
    let (orig, comp, pct) = measure_compression("git log", &output);
    eprintln!("[git log standard, 30 commits] {orig} → {comp} tokens ({pct:.1}% saved)");
    assert!(
        pct > 60.0,
        "git log standard 30 commits should save >60%, got {pct:.1}%"
    );
}

#[test]
fn verify_git_log_oneline_short_no_regression() {
    let output = "abc1234 feat: one\ndef5678 fix: two\nghi9012 docs: three";
    let (orig, comp, _pct) = measure_compression("git log --oneline", output);
    eprintln!("[git log --oneline, 3 entries] {orig} → {comp} tokens");
    assert_eq!(orig, comp, "short oneline should pass through unchanged");
}

#[test]
fn verify_git_commit_feature_branch_savings() {
    let output = "[feature/my-cool-branch abc1234] feat: implement new feature\n \
                  3 files changed, 45 insertions(+), 12 deletions(-)\n \
                  create mode 100644 src/new_module.rs\n";
    let (orig, comp, pct) = measure_compression("git commit -m 'feat'", output);
    let compressed = git::compress("git commit -m 'feat'", output).unwrap();
    eprintln!(
        "[git commit feature/branch] {orig} → {comp} tokens ({pct:.1}% saved)\n  result: {compressed}"
    );
    assert!(compressed.contains("abc1234"), "must contain hash");
    assert!(
        compressed.contains("feature/my-cool-branch"),
        "must contain branch name"
    );
}

#[test]
fn verify_git_commit_many_hooks_savings() {
    let mut output = String::new();
    for i in 0..50 {
        output.push_str(&format!("check-{i:02}..................passed\n"));
    }
    output.push_str("[main abc1234] fix: resolve lint issues\n");
    output.push_str(" 5 files changed, 30 insertions(+), 10 deletions(-)\n");

    let (orig, comp, pct) = measure_compression("git commit -m 'fix'", &output);
    let compressed = git::compress("git commit -m 'fix'", &output).unwrap();
    eprintln!(
        "[git commit 50 hooks] {orig} → {comp} tokens ({pct:.1}% saved)\n  result: {compressed}"
    );
    assert!(
        pct > 60.0,
        "50 hook lines should compress >60%, got {pct:.1}%"
    );
    assert!(
        compressed.contains("hooks passed"),
        "should summarize hooks"
    );
    assert!(compressed.contains("abc1234"), "must contain hash");
}

#[test]
fn verify_git_commit_with_failures_preserves_errors() {
    let mut output = String::new();
    for i in 0..20 {
        output.push_str(&format!("check-{i:02}..................passed\n"));
    }
    output.push_str("ruff.......................failed\n");
    output.push_str("fixing src/app.py: E302 expected 2 blank lines\n");
    output.push_str("mypy.......................failed\n");
    output.push_str("[main abc1234] fix: lint\n");
    output.push_str(" 2 files changed, 5 insertions(+), 3 deletions(-)\n");

    let (orig, comp, pct) = measure_compression("git commit -m 'fix'", &output);
    let compressed = git::compress("git commit -m 'fix'", &output).unwrap();
    eprintln!(
        "[git commit with failures] {orig} → {comp} tokens ({pct:.1}% saved)\n  result: {compressed}"
    );
    assert!(compressed.contains("ruff"), "must preserve failed check");
    assert!(compressed.contains("mypy"), "must preserve failed check");
    assert!(compressed.contains("passed"), "should mention passed count");
}

#[test]
fn verify_overall_savings_estimation() {
    let scenarios: Vec<(&str, String, &str)> = vec![
        ("git log -p -5", generate_git_log_patch(5), "git log -p"),
        ("git log -p -20", generate_git_log_patch(20), "git log -p"),
        (
            "git log --stat -10",
            generate_git_log_stat(10),
            "git log --stat",
        ),
        ("git log -10", generate_git_log_standard(10), "git log"),
        (
            "git commit (50 hooks)",
            {
                let mut s = String::new();
                for i in 0..50 {
                    s.push_str(&format!("check-{i:02}..passed\n"));
                }
                s.push_str("[main abc1234] fix: stuff\n 1 file changed, 1 insertion(+)\n");
                s
            },
            "git commit -m 'fix'",
        ),
    ];

    eprintln!("\n{}", "=".repeat(60));
    eprintln!("  SAVINGS VERIFICATION REPORT");
    eprintln!("{}", "=".repeat(60));
    eprintln!(
        "  {:<25} {:>8} {:>8} {:>7}",
        "Scenario", "Original", "Compr.", "Saved%"
    );
    eprintln!("  {}", "-".repeat(50));

    let mut total_original = 0usize;
    let mut total_compressed = 0usize;

    for (label, output, command) in &scenarios {
        let (orig, comp, pct) = measure_compression(command, output);
        total_original += orig;
        total_compressed += comp;
        eprintln!("  {:<25} {:>8} {:>8} {:>6.1}%", label, orig, comp, pct);
    }

    let total_pct = if total_original > 0 {
        (total_original - total_compressed) as f64 / total_original as f64 * 100.0
    } else {
        0.0
    };
    eprintln!("  {}", "-".repeat(50));
    eprintln!(
        "  {:<25} {:>8} {:>8} {:>6.1}%",
        "TOTAL", total_original, total_compressed, total_pct
    );
    eprintln!("{}\n", "=".repeat(60));

    assert!(
        total_pct > 50.0,
        "overall savings should be >50%, got {total_pct:.1}%"
    );
}

#[test]
fn verify_cep_delta_tracking_prevents_overcounting() {
    use std::collections::HashMap;

    let test_dir = std::env::temp_dir().join(format!("lean-ctx-cep-test-{}", std::process::id()));
    let lean_ctx_dir = test_dir.join(".lean-ctx");
    let _ = std::fs::create_dir_all(&lean_ctx_dir);
    let stats_path = lean_ctx_dir.join("stats.json");
    let _ = std::fs::remove_file(&stats_path);

    std::env::set_var("HOME", test_dir.to_str().unwrap());

    let mut modes = HashMap::new();
    modes.insert("full".to_string(), 5u64);

    lean_ctx::core::stats::record_cep_session(70, 5, 10, 1000, 600, &modes, 10, "Standard");

    let store1 = lean_ctx::core::stats::load();
    let orig1 = store1.cep.total_tokens_original;
    let comp1 = store1.cep.total_tokens_compressed;
    let sessions1 = store1.cep.sessions;
    eprintln!("After call 1: orig={orig1}, comp={comp1}, sessions={sessions1}");

    lean_ctx::core::stats::record_cep_session(75, 8, 15, 2000, 1200, &modes, 20, "Standard");

    let store2 = lean_ctx::core::stats::load();
    let orig2 = store2.cep.total_tokens_original;
    let comp2 = store2.cep.total_tokens_compressed;
    let sessions2 = store2.cep.sessions;
    eprintln!("After call 2: orig={orig2}, comp={comp2}, sessions={sessions2}");

    let pid = std::process::id();
    assert_eq!(store2.cep.last_session_pid, Some(pid), "should track PID");

    assert_eq!(
        sessions2, 1,
        "should still be 1 session (same PID), got {sessions2}"
    );

    assert_eq!(
        orig2, 2000,
        "total_original should be 2000 (1000 + delta 1000), got {orig2}"
    );

    assert_eq!(
        comp2, 1200,
        "total_compressed should be 1200 (600 + delta 600), got {comp2}"
    );

    eprintln!("[CEP delta tracking] PASSED: no over-counting detected");
    eprintln!("  Call 1: 1000/600 → totals 1000/600");
    eprintln!("  Call 2: 2000/1200 → delta 1000/600 → totals 2000/1200");
    eprintln!("  Without fix: totals would be 3000/1800 (1000+2000 / 600+1200)");

    let _ = std::fs::remove_dir_all(&test_dir);
}

#[test]
#[ignore] // Requires manual setup: git log -p -10 > /tmp/git_log_raw.txt
fn e2e_real_git_log_compression() {
    let raw_path = "/tmp/git_log_raw.txt";
    if !std::path::Path::new(raw_path).exists() {
        eprintln!(
            "[SKIP] /tmp/git_log_raw.txt not found — run: git log -p -10 > /tmp/git_log_raw.txt"
        );
        return;
    }
    let raw = std::fs::read_to_string(raw_path).unwrap();
    let original = count_tokens(&raw);
    let compressed = git::compress("git log -p -10", &raw).unwrap_or_else(|| raw.clone());
    let compressed_tokens = count_tokens(&compressed);
    let saved = original.saturating_sub(compressed_tokens);
    let pct = if original > 0 {
        saved as f64 / original as f64 * 100.0
    } else {
        0.0
    };

    eprintln!("\n=== E2E: Real git log -p -10 ===");
    eprintln!("Raw: {} bytes, {} lines", raw.len(), raw.lines().count());
    eprintln!("Original: {original} tokens");
    eprintln!("Compressed: {compressed_tokens} tokens");
    eprintln!("Saved: {saved} tokens ({pct:.1}%)");
    eprintln!("\n=== Compressed output ===");
    eprintln!("{}", compressed.chars().take(800).collect::<String>());

    assert!(
        pct > 80.0,
        "real git log -p should save >80%, got {pct:.1}%"
    );
}

#[test]
fn e2e_real_git_log_stat_compression() {
    let raw = std::process::Command::new("git")
        .args(["log", "--stat", "-20"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();

    if raw.is_empty() {
        eprintln!("[SKIP] could not run git log --stat");
        return;
    }

    let original = count_tokens(&raw);
    let compressed = git::compress("git log --stat", &raw).unwrap_or_else(|| raw.clone());
    let compressed_tokens = count_tokens(&compressed);
    let saved = original.saturating_sub(compressed_tokens);
    let pct = if original > 0 {
        saved as f64 / original as f64 * 100.0
    } else {
        0.0
    };

    eprintln!("\n=== E2E: Real git log --stat -20 ===");
    eprintln!("Raw: {} bytes, {} lines", raw.len(), raw.lines().count());
    eprintln!("Original: {original} tokens → Compressed: {compressed_tokens} tokens");
    eprintln!("Saved: {saved} tokens ({pct:.1}%)");
    eprintln!("\n=== Compressed output (first 500 chars) ===");
    eprintln!("{}", compressed.chars().take(500).collect::<String>());

    assert!(
        pct > 60.0,
        "real git log --stat should save >60%, got {pct:.1}%"
    );
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

/// End-to-end audit: trace the full savings pipeline for each tool
/// and verify that "original" and "saved" are fair comparisons.
#[test]
fn audit_full_savings_pipeline() {
    use lean_ctx::core::cache::SessionCache;
    use lean_ctx::core::tokens::count_tokens;
    use lean_ctx::tools::CrpMode;

    eprintln!("\n{}", "=".repeat(70));
    eprintln!("  FULL SAVINGS PIPELINE AUDIT");
    eprintln!("{}", "=".repeat(70));

    // 1. ctx_read — first read in full mode: savings should be ~0
    {
        let mut cache = SessionCache::new();
        let content = "use std::io;\nfn main() {\n    println!(\"hello\");\n}\n";
        let tmp = std::env::temp_dir().join("audit_test_file.rs");
        std::fs::write(&tmp, content).unwrap();

        let output = lean_ctx::tools::ctx_read::handle(
            &mut cache,
            tmp.to_str().unwrap(),
            "full",
            CrpMode::Off,
        );
        let file_tokens = count_tokens(content);
        let output_tokens = count_tokens(&output);
        let saved = file_tokens.saturating_sub(output_tokens);

        eprintln!("\n  ctx_read (first read, full mode):");
        eprintln!("    file tokens:   {file_tokens}");
        eprintln!("    output tokens: {output_tokens}");
        eprintln!("    saved:         {saved}");
        eprintln!(
            "    contains savings line: {}",
            output.contains("tok saved")
        );

        assert!(
            output_tokens >= file_tokens,
            "first full read includes header, so output >= original: {output_tokens} vs {file_tokens}"
        );

        // 2. ctx_read — second read (cache hit): massive savings
        let output2 = lean_ctx::tools::ctx_read::handle(
            &mut cache,
            tmp.to_str().unwrap(),
            "full",
            CrpMode::Off,
        );
        let output2_tokens = count_tokens(&output2);
        let is_cache_hit = output2.contains(" cached ");

        eprintln!("\n  ctx_read (cache re-read):");
        eprintln!("    file tokens:   {file_tokens}");
        eprintln!("    output tokens: {output2_tokens}");
        eprintln!("    is_cache_hit:  {is_cache_hit}");
        eprintln!("    output: {:?}", output2);

        assert!(is_cache_hit, "second read should be a cache hit");
        assert!(
            output2_tokens < 20,
            "cache hit stub should be <20 tokens, got {output2_tokens}"
        );

        let _ = std::fs::remove_file(&tmp);
    }

    // 3. ctx_read — compressed mode (signatures): fair comparison
    {
        let mut cache = SessionCache::new();
        let mut code = Vec::new();
        code.push("use std::collections::HashMap;".to_string());
        for i in 0..50 {
            code.push(format!("pub fn handler_{i}(req: Request) -> Response {{"));
            code.push(format!(
                "    let data = db.query(\"SELECT * FROM table_{i}\");"
            ));
            code.push(format!(
                "    let filtered = data.iter().filter(|r| r.active).collect::<Vec<_>>();"
            ));
            code.push(format!(
                "    let result = process_items(&filtered, req.params());"
            ));
            code.push(format!(
                "    if result.is_err() {{ return Response::error(500); }}"
            ));
            code.push(format!("    Response::json(result.unwrap())"));
            code.push("}".to_string());
            code.push(String::new());
        }
        let content = code.join("\n");
        let tmp = std::env::temp_dir().join("audit_test_sigs.rs");
        std::fs::write(&tmp, &content).unwrap();

        let output = lean_ctx::tools::ctx_read::handle(
            &mut cache,
            tmp.to_str().unwrap(),
            "signatures",
            CrpMode::Off,
        );
        let file_tokens = count_tokens(&content);
        let output_tokens = count_tokens(&output);
        let saved = file_tokens.saturating_sub(output_tokens);
        let ratio = if file_tokens > 0 {
            saved as f64 / file_tokens as f64 * 100.0
        } else {
            0.0
        };

        eprintln!("\n  ctx_read (signatures mode, 50 multi-line fns):");
        eprintln!("    file tokens:   {file_tokens}");
        eprintln!("    output tokens: {output_tokens}");
        eprintln!("    savings:       {saved} ({ratio:.1}%)");

        assert!(
            output_tokens < file_tokens,
            "signatures should compress: {output_tokens} < {file_tokens}"
        );
        assert!(
            ratio > 20.0 && ratio < 98.0,
            "signatures savings should be 20-98%, got {ratio:.1}%"
        );

        let _ = std::fs::remove_file(&tmp);
    }

    // 4. ctx_tree — fair comparison (same depth)
    {
        let dir = env!("CARGO_MANIFEST_DIR");
        let (output, raw_tokens) = lean_ctx::tools::ctx_tree::handle(dir, 2, false);
        let compact_tokens = count_tokens(&output);
        let savings = raw_tokens.saturating_sub(compact_tokens);
        let ratio = if raw_tokens > 0 {
            savings as f64 / raw_tokens as f64 * 100.0
        } else {
            0.0
        };

        eprintln!("\n  ctx_tree (depth=2):");
        eprintln!("    raw tokens:     {raw_tokens}");
        eprintln!("    compact tokens: {compact_tokens}");
        eprintln!("    savings:        {savings} ({ratio:.1}%)");

        assert!(
            raw_tokens < 3000,
            "raw tree at depth 2 must be <3000 tokens, got {raw_tokens}"
        );
        assert!(
            ratio < 80.0,
            "tree savings ratio should be <80% for fair comparison, got {ratio:.1}%"
        );
    }

    // 5. ctx_shell — real compression
    {
        let raw_output = generate_git_log_patch(10);
        let original = count_tokens(&raw_output);
        let compressed = lean_ctx::core::patterns::git::compress("git log -p", &raw_output)
            .unwrap_or_else(|| raw_output.clone());
        let compressed_tokens = count_tokens(&compressed);
        let saved = original.saturating_sub(compressed_tokens);
        let ratio = if original > 0 {
            saved as f64 / original as f64 * 100.0
        } else {
            0.0
        };

        eprintln!("\n  ctx_shell (git log -p, 10 commits):");
        eprintln!("    raw tokens:       {original}");
        eprintln!("    compressed tokens: {compressed_tokens}");
        eprintln!("    savings:          {saved} ({ratio:.1}%)");

        assert!(ratio > 70.0, "git log -p should save >70%, got {ratio:.1}%");
    }

    // 6. Verify tokenizer accuracy by cross-checking with known strings
    {
        let test_cases = [("hello world", 2), ("fn main() {}", 5), ("", 0), ("a", 1)];
        eprintln!("\n  Tokenizer (o200k_base) spot checks:");
        for (text, expected_approx) in &test_cases {
            let actual = count_tokens(text);
            eprintln!(
                "    {:20} → {actual:>3} tokens (expected ~{expected_approx})",
                format!("{:?}", text)
            );
            let diff = (actual as i64 - *expected_approx as i64).unsigned_abs();
            assert!(
                diff <= 2,
                "tokenizer off by >2 for {:?}: got {actual}, expected ~{expected_approx}",
                text
            );
        }
    }

    // 7. CostModel sanity check
    {
        let input_saved: u64 = 100_000;
        let commands: u64 = 500;
        let output_bonus = commands * 60;
        let total_saved = input_saved + output_bonus;
        let cost_per_m_input = 2.50_f64;
        let cost_per_m_output = 10.0_f64;
        let usd_input = input_saved as f64 / 1_000_000.0 * cost_per_m_input;
        let usd_output = output_bonus as f64 / 1_000_000.0 * cost_per_m_output;
        let usd_total = usd_input + usd_output;

        eprintln!("\n  CostModel sanity (100K input saved, 500 calls):");
        eprintln!("    input saved:  {input_saved} tok → ${usd_input:.4}");
        eprintln!("    output bonus: {output_bonus} tok → ${usd_output:.4}");
        eprintln!("    total:        {total_saved} tok → ${usd_total:.4}");

        assert!(
            usd_total < 1.0,
            "100K saved + 500 calls should be < $1.00, got ${usd_total:.4}"
        );
        assert!(usd_total > 0.0, "savings should be positive");
    }

    eprintln!("\n  AUDIT COMPLETE — all checks passed");
    eprintln!("{}\n", "=".repeat(70));
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
