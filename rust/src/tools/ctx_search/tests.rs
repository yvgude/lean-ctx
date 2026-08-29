use super::*;
use crate::tools::CrpMode;

/// Determinism contract (#498): identical search over identical files
/// must produce byte-identical output — a prerequisite for provider
/// prompt-cache hits on repeated tool results.
#[test]
fn search_output_is_byte_stable_across_calls() {
    let dir = tempfile::tempdir().unwrap();
    for i in 0..5 {
        std::fs::write(
            dir.path().join(format!("f{i}.rs")),
            format!("fn target_{i}() {{}}\nfn other() {{}}\n"),
        )
        .unwrap();
    }
    let root = dir.path().to_string_lossy().into_owned();
    let run = || {
        handle(
            "target",
            &root,
            Some("*.rs"),
            20,
            CrpMode::Off,
            true,
            true,
            false,
        )
        .text
    };
    assert_eq!(run(), run(), "search output must be deterministic");
}

/// #1008: opt-in anchored search tags each hit with `:hh` (matching
/// `ctx_read`/`ctx_patch`'s line hash) and ships a legend; the default
/// (anchored=false) output stays byte-identical so #498 is preserved.
#[test]
fn anchored_search_emits_line_hash_per_hit_opt_in_only() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.rs"), "let needle = 1;\nother\n").unwrap();
    let root = dir.path().to_string_lossy().into_owned();

    let plain = handle(
        "needle",
        &root,
        Some("*.rs"),
        10,
        CrpMode::Off,
        true,
        true,
        false,
    )
    .text;
    assert!(
        !plain.contains("[anchored:"),
        "default must carry no legend"
    );
    assert!(
        plain.contains("a.rs:1 "),
        "default keeps path:line content: {plain}"
    );

    let anchored = handle(
        "needle",
        &root,
        Some("*.rs"),
        10,
        CrpMode::Off,
        true,
        true,
        true,
    )
    .text;
    let hh = crate::core::anchor::line_hash("let needle = 1;");
    assert!(anchored.contains("[anchored: path:line:hh → edit via ctx_patch]"));
    assert!(
        anchored.contains(&format!("a.rs:1:{hh} ")),
        "anchored hit must carry the line hash: {anchored}"
    );
}

#[test]
#[cfg(feature = "tree-sitter")]
fn hits_inside_multiline_symbols_carry_enclosing_tag() {
    // #608: a hit inside a multi-line function names its enclosing symbol +
    // the handle anchor, and the output ships a self-describing legend.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("a.rs"),
        "fn outer() {\n    let needle = 1;\n    needle\n}\nfn tiny() {}\n",
    )
    .unwrap();
    let out = handle(
        "needle",
        dir.path().to_string_lossy().as_ref(),
        Some("*.rs"),
        10,
        CrpMode::Off,
        true,
        true,
        false,
    )
    .text;
    assert!(
        out.contains("∈outer@L1"),
        "hit must name its enclosing fn: {out}"
    );
    assert!(
        out.contains("[∈ enclosing symbol"),
        "self-describing legend must be present: {out}"
    );
}

#[test]
fn single_line_symbols_get_no_enclosing_tag() {
    // #498/#608: a match whose only enclosing symbol is single-line gets no
    // tag, so the default output stays byte-identical (no `∈`, no legend).
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.rs"), "fn one_liner() {}\n").unwrap();
    let out = handle(
        "one_liner",
        dir.path().to_string_lossy().as_ref(),
        Some("*.rs"),
        10,
        CrpMode::Off,
        true,
        true,
        false,
    )
    .text;
    assert!(!out.contains('∈'), "single-line symbol → no tag: {out}");
}

