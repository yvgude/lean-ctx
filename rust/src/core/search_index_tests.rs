// SPDX-License-Identifier: Apache-2.0
use super::*;

fn corpus() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.rs"), "fn handler() {}\nlet x = 1;\n").unwrap();
    std::fs::write(dir.path().join("b.rs"), "fn other() {}\n// nothing here\n").unwrap();
    std::fs::write(dir.path().join("c.txt"), "handler appears in text too\n").unwrap();
    dir
}

#[test]
fn build_refuses_to_index_home_directory() {
    let _home = crate::core::data_dir::test_env_lock();
    // Auto-indexing HOME would walk the entire home tree and, on Windows,
    // hydrate every OneDrive placeholder (#363). The build must bail out.
    if let Some(home) = dirs::home_dir() {
        assert!(
            SearchIndex::build(&home.to_string_lossy(), true, false).is_none(),
            "search index must never auto-build over the home directory"
        );
    }
}

#[test]
fn narrows_to_files_containing_literal() {
    let dir = corpus();
    let idx = SearchIndex::build(dir.path().to_str().unwrap(), true, false).unwrap();
    let cands = idx.candidate_paths("handler", &[], dir.path());
    let paths = cands.into_paths();
    // a.rs and c.txt contain "handler"; b.rs must be excluded.
    assert!(paths.iter().any(|p| p.ends_with("a.rs")));
    assert!(paths.iter().any(|p| p.ends_with("c.txt")));
    assert!(!paths.iter().any(|p| p.ends_with("b.rs")));
}

#[test]
fn absent_trigram_yields_empty_candidates() {
    let dir = corpus();
    let idx = SearchIndex::build(dir.path().to_str().unwrap(), true, false).unwrap();
    match idx.candidate_paths("zzzqqq", &[], dir.path()) {
        CandidateSet::Narrowed(p) => assert!(p.is_empty(), "{p:?}"),
        CandidateSet::FullList(_) => panic!("pure literal should narrow"),
    }
}

#[test]
fn ext_filter_restricts_candidates() {
    let dir = corpus();
    let idx = SearchIndex::build(dir.path().to_str().unwrap(), true, false).unwrap();
    let paths = idx
        .candidate_paths(
            "handler",
            &[glob::Pattern::new("*.rs").unwrap()],
            dir.path(),
        )
        .into_paths();
    assert!(paths.iter().all(|p| p.extension().unwrap() == "rs"));
    assert!(paths.iter().any(|p| p.ends_with("a.rs")));
}

#[test]
#[cfg(unix)]
fn build_skips_named_pipe_without_hanging() {
    use std::sync::mpsc;
    use std::time::Duration;
    // #336: the background index build read every file, so a FIFO in the
    // corpus blocked the build thread forever. It must be skipped while the
    // regular files are still indexed, and the build must return.
    let dir = corpus();
    let fifo = dir.path().join("pipe.fifo");
    let c = std::ffi::CString::new(fifo.to_string_lossy().as_bytes()).unwrap();
    assert_eq!(
        // SAFETY: `c` is a live CString providing a valid NUL-terminated
        // path pointer for the duration of the call.
        unsafe { libc::mkfifo(c.as_ptr(), 0o644) },
        0,
        "mkfifo failed"
    );

    let root = dir.path().to_str().unwrap().to_string();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let built = SearchIndex::build(&root, true, false);
        let _ = tx.send(built.map(|idx| {
            idx.candidate_paths("handler", &[], std::path::Path::new(&root))
                .into_paths()
        }));
    });
    let paths = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("SearchIndex::build hung on a FIFO (#336 regression)")
        .expect("index should build");
    assert!(paths.iter().any(|p| p.ends_with("a.rs")));
    assert!(!paths.iter().any(|p| p.ends_with("pipe.fifo")));
}

#[test]
fn regex_query_falls_back_to_full_list() {
    let dir = corpus();
    let idx = SearchIndex::build(dir.path().to_str().unwrap(), true, false).unwrap();
    match idx.candidate_paths("fn .*\\(\\)", &[], dir.path()) {
        CandidateSet::FullList(p) => assert!(!p.is_empty(), "`p` is empty"),
        CandidateSet::Narrowed(_) => panic!("metachar query must not narrow"),
    }
}

