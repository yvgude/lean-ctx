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
    std::env::set_var("LEAN_CTX_DATA_DIR", data_dir.path());
    std::env::set_var("LEAN_CTX_BM25_MAX_CACHE_MB", "512");
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

    std::env::remove_var("LEAN_CTX_BM25_MAX_CACHE_MB");
    std::env::remove_var("LEAN_CTX_DATA_DIR");
}

#[test]
fn load_quarantines_oversized_index() {
    let _env = crate::core::data_dir::test_env_lock();
    let td = tempdir().expect("tempdir");
    let root = td.path();
    let dir = crate::core::index_namespace::vectors_dir(root);
    std::fs::create_dir_all(&dir).expect("create vectors dir");

    let index_path = dir.join("bm25_index.json");
    std::env::set_var("LEAN_CTX_BM25_MAX_CACHE_MB", "0");
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

    std::env::remove_var("LEAN_CTX_BM25_MAX_CACHE_MB");
}

#[test]
fn save_refuses_oversized_output() {
    let _env = crate::core::data_dir::test_env_lock();
    let data_dir = tempdir().expect("data_dir");
    std::env::set_var("LEAN_CTX_DATA_DIR", data_dir.path());
    std::env::set_var("LEAN_CTX_BM25_MAX_CACHE_MB", "0");

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

    std::env::remove_var("LEAN_CTX_BM25_MAX_CACHE_MB");
}

#[test]
fn save_reports_persisted_outcome() {
    let _env = crate::core::data_dir::test_env_lock();
    let data_dir = tempdir().expect("data_dir");
    std::env::set_var("LEAN_CTX_DATA_DIR", data_dir.path());
    std::env::set_var("LEAN_CTX_BM25_MAX_CACHE_MB", "512");
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

    std::env::remove_var("LEAN_CTX_BM25_MAX_CACHE_MB");
    std::env::remove_var("LEAN_CTX_DATA_DIR");
}

#[test]
fn persist_ceiling_honors_env_override() {
    // The public ceiling accessor (shared with doctor) must honor an explicit
    // override exactly, so operators can size it to their monorepo.
    let _env = crate::core::data_dir::test_env_lock();
    std::env::set_var("LEAN_CTX_BM25_MAX_CACHE_MB", "777");
    assert_eq!(persist_ceiling_bytes(), 777 * 1024 * 1024);
    std::env::remove_var("LEAN_CTX_BM25_MAX_CACHE_MB");
}

#[test]
fn save_writes_project_root_marker() {
    let _env = crate::core::data_dir::test_env_lock();
    let td = tempdir().expect("tempdir");
    let root = td.path();
    std::fs::write(root.join("a.rs"), "pub fn a() {}\n").expect("write");

    std::env::remove_var("LEAN_CTX_BM25_MAX_CACHE_MB");
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
    std::env::set_var("LEAN_CTX_DATA_DIR", data_dir.path());
    std::env::set_var("LEAN_CTX_BM25_MAX_CACHE_MB", "512");
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

    std::env::remove_var("LEAN_CTX_BM25_MAX_CACHE_MB");
    std::env::remove_var("LEAN_CTX_DATA_DIR");
}

#[test]
fn auto_migrate_bin_to_zst() {
    let _env = crate::core::data_dir::test_env_lock();
    let data_dir = tempdir().expect("data_dir");
    std::env::set_var("LEAN_CTX_DATA_DIR", data_dir.path());
    std::env::set_var("LEAN_CTX_BM25_MAX_CACHE_MB", "512");
    let td = tempdir().expect("tempdir");
    let root = td.path();

    std::fs::write(root.join("a.rs"), "pub fn a() {}\n").expect("write");
    let index = BM25Index::build_from_directory(root);

    let dir = crate::core::index_namespace::vectors_dir(root);
    std::fs::create_dir_all(&dir).expect("mkdir");
    let data = bincode::serde::encode_to_vec(&index, bincode::config::standard()).expect("encode");
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

    std::env::remove_var("LEAN_CTX_BM25_MAX_CACHE_MB");
    std::env::remove_var("LEAN_CTX_DATA_DIR");
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
    std::env::set_var("LEAN_CTX_BM25_MAX_CACHE_MB", "64");
    let bytes = max_bm25_cache_bytes();
    assert_eq!(bytes, 64 * 1024 * 1024);
    std::env::remove_var("LEAN_CTX_BM25_MAX_CACHE_MB");
}
