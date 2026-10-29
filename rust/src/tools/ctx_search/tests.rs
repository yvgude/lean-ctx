use super::*;
use crate::tools::CrpMode;

/// Search answers "withheld under policy" while any content policy is active,
/// and policy tests elsewhere in the crate pin one process-wide. A test that
/// searches without a policy pins "none" itself, or a parallel test decides
/// its result (seen under load in the R05 export run).
fn no_policy() -> crate::core::policy::runtime::TestPolicyOverride {
    crate::core::policy::runtime::TestPolicyOverride::set(None)
}

/// Determinism contract (#498): identical search over identical files
/// must produce byte-identical output — a prerequisite for provider
/// prompt-cache hits on repeated tool results.
#[test]
fn search_output_is_byte_stable_across_calls() {
    let _no_policy = no_policy();
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

/// Agents search for code text: `foo(`, `items[0`, `fn run(&self`. Those are
/// not valid regex; the search runs literally and says so instead of failing.
#[test]
fn invalid_regex_is_searched_literally_with_a_note() {
    let _no_policy = no_policy();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("a.rs"),
        "fn run(&self) {}\nlet x = items[0];\nrun_other();\n",
    )
    .unwrap();
    let root = dir.path().to_string_lossy().into_owned();
    for (pattern, expected) in [("fn run(&self", "a.rs"), ("items[0", "items[0]")] {
        let out = handle(pattern, &root, None, 20, CrpMode::Off, true, true, false).text;
        assert!(!out.starts_with("ERROR:"), "{pattern}: {out}");
        assert!(
            out.contains("searched for it literally"),
            "{pattern}: {out}"
        );
        assert!(out.contains(expected), "{pattern}: {out}");
    }
    let regex = handle("run_\\w+", &root, None, 20, CrpMode::Off, true, true, false).text;
    assert!(
        !regex.contains("literally") && regex.contains("run_other"),
        "valid regex is unchanged: {regex}"
    );
}

