//! Tests for `ctx_read`. Extracted from `ctx_read/mod.rs`;
//! `super::*` resolves to the `ctx_read` module.

use super::*;
use std::time::Duration;

#[test]
fn test_header_toon_format_no_brackets() {
    let _lock = crate::core::data_dir::test_env_lock();
    crate::test_env::set_var("LEAN_CTX_META", "1");
    let content = "use std::io;\nfn main() {}\n";
    let header = build_header("F1", "main.rs", "rs", content, 2, false);
    assert!(!header.contains('['));
    assert!(!header.contains(']'));
    assert!(header.contains("F1=main.rs 2L"));
    crate::test_env::remove_var("LEAN_CTX_META");
}

#[test]
fn test_header_toon_deps_indented() {
    let _lock = crate::core::data_dir::test_env_lock();
    crate::test_env::set_var("LEAN_CTX_META", "1");
    let content = "use crate::core::cache;\nuse crate::tools;\npub fn main() {}\n";
    let header = build_header("F1", "main.rs", "rs", content, 3, true);
    if header.contains("deps") {
        assert!(
            header.contains("\n deps "),
            "deps should use indented TOON format"
        );
        assert!(
            !header.contains("deps:["),
            "deps should not use bracket format"
        );
    }
    crate::test_env::remove_var("LEAN_CTX_META");
}

#[test]
fn test_header_toon_saves_tokens() {
    let _lock = crate::core::data_dir::test_env_lock();
    crate::test_env::set_var("LEAN_CTX_META", "1");
    let content = "use crate::foo;\nuse crate::bar;\npub fn baz() {}\npub fn qux() {}\n";
    let old_header = "F1=main.rs [4L +] deps:[foo,bar] exports:[baz,qux]".to_string();
    let new_header = build_header("F1", "main.rs", "rs", content, 4, true);
    let old_tokens = count_tokens(&old_header);
    let new_tokens = count_tokens(&new_header);
    assert!(
        new_tokens <= old_tokens,
        "TOON header ({new_tokens} tok) should be <= old format ({old_tokens} tok)"
    );
    crate::test_env::remove_var("LEAN_CTX_META");
}

#[test]
fn test_tdd_symbols_are_compact() {
    let symbols = [
        "⊕", "⊖", "∆", "→", "⇒", "✓", "✗", "⚠", "λ", "§", "∂", "τ", "ε",
    ];
    for sym in &symbols {
        let tok = count_tokens(sym);
        assert!(tok <= 2, "Symbol {sym} should be 1-2 tokens, got {tok}");
    }
}