#[test]
fn short_query_falls_back_to_full_list() {
    let dir = corpus();
    let idx = SearchIndex::build(dir.path().to_str().unwrap(), true, false).unwrap();
    assert!(matches!(
        idx.candidate_paths("fn", &[], dir.path()),
        CandidateSet::FullList(_)
    ));
}

/// The core correctness claim: trigram narrowing never drops a real match.
/// For each literal query, the set of `file:line` hits found by scanning only
/// the narrowed candidates must equal the set found by scanning every file.
#[test]
fn narrowing_has_identical_recall_to_full_scan() {
    use regex::Regex;
    use std::collections::BTreeSet;

    let dir = tempfile::tempdir().unwrap();
    // A spread of files; some contain the query tokens, most do not.
    let samples = [
        (
            "auth/login.rs",
            "fn authenticate(user) {}\nlet token = mint();\n",
        ),
        (
            "auth/session.rs",
            "struct Session;\n// authenticate again here\n",
        ),
        ("db/pool.rs", "fn connect() {}\nlet retries = 3;\n"),
        (
            "ui/button.tsx",
            "export const Button = () => authenticate;\n",
        ),
        ("readme.md", "This project uses authenticate flows.\n"),
        ("unrelated.rs", "fn helper() { let v = 1; }\n"),
    ];
    for (rel, content) in samples {
        let p = dir.path().join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
    }
    let idx = SearchIndex::build(dir.path().to_str().unwrap(), true, false).unwrap();

    let full_scan = |pat: &str| -> BTreeSet<String> {
        let re = Regex::new(pat).unwrap();
        let mut hits = BTreeSet::new();
        for (rel, content) in samples {
            for (i, line) in content.lines().enumerate() {
                if re.is_match(line) {
                    hits.insert(format!("{rel}:{}", i + 1));
                }
            }
        }
        hits
    };

    for query in ["authenticate", "Session", "retries", "token"] {
        let re = Regex::new(query).unwrap();
        let candidates = idx.candidate_paths(query, &[], dir.path()).into_paths();
        let mut narrowed = BTreeSet::new();
        for path in &candidates {
            let content = std::fs::read_to_string(path).unwrap();
            let rel = path.strip_prefix(dir.path()).unwrap().to_string_lossy();
            for (i, line) in content.lines().enumerate() {
                if re.is_match(line) {
                    narrowed.insert(format!("{}:{}", rel.replace('\\', "/"), i + 1));
                }
            }
        }
        assert_eq!(
            narrowed,
            full_scan(query),
            "recall mismatch for query {query:?}"
        );
    }
}

#[test]
fn intersect_sorted_basic() {
    assert_eq!(
        intersect_sorted(&[1, 2, 3, 5], &[2, 3, 4, 5]),
        vec![2, 3, 5]
    );
    assert_eq!(intersect_sorted(&[1, 2], &[3, 4]), Vec::<u32>::new());
}

// ── Bloom tier ────────────────────────────────────────────────────────

fn trigrams_of(s: &str) -> Vec<u32> {
    let mut set = HashSet::new();
    let b = s.as_bytes();
    if b.len() >= 3 {
        for w in b.windows(3) {
            if is_word_byte(w[0]) && is_word_byte(w[1]) && is_word_byte(w[2]) {
                set.insert(pack(w[0], w[1], w[2]));
            }
        }
    }
    let mut v: Vec<u32> = set.into_iter().collect();
    v.sort_unstable();
    v
}

#[test]
fn file_bloom_has_no_false_negatives() {
    let tris = trigrams_of("fn authenticate(user) { let token = mint(); }");
    let mut bloom = FileBloom::with_capacity(tris.len());
    for &t in &tris {
        bloom.insert(t);
    }
    // Every inserted trigram must be reported present (Bloom guarantee).
    assert!(tris.iter().all(|&t| bloom.maybe_contains(t)));
}

