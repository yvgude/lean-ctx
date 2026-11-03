// SPDX-License-Identifier: Apache-2.0
use super::*;
use std::time::Duration;

/// #1287: the variant stub is a pure function of (file_ref, path, mode) —
/// byte-stable across identical re-reads (#498, provider prompt caching), tiny,
/// and always carries the static `fresh=true` escape.
#[test]
fn variant_stub_is_byte_stable_and_tiny() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    lean_ctx::test_env::set_var("LEAN_CTX_META", "1");
    let a = super::dispatch::render_unchanged_variant_stub("F3", "/src/router.js", "signatures");
    let b = super::dispatch::render_unchanged_variant_stub("F3", "/src/router.js", "signatures");
    assert_eq!(a.content, b.content, "identical inputs must be byte-stable");
    assert!(a.content.contains("unchanged signatures view"));
    assert!(a.content.contains("fresh=true"));
    assert!(a.is_cache_hit);
    assert_eq!(a.resolved_mode, "signatures");
    assert!(
        a.output_tokens < 40,
        "stub must stay tiny, got {} tokens",
        a.output_tokens
    );
    lean_ctx::test_env::remove_var("LEAN_CTX_META");
}

/// #1287 gate invariants, valid in every environment: when a conversation
/// scope is resolvable (e.g. CLAUDECODE per-process scope), the same-scope
/// variant re-read collapses to the tiny stub with the `fresh=true` escape;
/// without a provably delivered conversation the full payload is re-served
/// byte-identically — the stub must never appear on unknown delivery.
#[test]
fn variant_reread_stub_or_payload_invariants() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    let dir = std::env::temp_dir().join(format!("lean_ctx_vstub_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let p = dir.join("sample.rs");
    let mut body = String::new();
    for i in 0..80 {
        use std::fmt::Write as _;
        let _ = writeln!(
            body,
            "pub fn generated_function_{i}(value: usize) -> usize {{ value + {i} }}"
        );
    }
    std::fs::write(&p, &body).expect("write sample file");
    let path = p.to_str().expect("temp path is valid UTF-8");

    let mut cache = SessionCache::new();
    let first = super::dispatch::handle(&mut cache, path, "signatures", CrpMode::Off);
    let second = super::dispatch::handle(&mut cache, path, "signatures", CrpMode::Off);
    let _ = std::fs::remove_file(&p);

    assert!(
        !first.contains("unchanged signatures view"),
        "first delivery must always be the real payload"
    );
    if second.contains("unchanged signatures view") {
        // Positive path: a conversation scope was resolvable here.
        assert!(second.contains("fresh=true"), "stub must carry the escape");
        assert!(
            second.len() < first.len() / 3,
            "stub must be a fraction of the payload ({} vs {})",
            second.len(),
            first.len()
        );
    } else if second.contains("already in context") {
        // Pre-existing legacy short-circuit (conversation scoping explicitly
        // disabled: one daemon == one conversation by contract) — its own
        // stub, not ours; small by construction.
        assert!(second.len() < first.len() / 3);
    } else {
        // Conservative path: unknown delivery -> byte-identical payload.
        assert_eq!(first, second, "variant cache hit must be byte-stable");
    }
}

#[test]
fn test_header_toon_format_no_brackets() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    lean_ctx::test_env::set_var("LEAN_CTX_META", "1");
    let content = "use std::io;\nfn main() {}\n";
    let header = build_header("F1", "main.rs", "rs", content, 2, false);
    assert!(!header.contains('['));
    assert!(!header.contains(']'));
    assert!(header.contains("F1=main.rs 2L"));
    lean_ctx::test_env::remove_var("LEAN_CTX_META");
}

#[test]
fn test_header_toon_deps_indented() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    lean_ctx::test_env::set_var("LEAN_CTX_META", "1");
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
    lean_ctx::test_env::remove_var("LEAN_CTX_META");
}

#[test]
fn test_header_toon_saves_tokens() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    lean_ctx::test_env::set_var("LEAN_CTX_META", "1");
    let content = "use crate::foo;\nuse crate::bar;\npub fn baz() {}\npub fn qux() {}\n";
    let old_header = "F1=main.rs [4L +] deps:[foo,bar] exports:[baz,qux]".to_string();
    let new_header = build_header("F1", "main.rs", "rs", content, 4, true);
    let old_tokens = count_tokens(&old_header);
    let new_tokens = count_tokens(&new_header);
    assert!(
        new_tokens <= old_tokens,
        "TOON header ({new_tokens} tok) should be <= old format ({old_tokens} tok)"
    );
    lean_ctx::test_env::remove_var("LEAN_CTX_META");
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
    // #840: file must exceed TASK_MIN_LINES (250) to engage filtering.
    let content = (0..300)
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
    let _lock = lean_ctx::core::data_dir::test_env_lock();
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
    let content = generate_benchmark_code(300);
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

// `lines:5,10-12` is multi-select (comma-separated lines/ranges), not a range.
// Callers who meant a span get stray lines back — the output must say what
// the comma did so the mistake is visible (limitations audit 2026-07-03, #7).
#[test]
fn lines_comma_multiselect_gets_hint() {
    let content = (1..=30)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let tokens = count_tokens(&content);
    let (out, _) = process_mode(
        &content,
        "lines:5,10-12",
        "F1",
        "t.txt",
        "txt",
        tokens,
        CrpMode::Off,
        "t.txt",
        None,
    );
    assert!(out.contains("line 5") && out.contains("line 10"), "{out}");
    assert!(
        out.contains("multi-select"),
        "comma form must carry a hint: {out}"
    );
}

// A map/signatures read of a language with no extractor must say so instead of
// returning a bare header the caller mistakes for "file has no API"
// (limitations audit 2026-07-03, #4 residual).
#[test]
fn map_on_unsupported_language_carries_marker() {
    let content = "some plain text\nwith no code\nstructure at all\n";
    let tokens = count_tokens(content);
    let (out, _) = process_mode(
        content,
        "map",
        "F1",
        "notes.txt",
        "txt",
        tokens,
        CrpMode::Off,
        "notes.txt",
        None,
    );
    assert!(
        out.contains("no extractable structure"),
        "empty map must carry a marker: {out}"
    );
}

#[test]
fn signatures_on_unsupported_language_carries_marker() {
    let content = "some plain text\nwith no code\nstructure at all\n";
    let tokens = count_tokens(content);
    let (out, _) = process_mode(
        content,
        "signatures",
        "F1",
        "notes.txt",
        "txt",
        tokens,
        CrpMode::Off,
        "notes.txt",
        None,
    );
    assert!(
        out.contains("no extractable structure"),
        "empty signatures must carry a marker: {out}"
    );
}