#[test]
fn test_task_mode_filters_content() {
    let content = (0..200)
        .map(|i| {
            if i % 20 == 0 {
                format!("fn validate_token(token: &str) -> bool {{ /* line {i} */ }}")
            } else {
                format!("fn unrelated_helper_{i}(x: i32) -> i32 {{ x + {i} }}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let full_tokens = count_tokens(&content);
    let task = Some("fix bug in validate_token");
    let (result, result_tokens) = process_mode(
        &content,
        "task",
        "F1",
        "test.rs",
        "rs",
        full_tokens,
        CrpMode::Off,
        "test.rs",
        task,
    );
    assert!(
        result_tokens < full_tokens,
        "task mode ({result_tokens} tok) should be less than full ({full_tokens} tok)"
    );
    assert!(
        result.contains("task-filtered"),
        "output should contain task-filtered marker"
    );
}

#[test]
fn test_task_mode_without_task_returns_full() {
    let content = "fn main() {}\nfn helper() {}\n";
    let tokens = count_tokens(content);
    let (result, _sent) = process_mode(
        content,
        "task",
        "F1",
        "test.rs",
        "rs",
        tokens,
        CrpMode::Off,
        "test.rs",
        None,
    );
    assert!(
        result.contains("no task set"),
        "should indicate no task: {result}"
    );
}

#[test]
fn test_reference_mode_one_line() {
    let content = "fn main() {}\nfn helper() {}\nfn other() {}\n";
    let tokens = count_tokens(content);
    let (result, _sent) = process_mode(
        content,
        "reference",
        "F1",
        "test.rs",
        "rs",
        tokens,
        CrpMode::Off,
        "test.rs",
        None,
    );
    let lines: Vec<&str> = result.lines().collect();
    assert!(
        lines.len() <= 3,
        "reference mode should be very compact, got {} lines",
        lines.len()
    );
    assert!(result.contains("lines"), "should contain line count");
    assert!(result.contains("tok"), "should contain token count");
}

#[test]
fn cached_lines_mode_invalidates_on_mtime_change() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("file.txt");
    let p = path.to_string_lossy().to_string();

    std::fs::write(&path, "one\nsecond\n").unwrap();
    let mut cache = SessionCache::new();

    let r1 = handle_with_task_resolved(&mut cache, &p, "lines:1-1", CrpMode::Off, None);
    let l1: Vec<&str> = r1.content.lines().collect();
    let got1 = l1.get(1).copied().unwrap_or_default().trim();
    let got1 = got1.split_once('|').map_or(got1, |(_, s)| s.trim());
    assert_eq!(got1, "one");

    std::thread::sleep(Duration::from_secs(1));
    std::fs::write(&path, "two\nsecond\n").unwrap();

    let r2 = handle_with_task_resolved(&mut cache, &p, "lines:1-1", CrpMode::Off, None);
    let l2: Vec<&str> = r2.content.lines().collect();
    let got2 = l2.get(1).copied().unwrap_or_default().trim();
    let got2 = got2.split_once('|').map_or(got2, |(_, s)| s.trim());
    assert_eq!(got2, "two");
}

#[test]
fn try_stub_hit_readonly_none_for_uncached_and_stale() {
    let _lock = crate::core::data_dir::test_env_lock();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hot.rs");
    let p = path.to_string_lossy().to_string();
    std::fs::write(&path, "fn main() {}\n").unwrap();

    let mut cache = SessionCache::new();

    // Never read → no entry → the read-locked path declines (caller falls back).
    assert!(try_stub_hit_readonly(&cache, &p).is_none());

    // Populate the cache via a real full read.
    let _ = handle_with_task_resolved(&mut cache, &p, "full", CrpMode::Off, None);

    // Modify the file so the cached entry is stale → must NOT serve a stub,
    // independent of cache policy (the write path re-reads instead).
    std::thread::sleep(Duration::from_secs(1));
    std::fs::write(&path, "fn main() { changed(); }\n").unwrap();
    assert!(
        try_stub_hit_readonly(&cache, &p).is_none(),
        "stale file must never be served from the read-locked stub path"
    );
}

#[test]
#[cfg_attr(tarpaulin, ignore)]
fn benchmark_task_conditioned_compression() {
    // Keep this reasonably small so CI coverage instrumentation stays fast.
    let content = generate_benchmark_code(200);
    let full_tokens = count_tokens(&content);
    let task = Some("fix authentication in validate_token");

    let (_full_output, full_tok) = process_mode(
        &content,
        "full",
        "F1",
        "server.rs",
        "rs",
        full_tokens,
        CrpMode::Off,
        "server.rs",
        task,
    );
    let (_task_output, task_tok) = process_mode(
        &content,
        "task",
        "F1",
        "server.rs",
        "rs",
        full_tokens,
        CrpMode::Off,
        "server.rs",
        task,
    );
    let (_sig_output, sig_tok) = process_mode(
        &content,
        "signatures",
        "F1",
        "server.rs",
        "rs",
        full_tokens,
        CrpMode::Off,
        "server.rs",
        task,
    );
    let (_ref_output, ref_tok) = process_mode(
        &content,
        "reference",
        "F1",
        "server.rs",
        "rs",
        full_tokens,
        CrpMode::Off,
        "server.rs",
        task,
    );

    eprintln!("\n=== Task-Conditioned Compression Benchmark ===");
    eprintln!("Source: 200-line Rust file, task='fix authentication in validate_token'");
    eprintln!("  full:       {full_tok:>6} tokens (baseline)");
    eprintln!(
        "  task:       {task_tok:>6} tokens ({:.0}% savings)",
        (1.0 - task_tok as f64 / full_tok as f64) * 100.0
    );
    eprintln!(
        "  signatures: {sig_tok:>6} tokens ({:.0}% savings)",
        (1.0 - sig_tok as f64 / full_tok as f64) * 100.0
    );
    eprintln!(
        "  reference:  {ref_tok:>6} tokens ({:.0}% savings)",
        (1.0 - ref_tok as f64 / full_tok as f64) * 100.0
    );
    eprintln!("================================================\n");

    assert!(task_tok < full_tok, "task mode should save tokens");
    assert!(sig_tok < full_tok, "signatures should save tokens");
    assert!(ref_tok < sig_tok, "reference should be most compact");
}

fn generate_benchmark_code(lines: usize) -> String {
    let mut code = Vec::with_capacity(lines);
    code.push("use std::collections::HashMap;".to_string());
    code.push("use crate::core::auth;".to_string());
    code.push(String::new());
    code.push("pub struct Server {".to_string());
    code.push("    config: Config,".to_string());
    code.push("    cache: HashMap<String, String>,".to_string());
    code.push("}".to_string());
    code.push(String::new());
    code.push("impl Server {".to_string());
    code.push(
        "    pub fn validate_token(&self, token: &str) -> Result<Claims, AuthError> {".to_string(),
    );
    code.push("        let decoded = auth::decode_jwt(token)?;".to_string());
    code.push("        if decoded.exp < chrono::Utc::now().timestamp() {".to_string());
    code.push("            return Err(AuthError::Expired);".to_string());
    code.push("        }".to_string());
    code.push("        Ok(decoded.claims)".to_string());
    code.push("    }".to_string());
    code.push(String::new());

    let remaining = lines.saturating_sub(code.len());
    for i in 0..remaining {
        if i % 30 == 0 {
            code.push(format!(
                "    pub fn handler_{i}(&self, req: Request) -> Response {{"
            ));
        } else if i % 30 == 29 {
            code.push("    }".to_string());
        } else {
            code.push(format!("        let val_{i} = self.cache.get(\"key_{i}\").unwrap_or(&\"default\".to_string());"));
        }
    }
    code.push("}".to_string());
    code.join("\n")
}

#[test]
fn map_mode_inlines_task_relevant_body() {
    let content = "pub fn alpha() {\n    let a = 1;\n}\n\npub fn validate_token(t: &str) -> bool {\n    let ok = check(t);\n    ok\n}\n";
    let tokens = count_tokens(content);
    let (with_task, _) = process_mode(
        content,
        "map",
        "F1",
        "test.rs",
        "rs",
        tokens,
        CrpMode::Off,
        "test.rs",
        Some("fix bug in validate_token"),
    );
    assert!(
        with_task.contains("▸ body") && with_task.contains("validate_token"),
        "map with task should inline the matching body: {with_task}"
    );
    let (no_task, _) = process_mode(
        content,
        "map",
        "F1",
        "test.rs",
        "rs",
        tokens,
        CrpMode::Off,
        "test.rs",
        None,
    );
    assert!(
        !no_task.contains("▸ body"),
        "map without a task must not inline a body: {no_task}"
    );
}

#[test]
fn compressed_cache_key_distinguishes_task() {
    let no_task = compressed_cache_key("map", CrpMode::Off, None, None, &[]);
    let tdd_no_task = compressed_cache_key("map", CrpMode::Tdd, None, None, &[]);
    let with_task = compressed_cache_key("map", CrpMode::Off, Some("fix login"), None, &[]);
    let other_task = compressed_cache_key("map", CrpMode::Off, Some("refactor db"), None, &[]);
    // Versioned so stale pre-line-range entries cannot be served.
    assert_eq!(no_task, "map:v2");
    assert_eq!(tdd_no_task, "map:v2:tdd");
    assert_ne!(with_task, no_task);
    assert_ne!(with_task, other_task);
}

#[test]
fn compressed_cache_key_distinguishes_aggressiveness() {
    // None → byte-identical to today's keys (#714 must not shift existing cache).
    let base = compressed_cache_key("map", CrpMode::Off, None, None, &[]);
    assert_eq!(base, "map:v2");
    // Same aggressiveness → same key (determinism, #498).
    let a = compressed_cache_key("map", CrpMode::Off, None, Some(0.7), &[]);
    assert_eq!(
        a,
        compressed_cache_key("map", CrpMode::Off, None, Some(0.7), &[])
    );
    // Distinct buckets → distinct keys; jitter inside a 0.05 bucket collapses.
    assert_ne!(a, base);
    assert_ne!(
        a,
        compressed_cache_key("map", CrpMode::Off, None, Some(0.2), &[])
    );
    assert_eq!(
        a,
        compressed_cache_key("map", CrpMode::Off, None, Some(0.701), &[])
    );
}

#[test]
fn compressed_cache_key_distinguishes_protect() {
    // Empty protect → byte-identical to today's keys (#720 must not shift cache).
    let base = compressed_cache_key("entropy", CrpMode::Off, None, None, &[]);
    assert_eq!(base, "entropy");
    // A non-empty protect list changes the key (lossy output differs, #498)…
    let p = compressed_cache_key("entropy", CrpMode::Off, None, None, &["TODO".to_string()]);
    assert_ne!(p, base);
    // …deterministically, and independent of token order / duplicates.
    assert_eq!(
        p,
        compressed_cache_key("entropy", CrpMode::Off, None, None, &["TODO".to_string()])
    );
    let multi_a = compressed_cache_key(
        "entropy",
        CrpMode::Off,
        None,
        None,
        &["a".to_string(), "b".to_string()],
    );
    let multi_b = compressed_cache_key(
        "entropy",
        CrpMode::Off,
        None,
        None,
        &["b".to_string(), "a".to_string(), "a".to_string()],
    );
    assert_eq!(multi_a, multi_b);
    assert_ne!(multi_a, p);
}

#[test]
fn aggressiveness_is_deterministic_and_monotonic() {
    let _lock = crate::core::data_dir::test_env_lock();
    // Suppress the savings footer: it carries session-cumulative counters by
    // design (state-triggered suffix), so we compare the pure compressed body.
    crate::test_env::set_var("LEAN_CTX_SHOW_SAVINGS", "0");

    // Prose-y fixture with redundant low-information lines the density pass can
    // shed; enough lines that compression is meaningful.
    let mut content = String::new();
    for i in 0..60 {
        content.push_str(&format!(
            "line {i}: the quick brown fox jumps over the lazy dog\n"
        ));
    }
    let render_at = |a: f64| -> String {
        // Bare `density:` exercises the aggressiveness-target fallback (#714).
        let (out, _) = process_mode_tuned(
            &content,
            "density:",
            "F1",
            "f.txt",
            "txt",
            count_tokens(&content),
            CrpMode::Off,
            "/tmp/f.txt",
            None,
            ReadTuning {
                aggressiveness: Some(a),
                protect: &[],
            },
        );
        out
    };
    // Determinism (#498): same aggressiveness → byte-identical output. Guards the
    // canonical-order entropy summation fix in `token_entropy_from_ids`.
    assert_eq!(render_at(0.7), render_at(0.7));
    // Monotonic: more aggressive keeps no more tokens than less aggressive.
    let low = count_tokens(&render_at(0.2));
    let high = count_tokens(&render_at(0.9));
    assert!(
        high <= low,
        "aggressiveness 0.9 ({high} tok) must not exceed 0.2 ({low} tok)"
    );

    crate::test_env::remove_var("LEAN_CTX_SHOW_SAVINGS");
}

#[test]
fn aggressive_json_uses_lossless_crush_core() {
    let _lock = crate::core::data_dir::test_env_lock();
    crate::test_env::set_var("LEAN_CTX_SHOW_SAVINGS", "0");

    // A redundant array-of-objects JSON file: aggressive mode compacts it through
    // the shared json_crush core (#936) instead of generic text pruning, which
    // would mangle the structure. Constant columns + many rows so it halves.
    let items: Vec<String> = (0..40)
        .map(|i| {
            format!(r#"{{"status":"active","region":"eu-central-1","tier":"standard","id":{i}}}"#)
        })
        .collect();
    let content = format!("[{}]", items.join(","));
    let original = count_tokens(&content);

    let (out, sent) = process_mode_tuned(
        &content,
        "aggressive",
        "F1",
        "data.json",
        "json",
        original,
        CrpMode::Off,
        "/tmp/data.json",
        None,
        ReadTuning {
            aggressiveness: None,
            protect: &[],
        },
    );

    assert!(
        out.contains("_lc_crush"),
        "aggressive json must compact via the crush core: {out}"
    );
    assert!(
        sent < original,
        "crush must reduce tokens ({sent} >= {original})"
    );

    crate::test_env::remove_var("LEAN_CTX_SHOW_SAVINGS");
}

#[test]
fn map_mode_includes_signature_line_ranges() {
    // Map formatting is rendered by `process_mode`; assert it directly so the
    // structure check stays independent of the handle-level #361 cap, which
    // legitimately collapses this tiny fixture to raw.
    let content = "pub struct Config {}\n\npub fn build() -> Config { Config {} }\n";
    let (result, _) = process_mode(
        content,
        "map",
        "F1",
        "lib.rs",
        "rs",
        count_tokens(content),
        CrpMode::Off,
        "/tmp/lib.rs",
        None,
    );

    assert!(
        result.contains("API:"),
        "map output should include API: {result}"
    );
    assert!(
        result.contains("struct pub Config @L1"),
        "struct signature should include line suffix: {result}"
    );
    assert!(
        result.contains("fn pub build() → Config @L3"),
        "function signature should include line suffix: {result}"
    );
}

#[test]
fn map_mode_omits_exports_already_in_api() {
    // #361 follow-up: the `exports:` line duplicated symbols the API section
    // already lists with full signatures + line ranges. Map must not repeat
    // exports that the API already covers (pure redundant tokens). Rendered by
    // `process_mode`; assert it directly (handle would cap this tiny fixture).
    let content = "pub struct Config {}\n\npub fn build() -> Config { Config {} }\n";
    let (result, _) = process_mode(
        content,
        "map",
        "F1",
        "lib.rs",
        "rs",
        count_tokens(content),
        CrpMode::Off,
        "/tmp/lib.rs",
        None,
    );

    // Both exported symbols stay discoverable via the API section …
    assert!(
        result.contains("struct pub Config") && result.contains("fn pub build"),
        "API section must still list exported symbols: {result}"
    );
    // … and the redundant `exports:` line is gone (both are in the API).
    assert!(
        !result.contains("exports:"),
        "map must not repeat exports already shown in API: {result}"
    );
}

#[test]
fn tdd_map_output_carries_symbol_legend() {
    // GL #580: symbol notation must be self-describing for vanilla agents.
    // Rendered by `process_mode`; assert it directly (handle caps this fixture).
    let content = "pub struct Config {}\n\npub fn build() -> Config { Config {} }\n";
    let (result, _) = process_mode(
        content,
        "map",
        "F1",
        "lib.rs",
        "rs",
        count_tokens(content),
        CrpMode::Tdd,
        "/tmp/lib.rs",
        None,
    );
    assert!(
        result.contains("[λ=fn §=class +=pub]"),
        "TDD map output must carry the symbol legend: {result}"
    );

    let (sigs, _) = process_mode(
        content,
        "signatures",
        "F1",
        "lib.rs",
        "rs",
        count_tokens(content),
        CrpMode::Tdd,
        "/tmp/lib.rs",
        None,
    );
    assert!(
        sigs.contains("[λ=fn §=class +=pub]"),
        "TDD signatures output must carry the symbol legend: {sigs}"
    );
}

#[test]
fn instruction_file_detection() {
    assert!(is_instruction_file(
        "/home/user/.pi/agent/skills/committing-changes/SKILL.md"
    ));
    assert!(is_instruction_file("/workspace/.cursor/rules/lean-ctx.mdc"));
    assert!(is_instruction_file("/project/AGENTS.md"));
    assert!(is_instruction_file("/project/.cursorrules"));
    assert!(is_instruction_file("/home/user/.claude/rules/my-rule.md"));
    assert!(is_instruction_file("/skills/some-skill/README.md"));

    assert!(!is_instruction_file("/project/src/main.rs"));
    assert!(!is_instruction_file("/project/config.json"));
    assert!(!is_instruction_file("/project/data/report.csv"));
}

#[test]
fn resolve_auto_mode_returns_full_for_instruction_files() {
    let mode = resolve_auto_mode(
        None,
        "/home/user/.pi/agent/skills/committing-changes/SKILL.md",
        5000,
        Some("read"),
    );
    assert_eq!(mode, "full", "SKILL.md must always be read in full");

    let mode = resolve_auto_mode(None, "/workspace/AGENTS.md", 3000, Some("read"));
    assert_eq!(mode, "full", "AGENTS.md must always be read in full");

    let mode = resolve_auto_mode(None, "/workspace/.cursorrules", 2000, None);
    assert_eq!(mode, "full", ".cursorrules must always be read in full");
}

#[test]
fn raw_mode_returns_exact_file_content() {
    let _lock = crate::core::data_dir::test_env_lock();
    let content = "fn main() {\n    println!(\"hello\");\n}\n";
    let (output, _sent) = render::process_mode(
        content,
        "raw",
        "F1",
        "main.rs",
        "rs",
        100,
        CrpMode::Off,
        "/tmp/main.rs",
        None,
    );
    assert_eq!(
        output, content,
        "raw mode must return exact file content with zero overhead"
    );
    assert!(
        !output.contains("main.rs"),
        "raw mode must not contain filename header"
    );
    assert!(!output.contains("deps"), "raw mode must not contain deps");
}

/// Determinism contract (#498): tool output must be a pure function of
/// (content, mode, crp_mode, task). Timestamps, counters or random hints in
/// the body would make otherwise-identical outputs unique and defeat
/// provider-side prompt caching.
#[test]
fn process_mode_output_is_byte_stable_across_calls() {
    // Fresh, empty data dir (GL #556): the shared per-process test sandbox
    // accumulates feedback/bandit/session stores from parallel tests, which
    // feed adaptive_thresholds() and make entropy-mode output drift between
    // two calls. Purity only holds against a stable learning state.
    let _iso = crate::core::data_dir::isolated_data_dir();
    // Footer visibility must be the default (`never`) for purity: with a
    // visible footer, the process-global session accumulator appends a
    // `session: N saved` line every 10th call across ALL tests. Other tests
    // leaked `LEAN_CTX_SAVINGS_FOOTER=always` here in the past — neutralize
    // defensively while we hold the env lock.
    crate::test_env::remove_var("LEAN_CTX_SAVINGS_FOOTER");
    crate::test_env::remove_var("LEAN_CTX_SHOW_SAVINGS");
    crate::test_env::remove_var("LEAN_CTX_QUIET");
    let content: String = (0..120)
        .map(|i| format!("pub fn handler_{i}(x: u32) -> u32 {{ x * {i} }}"))
        .collect::<Vec<_>>()
        .join("\n");
    let tokens = count_tokens(&content);

    for mode in [
        "map",
        "signatures",
        "reference",
        "aggressive",
        "entropy",
        "raw",
        "lines:5-20",
    ] {
        let run = || {
            render::process_mode(
                &content,
                mode,
                "F1",
                "stable.rs",
                "rs",
                tokens,
                CrpMode::Off,
                "/tmp/stable.rs",
                None,
            )
            .0
        };
        let first = run();
        let second = run();
        assert_eq!(
            first, second,
            "mode '{mode}' produced non-deterministic output"
        );
    }
}

#[test]
fn raw_mode_no_savings_footer() {
    let _lock = crate::core::data_dir::test_env_lock();
    let content = "x = 1\n";
    let (output, _) = render::process_mode(
        content,
        "raw",
        "F1",
        "tiny.py",
        "py",
        50,
        CrpMode::Off,
        "/tmp/tiny.py",
        None,
    );
    assert!(
        !output.contains('\u{2500}'),
        "raw mode must not contain savings footer box-drawing chars"
    );
    assert_eq!(output, content);
}

// ---------------------------------------------------------------------------
// #361 anti-inflation invariant: a `ctx_read` must never cost more tokens than
// the raw file. The framing header only earns its keep on large files and
// cached re-reads; on a cold read of a small file it is pure overhead, so the
// guard ships bare content (break-even, never a loss). The guard now applies to
// every mode — auto-resolved AND explicitly requested — so no view can ever
// cost more tokens than reading the file raw.
// ---------------------------------------------------------------------------

#[test]
fn cap_to_raw_falls_back_when_framing_inflates() {
    let raw = "pub fn a() {}\n";
    let framed = format!("F1=x.rs 1L\n deps foo,bar\n{raw}");
    let raw_tokens = count_tokens(raw);
    let framed_tokens = count_tokens(&framed);
    assert!(
        framed_tokens > raw_tokens,
        "fixture must inflate to exercise the guard"
    );
    assert_eq!(
        cap_to_raw(framed, framed_tokens, raw, raw_tokens),
        raw,
        "framing larger than raw must fall back to bare content"
    );
}

#[test]
fn cap_to_raw_keeps_framing_when_not_larger() {
    let raw = "a long original body that compresses well";
    let framed = "sig summary".to_string();
    let framed_tokens = count_tokens(&framed);
    assert_eq!(
        cap_to_raw(framed.clone(), framed_tokens, raw, 100),
        framed,
        "output at or below raw must be returned untouched"
    );
}

#[test]
fn cap_to_raw_keeps_framing_for_empty_file() {
    // An empty file has zero content tokens; keep the framing so the reader
    // still gets an "empty / 0L" signal rather than a blank payload.
    let framed = "F1=empty.rs 0L".to_string();
    let framed_tokens = count_tokens(&framed);
    assert_eq!(
        cap_to_raw(framed.clone(), framed_tokens, "", 0),
        framed,
        "empty files keep their framing signal"
    );
}

#[test]
fn auto_read_never_inflates_small_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("small.rs");
    let p = path.to_string_lossy().to_string();
    let content =
        "use std::io;\n\npub fn greet(name: &str) -> String {\n    format!(\"hi {name}\")\n}\n";
    std::fs::write(&path, content).unwrap();

    let mut cache = SessionCache::new();
    let out = handle_with_task_resolved(&mut cache, &p, "auto", CrpMode::Off, None);
    assert!(
        out.output_tokens <= count_tokens(content),
        "auto cold read inflated a small file: {} output tok > {} raw tok\n{}",
        out.output_tokens,
        count_tokens(content),
        out.content
    );
}

#[test]
fn full_read_never_inflates_tiny_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tiny.rs");
    let p = path.to_string_lossy().to_string();
    let content = "pub fn a() {}\n";
    std::fs::write(&path, content).unwrap();

    let mut cache = SessionCache::new();
    let out = handle_with_task_resolved(&mut cache, &p, "full", CrpMode::Off, None);
    assert!(
        out.output_tokens <= count_tokens(content),
        "full cold read inflated a tiny file: {} > {}\n{}",
        out.output_tokens,
        count_tokens(content),
        out.content
    );
}

#[test]
fn auto_read_still_compresses_large_file() {
    // Isolate learning state so the resolver falls through to the size
    // heuristic (large code file → map), proving the guard never blocks a
    // genuine compression win.
    let _iso = crate::core::data_dir::isolated_data_dir();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("big.rs");
    let p = path.to_string_lossy().to_string();
    let mut content = String::new();
    for i in 0..400 {
        content.push_str(&format!(
            "pub fn function_number_{i}(x: i32, y: i32) -> i32 {{\n    let z = x + y + {i};\n    z * 2\n}}\n\n"
        ));
    }
    std::fs::write(&path, &content).unwrap();

    let mut cache = SessionCache::new();
    let out = handle_with_task_resolved(&mut cache, &p, "auto", CrpMode::Off, None);
    assert!(
        out.output_tokens < count_tokens(&content),
        "auto read of a large file must still compress: {} >= {} (mode={})",
        out.output_tokens,
        count_tokens(&content),
        out.resolved_mode
    );
}

#[test]
fn explicit_compressed_mode_capped_on_tiny_file() {
    // #361 now applies to explicit modes too: asking for `signatures` of a tiny
    // file must never cost more tokens than reading it raw. (On a tiny file the
    // capped result is the raw content, which still carries the symbols.)
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("lib.rs");
    let p = path.to_string_lossy().to_string();
    let content = "pub fn alpha() {}\npub fn beta() {}\n";
    std::fs::write(&path, content).unwrap();

    let mut cache = SessionCache::new();
    let out = handle_with_task_resolved(&mut cache, &p, "signatures", CrpMode::Off, None);
    assert!(
        out.output_tokens <= count_tokens(content),
        "explicit signatures of a tiny file must not inflate past raw: {} > {}\n{}",
        out.output_tokens,
        count_tokens(content),
        out.content
    );
}

#[test]
fn explicit_signatures_still_compresses_large_file() {
    // Capping explicit modes must not break legitimate compression: signatures
    // of a large file are far smaller than raw, so the cap is a no-op and the
    // compressed view (not raw) is returned.
    let _iso = crate::core::data_dir::isolated_data_dir();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("big.rs");
    let p = path.to_string_lossy().to_string();
    let mut content = String::new();
    for i in 0..400 {
        content.push_str(&format!(
            "pub fn function_number_{i}(x: i32, y: i32) -> i32 {{\n    let z = x + y + {i};\n    z * 2\n}}\n\n"
        ));
    }
    std::fs::write(&path, &content).unwrap();

    let mut cache = SessionCache::new();
    let out = handle_with_task_resolved(&mut cache, &p, "signatures", CrpMode::Off, None);
    assert!(
        out.output_tokens < count_tokens(&content),
        "explicit signatures of a large file must compress: {} >= {}",
        out.output_tokens,
        count_tokens(&content)
    );
}

#[test]
fn cache_hit_stub_is_byte_stable_across_rereads() {
    // #498 determinism: re-reading an unchanged file must yield byte-identical
    // output (no read-count note, no rotating proof line) so provider prompt
    // caching applies to the repeated stub.
    let _iso = crate::core::data_dir::isolated_data_dir();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("stable.rs");
    let p = path.to_string_lossy().to_string();
    std::fs::write(&path, "pub fn alpha() {}\npub fn beta() {}\n").unwrap();

    let mut cache = SessionCache::new();
    // Prime: the first full read marks full content as delivered.
    let _ = handle_with_task_resolved(&mut cache, &p, "full", CrpMode::Off, None);
    let r2 = handle_with_task_resolved(&mut cache, &p, "full", CrpMode::Off, None);
    let r3 = handle_with_task_resolved(&mut cache, &p, "full", CrpMode::Off, None);
    let r4 = handle_with_task_resolved(&mut cache, &p, "full", CrpMode::Off, None);
    assert_eq!(
        r2.content, r3.content,
        "re-read drifted between reads 2 and 3"
    );
    assert_eq!(
        r3.content, r4.content,
        "re-read drifted between reads 3 and 4"
    );
    assert!(
        !r2.content.contains("(read"),
        "read-count note must not appear in the cache-hit body: {}",
        r2.content
    );
}

// ---------------------------------------------------------------------------
// delta_explicit: serve explicit full/lines re-reads of changed cached files as
// diffs (opt-in). The decision is the pure `resolve_explicit_delta_mode`; the
// end-to-end diff base is exercised via the engine. Mirrors the
// `try_stub_hit_readonly` staleness-test conventions above.
// ---------------------------------------------------------------------------

/// Prime the cache with a full read of the file already on disk at `p`.
fn primed_full_cache(p: &str) -> SessionCache {
    let mut cache = SessionCache::new();
    let _ = handle_with_task_resolved(&mut cache, p, "full", CrpMode::Off, None);
    debug_assert!(
        cache.is_full_delivered(p),
        "fixture must deliver full content"
    );
    cache
}

/// Regression: an `auto` re-read of an unchanged, already-fully-delivered file
/// must collapse to the cheap `[unchanged]` stub — not re-deliver the whole body.
/// The auto path used to resolve modes with `cache: None`, so the resolver's
/// `("full","cache_hit")` short-circuit was dead and every `auto` re-read re-sent
/// the file ("re-reads aren't cached"). The cache-aware resolver restores it.
#[test]
fn auto_reread_of_fully_delivered_file_serves_unchanged_stub() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("warm.rs");
    let p = path.to_string_lossy().to_string();
    // Body big enough that a full re-delivery dwarfs the ~13-token stub.
    let body = (0..48)
        .map(|i| format!("fn function_number_{i}() {{ let value_{i} = {i} * 2; }}"))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&path, format!("{body}\n")).unwrap();

    // Cost of a full delivery, measured on a cold cache.
    let mut cold = SessionCache::new();
    let full = handle_with_task_resolved(&mut cold, &p, "full", CrpMode::Off, None);
    assert!(
        !full.content.contains("[unchanged"),
        "cold full read must deliver the body, not a stub"
    );

    // Warm cache: full body already delivered, file unchanged on disk.
    let mut cache = primed_full_cache(&p);
    let reread = handle_with_task_resolved(&mut cache, &p, "auto", CrpMode::Off, None);
    assert!(
        reread.content.contains("[unchanged"),
        "auto re-read of an unchanged fully-delivered file must serve the stub, got: {}",
        reread.content
    );
    assert!(
        reread.output_tokens.saturating_mul(4) < full.output_tokens,
        "stub ({} tok) must be far cheaper than a full re-delivery ({} tok)",
        reread.output_tokens,
        full.output_tokens
    );
}