/// Parity fuzz: the Bloom tier must return a *superset* of the exact posting
/// tier for every query (zero false negatives). False positives are allowed
/// (and verified away downstream), so we assert containment, not equality.
#[test]
fn bloom_tier_is_superset_of_postings_tier() {
    // Deterministic synthetic corpus (LCG → reproducible).
    let mut seed = 0x1234_5678_9abc_def0u64;
    let mut rng = || {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (seed >> 33) as u32
    };
    let mut per_file: Vec<Vec<u32>> = Vec::new();
    for _ in 0..80 {
        let n = 50 + (rng() % 250) as usize;
        let mut s = HashSet::new();
        for _ in 0..n {
            s.insert(rng() & 0x00FF_FFFF);
        }
        let mut v: Vec<u32> = s.into_iter().collect();
        v.sort_unstable();
        per_file.push(v);
    }
    let total: usize = per_file.iter().map(Vec::len).sum();

    let postings = build_narrowing(&per_file, total); // ≤ cap → postings
    let blooms = build_narrowing(&per_file, MAX_POSTING_ENTRIES + 1); // forced bloom
    let (Narrowing::Postings(pt), Narrowing::Blooms(bl)) = (&postings, &blooms) else {
        panic!("unexpected narrowing tiers");
    };

    // Queries drawn from real file trigrams (these MUST be found by both),
    // plus a few that are unlikely to exist anywhere.
    for f in &per_file {
        if f.len() < 3 {
            continue;
        }
        let q = vec![f[0], f[f.len() / 2], f[f.len() - 1]];
        let exact = SearchIndex::postings_intersect(pt, &q);
        let bloom: HashSet<u32> = SearchIndex::bloom_scan(bl, &q).into_iter().collect();
        for id in exact {
            assert!(
                bloom.contains(&id),
                "Bloom tier dropped a true match (false negative) for {q:?}"
            );
        }
    }
}

/// End-to-end: an index forced onto the Bloom tier must still surface every
/// file that actually contains the literal (recall parity with a full scan).
#[test]
fn bloom_tier_end_to_end_recall() {
    let samples = [
        (
            "auth_login.rs",
            "fn authenticate(user) {}\nlet token = mint();\n",
        ),
        (
            "auth_session.rs",
            "struct Session;\n// authenticate again here\n",
        ),
        ("db_pool.rs", "fn connect() {}\nlet retries = 3;\n"),
        (
            "ui_button.tsx",
            "export const Button = () => authenticate;\n",
        ),
        ("readme.md", "This project uses authenticate flows.\n"),
        ("unrelated.rs", "fn helper() { let v = 1; }\n"),
    ];
    let files: Vec<PathBuf> = samples.iter().map(|(rel, _)| PathBuf::from(rel)).collect();
    let per_file: Vec<Vec<u32>> = samples.iter().map(|(_, c)| trigrams_of(c)).collect();

    let idx = SearchIndex {
        files,
        narrowing: build_narrowing(&per_file, MAX_POSTING_ENTRIES + 1),
        respect_gitignore: true,
        allow_secret_paths: false,
        signature: 0, // freshness is not exercised by candidate_paths
        policy_epoch: crate::core::context_admission::stores::current_epoch(),
    };
    assert!(
        matches!(idx.narrowing, Narrowing::Blooms(_)),
        "test must exercise the Bloom tier"
    );

    for query in ["authenticate", "Session", "retries", "token"] {
        let cands: HashSet<String> = idx
            .candidate_paths(query, &[], std::path::Path::new(""))
            .into_paths()
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect();
        for (rel, content) in samples {
            if content.contains(query) {
                assert!(
                    cands.contains(rel),
                    "Bloom tier dropped real match {rel} for query {query:?}"
                );
            }
        }
    }
}

/// A scoped override of `LEAN_CTX_DATA_DIR`, restored on drop, so the
/// cross-process lock files land in an isolated temp dir during tests.
struct DataDirGuard {
    prev: Option<String>,
}
impl DataDirGuard {
    fn set(path: &std::path::Path) -> Self {
        let prev = std::env::var("LEAN_CTX_DATA_DIR").ok();
        crate::test_env::set_var("LEAN_CTX_DATA_DIR", path);
        Self { prev }
    }
}
impl Drop for DataDirGuard {
    fn drop(&mut self) {
        match self.prev.as_deref() {
            Some(v) => crate::test_env::set_var("LEAN_CTX_DATA_DIR", v),
            None => crate::test_env::remove_var("LEAN_CTX_DATA_DIR"),
        }
    }
}