#[test]
fn search_results_are_deterministically_ordered_by_path() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.txt");
    let b = dir.path().join("b.txt");
    std::fs::write(&b, "match\n").unwrap();
    std::fs::write(&a, "match\n").unwrap();

    let out = handle(
        "match",
        dir.path().to_string_lossy().as_ref(),
        Some("*.txt"),
        10,
        CrpMode::Off,
        true,
        true,
        false,
    )
    .text;

    let mut match_lines: Vec<&str> = out
        .lines()
        .filter(|l| l.contains(".txt:") && l.contains("match"))
        .collect();
    // Expect exactly the 2 match lines, ordered a.txt then b.txt.
    match_lines.truncate(2);
    assert_eq!(match_lines.len(), 2);
    assert!(
        match_lines[0].contains("a.txt:"),
        "first match should come from a.txt, got: {}",
        match_lines[0]
    );
    assert!(
        match_lines[1].contains("b.txt:"),
        "second match should come from b.txt, got: {}",
        match_lines[1]
    );
}

#[test]
fn warm_index_and_content_cache_path_returns_correct_matches() {
    // Exercises the trigram-index fast path together with the shared content
    // cache (#148): the index build reads the corpus once and publishes it,
    // then this search reuses those bytes. Results must be byte-identical to
    // the walk path — this asserts that correctness, independent of whether
    // any individual file is a cache hit or a fallback re-read.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("a.rs"),
        "fn authenticate() {}\nlet x = 1;\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("b.rs"), "fn connect() {}\n").unwrap();
    let root = dir.path().to_string_lossy().to_string();

    // Synchronously warm the resident trigram index (also populates the
    // shared content cache for these paths).
    assert!(
        crate::core::search_index::warm_blocking(&root, true, false),
        "index should warm for a small clean corpus"
    );

    let out = handle(
        "authenticate",
        &root,
        None,
        10,
        CrpMode::Off,
        true,
        false,
        false,
    )
    .text;
    assert!(
        out.contains("a.rs"),
        "warm-index + cache search must find the match: {out}"
    );
    assert!(
        out.contains("authenticate"),
        "the matched line must be present: {out}"
    );
    assert!(
        !out.contains("b.rs"),
        "a non-matching file must not appear in results: {out}"
    );
}

#[test]
fn search_finds_word_literals_added_after_index_warm() {
    // #624 regression: a native edit or add after the trigram index was
    // warmed must be found immediately. The resident index is gated by a live
    // corpus signature, so stale trigrams can never hide on-disk content from
    // a word-literal query — the very path that uses trigram narrowing.
    let _lock = crate::core::data_dir::test_env_lock();
    crate::test_env::remove_var("LEAN_CTX_DISABLE_SEARCH_INDEX");
    crate::test_env::remove_var("LEAN_CTX_SEARCH_INDEX_COALESCE_MS");

    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.rs"), "fn existing() {}\n").unwrap();
    std::fs::write(dir.path().join("b.rs"), "fn other() {}\n").unwrap();
    let root = dir.path().to_string_lossy().to_string();

    assert!(
        crate::core::search_index::warm_blocking(&root, true, false),
        "index should warm for a small clean corpus"
    );

    // (1) A brand-new file created after the warm.
    std::fs::write(dir.path().join("c.rs"), "fn added_after_warm_zzz() {}\n").unwrap();
    let out_new = handle(
        "added_after_warm_zzz",
        &root,
        None,
        10,
        CrpMode::Off,
        true,
        false,
        false,
    )
    .text;
    assert!(
        out_new.contains("c.rs") && out_new.contains("added_after_warm_zzz"),
        "a file created after the index warm must be found: {out_new}"
    );

    // (2) An in-place edit of a file that existed at warm time.
    std::fs::write(
        dir.path().join("a.rs"),
        "fn existing() {}\nfn edited_after_warm_zzz() {}\n",
    )
    .unwrap();
    let out_edit = handle(
        "edited_after_warm_zzz",
        &root,
        None,
        10,
        CrpMode::Off,
        true,
        false,
        false,
    )
    .text;
    assert!(
        out_edit.contains("a.rs") && out_edit.contains("edited_after_warm_zzz"),
        "content appended after the index warm must be found: {out_edit}"
    );
}