#[test]
fn delta_explicit_changed_file_diverts_full_reread_to_diff() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("changed.rs");
    let p = path.to_string_lossy().to_string();
    std::fs::write(&path, "fn main() {}\n").unwrap();

    let mut cache = primed_full_cache(&p);

    // File changes on disk after the first full read.
    std::thread::sleep(Duration::from_secs(1));
    std::fs::write(&path, "fn main() { changed(); }\n").unwrap();

    let decision = resolve_explicit_delta_mode(
        &cache, &p, "full", /*explicit*/ true, /*fresh*/ false, true,
    );
    assert_eq!(
        decision.mode, "diff",
        "changed full re-read must divert to diff"
    );
    let note = decision
        .note
        .expect("a diff diversion must carry an advisory note");
    assert!(
        note.contains("[delta-explicit]"),
        "note tag missing: {note}"
    );
    assert!(
        note.contains("fresh=true"),
        "note must mention the bypass: {note}"
    );

    // End-to-end: the engine renders the diff against the FULL cached content.
    let out = handle_with_task_resolved(&mut cache, &p, "diff", CrpMode::Off, None);
    assert_eq!(out.resolved_mode, "diff");
    assert!(
        out.content.contains("[diff]"),
        "engine must emit a diff: {}",
        out.content
    );
    assert!(
        out.content.contains("changed()"),
        "diff must reflect the new on-disk content: {}",
        out.content
    );
}