#[test]
fn search_index_lock_name_is_per_repo_and_distinct() {
    let a = search_index_lock_name("/tmp/repo-a");
    let b = search_index_lock_name("/tmp/repo-b");
    assert!(a.starts_with("search-idx-"), "unexpected lock name: {a}");
    assert_ne!(a, b, "lock name must be per-repo");
    assert_eq!(a, search_index_lock_name("/tmp/repo-a"), "stable per repo");
    // Must not collide with the graph/bm25 locks for the same repo, or the
    // three indexers would needlessly serialize against one another.
    let h = &crate::core::index_namespace::namespace_hash(Path::new("/tmp/repo-a"))[..8];
    assert_ne!(
        a,
        format!("graph-idx-{h}"),
        "must not collide with graph lock"
    );
    assert_ne!(
        a,
        format!("bm25-idx-{h}"),
        "must not collide with bm25 lock"
    );
}

#[test]
fn build_guarded_builds_when_uncontended() {
    let _env = crate::core::data_dir::test_env_lock();
    let data = tempfile::tempdir().unwrap();
    let _guard = DataDirGuard::set(data.path());

    let dir = corpus();
    let root = dir.path().to_string_lossy().to_string();
    // Seed the in-flight flag the way `ensure_background` does before spawn.
    {
        let mut map = cache()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        map.insert(
            root.clone(),
            CacheEntry {
                index: None,
                building: true,
                last_verified: None,
            },
        );
    }
    assert_eq!(
        build_guarded(&root, true, false),
        BuildOutcome::Built,
        "an uncontended root must build"
    );
    let map = cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let entry = map.get(&root).expect("entry present");
    assert!(!entry.building, "building flag must clear after build");
    assert!(entry.index.is_some(), "index must be installed after build");
}

#[test]
fn build_guarded_defers_when_another_process_holds_the_lock() {
    let _env = crate::core::data_dir::test_env_lock();
    let data = tempfile::tempdir().unwrap();
    let _guard = DataDirGuard::set(data.path());

    let dir = corpus();
    let root = dir.path().to_string_lossy().to_string();
    // Pre-hold the cross-process lock with *this* (alive) PID and a fresh
    // mtime, so neither the dead-owner nor the staleness reclaim can take it
    // — exactly the "another session is already building" state from #460.
    let lock_path = data
        .path()
        .join(format!(".{}.lock", search_index_lock_name(&root)));
    std::fs::write(&lock_path, format!("{}\n", std::process::id())).unwrap();

    {
        let mut map = cache()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        map.insert(
            root.clone(),
            CacheEntry {
                index: None,
                building: true,
                last_verified: None,
            },
        );
    }
    assert_eq!(
        build_guarded(&root, true, false),
        BuildOutcome::Deferred,
        "a contended root must defer the proactive pre-warm"
    );
    let map = cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let entry = map.get(&root).expect("entry present");
    assert!(
        !entry.building,
        "deferred build must clear the in-flight flag so a later nudge retries"
    );
    assert!(
        entry.index.is_none(),
        "deferred build must not run a second walk / install an index"
    );
}

#[test]
fn corpus_signature_matches_a_freshly_built_index() {
    let dir = corpus();
    let root = dir.path().to_string_lossy().to_string();
    let idx = SearchIndex::build(&root, true, false).expect("index builds");
    let sig = corpus_signature(&root, true, false).expect("signature computes");
    assert_eq!(
        idx.signature, sig,
        "a freshly built index must match the live corpus signature, or it \
         would be treated as permanently stale and never served"
    );
    // Re-derivation is deterministic for an unchanged corpus.
    assert_eq!(sig, corpus_signature(&root, true, false).unwrap());
}

#[test]
fn corpus_signature_and_index_include_searchable_hidden_files() {
    let dir = corpus();
    let root = dir.path().to_string_lossy().to_string();
    let hidden = dir.path().join(".searchable.txt");
    std::fs::write(&hidden, "hiddenmarker before\n").unwrap();
    let index = SearchIndex::build(&root, true, false).expect("index builds");
    assert!(
        index
            .candidate_paths("hiddenmarker", &[], dir.path())
            .into_paths()
            .contains(&hidden)
    );
    let before = corpus_signature(&root, true, false).unwrap();
    assert_eq!(index.signature, before);
    std::fs::write(&hidden, "hiddenmarker after an edit\n").unwrap();
    assert_ne!(before, corpus_signature(&root, true, false).unwrap());
}

