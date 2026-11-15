// SPDX-License-Identifier: Apache-2.0
use super::*;
use std::sync::atomic::AtomicUsize;

const RUST_SRC: &str = "use crate::auth::Token;\n\npub struct Session { id: u64 }\n\nimpl Session {\n    pub fn open(token: &Token) -> Self { Session { id: token.id() } }\n}\n\npub async fn login(name: &str) -> Result<Session, String> {\n    helper(name).await\n}\n";

fn identity(tag: &str) -> [u8; 32] {
    *blake3::hash(tag.as_bytes()).as_bytes()
}

fn open(dir: &Path) -> Arc<Store> {
    Arc::new(Store::open(dir, identity("build-a"), 64 * 1024 * 1024).expect("store opens"))
}

fn entries(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for shard in fs::read_dir(dir.join("v1")).unwrap().flatten() {
        if shard.path().is_dir() {
            for f in fs::read_dir(shard.path()).unwrap().flatten() {
                if f.path().extension().is_some_and(|e| e == "lcd") {
                    out.push(f.path());
                }
            }
        }
    }
    out.sort();
    out
}

#[test]
fn warm_results_equal_cold_results_for_every_derivation() {
    let tmp = tempfile::tempdir().unwrap();
    let store = open(tmp.path());

    let cold_chunks = crate::core::bm25_index::extract_chunks("src/session.rs", RUST_SRC);
    let cold_analysis = crate::core::deep_queries::analyze(RUST_SRC, "rs");
    let cold_sigs = crate::core::signatures::extract_signatures(RUST_SRC, "rs");
    assert!(!cold_chunks.is_empty() && !cold_sigs.is_empty());

    for round in 0..2 {
        with_store(Some(store.clone()), || {
            assert_eq!(
                chunks("src/session.rs", RUST_SRC),
                cold_chunks,
                "round {round}"
            );
            let warm = deep_analysis(RUST_SRC, "rs");
            assert_eq!(
                format!("{warm:?}"),
                format!("{cold_analysis:?}"),
                "round {round}"
            );
            let sigs = signatures(RUST_SRC, "rs");
            assert_eq!(
                format!("{sigs:?}"),
                format!("{cold_sigs:?}"),
                "round {round}"
            );
        });
        assert_eq!(entries(tmp.path()).len(), 3, "one entry per derivation");
    }
}

#[test]
fn a_warm_entry_is_served_without_parsing_and_any_key_change_is_a_miss() {
    let tmp = tempfile::tempdir().unwrap();
    let store = open(tmp.path());
    let parses = AtomicUsize::new(0);
    let get = |store: &Store, selector: &str, content: &str| {
        store.get_or_compute(
            Kind::DeepAnalysis,
            selector,
            content,
            StoredAnalysis::into_analysis,
            |a: &DeepAnalysis| Some(StoredAnalysis::from_analysis(a)),
            || {
                parses.fetch_add(1, Ordering::SeqCst);
                (crate::core::deep_queries::analyze(content, selector), true)
            },
        )
    };
    get(&store, "rs", RUST_SRC);
    get(&store, "rs", RUST_SRC);
    assert_eq!(
        parses.load(Ordering::SeqCst),
        1,
        "the warm call did not parse"
    );

    get(&store, "rs", &format!("{RUST_SRC}\nfn edited() {{}}\n"));
    get(&store, "ts", RUST_SRC);
    assert_eq!(
        parses.load(Ordering::SeqCst),
        3,
        "content and selector are keyed"
    );

    let other_build = Store::open(tmp.path(), identity("build-b"), 64 << 20).unwrap();
    get(&other_build, "rs", RUST_SRC);
    assert_eq!(
        parses.load(Ordering::SeqCst),
        4,
        "the build identity is keyed"
    );
}

#[test]
fn chunks_carry_the_callers_path_and_are_keyed_by_it() {
    let tmp = tempfile::tempdir().unwrap();
    let store = open(tmp.path());
    with_store(Some(store), || {
        let a = chunks("a/session.rs", RUST_SRC);
        let b = chunks("b/session.rs", RUST_SRC);
        assert!(a.iter().all(|c| c.file_path == "a/session.rs"));
        assert!(b.iter().all(|c| c.file_path == "b/session.rs"));
    });
    assert_eq!(entries(tmp.path()).len(), 2);
    let stored = fs::read(&entries(tmp.path())[0]).unwrap();
    assert!(
        !String::from_utf8_lossy(&stored).contains("session.rs"),
        "no path is stored in an entry"
    );
}