/// #1008: opt-in anchored search tags each hit with `:hh` (matching
/// `ctx_read`/`ctx_patch`'s line hash) and ships a legend; the default
/// (anchored=false) output stays byte-identical so #498 is preserved.
#[test]
fn anchored_search_emits_line_hash_per_hit_opt_in_only() {
    let _no_policy = no_policy();
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
    let _no_policy = no_policy();
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
    let _no_policy = no_policy();
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
    let _no_policy = no_policy();
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
    // Pin "no policy": a parallel test's override withheld both sources here
    // ("2 sources withheld under policy", ubuntu main CI).
    let _no_policy = crate::core::policy::runtime::TestPolicyOverride::set(None);
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

    // `super::handle`: this test already holds the pin, which is not reentrant.
    let out = super::handle(
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
    let _no_policy = no_policy();
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
    let _no_policy = no_policy();
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
    let _no_policy = no_policy();
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
    let _no_policy = no_policy();
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
    let _no_policy = no_policy();
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

/// #1994: `*.go,*.yaml` compiled to one glob no file matched, and the
/// result was a clean `0 matches` over an empty scope.
#[test]
fn comma_separated_include_is_a_glob_list() {
    let _no_policy = no_policy();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.go"), "comma_glob_needle_1994\n").unwrap();
    std::fs::write(dir.path().join("b.yaml"), "comma_glob_needle_1994\n").unwrap();
    std::fs::write(dir.path().join("c.py"), "comma_glob_needle_1994\n").unwrap();

    let out = handle(
        "comma_glob_needle_1994",
        dir.path().to_string_lossy().as_ref(),
        Some("*.go, *.yaml"),
        10,
        CrpMode::Off,
        true,
        true,
        false,
    )
    .text;

    assert!(out.contains("a.go"), "go file must match: {out}");
    assert!(out.contains("b.yaml"), "yaml file must match: {out}");
    assert!(!out.contains("c.py"), "py file must be excluded: {out}");
    assert_eq!(
        extract_extensions(Some("*.go,*.{ts,tsx}")),
        vec!["go", "ts", "tsx"]
    );
}

/// #1994: an include that admits no file and a genuine miss printed
/// byte-identical lines. Both now state the scanned count, and the empty
/// scope says why nothing was searched — with and without a warm index.
/// #2059: a file `path` plus `include` scanned 0 files — the glob was matched
/// against the empty path relative to the file itself.
#[test]
fn include_and_exclude_apply_to_a_file_root_by_its_name() {
    let _no_policy = no_policy();
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("main.rs");
    std::fs::write(&file, "fn main() { let needle2059 = 1; }\n").unwrap();
    let file = file.to_string_lossy().to_string();
    let search = |include: Option<&str>, exclude: Option<&str>| {
        handle_filtered(
            "needle2059",
            &file,
            include,
            10,
            CrpMode::Off,
            true,
            false,
            false,
            exclude,
            None,
        )
        .text
    };

    assert!(search(None, None).contains("let needle2059"));
    for include in ["*.rs", "main.rs", "**/*.rs"] {
        let hit = search(Some(include), None);
        assert!(hit.contains("needle2059"), "include={include}: {hit}");
    }
    let filtered = search(Some("*.go"), None);
    assert!(filtered.contains("(scanned 0 files)"), "{filtered}");
    let excluded = search(None, Some("*.rs"));
    assert!(!excluded.contains("let needle2059"), "{excluded}");
}

#[test]
fn zero_hits_state_the_scope_and_name_an_empty_filter() {
    let _no_policy = no_policy();
    let _lock = crate::core::data_dir::test_env_lock();
    crate::test_env::remove_var("LEAN_CTX_DISABLE_SEARCH_INDEX");
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.go"), "package a\n").unwrap();
    std::fs::write(dir.path().join("b.go"), "package b\n").unwrap();
    let root = dir.path().to_string_lossy().to_string();

    for warm in [false, true] {
        if warm {
            assert!(crate::core::search_index::warm_blocking(&root, true, false));
        }
        let search = |include: &str| {
            handle(
                "zzzqqqnomatch1994",
                &root,
                Some(include),
                10,
                CrpMode::Off,
                true,
                false,
                false,
            )
            .text
        };
        let miss = search("*.go");
        assert!(
            miss.contains("(scanned 2 files)"),
            "a genuine miss must report its scope (warm={warm}): {miss}"
        );
        assert!(!miss.contains("nothing was searched"), "{miss}");

        let empty = search("*.nothing");
        assert!(
            empty.contains("(scanned 0 files)") && empty.contains("nothing was searched"),
            "an empty filter must say so (warm={warm}): {empty}"
        );
        assert_ne!(miss, empty);
    }
}

#[test]
fn bare_include_glob_matches_at_any_depth() {
    let _no_policy = no_policy();
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
    let _no_policy = no_policy();
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

/// #2059 as reported: `path` = `Cargo.toml`, `include` = `*.toml`.
#[test]
fn explicit_manifest_file_matches_its_include_glob() {
    let _no_policy = no_policy();
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("Cargo.toml");
    std::fs::write(&file, "[package]\nname = \"manifest_include_2059\"\n").unwrap();

    let out = handle(
        "^\\[package\\]",
        file.to_string_lossy().as_ref(),
        Some("*.toml"),
        10,
        CrpMode::Off,
        true,
        true,
        false,
    )
    .text;

    assert!(
        out.contains("[package]") && out.contains("(scanned 1)"),
        "the explicit Cargo.toml must be scanned: {out}"
    );
}

#[test]
fn exclude_glob_drops_paths_and_exclude_pattern_drops_lines() {
    let _no_policy = no_policy();
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
    let _no_policy = no_policy();
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
    let _no_policy = no_policy();
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
    let _no_policy = no_policy();
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

/// #1625: the reporter's fixture — 14 matches across three files — audited
/// under a cap of 10. The old output was "10 matches in 1 files (scanned 1)"
/// with nothing to suggest the scan had stopped early, so an endpoint audit
/// over 32 files read as complete at 10 hits in 1 file. The scanned count is
/// not completeness evidence when the budget ran out, and now says so.
#[test]
fn result_cap_is_announced_instead_of_looking_exhaustive() {
    let _no_policy = no_policy();
    use std::fmt::Write as _;

    // Distinct literal per case: a repeated pattern in one process renders as
    // a *delta* against the previous result, which would compare the two runs
    // against each other rather than against the corpus.
    let fixture = |token: &str| {
        let dir = tempfile::tempdir().unwrap();
        let mut many = String::new();
        for i in 0..12 {
            writeln!(&mut many, "const {token}_{i} = \"/api/v{i}\";").unwrap();
        }
        std::fs::write(dir.path().join("a.ts"), many).unwrap();
        std::fs::write(
            dir.path().join("b.ts"),
            format!("const {token}_B = \"/api/b\";\n"),
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join("nested")).unwrap();
        std::fs::write(
            dir.path().join("nested/c.ts"),
            format!("const {token}_C = \"/api/c\";\n"),
        )
        .unwrap();
        dir
    };
    let search = |dir: &tempfile::TempDir, token: &str, max: usize| {
        handle(
            token,
            &dir.path().to_string_lossy(),
            Some("*.ts"),
            max,
            CrpMode::Off,
            true,
            true,
            false,
        )
        .text
    };

    let capped_dir = fixture("CAPPED_ENDPOINT");
    let capped = search(&capped_dir, "CAPPED_ENDPOINT", 10);

    assert!(
        capped.contains("stopped at the max_results=10 cap"),
        "hitting the cap must be stated, not inferred: {capped}"
    );
    assert!(
        capped.contains("of 3 files scanned"),
        "the notice must contrast files scanned with files eligible: {capped}"
    );
    assert!(
        capped.contains("floor, not a total"),
        "the counts must be labelled as a lower bound: {capped}"
    );

    // The same shape under a sufficient budget is exhaustive, and must carry
    // no notice — otherwise the warning becomes noise and stops being read.
    let full_dir = fixture("FULL_ENDPOINT");
    let complete = search(&full_dir, "FULL_ENDPOINT", 40);
    assert!(
        complete.starts_with("14 matches in 3 files"),
        "the uncapped search must find every match: {complete}"
    );
    assert!(
        !complete.contains("stopped at the max_results"),
        "an exhaustive search must not warn about a cap it never reached: {complete}"
    );
}

/// ~600 KB of text with one needle near the end: above `MAX_FILE_SIZE`,
/// below `MAX_EXPLICIT_FILE_SIZE`.
fn large_log(needle: &str) -> String {
    let mut body = "2026-09-29 INFO routine line without anything special\n".repeat(11_000);
    body.push_str(needle);
    body.push('\n');
    assert!(body.len() as u64 > MAX_FILE_SIZE);
    body
}

#[test]
fn explicitly_named_large_file_is_searched() {
    let _no_policy = no_policy();
    // #1917: `grep NEEDLE big.log` must look inside the file the caller named,
    // not report "0 matches" because the file exceeds the directory-walk cap.
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("big.log");
    std::fs::write(&file, large_log("NEEDLE_1917 found")).unwrap();
    let out = handle(
        "NEEDLE_1917",
        file.to_string_lossy().as_ref(),
        None,
        10,
        CrpMode::Off,
        true,
        true,
        false,
    )
    .text;
    assert!(out.starts_with("1 matches"), "{out}");
    assert!(out.contains("NEEDLE_1917 found"), "{out}");
}

#[test]
fn directory_walk_names_the_large_files_it_skipped() {
    let _no_policy = no_policy();
    // #1917: a skipped file must be named so the caller can search it directly.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("big.log"), large_log("NEEDLE_1917")).unwrap();
    std::fs::write(dir.path().join("small.txt"), "nothing here\n").unwrap();
    let out = handle(
        "NEEDLE_1917",
        dir.path().to_string_lossy().as_ref(),
        None,
        10,
        CrpMode::Off,
        true,
        true,
        false,
    )
    .text;
    assert!(out.starts_with("0 matches"), "{out}");
    assert!(out.contains("large files skipped"), "{out}");
    assert!(out.contains("big.log"), "skipped file must be named: {out}");
}

#[test]
fn name_skipped_caps_the_list() {
    let names: Vec<String> = ["a", "b", "c"].iter().map(|s| (*s).to_string()).collect();
    assert_eq!(name_skipped(&names, 3), "a, b, c");
    assert_eq!(name_skipped(&names, 7), "a, b, c, +4 more");
}

/// Regression (M7 eval E1): under a policy, every scanned original is charged
/// to one per-search byte budget. Once it was spent, every remaining file was
/// reported as "withheld under policy" — 2087 of 2399 sources on a real tree,
/// although the policy withheld none of them. The scan now stops and says why.
///
/// The policy is the project's own pack, scoped to this request, so it never
/// reaches a parallel test; the override lock keeps any test-wide policy out
/// for the duration. The corpus is larger than the default budget, so no
/// process-wide limit is changed.
#[test]
fn spent_protected_read_budget_is_not_reported_as_policy_withholding() {
    let _no_test_policy = crate::core::policy::runtime::lock_test_override();
    let _data = crate::core::data_dir::isolated_data_dir();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    std::fs::create_dir_all(root.join(".lean-ctx")).unwrap();
    std::fs::write(
        root.join(".lean-ctx/policy.toml"),
        "name = 'budget'\nversion = '1.0.0'\ndescription = 'test'\n[redaction]\ncustomer = 'CUS-[0-9]{4}'\n",
    )
    .unwrap();
    // Ten sources just below the per-file size limit: ~4.8 MB against the
    // 4 MiB default budget of a protected search.
    let filler = "// filler line of the budget fixture\n".repeat(12_500);
    for i in 0..10 {
        std::fs::write(
            root.join(format!("f{i}.rs")),
            format!("fn needle_{i}() {{}}\n{filler}"),
        )
        .unwrap();
    }
    let root_str = root.to_string_lossy().to_string();
    let out = crate::core::policy::runtime::REQUEST_PROJECT.sync_scope(
        std::cell::RefCell::new(Some(root.clone())),
        || {
            handle(
                "needle_",
                &root_str,
                None,
                50,
                CrpMode::Off,
                true,
                false,
                false,
            )
            .text
        },
    );
    assert!(
        !out.contains("withheld under policy"),
        "unscanned files were counted as withheld: {out}"
    );
    // On a loaded host the time budget can end the scan before the read
    // budget does; either way the early stop must be explained as a budget.
    assert!(
        out.contains("read budget of this search is spent")
            || out.contains("search stopped after the"),
        "the early stop is not explained: {out}"
    );
}