#[test]
fn corpus_signature_changes_on_edit_add_and_delete() {
    let dir = corpus();
    let root = dir.path().to_string_lossy().to_string();
    let base = corpus_signature(&root, true, false).unwrap();

    std::fs::write(
        dir.path().join("a.rs"),
        "fn handler() {}\nlet x = 1;\nlet y = 2;\n",
    )
    .unwrap();
    let after_edit = corpus_signature(&root, true, false).unwrap();
    assert_ne!(
        base, after_edit,
        "an in-place edit must change the signature"
    );

    std::fs::write(dir.path().join("d.rs"), "fn fresh() {}\n").unwrap();
    let after_add = corpus_signature(&root, true, false).unwrap();
    assert_ne!(
        after_edit, after_add,
        "adding a file must change the signature"
    );

    std::fs::remove_file(dir.path().join("b.rs")).unwrap();
    let after_delete = corpus_signature(&root, true, false).unwrap();
    assert_ne!(
        after_add, after_delete,
        "deleting a file must change the signature"
    );
}

#[test]
fn get_fresh_serves_unchanged_then_refuses_after_edit() {
    let _env = crate::core::data_dir::test_env_lock();
    let data = tempfile::tempdir().unwrap();
    let _guard = DataDirGuard::set(data.path());
    crate::test_env::remove_var("LEAN_CTX_DISABLE_SEARCH_INDEX");
    crate::test_env::remove_var("LEAN_CTX_SEARCH_INDEX_COALESCE_MS");

    let dir = corpus();
    let root = dir.path().to_string_lossy().to_string();
    assert!(warm_blocking(&root, true, false), "index warms");

    // Unchanged corpus → the resident index is served.
    assert!(
        get_fresh(&root, true, false).is_some(),
        "an unchanged corpus must serve the resident index"
    );

    // Native edit (size changes) → the now-stale index is refused so the
    // caller walks the live corpus instead of trusting outdated trigrams.
    std::fs::write(
        dir.path().join("a.rs"),
        "fn handler() {}\nlet x = 1;\nlet z = 9;\n",
    )
    .unwrap();
    assert!(
        get_fresh(&root, true, false).is_none(),
        "an edited corpus must refuse the stale resident index (#624)"
    );
}

fn resident(idx: &Arc<SearchIndex>, verified: Instant, building: bool) -> CacheEntry {
    CacheEntry {
        index: Some(Arc::clone(idx)),
        building,
        last_verified: Some(verified),
    }
}

#[test]
fn resident_roots_are_capped_lru_first() {
    // #2075: a daemon indexed every root its sessions touched and kept each
    // index forever. Beyond the cap the least recently verified root goes.
    let dir = corpus();
    let idx = Arc::new(SearchIndex::build(dir.path().to_str().unwrap(), true, false).unwrap());
    let t0 = Instant::now();
    let mut map = HashMap::new();
    for i in 0..=MAX_RESIDENT_ROOTS {
        let root = format!("/r{i}");
        let at = t0 + Duration::from_secs(i as u64);
        map.insert(root.clone(), resident(&idx, at, false));
        enforce_root_cap(&mut map, &root);
    }
    assert_eq!(
        map.values().filter(|e| e.index.is_some()).count(),
        MAX_RESIDENT_ROOTS
    );
    assert!(!map.contains_key("/r0"), "oldest root must be evicted");
    assert!(map.contains_key(&format!("/r{MAX_RESIDENT_ROOTS}")));
}

#[test]
fn cap_keeps_the_installed_root_and_in_flight_build_entries() {
    let dir = corpus();
    let idx = Arc::new(SearchIndex::build(dir.path().to_str().unwrap(), true, false).unwrap());
    let t0 = Instant::now();
    let mut map = HashMap::new();
    // The root being installed carries the oldest timestamp; it still stays.
    map.insert("/new".to_string(), resident(&idx, t0, false));
    // The next-oldest is mid-rebuild: its index goes, its `building` flag stays.
    let next = t0 + Duration::from_secs(1);
    map.insert("/building".to_string(), resident(&idx, next, true));
    for i in 0..MAX_RESIDENT_ROOTS {
        let at = t0 + Duration::from_secs(2 + i as u64);
        map.insert(format!("/r{i}"), resident(&idx, at, false));
    }
    enforce_root_cap(&mut map, "/new");
    assert!(map["/new"].index.is_some());
    assert!(map["/building"].index.is_none() && map["/building"].building);
    assert_eq!(
        map.values().filter(|e| e.index.is_some()).count(),
        MAX_RESIDENT_ROOTS
    );
}