#[test]
fn delta_explicit_changed_lines_request_diverts_to_diff() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("lines.rs");
    let p = path.to_string_lossy().to_string();
    std::fs::write(&path, "fn a() {}\nfn b() {}\n").unwrap();

    let cache = primed_full_cache(&p);

    std::thread::sleep(Duration::from_secs(1));
    std::fs::write(&path, "fn a() { x(); }\nfn b() {}\n").unwrap();

    let decision = resolve_explicit_delta_mode(&cache, &p, "lines:1-1", true, false, true);
    assert_eq!(
        decision.mode, "diff",
        "a changed-file lines: re-read must divert to diff, not re-extract a window"
    );
    assert!(decision.note.is_some());
}

#[test]
fn delta_explicit_diff_base_is_full_cached_content_not_compressed() {
    // Fix #2 guard: the diff base must be the full source the cache stored, even
    // when the most recent read of the file was a COMPRESSED view (map). If the
    // base were the compressed view, the diff would be garbage.
    let _iso = crate::core::data_dir::isolated_data_dir();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("big.rs");
    let p = path.to_string_lossy().to_string();
    let mut content = String::new();
    for i in 0..60 {
        content.push_str(&format!(
            "pub fn original_fn_{i}(x: i32) -> i32 {{ x + {i} }}\n"
        ));
    }
    std::fs::write(&path, &content).unwrap();

    let mut cache = SessionCache::new();
    // Cache the full content, then read a compressed (map) view — last_mode=map,
    // but the entry still stores the full source.
    let _ = handle_with_task_resolved(&mut cache, &p, "full", CrpMode::Off, None);
    let _ = handle_with_task_resolved(&mut cache, &p, "map", CrpMode::Off, None);

    // Change exactly one line on disk.
    std::thread::sleep(Duration::from_secs(1));
    let changed = content.replace(
        "pub fn original_fn_7(x: i32) -> i32 { x + 7 }",
        "pub fn original_fn_7(x: i32) -> i32 { x + 70707 }",
    );
    std::fs::write(&path, &changed).unwrap();

    let out = handle_with_task_resolved(&mut cache, &p, "diff", CrpMode::Off, None);
    assert!(
        out.content.contains("[diff]"),
        "expected a diff: {}",
        out.content
    );
    // The marker appears only if the diff compared against the FULL original
    // source (a compressed map base would never contain this literal).
    assert!(
        out.content.contains("70707"),
        "diff must be computed against full cached source, got: {}",
        out.content
    );
    // And it must be a one-line edit, not a wholesale replacement of a
    // compressed base against the full file.
    assert!(
        out.content.contains("+1/-1") || out.content.contains("-1/+1"),
        "single-line change should diff as +1/-1: {}",
        out.content
    );
}

