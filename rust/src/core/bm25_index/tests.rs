//! Tests for the BM25 index. Extracted from `bm25_index/mod.rs`;
//! `super::*` resolves to the `bm25_index` module.

use super::chunking::{detect_symbol, split_camel_case_tokens};
use super::*;
use crate::core::index_types;
use tempfile::tempdir;

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

    let idx = BM25Index::from_chunks_for_test(vec![CodeChunk {
        file_path: "a.rs".into(),
        symbol_name: "a".into(),
        kind: ChunkKind::Function,
        start_line: 1,
        end_line: 1,
        content: "pub fn a() {}".into(),
        tokens: vec![],
        token_count: 0,
    }]);
    assert!(!bm25_index_looks_stale_fast(&idx, root));

    std::fs::remove_file(root.join("a.rs")).expect("remove a.rs");
    assert!(bm25_index_looks_stale_fast(&idx, root));
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

    let mut index = BM25Index::from_chunks_for_test(vec![CodeChunk {
        file_path: "big.rs".into(),
        symbol_name: "big".into(),
        kind: ChunkKind::Function,
        start_line: 1,
        end_line: 8,
        content: "pub fn big() {\n  let a = 1;\n  let b = 2;\n  let c = 3;\n  let d = 4;\n  let e = 5;\n  let f = 6;\n}\n".into(),
        tokens: vec![],
        token_count: 0,
    }]);
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

    let index = BM25Index::from_chunks_for_test(vec![CodeChunk {
        file_path: "a.rs".into(),
        symbol_name: "alpha".into(),
        kind: ChunkKind::Function,
        start_line: 1,
        end_line: 1,
        content: "pub fn alpha() {}".into(),
        tokens: vec![],
        token_count: 0,
    }]);
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

    crate::test_env::remove_var("LEAN_CTX_BM25_MAX_CACHE_MB");
    let index = BM25Index::from_chunks_for_test(vec![CodeChunk {
        file_path: "a.rs".into(),
        symbol_name: "a".into(),
        kind: ChunkKind::Function,
        start_line: 1,
        end_line: 1,
        content: "pub fn a() {}".into(),
        tokens: vec![],
        token_count: 0,
    }]);
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

    let chunks: Vec<CodeChunk> = (0..20)
        .map(|i| CodeChunk {
            file_path: format!("mod{}.rs", i / 2),
            symbol_name: format!("fn_{i}"),
            kind: ChunkKind::Function,
            start_line: 1,
            end_line: 1,
            content: format!("pub fn fn_{i}() {{}}"),
            tokens: vec![],
            token_count: 0,
        })
        .collect();
    let index = BM25Index::from_chunks_for_test(chunks);
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

    let index = BM25Index::from_chunks_for_test(vec![CodeChunk {
        file_path: "a.rs".into(),
        symbol_name: "a".into(),
        kind: ChunkKind::Function,
        start_line: 1,
        end_line: 1,
        content: "pub fn a() {}".into(),
        tokens: vec![],
        token_count: 0,
    }]);

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
    let index = BM25Index::from_chunks_for_test(vec![
        CodeChunk {
            file_path: "a.rs".into(),
            symbol_name: "a".into(),
            kind: ChunkKind::Function,
            start_line: 1,
            end_line: 1,
            content: "pub fn a() {}".into(),
            tokens: vec![],
            token_count: 0,
        },
        CodeChunk {
            file_path: "a.rs".into(),
            symbol_name: "b".into(),
            kind: ChunkKind::Function,
            start_line: 2,
            end_line: 2,
            content: "pub fn b() {}".into(),
            tokens: vec![],
            token_count: 0,
        },
    ]);
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
fn max_bm25_cache_bytes_reads_env() {
    let _env = crate::core::data_dir::test_env_lock();
    crate::test_env::set_var("LEAN_CTX_BM25_MAX_CACHE_MB", "64");
    let bytes = max_bm25_cache_bytes();
    assert_eq!(bytes, 64 * 1024 * 1024);
    crate::test_env::remove_var("LEAN_CTX_BM25_MAX_CACHE_MB");
}

// ---------------------------------------------------------------------------
// from_chunks (Phase 5)
// ---------------------------------------------------------------------------

