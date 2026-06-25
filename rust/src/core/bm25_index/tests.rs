//! Tests for the BM25 index module (types and utilities only).
//! BM25 search tests removed — use FTS5's native bm25() instead.

use super::chunking::{detect_symbol, split_camel_case_tokens};
use super::*;
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
