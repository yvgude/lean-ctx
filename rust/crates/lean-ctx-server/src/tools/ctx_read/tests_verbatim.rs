// SPDX-License-Identifier: Apache-2.0
use super::*;

/// Phase 1a (epic #1008): `mode=anchored` returns each source line as a
/// `N:hh|content` anchor the model can edit against via `ctx_patch`, plus a
/// self-describing legend. End-to-end through the real read pipeline.
#[test]
fn anchored_mode_emits_line_hash_anchors() {
    // Reads go raw while another test holds LEAN_CTX_DISABLED=1; reading the
    // environment under the same lock keeps this test out of that window.
    let _env = lean_ctx::core::data_dir::test_env_lock();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("anc.rs");
    let p = path.to_string_lossy().to_string();
    let content = "fn main() {\n    let x = 1;\n}\n";
    std::fs::write(&path, content).unwrap();

    let mut cache = SessionCache::new();
    let r = handle_with_task_resolved(&mut cache, &p, "anchored", CrpMode::Off, None);
    assert_eq!(r.resolved_mode, "anchored");
    assert!(
        r.content.contains("[anchored:"),
        "anchored output must carry the self-describing legend: {}",
        r.content
    );

    // Every source line appears as `N:hh|<line>` with the SSOT anchor hash.
    for (i, line) in content.lines().enumerate() {
        let n = i + 1;
        let expected = format!("{n}:{}|{line}", lean_ctx::core::anchor::line_hash(line));
        assert!(
            r.content.contains(&expected),
            "missing anchor for line {n}: expected `{expected}` in:\n{}",
            r.content
        );
    }
}

/// Anchored mode is lossless, so the #361 raw cap must never strip the anchors
/// on a small file (it opts out of the cap) — the agent always gets editable
/// anchors back.
#[test]
fn anchored_mode_is_not_capped_to_raw_on_small_files() {
    let _env = lean_ctx::core::data_dir::test_env_lock();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tiny.rs");
    let p = path.to_string_lossy().to_string();
    std::fs::write(&path, "a\n").unwrap();

    let mut cache = SessionCache::new();
    let r = handle_with_task_resolved(&mut cache, &p, "anchored", CrpMode::Off, None);
    assert!(
        r.content.contains("|a"),
        "anchored output must keep anchors even on a tiny file: {}",
        r.content
    );
    assert!(r.content.contains("[anchored:"), "legend must survive");
}

#[test]
fn raw_mode_returns_exact_file_content() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
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

/// Regression for GH #628: the verbatim views (`full`, `raw`, `lines:N-M`) must
/// reproduce every source line — including decorative separator comments
/// (`// ————`, `// ----`) — so the content the model edits never diverges from
/// disk. The original report saw these silently stripped, which then broke
/// `ctx_edit` on a whitespace mismatch.
#[test]
fn verbatim_modes_preserve_decorative_comment_lines() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    let sep_em = "// ————————————————————————————————————————";
    let sep_dash = "// ------------------------------------------";
    let content = format!(
        "import {{ describe, it, expect }} from \"vitest\";\n\
         \n\
         {sep_em}\n\
         // Section: arithmetic\n\
         {sep_em}\n\
         describe(\"add\", () => {{\n  it(\"adds\", () => expect(1 + 1).toBe(2));\n}});\n\
         \n\
         {sep_dash}\n\
         // Section: strings\n\
         {sep_dash}\n"
    );

    for mode in ["full", "raw"] {
        let (output, _) = render::process_mode(
            &content,
            mode,
            "F1",
            "math.test.ts",
            "ts",
            count_tokens(&content),
            CrpMode::Off,
            "/tmp/math.test.ts",
            None,
        );
        assert!(
            output.contains(sep_em) && output.contains(sep_dash),
            "{mode} mode must keep every separator comment verbatim:\n{output}"
        );
        // Every source line is present (modes may add a header/footer, never drop).
        for line in content.lines().filter(|l| !l.is_empty()) {
            assert!(
                output.contains(line),
                "{mode} mode dropped a source line: {line:?}"
            );
        }
    }

    // A `lines:` window must keep separators too, with original line numbering.
    let window = render::extract_line_range(&content, "1-5");
    assert!(
        window.contains(sep_em),
        "lines: window dropped the separator comment:\n{window}"
    );
    assert!(
        window.contains("   3| ") && window.contains("   5| "),
        "lines: window must number the separator lines (3 and 5):\n{window}"
    );
}

