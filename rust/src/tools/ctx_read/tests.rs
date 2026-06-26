//! Tests for `ctx_read`. Extracted from `ctx_read/mod.rs`;
//! `super::*` resolves to the `ctx_read` module.

use super::*;
use crate::core::tokens::count_tokens;

#[test]
fn test_header_toon_format_no_brackets() {
    let _lock = crate::core::data_dir::test_env_lock();
    crate::test_env::set_var("LEAN_CTX_META", "1");
    let content = "use std::io;\nfn main() {}\n";
    let header = build_header("", "main.rs", "rs", content, 2, false);
    assert!(!header.contains('['));
    assert!(!header.contains(']'));
    assert!(header.contains("main.rs 2L"));
    crate::test_env::remove_var("LEAN_CTX_META");
}

#[test]
fn test_header_toon_deps_indented() {
    let _lock = crate::core::data_dir::test_env_lock();
    crate::test_env::set_var("LEAN_CTX_META", "1");
    let content = "use crate::core::types;\nuse crate::tools;\npub fn main() {}\n";
    let header = build_header("", "main.rs", "rs", content, 3, true);
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
    let old_header = "main.rs [4L +] deps:[foo,bar] exports:[baz,qux]".to_string();
    let new_header = build_header("", "main.rs", "rs", content, 4, true);
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
fn map_mode_inlines_task_relevant_body() {
    let content = "pub fn alpha() {\n    let a = 1;\n}\n\npub fn validate_token(t: &str) -> bool {\n    let ok = check(t);\n    ok\n}\n";
    let with_task = render_content(
        content,
        "test.rs",
        &ReadMode::Map,
        CrpMode::Off,
        Some("fix bug in validate_token"),
    );
    assert!(
        with_task.content.contains("▸ body") && with_task.content.contains("validate_token"),
        "map with task should inline the matching body: {}",
        with_task.content
    );
    let no_task = render_content(content, "test.rs", &ReadMode::Map, CrpMode::Off, None);
    assert!(
        !no_task.content.contains("▸ body"),
        "map without a task must not inline a body: {}",
        no_task.content
    );
}

#[test]
fn map_mode_includes_signature_line_ranges() {
    let content = "pub struct Config {}\n\npub fn build() -> Config { Config {} }\n";
    let out = render_content(content, "/tmp/lib.rs", &ReadMode::Map, CrpMode::Off, None);
    assert!(
        out.content.contains("API:"),
        "map output should include API: {}",
        out.content
    );
    assert!(
        out.content.contains("pub struct Config @L1"),
        "struct signature should include line suffix: {}",
        out.content
    );
    assert!(
        out.content.contains("pub fn build() → Config @L3"),
        "function signature should include line suffix: {}",
        out.content
    );
}

#[test]
fn map_mode_omits_exports_already_in_api() {
    let content = "pub struct Config {}\n\npub fn build() -> Config { Config {} }\n";
    let out = render_content(content, "/tmp/lib.rs", &ReadMode::Map, CrpMode::Off, None);
    assert!(
        out.content.contains("pub struct Config") && out.content.contains("pub fn build"),
        "API section must still list exported symbols: {}",
        out.content
    );
    assert!(
        !out.content.contains("exports:"),
        "map must not repeat exports already shown in API: {}",
        out.content
    );
}

#[test]
fn tdd_map_output_carries_symbol_legend() {
    let content = "pub struct Config {}\n\npub fn build() -> Config { Config {} }\n";
    let map_out = render_content(content, "/tmp/lib.rs", &ReadMode::Map, CrpMode::Tdd, None);
    assert!(
        map_out.content.contains("[λ=fn §=class +=pub]"),
        "TDD map output must carry the symbol legend: {}",
        map_out.content
    );

    let sigs_out = render_content(
        content,
        "/tmp/lib.rs",
        &ReadMode::Signatures,
        CrpMode::Tdd,
        None,
    );
    assert!(
        sigs_out.content.contains("[λ=fn §=class +=pub]"),
        "TDD signatures output must carry the symbol legend: {}",
        sigs_out.content
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
    for mode in [ReadMode::Map, ReadMode::Signatures] {
        let run = || render_content(&content, "/tmp/stable.rs", &mode, CrpMode::Off, None).content;
        let first = run();
        let second = run();
        assert_eq!(
            first,
            second,
            "mode '{:?}' produced non-deterministic output",
            mode.label()
        );
    }
}

// ---------------------------------------------------------------------------

// ============================================================================
// ReadMode
// ============================================================================

#[test]
fn read_mode_label() {
    assert_eq!(ReadMode::Full(None).label(), "full");
    assert_eq!(ReadMode::Full(Some(LineRange::new(1, 10))).label(), "full");
    assert_eq!(ReadMode::Signatures.label(), "signatures");
    assert_eq!(ReadMode::Map.label(), "map");
    assert_eq!(ReadMode::Diff.label(), "diff");
    assert_eq!(ReadMode::Diff.label(), "diff");
}

#[test]
fn read_mode_supports_range() {
    assert!(ReadMode::Full(None).supports_range());
    assert!(ReadMode::Full(Some(LineRange::new(1, 5))).supports_range());
    assert!(!ReadMode::Signatures.supports_range());
    assert!(!ReadMode::Map.supports_range());
    assert!(!ReadMode::Diff.supports_range());
}

// ============================================================================
// LineRange
// ============================================================================

#[test]
fn line_range_valid() {
    let r = LineRange::new(1, 1);
    assert_eq!(r.start, 1);
    assert_eq!(r.end, 1);

    let r = LineRange::new(1, 100);
    assert_eq!(r.start, 1);
    assert_eq!(r.end, 100);

    let r = LineRange::new(42, 99);
    assert_eq!(r.start, 42);
    assert_eq!(r.end, 99);
}

#[test]
#[should_panic(expected = "must be \u{2265} 1")]
fn line_range_panics_on_zero_start() {
    let _ = LineRange::new(0, 5);
}

#[test]
#[should_panic(expected = "must be \u{2265} start")]
fn line_range_panics_on_end_less_than_start() {
    let _ = LineRange::new(5, 3);
}

// ============================================================================
// render_content — Full mode
// ============================================================================

#[test]
fn render_content_full_mode_returns_framed_content() {
    let content = "pub fn alpha() {}\npub fn beta() {}\nfn gamma() {}\n";
    let out = render_content(
        content,
        "/tmp/test.rs",
        &ReadMode::Full(None),
        CrpMode::Off,
        None,
    );
    // Full mode returns framed output: header + content (no cap_to_raw)
    assert!(
        out.content.contains(content.trim()),
        "content should be in output: {}",
        out.content
    );
    assert!(
        out.content.contains("test.rs"),
        "header should contain path: {}",
        out.content
    );
    assert!(
        out.content.contains("3L"),
        "header should contain line count: {}",
        out.content
    );
    assert_eq!(out.original_tokens, count_tokens(content));
    assert_eq!(out.mode, ReadMode::Full(None));
}

#[test]
fn render_content_full_with_range_selects_lines() {
    let content = "line1\nline2\nline3\nline4\nline5\n";
    let range = LineRange::new(2, 4);
    let out = render_content(
        content,
        "/tmp/test.rs",
        &ReadMode::Full(Some(range)),
        CrpMode::Off,
        None,
    );
    // Full mode always returns framed output; assert only that the ranged content is present
    assert!(
        out.content.contains("line2"),
        "ranged content should be in output: {}",
        out.content
    );
    assert!(
        !out.content.contains("line1"),
        "ranged content excludes pre-range"
    );
    assert!(
        !out.content.contains("line5"),
        "ranged content excludes post-range"
    );
}

#[test]
fn render_content_full_empty_file() {
    let out = render_content(
        "",
        "/tmp/empty.rs",
        &ReadMode::Full(None),
        CrpMode::Off,
        None,
    );
    assert!(out.content.contains("empty.rs") && out.content.contains("0L"));
}

// ============================================================================
// render_content — Signatures mode
// ============================================================================

#[test]
fn render_content_signatures_mode() {
    let mut content = String::new();
    for i in 0..80 {
        content.push_str(&format!("pub fn fn_{i}(x: i32) -> i32 {{ x + {i} }}\n"));
    }
    let out = render_content(
        &content,
        "/tmp/lib.rs",
        &ReadMode::Signatures,
        CrpMode::Off,
        None,
    );
    assert!(
        out.content.contains("lib.rs"),
        "output should contain short path"
    );
    assert!(
        out.output_tokens < out.original_tokens,
        "signatures of a large file must compress"
    );
}

#[test]
fn render_content_signatures_tdd_output() {
    let content = "pub struct Config {}\npub fn build() -> Config { Config {} }\n";
    let out = render_content(
        content,
        "/tmp/lib.rs",
        &ReadMode::Signatures,
        CrpMode::Tdd,
        None,
    );
    assert!(out.content.contains("[λ=fn §=class +=pub]"));
}

// ============================================================================
// render_content — Map mode
// ============================================================================

#[test]
fn render_content_map_mode() {
    let content = "pub struct X {}\npub fn do_thing() -> X { X {} }\n";
    let out = render_content(content, "/tmp/lib.rs", &ReadMode::Map, CrpMode::Off, None);
    assert!(out.content.contains("lib.rs"));
    assert!(out.content.contains("API:"));
    assert!(out.content.contains("pub struct X @L1"));
    assert!(out.content.contains("pub fn do_thing() → X @L2"));
}

#[test]
fn render_content_map_mode_tdd() {
    let content = "pub struct Config {}\npub fn build() -> Config { Config {} }\n";
    let out = render_content(content, "/tmp/lib.rs", &ReadMode::Map, CrpMode::Tdd, None);
    assert!(out.content.contains("[λ=fn §=class +=pub]"));
    assert!(out.content.contains("API:"));
}

// ============================================================================
// render_content — Diff mode
// ============================================================================

#[test]
fn render_content_diff_git() {
    let content = "fn main() {}\n";
    let out = render_content(content, "/tmp/diff.rs", &ReadMode::Diff, CrpMode::Off, None);
    // Should produce either a git diff or a "no changes" message
    assert!(
        out.content.contains("diff.rs")
            || out.content.contains("no uncommitted changes")
            || out.content.contains("git diff failed")
            || out.content.contains("HEAD"),
        "diff output should contain relevant git info: {}",
        out.content
    );
}

// ============================================================================
// render_content — determinism (#498)
// ============================================================================

#[test]
fn render_content_is_deterministic() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    crate::test_env::remove_var("LEAN_CTX_SAVINGS_FOOTER");
    crate::test_env::remove_var("LEAN_CTX_SHOW_SAVINGS");
    crate::test_env::remove_var("LEAN_CTX_QUIET");

    let content = "pub fn alpha() {}\npub fn beta() {}\nfn gamma() {}\n";
    for mode in [
        ReadMode::Full(None),
        ReadMode::Signatures,
        ReadMode::Map,
        // Diff mode is non-deterministic (runs git) — not tested here
    ] {
        let first = render_content(content, "/tmp/deterministic.rs", &mode, CrpMode::Off, None);
        let second = render_content(content, "/tmp/deterministic.rs", &mode, CrpMode::Off, None);
        assert_eq!(
            first.content,
            second.content,
            "mode {:?} produced non-deterministic output",
            mode.label()
        );
        assert_eq!(
            first.output_tokens,
            second.output_tokens,
            "mode {:?} produced non-deterministic token count",
            mode.label()
        );
    }
}

// ============================================================================
// read() function (disk I/O)
// ============================================================================

#[test]
fn read_function_reads_file_from_disk() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hello.rs");
    let content = "fn greet() -> &'static str { \"hello\" }\n";
    std::fs::write(&path, content).unwrap();
    let p = path.to_string_lossy().to_string();

    let out = read(&p, &ReadMode::Map, CrpMode::Off, None).unwrap();
    assert!(out.content.contains("greet") || out.content.contains("lib"));
}

#[test]
fn read_function_returns_error_for_nonexistent_file() {
    let err = read(
        "/tmp/nonexistent-file-for-test-xyz123.rs",
        &ReadMode::Full(None),
        CrpMode::Off,
        None,
    )
    .unwrap_err();
    match err {
        ReadError::NotFound(_) => {} // expected
        other => panic!("expected NotFound, got: {other}"),
    }
}

// ============================================================================
// append_compressed_hint
// ============================================================================

#[test]
fn append_compressed_hint_identity_when_disabled() {
    let output = "some content".to_string();
    let result = append_compressed_hint(&output, "/tmp/test.rs");
    assert_eq!(result, output, "hint disabled by default, output unchanged");
}

// ============================================================================
// ReadOutput struct
// ============================================================================

#[test]
fn read_output_contains_expected_fields() {
    let content = "pub fn f() {}\n";
    let out = render_content(content, "/tmp/f.rs", &ReadMode::Map, CrpMode::Off, None);
    assert_eq!(out.original_tokens, count_tokens(content));
    assert!(
        out.content.contains("pub fn f() @L1"),
        "map output should include the function signature, got: {:?}",
        out.content
    );
    assert_eq!(out.mode, ReadMode::Map);
}