#[test]
fn corrupt_truncated_or_misplaced_entries_are_misses_and_removed() {
    let tmp = tempfile::tempdir().unwrap();
    let store = open(tmp.path());
    let expected = crate::core::bm25_index::extract_chunks("x.rs", RUST_SRC);
    let other = "fn other() {}\n";
    with_store(Some(store.clone()), || {
        chunks("x.rs", RUST_SRC);
        chunks("x.rs", other);
    });
    let files = entries(tmp.path());
    assert_eq!(files.len(), 2);

    let damage: [&dyn Fn(&Path); 4] = [
        &|p| {
            let mut b = fs::read(p).unwrap();
            let last = b.len() - 1;
            b[last] ^= 0xff;
            fs::write(p, b).unwrap();
        },
        &|p| {
            let b = fs::read(p).unwrap();
            fs::write(p, &b[..b.len() / 2]).unwrap();
        },
        &|p| fs::write(p, b"").unwrap(),
        &|p| {
            // A valid entry of another key, moved into this key's place.
            let donor = entries(p.parent().unwrap().parent().unwrap().parent().unwrap())
                .into_iter()
                .find(|d| d != p)
                .unwrap();
            fs::copy(donor, p).unwrap();
        },
    ];
    for (i, harm) in damage.iter().enumerate() {
        with_store(Some(store.clone()), || {
            chunks("x.rs", RUST_SRC);
            chunks("x.rs", other);
        });
        let target = store.entry_path(&store.key(Kind::Chunks, "x.rs", RUST_SRC));
        harm(&target);
        let got = with_store(Some(store.clone()), || chunks("x.rs", RUST_SRC));
        assert_eq!(got, expected, "damage {i} served as a miss");
        let reread = fs::read(&target).unwrap();
        assert!(decode_entry(&reread, &store.key(Kind::Chunks, "x.rs", RUST_SRC)).is_some());
    }
}

#[cfg(unix)]
#[test]
fn links_fifos_and_oversized_entries_are_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let store = open(tmp.path());
    let key = store.key(Kind::Signatures, "rs", RUST_SRC);
    let path = store.entry_path(&key);
    fs::create_dir_all(path.parent().unwrap()).unwrap();

    // A link to a valid entry of the same key is still refused.
    let real = tmp.path().join("real.lcd");
    with_store(Some(store.clone()), || signatures(RUST_SRC, "rs"));
    fs::rename(&path, &real).unwrap();
    std::os::unix::fs::symlink(&real, &path).unwrap();
    assert!(read_entry(&path).is_none(), "symlink refused");
    fs::remove_file(&path).unwrap();

    let c = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
    // SAFETY: a valid NUL-terminated path; mkfifo has no other preconditions.
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
    let started = std::time::Instant::now();
    assert!(read_entry(&path).is_none(), "FIFO refused");
    let sigs = with_store(Some(store.clone()), || signatures(RUST_SRC, "rs"));
    assert!(!sigs.is_empty());
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "no blocking open"
    );
    fs::remove_file(&path).unwrap();

    let file = fs::File::create(&path).unwrap();
    file.set_len((HEADER_LEN + MAX_PAYLOAD_BYTES + 1) as u64)
        .unwrap();
    assert!(read_entry(&path).is_none(), "oversized refused");
}

#[cfg(unix)]
#[test]
fn a_store_directory_open_to_others_or_a_link_keeps_the_cache_off() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let shared = tmp.path().join("shared");
    fs::create_dir(&shared).unwrap();
    fs::set_permissions(&shared, fs::Permissions::from_mode(0o777)).unwrap();
    assert!(Store::open(&shared, identity("x"), 1 << 20).is_none());

    let private = tmp.path().join("private");
    assert!(Store::open(&private, identity("x"), 1 << 20).is_some());
    let mode = fs::metadata(private.join("v1"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o700);

    let link = tmp.path().join("link");
    std::os::unix::fs::symlink(&private, &link).unwrap();
    assert!(Store::open(&link, identity("x"), 1 << 20).is_none());
}

