//! Tests for the BM25 index. Extracted from `bm25_index/mod.rs`;
//! `super::*` resolves to the `bm25_index` module.

use super::*;
use tempfile::tempdir;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

#[test]
fn tokenize_splits_code() {
    let tokens = tokenize("fn calculate_total(items: Vec<Item>) -> f64");
    assert!(tokens.contains(&"calculate_total".to_string()));
    assert!(tokens.contains(&"items".to_string()));
    assert!(tokens.contains(&"Vec".to_string()));
}

#[test]
fn format_search_results_normalizes_windows_separators() {
    // Issue #324: Windows backslash paths in search output were dropped or
    // escape-mangled by client render layers. They must come out with
    // forward slashes.
    let r = SearchResult {
        chunk_idx: 0,
        score: 1.0,
        file_path: r"C:\Users\zir\AppData\Local\Temp\win-build-log.txt".to_string(),
        symbol_name: "main".to_string(),
        kind: ChunkKind::Function,
        start_line: 1,
        end_line: 2,
        snippet: "x".to_string(),
    };
    let compact = format_search_results(std::slice::from_ref(&r), true);
    assert!(compact.contains("C:/Users/zir/AppData/Local/Temp/win-build-log.txt"));
    assert!(!compact.contains('\\'));

    let verbose = format_search_results(std::slice::from_ref(&r), false);
    assert!(verbose.contains("C:/Users/zir/AppData/Local/Temp/win-build-log.txt"));
    assert!(!verbose.contains('\\'));
}

#[test]
fn format_search_results_leaves_external_uris_untouched() {
    // Provider results (e.g. github://) are not OS paths and must not be
    // rewritten.
    let r = SearchResult {
        chunk_idx: 0,
        score: 1.0,
        file_path: "github://owner/repo/issues/42".to_string(),
        symbol_name: "issue".to_string(),
        kind: ChunkKind::Module,
        start_line: 0,
        end_line: 0,
        snippet: "y".to_string(),
    };
    let out = format_search_results(std::slice::from_ref(&r), true);
    assert!(out.contains("github://owner/repo/issues/42"));
}

#[test]
fn camel_case_splitting() {
    let tokens = split_camel_case_tokens(&["calculateTotal".to_string()]);
    assert!(tokens.contains(&"calculateTotal".to_string()));
    assert!(tokens.contains(&"calculate".to_string()));
    assert!(tokens.contains(&"Total".to_string()));
}

#[test]
fn detect_rust_function() {
    let (name, kind) = detect_symbol("pub fn process_request(req: Request) -> Response {").unwrap();
    assert_eq!(name, "process_request");
    assert_eq!(kind, ChunkKind::Function);
}

#[test]
fn bm25_search_finds_relevant() {
    let mut index = BM25Index::new();
    index.add_chunk(CodeChunk {
        file_path: "auth.rs".into(),
        symbol_name: "validate_token".into(),
        kind: ChunkKind::Function,
        start_line: 1,
        end_line: 10,
        content: "fn validate_token(token: &str) -> bool { check_jwt_expiry(token) }".into(),
        tokens: tokenize("fn validate_token token str bool check_jwt_expiry token"),
        token_count: 8,
    });
    index.add_chunk(CodeChunk {
        file_path: "db.rs".into(),
        symbol_name: "connect_database".into(),
        kind: ChunkKind::Function,
        start_line: 1,
        end_line: 5,
        content: "fn connect_database(url: &str) -> Pool { create_pool(url) }".into(),
        tokens: tokenize("fn connect_database url str Pool create_pool url"),
        token_count: 7,
    });
    index.finalize();

    let results = index.search("jwt token validation", 5);
    assert!(!results.is_empty());
    assert_eq!(results[0].symbol_name, "validate_token");
}

#[test]
fn bm25_search_sorts_ties_deterministically() {
    let mut index = BM25Index::new();

    // Insert in reverse path order to ensure the sort tie-break matters.
    index.add_chunk(CodeChunk {
        file_path: "b.rs".into(),
        symbol_name: "same".into(),
        kind: ChunkKind::Function,
        start_line: 1,
        end_line: 1,
        content: "fn same() {}".into(),
        tokens: tokenize("same token"),
        token_count: 2,
    });
    index.add_chunk(CodeChunk {
        file_path: "a.rs".into(),
        symbol_name: "same".into(),
        kind: ChunkKind::Function,
        start_line: 1,
        end_line: 1,
        content: "fn same() {}".into(),
        tokens: tokenize("same token"),
        token_count: 2,
    });
    index.finalize();

    let results = index.search("same", 10);
    assert!(results.len() >= 2);
    assert_eq!(results[0].file_path, "a.rs");
    assert_eq!(results[1].file_path, "b.rs");
}

