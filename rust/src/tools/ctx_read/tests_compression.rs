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

/// #1910: an explicit request's raw fallback carries a banner, an `auto` one
/// does not — both resolve to the same mode, so the key must tell them apart.
/// Explicit keys stay byte-identical to their pre-#1910 form.
#[test]
fn request_scoped_key_separates_auto_from_explicit() {
    let base = compressed_cache_key("cognitive", CrpMode::Off, None, None, &[]);
    let explicit = request_scoped_key(base.clone(), false);
    let auto = request_scoped_key(base.clone(), true);
    assert_eq!(explicit, base);
    assert_ne!(auto, explicit);
    assert_eq!(auto, request_scoped_key(base, true), "deterministic (#498)");
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

/// #1911: `entropy` on real source used to save 0% (the absolute H/surprise
/// cut kept every line of a typical Rust file) and `aggressiveness` did not
/// move it. The file-relative surprise floor must shed real tokens as
/// aggressiveness rises, strictly more at each step.
///
/// Every shed run is marked `[lean-ctx: omitted N lines]`: the #1910 savings
/// on dense code came largely from *silently* deleting short structural lines
/// (`Vcs,`, `}`, a table row), which is cheaper than any honest marker. A
/// setting that cannot honestly beat the raw file therefore takes the labelled
/// "no compression applied" path instead of inventing savings.
#[test]
fn entropy_saves_on_real_source_and_scales_with_aggressiveness() {
    let _lock = crate::core::data_dir::test_env_lock();
    crate::test_env::set_var("LEAN_CTX_SHOW_SAVINGS", "0");
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/core/compressor.rs");
    let content = std::fs::read_to_string(&path).unwrap();
    let raw = count_tokens(&content);
    let render_at = |a: Option<f64>| -> String {
        process_mode_tuned(
            &content,
            "entropy",
            "F1",
            "compressor.rs",
            "rs",
            raw,
            CrpMode::Off,
            "src/core/compressor.rs",
            None,
            ReadTuning {
                aggressiveness: a,
                protect: &[],
            },
        )
        .0
    };
    let default = render_at(None);
    // The default threshold is file-adaptive (per-language base plus the
    // content's compressibility), not a point on the aggressiveness scale;
    // monotonicity is checked on explicit levels only.
    let low = count_tokens(&render_at(Some(0.1)));
    let mid = count_tokens(&render_at(Some(0.5)));
    let high_out = render_at(Some(0.9));
    let high = count_tokens(&high_out);
    let default_tokens = count_tokens(&default);
    let again = render_at(None);
    crate::test_env::remove_var("LEAN_CTX_SHOW_SAVINGS");

    assert_eq!(default, again, "entropy output must be deterministic");
    let honest = |out: &str, tokens: usize| {
        tokens < raw || out.starts_with("[lean-ctx] no compression applied")
    };
    assert!(
        honest(&default, default_tokens),
        "default inflated without saying so: {default_tokens} > {raw}"
    );
    assert!(
        !high_out.starts_with("[lean-ctx] no compression applied"),
        "aggressive entropy fell back on compressor.rs"
    );
    // Measured 2026-10-12: a=0.9 keeps 4104 of 4554 tok (~10%) with every
    // omission marked; the floor leaves headroom for tokenizer/fixture drift.
    assert!(
        high * 100 <= raw * 95,
        "aggressive entropy saves <5%: {high} of {raw} tok"
    );
    assert!(
        high_out.contains("[lean-ctx: omitted "),
        "shed lines must be marked, never silently dropped"
    );
    assert!(
        high < mid && mid <= low,
        "not monotonic: a0.1={low} a0.5={mid} a0.9={high}"
    );
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

/// A plain `ctx_read` of a large CLAUDE.md returned headings only: the
/// standing instruction files of every supported agent are read in full.
#[test]
fn agent_instruction_files_are_read_in_full() {
    for path in [
        "/project/CLAUDE.md",
        "/project/sub/claude.md",
        "/project/CLAUDE.local.md",
        "/home/user/.claude/CLAUDE.md",
        "/project/GEMINI.md",
        "/project/.github/copilot-instructions.md",
        "/project/.windsurfrules",
        "/project/.claude/agents/reviewer.md",
        "/project/.claude/commands/release.md",
        r"C:\Users\nina\project\CLAUDE.md",
    ] {
        assert!(is_instruction_file(path), "{path} carries instructions");
    }
    assert!(!is_instruction_file("/project/docs/claude-notes.md"));
    assert!(!is_instruction_file("/project/.claude/commands/run.sh"));
}

/// #60: `redirect_exclude` was parsed but never applied, so listing CLAUDE.md
/// there changed nothing. Globs match trailing path components.
#[test]
fn redirect_exclude_globs_match_trailing_components() {
    use super::helpers::excluded_by;
    let configured = || {
        vec![
            "docs/**".to_string(),
            "*.json".to_string(),
            "./NOTES.md".to_string(),
            ".claude/**".to_string(),
        ]
    };
    for path in [
        "/project/docs/guide.md",
        "/project/docs/deep/nested/spec.md",
        "/project/package.json",
        "/project/sub/NOTES.md",
        "/home/user/.claude/settings.local.json",
        r"C:\work\project\docs\guide.md",
    ] {
        assert!(excluded_by(path, None, configured), "{path} is excluded");
    }
    for path in [
        "/project/src/docs.rs",
        "/project/src/main.rs",
        "/project/NOTES.txt",
    ] {
        assert!(
            !excluded_by(path, None, configured),
            "{path} is not excluded"
        );
    }
    // Nothing configured: nothing excluded.
    assert!(!excluded_by("/project/CLAUDE.md", None, Vec::new));
}

#[test]
fn hook_exclude_env_takes_precedence_over_config() {
    use super::helpers::excluded_by;
    let configured = || vec!["*.json".to_string()];
    let env = Some(" docs/** , NOTES.md ");
    assert!(excluded_by("/p/docs/a.md", env, configured));
    assert!(excluded_by("/p/NOTES.md", env, configured));
    assert!(
        !excluded_by("/p/package.json", env, configured),
        "env replaces config"
    );
    // A blank env value falls back to the configured globs.
    assert!(excluded_by("/p/package.json", Some("  "), configured));
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

/// Relative paths that start with the instruction directory must match too:
/// `skills/…` has no leading `/` before the segment.
#[test]
fn relative_instruction_directories_are_recognised() {
    for path in [
        "skills/leanctx-gateway-delivery/SKILL.md",
        "skills/demo/guide.md",
        "docs/internal/skills/agent-orchestration.md",
        ".claude/rules/house-style.md",
        ".cursor/rules/lean-ctx.mdc",
        "AGENTS.md",
        "Claude.md",
        "GEMINI.md",
        "LEAN-CTX.md",
        ".cursorrules",
    ] {
        assert!(is_instruction_file(path), "{path} carries instructions");
    }
    assert!(!is_instruction_file("skills/demo/run.py"));
    assert!(!is_instruction_file("myskills/notes.md"));
}

/// The remaining host conventions (Codex override, Copilot path
/// instructions, Windsurf/Cline rule directories) are instruction documents
/// too, so `auto` delivers them in full.
#[test]
fn host_specific_instruction_paths_are_detected() {
    for path in [
        "project/AGENTS.override.md",
        "/repo/.github/instructions/rust.instructions.md",
        "src/api.instructions.md",
        "/repo/.windsurf/rules/style.md",
        "/repo/.clinerules/house.md",
        ".clinerules/house.md",
    ] {
        assert!(is_instruction_file(path), "{path} carries instructions");
    }
    for path in ["/repo/.github/workflows/ci.yml", "/repo/docs/skillset.md"] {
        assert!(!is_instruction_file(path), "{path} is not an instruction");
    }
}

#[test]
fn instruction_files_force_full_only_for_compressing_modes() {
    let path = "docs/internal/skills/agent-orchestration.md";
    for mode in [
        "auto",
        "map",
        "signatures",
        "aggressive",
        "entropy",
        "task",
        "reference",
    ] {
        assert!(instruction_file_forces_full(path, mode), "{mode}");
    }
    for mode in [
        "full",
        "raw",
        "anchored",
        "anchored:1-9",
        "diff",
        "lines:1-40",
    ] {
        assert!(!instruction_file_forces_full(path, mode), "{mode} stays");
    }
    assert!(!instruction_file_forces_full("src/main.rs", "map"));
}

/// The dispatch path (CLI/daemon tools) serves an instruction file in full for
/// an explicit compressing mode, like the MCP tool does.
#[test]
fn dispatch_serves_instruction_file_in_full_for_map() {
    let dir = tempfile::tempdir().unwrap();
    let skill = dir.path().join("skills").join("demo");
    std::fs::create_dir_all(&skill).unwrap();
    let file = skill.join("SKILL.md");
    let mut body = String::new();
    for i in 0..80 {
        use std::fmt::Write as _;
        let _ = write!(
            body,
            "## Step {i}\n\nDo the thing number {i} carefully.\n\n"
        );
    }
    std::fs::write(&file, &body).unwrap();
    let path = file.to_string_lossy().to_string();
    let mut cache = SessionCache::new();
    let out = handle(&mut cache, &path, "map", CrpMode::Off);
    assert!(
        out.contains("Do the thing number 79 carefully."),
        "map of an instruction file must deliver the complete text"
    );
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

    // A large CLAUDE.md used to resolve to `map` (headings only).
    let mode = resolve_auto_mode(None, "/workspace/CLAUDE.md", 6000, None, None);
    assert_eq!(mode, "full", "CLAUDE.md must always be read in full");
}