#[test]
fn delta_explicit_unchanged_lines_collapse_to_full_stub() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("same.rs");
    let p = path.to_string_lossy().to_string();
    std::fs::write(&path, "fn a() {}\nfn b() {}\n").unwrap();

    let cache = primed_full_cache(&p);

    // No disk change. A lines: re-read of a fully-delivered file re-emits text
    // the model holds → collapse to the full-mode stub (no diff, no note).
    let decision = resolve_explicit_delta_mode(&cache, &p, "lines:1-1", true, false, true);
    assert_eq!(
        decision.mode, "full",
        "unchanged lines: of a full file must collapse to the stub"
    );
    assert!(
        decision.note.is_none(),
        "a silent stub collapse must not carry a note"
    );
}

#[test]
fn delta_explicit_unchanged_full_reread_is_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("same.rs");
    let p = path.to_string_lossy().to_string();
    std::fs::write(&path, "fn a() {}\n").unwrap();

    let cache = primed_full_cache(&p);

    // An unchanged full re-read already hits the downstream `[unchanged]` stub;
    // the resolver leaves it untouched.
    let decision = resolve_explicit_delta_mode(&cache, &p, "full", true, false, true);
    assert_eq!(decision.mode, "full");
    assert!(decision.note.is_none());
}

#[test]
fn delta_explicit_off_preserves_current_behavior() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("changed.rs");
    let p = path.to_string_lossy().to_string();
    std::fs::write(&path, "fn main() {}\n").unwrap();

    let cache = primed_full_cache(&p);

    std::thread::sleep(Duration::from_secs(1));
    std::fs::write(&path, "fn main() { changed(); }\n").unwrap();

    // enabled=false → the mode is never rewritten, no matter the disk state.
    let decision =
        resolve_explicit_delta_mode(&cache, &p, "full", true, false, /*enabled*/ false);
    assert_eq!(
        decision.mode, "full",
        "feature OFF must preserve the requested mode"
    );
    assert!(decision.note.is_none());

    let lines = resolve_explicit_delta_mode(&cache, &p, "lines:1-1", true, false, false);
    assert_eq!(
        lines.mode, "lines:1-1",
        "feature OFF must not touch lines: either"
    );
    assert!(lines.note.is_none());
}