#[test]
fn bm25_index_is_stale_when_any_indexed_file_is_missing() {
    let td = tempdir().expect("tempdir");
    let root = td.path();
    std::fs::write(root.join("a.rs"), "pub fn a() {}\n").expect("write a.rs");

    let idx = BM25Index::build_from_directory(root);
    assert!(!bm25_index_looks_stale(&idx, root));

    std::fs::remove_file(root.join("a.rs")).expect("remove a.rs");
    assert!(bm25_index_looks_stale(&idx, root));
}

#[test]
#[cfg(unix)]
fn bm25_incremental_rebuild_reuses_unchanged_files_without_reading() {
    let td = tempdir().expect("tempdir");
    let root = td.path();

    std::fs::write(root.join("a.rs"), "pub fn a() { println!(\"A\"); }\n").expect("write a.rs");
    std::fs::write(root.join("b.rs"), "pub fn b() { println!(\"B\"); }\n").expect("write b.rs");

    let idx1 = BM25Index::build_from_directory(root);
    assert!(idx1.files.contains_key("a.rs"));
    assert!(idx1.files.contains_key("b.rs"));

    // Make a.rs unreadable. Incremental rebuild must keep it indexed by reusing prior chunks.
    let a_path = root.join("a.rs");
    let mut perms = std::fs::metadata(&a_path).expect("meta a.rs").permissions();
    perms.set_mode(0o000);
    std::fs::set_permissions(&a_path, perms).expect("chmod a.rs");

    // Change b.rs (size changes) to force a re-read for that file.
    std::fs::write(root.join("b.rs"), "pub fn b() { println!(\"B2\"); }\n").expect("rewrite b.rs");

    let idx2 = BM25Index::rebuild_incremental(root, &idx1);
    assert!(
        idx2.files.contains_key("a.rs"),
        "a.rs should be kept via reuse"
    );
    assert!(idx2.files.contains_key("b.rs"));

    let b_has_b2 = idx2
        .chunks
        .iter()
        .any(|c| c.file_path == "b.rs" && c.content.contains("B2"));
    assert!(b_has_b2, "b.rs should be re-read and re-chunked");

    // Restore permissions to avoid cleanup surprises.
    let mut perms = std::fs::metadata(&a_path).expect("meta a.rs").permissions();
    perms.set_mode(0o644);
    let _ = std::fs::set_permissions(&a_path, perms);
}

#[test]
fn shrink_resident_trims_long_bodies_keeps_short_and_flags() {
    let mut index = BM25Index::new();
    let long_body = (0..20)
        .map(|i| format!("line{i}"))
        .collect::<Vec<_>>()
        .join("\n");
    index.add_chunk(CodeChunk {
        file_path: "long.rs".into(),
        symbol_name: "long".into(),
        kind: ChunkKind::Function,
        start_line: 1,
        end_line: 20,
        content: long_body,
        tokens: tokenize("long body"),
        token_count: 2,
    });
    index.add_chunk(CodeChunk {
        file_path: "short.rs".into(),
        symbol_name: "short".into(),
        kind: ChunkKind::Function,
        start_line: 1,
        end_line: 2,
        content: "fn short() {}".into(),
        tokens: tokenize("short body"),
        token_count: 2,
    });
    index.finalize();

    let before = index.memory_usage_bytes();
    assert!(!index.content_truncated);

    index.shrink_resident_content_to_snippet(5);

    assert!(index.content_truncated, "flag must be set after shrink");
    let long_chunk = index
        .chunks
        .iter()
        .find(|c| c.file_path == "long.rs")
        .unwrap();
    assert_eq!(
        long_chunk.content.lines().count(),
        5,
        "long body trimmed to 5 lines"
    );
    assert!(long_chunk.content.starts_with("line0"));
    assert!(
        !long_chunk.content.contains("line5"),
        "lines beyond the snippet window are dropped"
    );

    let short_chunk = index
        .chunks
        .iter()
        .find(|c| c.file_path == "short.rs")
        .unwrap();
    assert_eq!(
        short_chunk.content, "fn short() {}",
        "short body left untouched"
    );

    assert!(
        index.memory_usage_bytes() < before,
        "shrink should reduce reported heap usage"
    );

    // Keyword search snippet still works off the retained lines.
    let results = index.search("line0", 5);
    assert!(!results.is_empty());

    // Idempotent: second shrink is a no-op on already-short content.
    index.shrink_resident_content_to_snippet(5);
    let long_chunk = index
        .chunks
        .iter()
        .find(|c| c.file_path == "long.rs")
        .unwrap();
    assert_eq!(long_chunk.content.lines().count(), 5);
}

