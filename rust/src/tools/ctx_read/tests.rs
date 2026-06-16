//! Tests for `ctx_read`. Extracted from `ctx_read/mod.rs`;
//! `super::*` resolves to the `ctx_read` module.

use super::*;
use std::time::Duration;

#[test]
fn test_header_toon_format_no_brackets() {
    let _lock = crate::core::data_dir::test_env_lock();
    unsafe { std::env::set_var("LEAN_CTX_META", "1") };
    let content = "use std::io;\nfn main() {}\n";
    let header = build_header("F1", "main.rs", "rs", content, 2, false);
    assert!(!header.contains('['));
    assert!(!header.contains(']'));
    assert!(header.contains("F1=main.rs 2L"));
    unsafe { std::env::remove_var("LEAN_CTX_META") };
}

#[test]
fn test_header_toon_deps_indented() {
    let _lock = crate::core::data_dir::test_env_lock();
    unsafe { std::env::set_var("LEAN_CTX_META", "1") };
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
    unsafe { std::env::remove_var("LEAN_CTX_META") };
}

#[test]
fn test_header_toon_saves_tokens() {
    let _lock = crate::core::data_dir::test_env_lock();
    unsafe { std::env::set_var("LEAN_CTX_META", "1") };
    let content = "use crate::foo;\nuse crate::bar;\npub fn baz() {}\npub fn qux() {}\n";
    let old_header = "F1=main.rs [4L +] deps:[foo,bar] exports:[baz,qux]".to_string();
    let new_header = build_header("F1", "main.rs", "rs", content, 4, true);
    let old_tokens = count_tokens(&old_header);
    let new_tokens = count_tokens(&new_header);
    assert!(
        new_tokens <= old_tokens,
        "TOON header ({new_tokens} tok) should be <= old format ({old_tokens} tok)"
    );
    unsafe { std::env::remove_var("LEAN_CTX_META") };
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
    let no_task = compressed_cache_key("map", CrpMode::Off, None);
    let tdd_no_task = compressed_cache_key("map", CrpMode::Tdd, None);
    let with_task = compressed_cache_key("map", CrpMode::Off, Some("fix login"));
    let other_task = compressed_cache_key("map", CrpMode::Off, Some("refactor db"));
    // Versioned so stale pre-line-range entries cannot be served.
    assert_eq!(no_task, "map:v2");
    assert_eq!(tdd_no_task, "map:v2:tdd");
    assert_ne!(with_task, no_task);
    assert_ne!(with_task, other_task);
}

#[test]
fn map_mode_includes_signature_line_ranges() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("lib.rs");
    let p = path.to_string_lossy().to_string();
    std::fs::write(
        &path,
        "pub struct Config {}\n\npub fn build() -> Config { Config {} }\n",
    )
    .unwrap();

    let mut cache = SessionCache::new();
    let result = handle(&mut cache, &p, "map", CrpMode::Off);

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
    // exports that the API already covers (pure redundant tokens).
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("lib.rs");
    let p = path.to_string_lossy().to_string();
    std::fs::write(
        &path,
        "pub struct Config {}\n\npub fn build() -> Config { Config {} }\n",
    )
    .unwrap();

    let mut cache = SessionCache::new();
    let result = handle(&mut cache, &p, "map", CrpMode::Off);

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
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("lib.rs");
    let p = path.to_string_lossy().to_string();
    std::fs::write(
        &path,
        "pub struct Config {}\n\npub fn build() -> Config { Config {} }\n",
    )
    .unwrap();

    let mut cache = SessionCache::new();
    let result = handle(&mut cache, &p, "map", CrpMode::Tdd);
    assert!(
        result.contains("[λ=fn §=class +=pub]"),
        "TDD map output must carry the symbol legend: {result}"
    );

    let mut cache2 = SessionCache::new();
    let sigs = handle(&mut cache2, &p, "signatures", CrpMode::Tdd);
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
        "/home/user/.pi/agent/skills/committing-changes/SKILL.md",
        5000,
        Some("read"),
    );
    assert_eq!(mode, "full", "SKILL.md must always be read in full");

    let mode = resolve_auto_mode("/workspace/AGENTS.md", 3000, Some("read"));
    assert_eq!(mode, "full", "AGENTS.md must always be read in full");

    let mode = resolve_auto_mode("/workspace/.cursorrules", 2000, None);
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
    unsafe { std::env::remove_var("LEAN_CTX_SAVINGS_FOOTER") };
    unsafe { std::env::remove_var("LEAN_CTX_SHOW_SAVINGS") };
    unsafe { std::env::remove_var("LEAN_CTX_QUIET") };
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
// guard ships bare content (break-even, never a loss). Auto-resolved reads are
// guarded; an explicitly requested view is honoured verbatim.
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
fn explicit_compressed_mode_honoured_on_tiny_file() {
    // The guard must not override an explicit view: asking for `signatures` of
    // a tiny file returns signatures, even when that happens not to save.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("lib.rs");
    let p = path.to_string_lossy().to_string();
    std::fs::write(&path, "pub fn alpha() {}\npub fn beta() {}\n").unwrap();

    let mut cache = SessionCache::new();
    let out = handle_with_task_resolved(&mut cache, &p, "signatures", CrpMode::Off, None);
    assert_eq!(out.resolved_mode, "signatures");
    assert!(
        out.content.contains("alpha") && out.content.contains("beta"),
        "explicit signatures must be returned verbatim, not capped to raw: {}",
        out.content
    );
}
