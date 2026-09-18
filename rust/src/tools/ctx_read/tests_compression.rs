use super::*;

// UTF-8 BOM must not leak into the first line of ctx_read output
// (limitations doc #11).
#[test]
fn read_file_lossy_strips_utf8_bom() {
    let p = std::env::temp_dir().join("lean_ctx_bom_test.txt");
    std::fs::write(&p, b"\xEF\xBB\xBFhello\n").unwrap();
    let s = read_file_lossy(p.to_str().unwrap()).unwrap();
    let _ = std::fs::remove_file(&p);
    assert!(
        !s.starts_with('\u{feff}'),
        "BOM must be stripped from read content"
    );
    assert!(
        s.starts_with("hello"),
        "content after BOM must survive: {s}"
    );
}

#[test]
fn compressed_cache_key_distinguishes_task() {
    let no_task = compressed_cache_key("map", CrpMode::Off, None, None, &[]);
    let tdd_no_task = compressed_cache_key("map", CrpMode::Tdd, None, None, &[]);
    let with_task = compressed_cache_key("map", CrpMode::Off, Some("fix login"), None, &[]);
    let other_task = compressed_cache_key("map", CrpMode::Off, Some("refactor db"), None, &[]);
    assert_eq!(no_task, "map:v2");
    assert_eq!(tdd_no_task, "map:v2:tdd");
    // #E26: map/signatures are structure-preserving and task-independent.
    // Their cache key must NOT vary with task to improve provider cache hits.
    assert_eq!(with_task, no_task, "map key must be task-independent");
    assert_eq!(with_task, other_task, "map key must be task-independent");

    // Task-dependent modes MUST still distinguish tasks.
    let density_a = compressed_cache_key("density:0.3", CrpMode::Off, Some("fix login"), None, &[]);
    let density_b =
        compressed_cache_key("density:0.3", CrpMode::Off, Some("refactor db"), None, &[]);
    assert_ne!(density_a, density_b, "density key must vary with task");
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

/// #1794: classification is by file, not by ancestor directory. A skill ships
/// its instructions as documents and its implementation as source; forcing the
/// latter to `full` turned a bounded `map` request into a truncated dump.
#[test]
fn source_files_under_a_skill_are_not_instruction_files() {
    for path in [
        "/project/.agents/skills/example/scripts/runtime.ts",
        "/project/.agents/skills/example/scripts/state.ts",
        "/project/skills/demo/helper.py",
        "/project/skills/demo/lib.rs",
        "/workspace/.cursor/rules/generate.js",
        "/home/user/.claude/rules/build.sh",
    ] {
        assert!(
            !is_instruction_file(path),
            "{path} is implementation, not an instruction document"
        );
    }
}

/// The documents themselves must keep their full-read guarantee, including
/// extensionless rule files.
#[test]
fn documents_under_a_skill_remain_instruction_files() {
    for path in [
        "/project/.agents/skills/example/SKILL.md",
        "/project/skills/demo/notes.txt",
        "/project/skills/demo/guide.markdown",
        "/workspace/.cursor/rules/house-style.mdc",
        "/home/user/.claude/rules/PROMPT",
    ] {
        assert!(is_instruction_file(path), "{path} carries instructions");
    }
}

#[test]
fn resolve_auto_mode_returns_full_for_instruction_files() {
    let mode = resolve_auto_mode(
        None,
        "/home/user/.pi/agent/skills/committing-changes/SKILL.md",
        5000,
        None,
        Some("read"),
    );
    assert_eq!(mode, "full", "SKILL.md must always be read in full");

    let mode = resolve_auto_mode(None, "/workspace/AGENTS.md", 3000, None, Some("read"));
    assert_eq!(mode, "full", "AGENTS.md must always be read in full");

    let mode = resolve_auto_mode(None, "/workspace/.cursorrules", 2000, None, None);
    assert_eq!(mode, "full", ".cursorrules must always be read in full");
}