#[test]
fn shrink_resident_is_not_persisted_to_disk() {
    let _env = crate::core::data_dir::test_env_lock();
    let data_dir = tempdir().expect("data_dir");
    crate::test_env::set_var("LEAN_CTX_DATA_DIR", data_dir.path());
    crate::test_env::set_var("LEAN_CTX_BM25_MAX_CACHE_MB", "512");
    let td = tempdir().expect("tempdir");
    let root = td.path();
    std::fs::write(
        root.join("big.rs"),
        "pub fn big() {\n  let a = 1;\n  let b = 2;\n  let c = 3;\n  let d = 4;\n  let e = 5;\n  let f = 6;\n}\n",
    )
    .expect("write");

    let mut index = BM25Index::build_from_directory(root);
    let full_lines: usize = index
        .chunks
        .iter()
        .map(|c| c.content.lines().count())
        .max()
        .unwrap();
    assert!(full_lines > 5, "fixture body must exceed snippet window");

    // Real-flow ordering: the index is persisted with FULL content (during the
    // build/orchestrator pass) BEFORE any resident truncation. Truncation only
    // mutates the in-memory copy afterwards and is never followed by a save.
    index.save(root).expect("save full-content index");
    index.shrink_resident_content_to_snippet(5);
    assert!(index.content_truncated);

    // Reloading from the already-persisted file restores the FULL body, and the
    // resident-only `content_truncated` flag never survives serialization.
    let reloaded = BM25Index::load(root).expect("load");
    assert!(!reloaded.content_truncated, "flag must not survive reload");
    let max_lines: usize = reloaded
        .chunks
        .iter()
        .map(|c| c.content.lines().count())
        .max()
        .unwrap();
    assert_eq!(max_lines, full_lines, "reload restores full content");

    crate::test_env::remove_var("LEAN_CTX_BM25_MAX_CACHE_MB");
    crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
}