#[test]
fn delta_explicit_fresh_bypasses() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("changed.rs");
    let p = path.to_string_lossy().to_string();
    std::fs::write(&path, "fn main() {}\n").unwrap();

    let cache = primed_full_cache(&p);

    std::thread::sleep(Duration::from_secs(1));
    std::fs::write(&path, "fn main() { changed(); }\n").unwrap();

    // fresh=true → always bypass even with the feature on and a changed file.
    let decision = resolve_explicit_delta_mode(&cache, &p, "full", true, /*fresh*/ true, true);
    assert_eq!(
        decision.mode, "full",
        "fresh=true must bypass the diff diversion"
    );
    assert!(decision.note.is_none());
}

#[test]
fn delta_explicit_first_read_unaffected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("new.rs");
    let p = path.to_string_lossy().to_string();
    std::fs::write(&path, "fn main() {}\n").unwrap();

    // Nothing cached yet — the very first read can never be a diff.
    let cache = SessionCache::new();
    let decision = resolve_explicit_delta_mode(&cache, &p, "full", true, false, true);
    assert_eq!(
        decision.mode, "full",
        "an uncached first read must be served normally"
    );
    assert!(decision.note.is_none());

    let lines = resolve_explicit_delta_mode(&cache, &p, "lines:1-1", true, false, true);
    assert_eq!(lines.mode, "lines:1-1");
    assert!(lines.note.is_none());
}