#[cfg(unix)]
#[test]
fn entries_are_private_files() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    with_store(Some(open(tmp.path())), || deep_analysis(RUST_SRC, "rs"));
    for f in entries(tmp.path()) {
        assert_eq!(fs::metadata(f).unwrap().permissions().mode() & 0o777, 0o600);
    }
}

#[test]
fn oversized_results_are_not_stored_and_the_store_stays_in_budget() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path(), identity("b"), 1024 * 1024).unwrap();
    let big = "x".repeat(MAX_PAYLOAD_BYTES + 1);
    let out = store.get_or_compute(
        Kind::Chunks,
        "big",
        "big",
        Some,
        |s: &String| Some(s.clone()),
        || (big.clone(), true),
    );
    assert_eq!(out.len(), big.len());
    assert!(entries(tmp.path()).is_empty(), "oversized payload skipped");

    let blob = "y".repeat(64 * 1024);
    for i in 0..64 {
        store.get_or_compute(
            Kind::Chunks,
            "blob",
            &i.to_string(),
            Some,
            |s: &String| Some(s.clone()),
            || (blob.clone(), true),
        );
    }
    let total: u64 = entries(tmp.path())
        .iter()
        .map(|p| fs::metadata(p).unwrap().len())
        .sum();
    assert!(total <= 1024 * 1024, "eviction keeps {total} within budget");
    assert!(!entries(tmp.path()).is_empty());
}

#[test]
fn a_parse_that_panics_or_fell_back_after_a_panic_is_never_stored() {
    let tmp = tempfile::tempdir().unwrap();
    let store = open(tmp.path());
    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        store.get_or_compute(
            Kind::DeepAnalysis,
            "rs",
            RUST_SRC,
            StoredAnalysis::into_analysis,
            |a: &DeepAnalysis| Some(StoredAnalysis::from_analysis(a)),
            || -> (DeepAnalysis, bool) { panic!("grammar panic") },
        )
    }));
    assert!(panicked.is_err());
    assert!(entries(tmp.path()).is_empty());

    // A language blocklisted after a panic yields the regex fallback: neither
    // stored nor served from an earlier entry.
    let ext = "zz2247";
    crate::core::signatures::block_tree_sitter_language_for_tests(ext);
    let sigs = with_store(Some(store.clone()), || signatures(RUST_SRC, ext));
    assert_eq!(
        format!("{sigs:?}"),
        format!(
            "{:?}",
            crate::core::signatures::extract_signatures(RUST_SRC, ext)
        )
    );
    if crate::core::signatures::tree_sitter_language_blocked(ext) {
        assert!(entries(tmp.path()).is_empty(), "fallback not persisted");
    }
}

/// Store a signature entry for `content`/`ext` whose only signature is named
/// `name`, as an earlier run would have.
fn seed_signatures(store: &Store, ext: &str, content: &str, name: &str, tree_sitter: bool) {
    let seeded = StoredSignatures {
        tree_sitter,
        sigs: vec![StoredSignature {
            kind: "fn".into(),
            name: name.into(),
            params: String::new(),
            return_type: String::new(),
            is_async: false,
            is_exported: true,
            indent: 0,
            start_line: Some(1),
            end_line: Some(1),
        }],
    };
    let key = store.key(Kind::Signatures, ext, content);
    store.save(&key, &postcard::to_stdvec(&seeded).unwrap());
}

#[test]
fn an_entry_stored_before_a_panic_is_not_served_while_the_language_is_blocked() {
    let tmp = tempfile::tempdir().unwrap();
    let store = open(tmp.path());
    let ext = "zz2247seeded";
    let content = "fn plain() {}\n";
    seed_signatures(&store, ext, content, "SEEDED", false);

    let served = with_store(Some(store.clone()), || signatures(content, ext));
    assert_eq!(served.len(), 1);
    assert_eq!(served[0].name, "SEEDED", "the seeded entry is served");

    crate::core::signatures::block_tree_sitter_language_for_tests(ext);
    if crate::core::signatures::tree_sitter_language_blocked(ext) {
        let blocked = with_store(Some(store.clone()), || signatures(content, ext));
        assert!(
            blocked.iter().all(|s| s.name != "SEEDED"),
            "a blocked language bypasses the store"
        );
        assert_eq!(
            format!("{blocked:?}"),
            format!(
                "{:?}",
                crate::core::signatures::extract_signatures(content, ext)
            )
        );
    }
}