#[test]
fn load_quarantines_oversized_index() {
    let _env = crate::core::data_dir::test_env_lock();
    let td = tempdir().expect("tempdir");
    let root = td.path();
    let dir = crate::core::index_namespace::vectors_dir(root);
    std::fs::create_dir_all(&dir).expect("create vectors dir");

    let index_path = dir.join("bm25_index.json");
    crate::test_env::set_var("LEAN_CTX_BM25_MAX_CACHE_MB", "0");
    std::fs::write(&index_path, r#"{"chunks":[]}"#).expect("write index");

    let result = BM25Index::load(root);
    assert!(result.is_none(), "oversized index should return None");
    assert!(
        !index_path.exists(),
        "original index should be removed after quarantine"
    );
    assert!(
        dir.join("bm25_index.json.quarantined").exists(),
        "quarantined file should exist"
    );

    crate::test_env::remove_var("LEAN_CTX_BM25_MAX_CACHE_MB");
}

#[test]
fn save_refuses_oversized_output() {
    let _env = crate::core::data_dir::test_env_lock();
    let data_dir = tempdir().expect("data_dir");
    crate::test_env::set_var("LEAN_CTX_DATA_DIR", data_dir.path());
    crate::test_env::set_var("LEAN_CTX_BM25_MAX_CACHE_MB", "0");

    let td = tempdir().expect("tempdir");
    let root = td.path();

    let mut index = BM25Index::new();
    index.add_chunk(CodeChunk {
        file_path: "a.rs".into(),
        symbol_name: "a".into(),
        kind: ChunkKind::Function,
        start_line: 1,
        end_line: 1,
        content: "fn a() {}".into(),
        tokens: tokenize("fn a"),
        token_count: 2,
    });
    index.finalize();

    let outcome = index
        .save(root)
        .expect("save returns Ok even when refusing");
    assert!(
        matches!(outcome, SaveOutcome::SkippedTooLarge { .. }),
        "oversized save must report SkippedTooLarge (not a silent success), got {outcome:?}"
    );
    let index_path = BM25Index::index_file_path(root);
    assert!(
        !index_path.exists(),
        "save should refuse to persist oversized index"
    );

    crate::test_env::remove_var("LEAN_CTX_BM25_MAX_CACHE_MB");
}

#[test]
fn save_reports_persisted_outcome() {
    let _env = crate::core::data_dir::test_env_lock();
    let data_dir = tempdir().expect("data_dir");
    crate::test_env::set_var("LEAN_CTX_DATA_DIR", data_dir.path());
    crate::test_env::set_var("LEAN_CTX_BM25_MAX_CACHE_MB", "512");
    let td = tempdir().expect("tempdir");
    let root = td.path();
    std::fs::write(root.join("a.rs"), "pub fn alpha() {}\n").expect("write");

    let index = BM25Index::build_from_directory(root);
    let outcome = index.save(root).expect("save");
    match outcome {
        SaveOutcome::Persisted { compressed_bytes } => {
            assert!(compressed_bytes > 0, "persisted size should be non-zero");
        }
        SaveOutcome::SkippedTooLarge { .. } => {
            panic!("expected Persisted, got {outcome:?}")
        }
    }

    crate::test_env::remove_var("LEAN_CTX_BM25_MAX_CACHE_MB");
    crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
}

#[test]
fn persist_ceiling_honors_env_override() {
    // The public ceiling accessor (shared with doctor) must honor an explicit
    // override exactly, so operators can size it to their monorepo.
    let _env = crate::core::data_dir::test_env_lock();
    crate::test_env::set_var("LEAN_CTX_BM25_MAX_CACHE_MB", "777");
    assert_eq!(persist_ceiling_bytes(), 777 * 1024 * 1024);
    crate::test_env::remove_var("LEAN_CTX_BM25_MAX_CACHE_MB");
}

#[test]
fn save_writes_project_root_marker() {
    let _env = crate::core::data_dir::test_env_lock();
    let td = tempdir().expect("tempdir");
    let root = td.path();
    std::fs::write(root.join("a.rs"), "pub fn a() {}\n").expect("write");

    crate::test_env::remove_var("LEAN_CTX_BM25_MAX_CACHE_MB");
    let index = BM25Index::build_from_directory(root);
    index.save(root).expect("save");

    let dir = crate::core::index_namespace::vectors_dir(root);
    let marker = dir.join("project_root.txt");
    assert!(marker.exists(), "project_root.txt marker should exist");
    let content = std::fs::read_to_string(&marker).expect("read marker");
    assert_eq!(content, root.to_string_lossy());
}

#[test]
fn save_load_roundtrip_uses_zstd() {
    let _env = crate::core::data_dir::test_env_lock();
    let data_dir = tempdir().expect("data_dir");
    crate::test_env::set_var("LEAN_CTX_DATA_DIR", data_dir.path());
    crate::test_env::set_var("LEAN_CTX_BM25_MAX_CACHE_MB", "512");
    let td = tempdir().expect("tempdir");
    let root = td.path();

    for i in 0..10 {
        std::fs::write(
            root.join(format!("mod{i}.rs")),
            format!(
                "pub fn handler_{i}() {{\n    println!(\"hello\");\n}}\n\n\
                     pub fn helper_{i}() {{\n    println!(\"world\");\n}}\n"
            ),
        )
        .expect("write");
    }

    let index = BM25Index::build_from_directory(root);
    assert!(index.doc_count > 0, "should have indexed chunks");
    index.save(root).expect("save");

    let dir = crate::core::index_namespace::vectors_dir(root);
    let zst = dir.join("bm25_index.bin.zst");
    assert!(zst.exists(), "should write .bin.zst");
    assert!(
        !dir.join("bm25_index.bin").exists(),
        ".bin should be deleted"
    );

    let loaded = BM25Index::load(root).expect("load compressed index");
    assert_eq!(loaded.doc_count, index.doc_count);
    assert_eq!(loaded.chunks.len(), index.chunks.len());

    crate::test_env::remove_var("LEAN_CTX_BM25_MAX_CACHE_MB");
    crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
}

#[test]
fn auto_migrate_bin_to_zst() {
    let _env = crate::core::data_dir::test_env_lock();
    let data_dir = tempdir().expect("data_dir");
    crate::test_env::set_var("LEAN_CTX_DATA_DIR", data_dir.path());
    crate::test_env::set_var("LEAN_CTX_BM25_MAX_CACHE_MB", "512");
    let td = tempdir().expect("tempdir");
    let root = td.path();

    std::fs::write(root.join("a.rs"), "pub fn a() {}\n").expect("write");
    let index = BM25Index::build_from_directory(root);

    let dir = crate::core::index_namespace::vectors_dir(root);
    std::fs::create_dir_all(&dir).expect("mkdir");
    let data = postcard::to_allocvec(&index).expect("encode");
    std::fs::write(dir.join("bm25_index.bin"), &data).expect("write bin");

    let loaded = BM25Index::load(root).expect("load should auto-migrate");
    assert_eq!(loaded.doc_count, index.doc_count);
    assert!(
        dir.join("bm25_index.bin.zst").exists(),
        ".bin.zst should be created"
    );
    assert!(
        !dir.join("bm25_index.bin").exists(),
        ".bin should be removed"
    );

    crate::test_env::remove_var("LEAN_CTX_BM25_MAX_CACHE_MB");
    crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
}

#[test]
fn postcard_roundtrip_and_garbage_is_graceful() {
    let td = tempdir().expect("tempdir");
    let root = td.path();
    std::fs::write(root.join("a.rs"), "pub fn a() {}\npub fn b() {}\n").expect("write");
    let index = BM25Index::build_from_directory(root);
    assert!(
        index.doc_count > 0,
        "fixture must produce at least one chunk"
    );

    // (a) postcard round-trip: encode → decode → structural equality.
    let data = postcard::to_allocvec(&index).expect("postcard encode");
    let decoded: BM25Index = postcard::from_bytes(&data).expect("postcard decode");
    assert_eq!(
        decoded.doc_count, index.doc_count,
        "doc_count survives round-trip"
    );
    assert_eq!(
        decoded.chunks.len(),
        index.chunks.len(),
        "chunk count survives round-trip"
    );

    // (b) corrupt/legacy bytes must NOT panic — decode returns Err,
    //     which load() maps via .ok()? to None (→ rebuild), never a crash.
    let garbage = b"\x00\x01\x02not-a-valid-postcard-index\xff\xff";
    let res: Result<BM25Index, _> = postcard::from_bytes(garbage);
    assert!(
        res.is_err(),
        "corrupt/legacy bytes must decode to Err, not panic"
    );
}

#[test]
fn list_code_files_skips_default_vendor_ignores() {
    let td = tempdir().expect("tempdir");
    let root = td.path();

    std::fs::write(root.join("main.rs"), "pub fn main() {}\n").expect("write main");
    std::fs::create_dir_all(root.join("vendor/lib")).expect("mkdir vendor");
    std::fs::write(root.join("vendor/lib/dep.rs"), "pub fn dep() {}\n").expect("write vendor");
    std::fs::create_dir_all(root.join("dist")).expect("mkdir dist");
    std::fs::write(root.join("dist/bundle.js"), "function x() {}").expect("write dist");

    let files = list_code_files(root);
    assert!(
        files.iter().any(|f| f == "main.rs"),
        "main.rs should be included"
    );
    assert!(
        !files.iter().any(|f| f.starts_with("vendor/")),
        "vendor/ files should be excluded by DEFAULT_BM25_IGNORES"
    );
    assert!(
        !files.iter().any(|f| f.starts_with("dist/")),
        "dist/ files should be excluded by DEFAULT_BM25_IGNORES"
    );
}

#[test]
fn list_code_files_includes_extractable_pdf() {
    let td = tempdir().expect("tempdir");
    let root = td.path();

    std::fs::write(root.join("main.rs"), "pub fn main() {}\n").expect("write main");
    // A PDF is a binary document with a dedicated extractor: it must reach the
    // indexer (gate change) even though office binaries without one do not.
    std::fs::write(root.join("report.pdf"), b"%PDF-1.7\n%stub\n").expect("write pdf");
    std::fs::write(root.join("sheet.xlsx"), b"PK\x03\x04binary").expect("write xlsx");

    let files = list_code_files(root);
    assert!(
        files.iter().any(|f| f == "report.pdf"),
        "report.pdf should be ingestible via the extractor"
    );
    assert!(
        !files.iter().any(|f| f == "sheet.xlsx"),
        "office binaries without an extractor stay excluded"
    );
}

#[test]
fn list_code_files_respects_max_files_cap() {
    let td = tempdir().expect("tempdir");
    let root = td.path();

    // Create more files than MAX_BM25_FILES wouldn't let us test easily (5000),
    // but we can verify the cap constant exists and the function returns a bounded vec.
    for i in 0..10 {
        std::fs::write(
            root.join(format!("f{i}.rs")),
            format!("pub fn f{i}() {{}}\n"),
        )
        .expect("write");
    }
    let files = list_code_files(root);
    assert!(
        files.len() <= MAX_BM25_FILES,
        "file count should not exceed MAX_BM25_FILES"
    );
}

#[test]
fn max_bm25_cache_bytes_reads_env() {
    let _env = crate::core::data_dir::test_env_lock();
    crate::test_env::set_var("LEAN_CTX_BM25_MAX_CACHE_MB", "64");
    let bytes = max_bm25_cache_bytes();
    assert_eq!(bytes, 64 * 1024 * 1024);
    crate::test_env::remove_var("LEAN_CTX_BM25_MAX_CACHE_MB");
}

// ---- #933: parallel build determinism ----

/// Write a varied corpus that exercises the multi-chunk symbol path, the
/// fallback (prose) chunker, camelCase splitting and repeated-token `doc_freqs`
/// dedup. Returns the sorted relative paths.
fn write_parallel_corpus(root: &std::path::Path, n: usize) -> Vec<String> {
    std::fs::create_dir_all(root.join("src")).expect("mkdir src");
    std::fs::create_dir_all(root.join("docs")).expect("mkdir docs");
    let mut files = Vec::new();
    for i in 0..n {
        let rel = format!("src/mod_{i:03}.rs");
        let content = format!(
            "pub fn process_item_{i}(item: Item) -> Result<Item> {{\n    \
             let processedValue = transformItem(item);\n    \
             validate(processedValue); validate(processedValue);\n    \
             Ok(processedValue)\n}}\n\n\
             pub struct DataHolder{i} {{\n    field_one: String,\n    field_two: usize,\n}}\n\n\
             // shared keyword repeated repeated repeated across files\n"
        );
        std::fs::write(root.join(&rel), content).expect("write src");
        files.push(rel);
    }
    let notes = "docs/notes.md".to_string();
    std::fs::write(
        root.join(&notes),
        "# Notes\n\nProse with CamelCaseWords and snake_case_words.\n\
         Repeated repeated tokens exercise document-frequency counting.\n",
    )
    .expect("write notes");
    files.push(notes);
    files.sort();
    files.dedup();
    files
}

/// Assert two indexes are logically identical: same chunk order, same inverted
/// postings, same `doc_freqs`, same file set and corpus statistics. This is the
/// determinism contract between the parallel and sequential builds (#933/#498).
fn assert_same_index(a: &BM25Index, b: &BM25Index) {
    assert_eq!(a.doc_count, b.doc_count, "doc_count");
    assert_eq!(a.chunks, b.chunks, "chunk vector (order + content)");
    assert_eq!(a.inverted, b.inverted, "inverted index");
    assert_eq!(a.doc_freqs, b.doc_freqs, "doc_freqs");
    assert_eq!(a.files, b.files, "tracked files");
    assert_eq!(
        a.avg_doc_len.to_bits(),
        b.avg_doc_len.to_bits(),
        "avg_doc_len"
    );
}

#[test]
fn parallel_build_matches_sequential() {
    let td = tempdir().expect("tempdir");
    let root = td.path();
    let files = write_parallel_corpus(root, 40);

    let hint = HashMap::new();
    let seq = BM25Index::build_sequential(root, &hint, &files);
    let par = BM25Index::build_parallel(root, &hint, &files);

    assert!(par.doc_count > files.len(), "expected multiple chunks/file");
    assert_same_index(&seq, &par);
}

#[test]
fn parallel_build_is_deterministic_across_runs() {
    let td = tempdir().expect("tempdir");
    let root = td.path();
    let files = write_parallel_corpus(root, 48);
    let hint = HashMap::new();

    let a = BM25Index::build_parallel(root, &hint, &files);
    let b = BM25Index::build_parallel(root, &hint, &files);
    assert_same_index(&a, &b);
}

#[test]
fn build_from_directory_dispatches_parallel_and_matches_sequential() {
    // >= PARALLEL_MIN_FILES files so the public entry point takes the parallel
    // path; its result must equal an explicit sequential build over the same set.
    let td = tempdir().expect("tempdir");
    let root = td.path();
    write_parallel_corpus(root, 40);

    let files = list_code_files(root);
    assert!(
        files.len() >= super::build::PARALLEL_MIN_FILES,
        "corpus must trigger the parallel path"
    );

    let public = BM25Index::build_from_directory(root);
    let seq = BM25Index::build_sequential(root, &HashMap::new(), &files);
    assert_same_index(&seq, &public);
}

// ---- #581: parallel incremental rebuild determinism ----

/// Mutate a freshly-built corpus to exercise every incremental branch — a
/// changed file (re-extract), a new file, a removed file, and many unchanged
/// files (reuse) — then assert the parallel and sequential incremental rebuilds
/// produce a byte-identical index over the same `prev` + disk state.
#[test]
fn parallel_incremental_matches_sequential() {
    let td = tempdir().expect("tempdir");
    let root = td.path();
    write_parallel_corpus(root, 40);

    // Build the baseline index (the corpus is large enough to take the parallel
    // full-build path), then mutate disk so the rebuild hits all branches.
    let prev = BM25Index::build_from_directory(root);
    // `files` keys and `chunk.file_path` keep the OS-native separator (output is
    // normalized to '/' only at render time, #324); build expected keys the same
    // way so these lookups also hit on Windows (`src\mod_001.rs`).
    let key = |rel: &str| rel.replace('/', std::path::MAIN_SEPARATOR_STR);
    assert!(prev.files.contains_key(&key("src/mod_001.rs")));

    // Changed: different body (and size) forces a re-extract.
    std::fs::write(
        root.join("src/mod_000.rs"),
        "pub fn changed_entry() -> usize {\n    let recomputed = 42 + 7;\n    recomputed\n}\n",
    )
    .expect("rewrite mod_000");
    // New file the baseline never saw.
    std::fs::write(
        root.join("src/mod_new.rs"),
        "pub fn brand_new(token: Token) -> Token {\n    token\n}\n",
    )
    .expect("write mod_new");
    // Removed file must drop out of the rebuilt index.
    std::fs::remove_file(root.join("src/mod_001.rs")).expect("remove mod_001");

    let old_by_file = BM25Index::group_prev_chunks_by_file(&prev);
    let files = list_code_files(root);
    assert!(
        files.len() >= super::build::PARALLEL_MIN_FILES,
        "corpus must trigger the parallel rebuild path"
    );

    let seq = BM25Index::rebuild_incremental_sequential(root, &prev, &old_by_file, &files);
    let par = BM25Index::rebuild_incremental_parallel(root, &prev, &old_by_file, &files);

    // The whole contract: identical chunk order, postings, doc_freqs, file set.
    assert_same_index(&seq, &par);

    // And the mutation actually exercised every branch (guards against a vacuous
    // pass where nothing changed).
    assert!(par.files.contains_key(&key("src/mod_000.rs")));
    assert!(par.files.contains_key(&key("src/mod_new.rs")));
    assert!(
        !par.files.contains_key(&key("src/mod_001.rs")),
        "removed file must not survive the rebuild"
    );
    assert!(
        par.chunks
            .iter()
            .any(|c| c.file_path == key("src/mod_000.rs") && c.content.contains("recomputed")),
        "changed file must be re-extracted with new content"
    );
}

/// The public `rebuild_incremental` dispatcher must take the parallel path for a
/// large corpus yet match an explicit sequential rebuild over the same inputs —
/// guards the dispatch wiring the way the full-build test guards `build()`.
#[test]
fn rebuild_incremental_dispatches_parallel_and_matches_sequential() {
    let td = tempdir().expect("tempdir");
    let root = td.path();
    write_parallel_corpus(root, 40);

    let prev = BM25Index::build_from_directory(root);
    std::fs::write(
        root.join("src/mod_005.rs"),
        "pub fn touched() -> bool {\n    true\n}\n",
    )
    .expect("rewrite mod_005");

    let old_by_file = BM25Index::group_prev_chunks_by_file(&prev);
    let files = list_code_files(root);
    assert!(files.len() >= super::build::PARALLEL_MIN_FILES);

    let public = BM25Index::rebuild_incremental(root, &prev);
    let seq = BM25Index::rebuild_incremental_sequential(root, &prev, &old_by_file, &files);
    assert_same_index(&seq, &public);
}

/// CI build-time regression gate for the parallel incremental rebuild
/// (#581, locking in #933). Off by default — wall-clock timing is meaningless
/// under the concurrent local / nextest runner — so it no-ops unless the CI job
/// sets `LEAN_CTX_PERF_GATE=1` and runs it on a quiet, single-threaded runner.
/// Asserts the parallel rebuild is not slower than the sequential one
/// (best-of-3, generous noise slack) and stays under a catastrophic-blowup
/// ceiling, without depending on a specific core count.
#[test]
fn parallel_incremental_rebuild_perf_gate() {
    if std::env::var("LEAN_CTX_PERF_GATE").as_deref() != Ok("1") {
        return; // CI-only gate; no-op elsewhere to avoid timing flakes
    }

    let td = tempdir().expect("tempdir");
    let root = td.path();
    write_parallel_corpus(root, 240);

    let prev = BM25Index::build_from_directory(root);
    // Touch a handful so the rebuild does real re-extraction on top of the reuse
    // path; the bulk stays unchanged — the realistic edit-loop shape.
    for i in [0usize, 60, 120, 180] {
        std::fs::write(
            root.join(format!("src/mod_{i:03}.rs")),
            format!("pub fn touched_{i}() -> usize {{ {i} }}\n"),
        )
        .expect("touch file");
    }

    let old_by_file = BM25Index::group_prev_chunks_by_file(&prev);
    let files = list_code_files(root);

    let best = |f: &dyn Fn() -> std::time::Duration| (0..3).map(|_| f()).min().unwrap();
    let seq = best(&|| {
        let t = std::time::Instant::now();
        let idx = BM25Index::rebuild_incremental_sequential(root, &prev, &old_by_file, &files);
        std::hint::black_box(&idx);
        t.elapsed()
    });
    let par = best(&|| {
        let t = std::time::Instant::now();
        let idx = BM25Index::rebuild_incremental_parallel(root, &prev, &old_by_file, &files);
        std::hint::black_box(&idx);
        t.elapsed()
    });

    eprintln!(
        "[perf-gate] incremental rebuild: seq={seq:?} par={par:?} ratio={:.2}",
        par.as_secs_f64() / seq.as_secs_f64().max(f64::MIN_POSITIVE)
    );

    // CI-safe gate (plan: "großzügiges CI-sicheres Budget"). The parallel path
    // must not be *dramatically* slower than sequential: a 2x ceiling flags a path
    // that serialized and piled on overhead (e.g. a deadlock-y merge), while
    // tolerating scheduler noise and low-core CI runners where a small fixture's
    // parallel speedup shrinks. Byte-for-byte equivalence is asserted separately
    // (parallel_incremental_matches_sequential), so this guard only has to catch a
    // catastrophic perf regression — not police a tight ratio that would flake.
    assert!(
        par.as_secs_f64() <= seq.as_secs_f64() * 2.0,
        "parallel incremental rebuild regressed: par={par:?} > seq*2 ({:?})",
        seq.mul_f64(2.0)
    );
    // Absolute blowup ceiling, generous for a slow debug CI runner.
    assert!(
        par < std::time::Duration::from_secs(30),
        "parallel incremental rebuild absurdly slow: {par:?}"
    );
}

#[test]
fn parallel_build_search_parity() {
    // End-to-end: the parallel index must rank a query identically to the
    // sequential one (same scores, same order).
    let td = tempdir().expect("tempdir");
    let root = td.path();
    let files = write_parallel_corpus(root, 40);
    let hint = HashMap::new();

    let seq = BM25Index::build_sequential(root, &hint, &files);
    let par = BM25Index::build_parallel(root, &hint, &files);

    let q = "process item transform";
    let rs = seq.search(q, 10);
    let rp = par.search(q, 10);
    assert!(!rp.is_empty(), "query should return hits");
    assert_eq!(rs.len(), rp.len(), "result count");
    for (s, p) in rs.iter().zip(rp.iter()) {
        assert_eq!(s.file_path, p.file_path);
        assert_eq!(s.symbol_name, p.symbol_name);
        assert_eq!(s.start_line, p.start_line);
        assert_eq!(s.score.to_bits(), p.score.to_bits(), "score parity");
    }
}

#[test]
fn remove_chunks_with_prefix_evicts_only_matching() {
    use crate::core::content_chunk::ContentChunk;

    let mut index = BM25Index::from_chunks_for_test(vec![CodeChunk {
        file_path: "src/lib.rs".into(),
        symbol_name: "main".into(),
        kind: ChunkKind::Function,
        start_line: 1,
        end_line: 2,
        content: "fn main() {}".into(),
        tokens: Vec::new(),
        token_count: 0,
    }]);
    index.ingest_content_chunks(vec![
        ContentChunk::from_provider(
            "health",
            "complexity",
            "src/a.rs#big",
            "big",
            ChunkKind::Other,
            "complex function body".into(),
            vec![],
            None,
        ),
        ContentChunk::from_provider(
            "github",
            "issues",
            "42",
            "bug",
            ChunkKind::Issue,
            "issue text".into(),
            vec![],
            None,
        ),
    ]);
    assert_eq!(index.chunks.len(), 3);

    let removed = index.remove_chunks_with_prefix("health://");
    assert_eq!(removed, 1, "only the health chunk is evicted");
    assert_eq!(index.chunks.len(), 2);
    assert!(
        index
            .chunks
            .iter()
            .all(|c| !c.file_path.starts_with("health://")),
        "no health chunks remain"
    );
    // Code chunk and the unrelated provider chunk survive.
    assert!(index.chunks.iter().any(|c| c.file_path == "src/lib.rs"));
    assert!(
        index
            .chunks
            .iter()
            .any(|c| c.file_path.starts_with("github://"))
    );
    // A no-match prefix is a no-op.
    assert_eq!(index.remove_chunks_with_prefix("nope://"), 0);
}