#[test]
fn symbol_substitution_is_off_by_default() {
    let _lock = crate::core::data_dir::test_env_lock();
    crate::test_env::remove_var("LEAN_CTX_SYMBOL_MAP");
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("a.rs");
    std::fs::write(
        &f,
        "fn longIdentifierAlpha() {}\nfn longIdentifierBeta() {}\nfn longIdentifierGamma() {}\n",
    )
    .unwrap();

    let out = handle(
        "longIdentifier",
        dir.path().to_string_lossy().as_ref(),
        Some("*.rs"),
        10,
        CrpMode::Off,
        true,
        true,
        false,
    )
    .text;

    assert!(
        !out.contains("§MAP"),
        "default agent-facing output must not carry a §MAP table: {out}"
    );
    assert!(
        !out.contains('α'),
        "default agent-facing output must not carry α-symbols: {out}"
    );
    assert!(
        out.contains("longIdentifierAlpha"),
        "identifiers should appear raw by default: {out}"
    );
}

#[test]
fn secret_like_files_are_skipped_by_default() {
    let dir = tempfile::tempdir().unwrap();
    let secret = dir.path().join("key.pem");
    let ok = dir.path().join("ok.txt");
    std::fs::write(&secret, "match\n").unwrap();
    std::fs::write(&ok, "match\n").unwrap();

    let out = handle(
        "match",
        dir.path().to_string_lossy().as_ref(),
        None,
        10,
        CrpMode::Off,
        true,
        false,
        false,
    )
    .text;

    assert!(out.contains("ok.txt:"), "expected ok.txt match, got: {out}");
    assert!(
        !out.contains("key.pem:"),
        "secret-like file should be skipped, got: {out}"
    );
    assert!(
        out.contains("secret-like files skipped"),
        "expected boundary skip note, got: {out}"
    );
}

#[test]
#[cfg(unix)]
fn search_skips_named_pipe_without_hanging() {
    use std::sync::mpsc;
    // #336: a named pipe (FIFO) in the search universe used to block
    // `read_to_string` forever, hanging the whole call with no output. It
    // must be skipped, the real file still matched, and the call must return.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("real.txt"), "needle_here = 1\n").unwrap();
    let fifo = dir.path().join("pipe.fifo");
    let c = std::ffi::CString::new(fifo.to_string_lossy().as_bytes()).unwrap();
    assert_eq!(
        // SAFETY: `c` is a live CString providing a valid NUL-terminated
        // path pointer for the duration of the call.
        unsafe { libc::mkfifo(c.as_ptr(), 0o644) },
        0,
        "mkfifo failed"
    );

    let dir_path = dir.path().to_string_lossy().to_string();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        // Fresh temp dir → no warm index yet, so this exercises the walk path.
        let out = handle(
            "needle_here",
            &dir_path,
            None,
            10,
            CrpMode::Off,
            true,
            true,
            false,
        )
        .text;
        let _ = tx.send(out);
    });
    let out = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("ctx_search hung on a FIFO (#336 regression)");

    assert!(
        out.contains("real.txt"),
        "the real file must still match: {out}"
    );
    assert!(
        out.contains("special files skipped"),
        "the FIFO must be reported as a skipped special file: {out}"
    );
}

#[test]
fn search_deadline_env_override_is_respected() {
    let _lock = crate::core::data_dir::test_env_lock();
    crate::test_env::set_var("LEAN_CTX_SEARCH_DEADLINE_MS", "0");
    assert!(search_deadline().is_none(), "0 must disable the deadline");
    crate::test_env::set_var("LEAN_CTX_SEARCH_DEADLINE_MS", "250");
    assert_eq!(search_deadline(), Some(Duration::from_millis(250)));
    crate::test_env::remove_var("LEAN_CTX_SEARCH_DEADLINE_MS");
    assert_eq!(
        search_deadline(),
        Some(Duration::from_secs(10)),
        "default budget is 10s"
    );
}