#[test]
fn from_chunks_basic_chunk_count_matches() {
    let chunks = vec![
        index_types::CodeChunk {
            file_path: "a.rs".into(),
            content: "pub fn validate(token: &str) -> bool { check(token) }".into(),
            content_hash: String::new(),
            start_line: 1,
            end_line: 3,
            language: "rs".into(),
        },
        index_types::CodeChunk {
            file_path: "b.rs".into(),
            content: "pub fn connect(url: &str) -> Pool { create_pool(url) }".into(),
            content_hash: String::new(),
            start_line: 1,
            end_line: 5,
            language: "rs".into(),
        },
    ];

    let idx = BM25Index::from_chunks(&chunks);
    assert_eq!(idx.chunks.len(), 2);
    assert_eq!(idx.doc_count, 2);
}

#[test]
fn from_chunks_search_finds_relevant() {
    let chunks = vec![
        index_types::CodeChunk {
            file_path: "auth.rs".into(),
            content: "fn validate_token(token: &str) -> bool { check_jwt_expiry(token) }".into(),
            content_hash: String::new(),
            start_line: 1,
            end_line: 2,
            language: "rs".into(),
        },
        index_types::CodeChunk {
            file_path: "db.rs".into(),
            content: "fn connect_database(url: &str) -> Pool { create_pool(url) }".into(),
            content_hash: String::new(),
            start_line: 1,
            end_line: 2,
            language: "rs".into(),
        },
    ];

    let idx = BM25Index::from_chunks(&chunks);
    let results = idx.search("jwt token validation", 5);
    assert!(!results.is_empty());
    assert_eq!(results[0].file_path, "auth.rs");
}

#[test]
fn from_chunks_empty_chunks_produces_valid_empty_index() {
    let chunks: Vec<index_types::CodeChunk> = Vec::new();
    let idx = BM25Index::from_chunks(&chunks);
    assert!(idx.chunks.is_empty());
    assert_eq!(idx.doc_count, 0);
    assert!(idx.search("anything", 10).is_empty());
    assert!(idx.inverted.is_empty());
}

#[test]
fn from_chunks_parallel_tokenization_matches_sequential() {
    let pipeline_chunks = vec![
        index_types::CodeChunk {
            file_path: "m.rs".into(),
            content: "fn alpha() -> i32 { 42 }".into(),
            content_hash: String::new(),
            start_line: 1,
            end_line: 1,
            language: "rs".into(),
        },
        index_types::CodeChunk {
            file_path: "m.rs".into(),
            content: "fn beta(x: i32) -> i32 { x * 2 }".into(),
            content_hash: String::new(),
            start_line: 3,
            end_line: 3,
            language: "rs".into(),
        },
    ];
    let idx_par = BM25Index::from_chunks(&pipeline_chunks);

    let mut idx_seq = BM25Index::new();
    for c in &pipeline_chunks {
        let enriched = enrich_for_bm25(&CodeChunk {
            file_path: c.file_path.clone(),
            symbol_name: String::new(),
            kind: ChunkKind::Other,
            start_line: c.start_line as usize,
            end_line: c.end_line as usize,
            content: c.content.clone(),
            tokens: Vec::new(),
            token_count: 0,
        });
        let tokens = tokenize(&enriched);
        let code_chunk = CodeChunk {
            file_path: c.file_path.clone(),
            symbol_name: String::new(),
            kind: ChunkKind::Other,
            start_line: c.start_line as usize,
            end_line: c.end_line as usize,
            content: c.content.clone(),
            tokens: Vec::new(),
            token_count: tokens.len(),
        };
        idx_seq.add_tokenized_chunk(idx_seq.chunks.len(), code_chunk, &tokens);
    }
    idx_seq.finalize();

    assert_eq!(idx_par.chunks.len(), idx_seq.chunks.len());
    assert_eq!(idx_par.doc_count, idx_seq.doc_count);
    assert!(
        (idx_par.avg_doc_len - idx_seq.avg_doc_len).abs() < 1e-10,
        "avg_doc_len should match"
    );
}

#[test]
fn from_chunks_search_sorts_ties_deterministically() {
    let chunks = vec![
        index_types::CodeChunk {
            file_path: "b.rs".into(),
            content: "fn same() {}".into(),
            content_hash: String::new(),
            start_line: 1,
            end_line: 1,
            language: "rs".into(),
        },
        index_types::CodeChunk {
            file_path: "a.rs".into(),
            content: "fn same() {}".into(),
            content_hash: String::new(),
            start_line: 1,
            end_line: 1,
            language: "rs".into(),
        },
    ];

    let idx = BM25Index::from_chunks(&chunks);
    let results = idx.search("same", 10);
    assert!(results.len() >= 2);
    assert_eq!(results[0].file_path, "a.rs");
    assert_eq!(results[1].file_path, "b.rs");
}