#[test]
fn delta_explicit_only_fires_for_explicit_mode() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("changed.rs");
    let p = path.to_string_lossy().to_string();
    std::fs::write(&path, "fn main() {}\n").unwrap();

    let cache = primed_full_cache(&p);

    std::thread::sleep(Duration::from_secs(1));
    std::fs::write(&path, "fn main() { changed(); }\n").unwrap();

    // explicit_mode=false (mode was auto-resolved) → never diverted; auto-mode
    // already has its own staleness handling.
    let decision =
        resolve_explicit_delta_mode(&cache, &p, "full", /*explicit*/ false, false, true);
    assert_eq!(
        decision.mode, "full",
        "auto-resolved modes must not be diverted to diff"
    );
    assert!(decision.note.is_none());
}

#[test]
fn delta_explicit_decision_is_byte_stable() {
    // #498 determinism: the resolver's note carries no timestamp/counter, so
    // repeated calls on the same changed-file state are byte-identical.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("changed.rs");
    let p = path.to_string_lossy().to_string();
    std::fs::write(&path, "fn main() {}\n").unwrap();

    let cache = primed_full_cache(&p);
    std::thread::sleep(Duration::from_secs(1));
    std::fs::write(&path, "fn main() { changed(); }\n").unwrap();

    let d1 = resolve_explicit_delta_mode(&cache, &p, "full", true, false, true);
    let d2 = resolve_explicit_delta_mode(&cache, &p, "full", true, false, true);
    assert_eq!(
        d1, d2,
        "delta-explicit decision drifted between identical calls"
    );
}