#[test]
fn a_served_signature_set_counts_its_backend_like_a_parse() {
    let tmp = tempfile::tempdir().unwrap();
    let store = open(tmp.path());
    let ext = "zz2247count";
    let content = "fn counted() {}\n";
    seed_signatures(&store, ext, content, "COUNTED", false);
    let (_, regex_before) = crate::core::signatures::signature_backend_stats();
    let served = with_store(Some(store.clone()), || signatures(content, ext));
    assert_eq!(served[0].name, "COUNTED", "served from the store");
    let (_, regex_after) = crate::core::signatures::signature_backend_stats();
    assert!(
        regex_after > regex_before,
        "a hit counts in signature_backend_stats like the parse it replaces"
    );
}

#[test]
fn every_kind_round_trips_through_its_tag() {
    let mut decodable = [0usize; 3];
    for tag in 0..=u8::MAX {
        if let Some(k) = chunk_kind_from_tag(tag) {
            assert_eq!(chunk_kind_tag(&k), Some(tag));
            decodable[0] += 1;
        }
        if let Some(k) = import_kind_from_tag(tag) {
            assert_eq!(import_kind_tag(&k), tag);
            decodable[1] += 1;
        }
        if let Some(k) = type_kind_from_tag(tag) {
            assert_eq!(type_kind_tag(&k), tag);
            decodable[2] += 1;
        }
    }
    // Every encodable variant decodes: 7 parse chunk kinds, all import and
    // type kinds (the encoders match exhaustively).
    assert_eq!(decodable, [7, 6, 10]);
    assert_eq!(
        chunk_kind_tag(&ChunkKind::Issue),
        None,
        "external kinds are not stored"
    );
    assert!(intern_kind("fn").is_some());
    assert!(intern_kind("").is_none());
    assert!(intern_kind(&"k".repeat(33)).is_none());
}

/// Measurement for #2247 (run alone, `--ignored --nocapture`): graph scan +
/// BM25 build of a corpus (`LEAN_CTX_MEASURE_CORPUS`, default this crate's
/// `src`) as cold A (empty store), warm fresh A (a second copy), and B (a copy
/// with edited files), reporting wall, process CPU and store size, and checking
/// that every warm index equals its cold one.
#[cfg(unix)]
#[test]
#[ignore = "measurement; run explicitly"]
fn measure_cold_warm_and_edited_worktree_builds() {
    fn copy_tree(from: &Path, to: &Path) -> usize {
        let mut n = 0;
        for entry in fs::read_dir(from).unwrap().flatten() {
            let (src, dst) = (entry.path(), to.join(entry.file_name()));
            let kind = entry.file_type().unwrap();
            if kind.is_dir() {
                fs::create_dir_all(&dst).unwrap();
                n += copy_tree(&src, &dst);
            } else if kind.is_file() {
                fs::copy(&src, &dst).unwrap();
                n += 1;
            }
        }
        n
    }
    fn cpu() -> Duration {
        // SAFETY: a zeroed rusage is a valid out-parameter for getrusage.
        let mut u: libc::rusage = unsafe { std::mem::zeroed() };
        // SAFETY: RUSAGE_SELF with a valid pointer.
        unsafe { libc::getrusage(libc::RUSAGE_SELF, &raw mut u) };
        let tv = |t: libc::timeval| {
            Duration::from_secs(t.tv_sec as u64) + Duration::from_micros(t.tv_usec as u64)
        };
        tv(u.ru_utime) + tv(u.ru_stime)
    }
    fn dir_bytes(dir: &Path) -> u64 {
        fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| {
                let m = e.metadata().unwrap();
                if m.is_dir() {
                    dir_bytes(&e.path())
                } else {
                    m.len()
                }
            })
            .sum()
    }
    let build = |root: &Path| {
        let graph = crate::core::graph_index::scan(&root.to_string_lossy());
        let bm25 = crate::core::bm25_index::BM25Index::build_from_directory(root);
        let mut edges: Vec<String> = graph
            .edges
            .iter()
            .map(|e| format!("{}>{}:{}", e.from, e.to, e.kind))
            .collect();
        edges.sort();
        (edges, bm25.chunks)
    };

    let corpus = std::env::var("LEAN_CTX_MEASURE_CORPUS").map_or_else(
        |_| Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        PathBuf::from,
    );
    let work = tempfile::tempdir().unwrap();
    let data = work.path().join("data");
    fs::create_dir_all(&data).unwrap();
    // SAFETY: this ignored test runs alone; no other thread reads the env.
    unsafe { std::env::set_var("LEAN_CTX_DATA_DIR", &data) };
    let roots: Vec<PathBuf> = ["a", "a2", "b", "b_ref"]
        .iter()
        .map(|n| work.path().join(n))
        .collect();
    let mut files = 0;
    for r in &roots {
        fs::create_dir_all(r).unwrap();
        files = copy_tree(&corpus, r);
    }
    let mut edited = 0;
    for r in [&roots[2], &roots[3]] {
        edited = 0;
        for entry in fs::read_dir(r.join("core")).unwrap().flatten() {
            let p = entry.path();
            if edited < 20 && p.extension().is_some_and(|e| e == "rs") {
                let mut body = fs::read_to_string(&p).unwrap();
                body.push_str("\npub fn edited_for_measurement() -> u32 { 2247 }\n");
                fs::write(&p, body).unwrap();
                edited += 1;
            }
        }
    }

    let store =
        Arc::new(Store::open(&work.path().join("store"), identity("measure"), 4 << 30).unwrap());
    let reference = with_store(None, || build(&roots[3]));
    let mut cold_a = None;
    for (label, root) in [
        ("cold A", &roots[0]),
        ("warm fresh A", &roots[1]),
        ("B edited", &roots[2]),
    ] {
        let (w0, c0) = (std::time::Instant::now(), cpu());
        let out = with_store(Some(store.clone()), || build(root));
        let (wall, cpu_used) = (w0.elapsed(), cpu().saturating_sub(c0));
        eprintln!(
            "#2247 {label}: {files} files ({edited} edited in B), wall {:.2}s, cpu {:.2}s, store {} KiB",
            wall.as_secs_f64(),
            cpu_used.as_secs_f64(),
            dir_bytes(&work.path().join("store")) / 1024
        );
        match label {
            "cold A" => cold_a = Some(out),
            "warm fresh A" => {
                let cold = cold_a.as_ref().unwrap();
                assert_eq!(out.0, cold.0, "warm A graph edges equal cold A");
                assert_eq!(out.1, cold.1, "warm A chunks equal cold A");
            }
            _ => {
                assert_eq!(out.0, reference.0, "B graph edges equal uncached B");
                assert_eq!(out.1, reference.1, "B chunks equal uncached B");
            }
        }
    }
}