/// Determinism contract (#498): tool output must be a pure function of
/// (content, mode, crp_mode, task). Timestamps, counters or random hints in
/// the body would make otherwise-identical outputs unique and defeat
/// provider-side prompt caching.
#[test]
fn process_mode_output_is_byte_stable_across_calls() {
    // Env-independent setup first: the first `count_tokens` in the process
    // loads the tokenizer, which must not happen under the global env lock.
    let content: String = (0..120)
        .map(|i| format!("pub fn handler_{i}(x: u32) -> u32 {{ x * {i} }}"))
        .collect::<Vec<_>>()
        .join("\n");
    let tokens = count_tokens(&content);
    // Fresh, empty data dir (GL #556): the shared per-process test sandbox
    // accumulates feedback/bandit/session stores from parallel tests, which
    // feed adaptive_thresholds() and make entropy-mode output drift between
    // two calls. Purity only holds against a stable learning state.
    let _iso = lean_ctx::core::data_dir::isolated_data_dir();
    // Footer visibility must be the default (`never`) for purity: with a
    // visible footer, the process-global session accumulator appends a
    // `session: N saved` line every 10th call across ALL tests. Other tests
    // leaked `LEAN_CTX_SAVINGS_FOOTER=always` here in the past — neutralize
    // defensively while we hold the env lock.
    lean_ctx::test_env::remove_var("LEAN_CTX_SAVINGS_FOOTER");
    lean_ctx::test_env::remove_var("LEAN_CTX_SHOW_SAVINGS");
    lean_ctx::test_env::remove_var("LEAN_CTX_QUIET");

    for mode in [
        "map",
        "signatures",
        "reference",
        "aggressive",
        "entropy",
        "raw",
        "lines:5-20",
        "anchored",
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

/// The reactive recovery footer (#premium-recovery): present on compressed views,
/// leading with the MCP-free native path; absent from verbatim views and when the
/// `recovery_hints` tier is `off`; and byte-stable across calls (#498).
#[test]
fn recovery_footer_is_compressed_only_and_togglable() {
    // `isolated_data_dir()` already holds `test_env_lock` for its lifetime; taking
    // the lock again here would self-deadlock (the mutex is non-reentrant).
    let _iso = lean_ctx::core::data_dir::isolated_data_dir();
    let content: String = (0..120)
        .map(|i| format!("pub fn handler_{i}(x: u32) -> u32 {{ x * {i} }}"))
        .collect::<Vec<_>>()
        .join("\n");
    let tokens = count_tokens(&content);
    let run = |mode: &str| {
        render::process_mode(
            &content,
            mode,
            "F1",
            "rec.rs",
            "rs",
            tokens,
            CrpMode::Off,
            "/tmp/rec.rs",
            None,
        )
        .0
    };

    // Default tier (minimal): a compressed view leads its footer with the native,
    // MCP-free path so an agent needing the full source never reads line-by-line.
    lean_ctx::test_env::set_var("LEAN_CTX_RECOVERY_HINTS", "minimal");
    let sigs = run("signatures");
    assert!(
        sigs.contains("read \"/tmp/rec.rs\" directly (no MCP)"),
        "compressed view must surface the MCP-free recovery path: {sigs}"
    );
    // Determinism (#498): byte-stable across calls.
    assert_eq!(sigs, run("signatures"), "footer must be byte-stable");

    // The verbatim escape hatch itself carries no footer (nothing to recover).
    assert!(
        !run("raw").contains("(no MCP)"),
        "raw view needs no recovery footer"
    );

    // The off switch suppresses the footer cleanly.
    lean_ctx::test_env::set_var("LEAN_CTX_RECOVERY_HINTS", "off");
    assert!(
        !run("signatures").contains("(no MCP)"),
        "recovery_hints=off must drop the footer"
    );
    lean_ctx::test_env::remove_var("LEAN_CTX_RECOVERY_HINTS");
}

#[test]
fn raw_mode_no_savings_footer() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
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