#[test]
fn extract_extensions_handles_single_brace_and_none() {
    assert_eq!(extract_extensions(Some("*.rs")), vec!["rs"]);
    assert_eq!(extract_extensions(Some("src/**/*.tsx")), vec!["tsx"]);
    assert_eq!(extract_extensions(Some("*.{rs,ts}")), vec!["rs", "ts"]);
    assert_eq!(
        extract_extensions(Some("*.{rs, ts , js}")),
        vec!["rs", "ts", "js"]
    );
    assert_eq!(extract_extensions(None), Vec::<String>::new());
}

#[test]
fn extract_extensions_ignores_dots_in_directory_segments() {
    // A dot in a directory name must not be mistaken for the extension.
    assert_eq!(
        extract_extensions(Some("config.v2/src/**/*.rs")),
        vec!["rs"]
    );
    assert_eq!(extract_extensions(Some("src/v2.0/*.module.ts")), vec!["ts"]);
    // No extension on the final component → empty.
    assert_eq!(extract_extensions(Some("src/**/*")), Vec::<String>::new());
    assert_eq!(
        extract_extensions(Some("config.v2/Makefile")),
        Vec::<String>::new()
    );
}

#[test]
fn include_glob_filters_by_brace_expansion() {
    let dir = tempfile::tempdir().unwrap();
    // Unique needle: the global search-delta tracker (src/core/search_delta.rs)
    // is keyed by pattern, so sharing "needle" with sibling tests lets a
    // parallel run mark our matches "unchanged" and drop them. A pattern
    // unique to this test guarantees a fresh tracker key. (#flaky)
    std::fs::write(dir.path().join("a.rs"), "brace_glob_needle\n").unwrap();
    std::fs::write(dir.path().join("b.ts"), "brace_glob_needle\n").unwrap();
    std::fs::write(dir.path().join("c.py"), "brace_glob_needle\n").unwrap();

    let out = handle(
        "brace_glob_needle",
        dir.path().to_string_lossy().as_ref(),
        Some("*.{rs,ts}"),
        10,
        CrpMode::Off,
        true,
        true,
        false,
    )
    .text;

    assert!(out.contains("a.rs"), "rs file must match: {out}");
    assert!(out.contains("b.ts"), "ts file must match: {out}");
    assert!(!out.contains("c.py"), "py file must be excluded: {out}");
}

#[test]
fn bare_include_glob_matches_at_any_depth() {
    // rg/git grep behaviour: a bare glob without `/` should match
    // files at any depth, not just in the search root.
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("a/deep/path")).unwrap();
    std::fs::write(dir.path().join("a/deep/path/file.rs"), "needle\n").unwrap();
    std::fs::write(dir.path().join("root.rs"), "needle\n").unwrap();
    std::fs::write(dir.path().join("other.py"), "needle\n").unwrap();

    let out = handle(
        "needle",
        dir.path().to_string_lossy().as_ref(),
        Some("*.rs"),
        10,
        CrpMode::Off,
        true,
        true,
        false,
    )
    .text;

    assert!(out.contains("root.rs"), "root .rs file must match: {out}");
    assert!(
        out.contains("file.rs"),
        "nested .rs file must match bare *.rs glob: {out}"
    );
    assert!(!out.contains("other.py"), ".py must be excluded: {out}");

    // Also test bare filename glob (no wildcard at all)
    let out2 = handle(
        "needle",
        dir.path().to_string_lossy().as_ref(),
        Some("file.rs"),
        10,
        CrpMode::Off,
        true,
        true,
        false,
    )
    .text;

    assert!(
        out2.contains("file.rs"),
        "bare filename glob must match nested file: {out2}"
    );
}