/// Two roots of one repository: B shares most files with A and has one edited
/// and one new file. B's BM25 index built against A's warm store equals B's
/// index built with no store, and the store holds no root path.
#[test]
fn a_second_root_with_edited_files_indexes_identically_from_a_warm_store() {
    let cache = tempfile::tempdir().unwrap();
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let files = [
        ("src/session.rs", RUST_SRC.to_string()),
        (
            "src/util.rs",
            "pub fn helper(x: u32) -> u32 { x + 1 }\n".to_string(),
        ),
        (
            "web/app.ts",
            "export function boot(): void { start(); }\n".to_string(),
        ),
    ];
    for root in [a.path(), b.path()] {
        for (rel, body) in &files {
            let p = root.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, body).unwrap();
        }
    }
    fs::write(
        b.path().join("src/util.rs"),
        "pub fn helper(x: u32) -> u32 { x + 2 }\npub fn edited() {}\n",
    )
    .unwrap();
    fs::write(b.path().join("src/new.rs"), "pub struct Fresh;\n").unwrap();

    let uncached = with_store(None, || {
        crate::core::bm25_index::BM25Index::build_from_directory(b.path())
    });
    let store = open(cache.path());
    with_store(Some(store.clone()), || {
        crate::core::bm25_index::BM25Index::build_from_directory(a.path())
    });
    let warm_entries = entries(cache.path()).len();
    let warm = with_store(Some(store), || {
        crate::core::bm25_index::BM25Index::build_from_directory(b.path())
    });
    assert_eq!(warm.chunks, uncached.chunks);
    assert!(!warm.chunks.is_empty());
    assert_eq!(
        entries(cache.path()).len(),
        warm_entries + 2,
        "only B's edited and new files were parsed"
    );
    for entry in entries(cache.path()) {
        let bytes = fs::read(entry).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        for root in [a.path(), b.path()] {
            assert!(
                !text.contains(&*root.to_string_lossy()),
                "no root path stored"
            );
        }
    }
}