#[test]
fn include_glob_recursive_path_pattern() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src/inner")).unwrap();
    std::fs::write(dir.path().join("src/inner/deep.rs"), "needle\n").unwrap();
    std::fs::write(dir.path().join("top.rs"), "needle\n").unwrap();

    let out = handle(
        "needle",
        dir.path().to_string_lossy().as_ref(),
        Some("src/**/*.rs"),
        10,
        CrpMode::Off,
        true,
        true,
        false,
    )
    .text;

    assert!(out.contains("deep.rs"), "nested match expected: {out}");
    assert!(
        !out.contains("top.rs"),
        "root file outside src/ must be excluded: {out}"
    );
}

#[test]
fn exclude_glob_drops_paths_and_exclude_pattern_drops_lines() {
    // #870: `exclude` is the negative complement of `include` (path glob),
    // and `exclude_pattern` is a `grep -v` over result lines.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("app.rs"), "needle\n// needle skip me\n").unwrap();
    std::fs::write(dir.path().join("app_test.rs"), "needle\n").unwrap();
    let root = dir.path().to_string_lossy().into_owned();

    // exclude drops the *_test.rs file entirely.
    let out = handle_filtered(
        "needle",
        &root,
        None,
        10,
        CrpMode::Off,
        true,
        true,
        false,
        Some("*_test.rs"),
        None,
    )
    .text;
    assert!(out.contains("app.rs"), "app.rs must match: {out}");
    assert!(
        !out.contains("app_test.rs"),
        "excluded path must be dropped: {out}"
    );

    // exclude_pattern drops the commented match line but keeps the real one.
    let out2 = handle_filtered(
        "needle",
        &root,
        Some("app.rs"),
        10,
        CrpMode::Off,
        true,
        true,
        false,
        None,
        Some("skip me"),
    )
    .text;
    assert!(out2.contains("1 matches"), "one line kept: {out2}");
    assert!(!out2.contains("skip me"), "grep -v line dropped: {out2}");
}

#[test]
fn invalid_exclude_pattern_is_ignored_not_fatal() {
    // #870: a malformed exclude regex must not hide matches — it's ignored.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.rs"), "needle\n").unwrap();
    let root = dir.path().to_string_lossy().into_owned();
    let out = handle_filtered(
        "needle",
        &root,
        None,
        10,
        CrpMode::Off,
        true,
        true,
        false,
        None,
        Some("([unclosed"),
    )
    .text;
    assert!(out.contains("a.rs"), "invalid exclude → no filter: {out}");
}

#[test]
fn search_refuses_home_directory_root() {
    // #356 class: the MCP server often runs with cwd == $HOME; a defaulted
    // `path` must never walk the whole home dir (macOS TCC prompts).
    let home = dirs::home_dir().expect("home dir in test env");
    let out = handle(
        "needle",
        home.to_string_lossy().as_ref(),
        None,
        10,
        CrpMode::Off,
        true,
        true,
        false,
    )
    .text;
    assert!(
        out.starts_with("ERROR:") && out.contains("refusing to scan"),
        "home root must be refused: {out}"
    );
}

/// #1591: "13 matches in 40 files" reported the number of files *scanned* as
/// the number that matched. The two counts are now separate and labelled.
#[test]
fn match_count_reports_matched_files_not_scanned_files() {
    let dir = tempfile::tempdir().unwrap();
    for i in 0..8 {
        std::fs::write(dir.path().join(format!("quiet{i}.rs")), "fn nothing() {}\n").unwrap();
    }
    std::fs::write(
        dir.path().join("hit_a.rs"),
        "let needle = 1;\nlet needle = 2;\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("hit_b.rs"), "let needle = 3;\n").unwrap();

    let out = handle(
        "needle",
        &dir.path().to_string_lossy(),
        Some("*.rs"),
        50,
        CrpMode::Off,
        true,
        true,
        false,
    )
    .text;

    assert!(
        out.starts_with("3 matches in 2 files"),
        "the headline count must be files that matched, not files walked: {out}"
    );
    assert!(
        out.contains("scanned 10"),
        "the scanned count stays visible, explicitly labelled: {out}"
    );
}
