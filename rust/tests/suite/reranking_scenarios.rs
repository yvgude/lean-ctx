//! End-to-end scenario tests for the Post-RRF Reranking Pipeline.
//!
//! Tests validate that the reranking pipeline produces correct behavior
//! across realistic code search scenarios.

use lean_ctx::core::bm25_index::ChunkKind;
use lean_ctx::core::hybrid_search::HybridResult;
use lean_ctx::core::search_reranking::{
    QueryType, classify_query, rerank_pipeline, resolve_weights,
};

fn make_result(file: &str, symbol: &str, kind: ChunkKind, score: f64) -> HybridResult {
    HybridResult {
        file_path: file.to_string(),
        symbol_name: symbol.to_string(),
        kind,
        start_line: 1,
        end_line: 20,
        snippet: format!("fn {symbol}() {{ /* ... */ }}"),
        rrf_score: score,
        bm25_score: Some(score),
        dense_score: None,
        bm25_rank: Some(1),
        dense_rank: None,
    }
}

// ==========================================================================
// Scenario 1: Symbol query should boost definitions to top
// ==========================================================================

#[test]
fn scenario_symbol_query_boosts_definition() {
    let mut results = vec![
        make_result(
            "src/handlers/auth.rs",
            "handle_login",
            ChunkKind::Function,
            0.9,
        ),
        make_result("src/main.rs", "main", ChunkKind::Function, 0.85),
        make_result("src/models/user.rs", "UserService", ChunkKind::Struct, 0.4),
        make_result(
            "src/services/auth.rs",
            "UserService",
            ChunkKind::Struct,
            0.3,
        ),
        make_result(
            "tests/test_user.rs",
            "test_user_service",
            ChunkKind::Function,
            0.7,
        ),
    ];

    rerank_pipeline(&mut results, "UserService", 5);

    // UserService struct definitions should be at top despite lower initial scores
    assert_eq!(results[0].symbol_name, "UserService");
    assert_eq!(results[0].kind, ChunkKind::Struct);
}

// ==========================================================================
// Scenario 2: NL query should not apply definition boost
// ==========================================================================

#[test]
fn scenario_nl_query_no_definition_boost() {
    let mut results = vec![
        make_result("src/auth.rs", "authenticate", ChunkKind::Function, 0.9),
        make_result("src/config.rs", "load_config", ChunkKind::Function, 0.8),
        make_result("src/auth.rs", "verify_token", ChunkKind::Function, 0.7),
    ];

    let scores_before: Vec<f64> = results.iter().map(|r| r.rrf_score).collect();

    rerank_pipeline(&mut results, "how to handle authentication", 3);

    // No symbol definition boost for NL queries; order based on coherence + diversity
    // The first result should still be from auth.rs (coherence boost)
    assert!(results[0].file_path.contains("auth"));
    // Scores should change (coherence/noise/diversity) but no massive boost
    assert!(results[0].rrf_score - scores_before[0] < scores_before[0] * 4.0);
}

// ==========================================================================
// Scenario 3: Test files get penalized
// ==========================================================================

#[test]
fn scenario_test_files_penalized() {
    let mut results = vec![
        make_result(
            "tests/integration/test_auth.rs",
            "test_login",
            ChunkKind::Function,
            1.0,
        ),
        make_result(
            "src/__tests__/auth.spec.ts",
            "auth_spec",
            ChunkKind::Function,
            1.0,
        ),
        make_result("src/auth.rs", "login", ChunkKind::Function, 0.7),
        make_result(
            "test/unit/auth_test.go",
            "TestAuth",
            ChunkKind::Function,
            1.0,
        ),
    ];

    rerank_pipeline(&mut results, "authentication logic", 4);

    // src/auth.rs should rank higher than test files despite lower initial score
    assert_eq!(results[0].file_path, "src/auth.rs");
}

// ==========================================================================
// Scenario 4: Diversity prevents single-file dominance
// ==========================================================================

#[test]
fn scenario_diversity_prevents_single_file_dominance() {
    let mut results = vec![
        make_result("src/giant_module.rs", "fn_a", ChunkKind::Function, 1.0),
        make_result("src/giant_module.rs", "fn_b", ChunkKind::Function, 0.95),
        make_result("src/giant_module.rs", "fn_c", ChunkKind::Function, 0.90),
        make_result("src/giant_module.rs", "fn_d", ChunkKind::Function, 0.85),
        make_result("src/giant_module.rs", "fn_e", ChunkKind::Function, 0.80),
        make_result("src/helper.rs", "helper_fn", ChunkKind::Function, 0.75),
        make_result("src/utils.rs", "util_fn", ChunkKind::Function, 0.70),
    ];

    rerank_pipeline(&mut results, "module functions", 5);

    // Within top 5, we should see helper.rs and utils.rs (not just giant_module.rs)
    let files_in_top5: Vec<&str> = results.iter().map(|r| r.file_path.as_str()).collect();
    assert!(
        files_in_top5.contains(&"src/helper.rs"),
        "helper.rs should be in top 5 due to diversity, got: {files_in_top5:?}",
    );
    assert!(
        files_in_top5.contains(&"src/utils.rs"),
        "utils.rs should be in top 5 due to diversity, got: {files_in_top5:?}",
    );
}

// ==========================================================================
// Scenario 5: File coherence boosts multi-hit files
// ==========================================================================

#[test]
fn scenario_file_coherence_boosts_multi_hit() {
    let mut results = vec![
        make_result("src/isolated.rs", "isolated_fn", ChunkKind::Function, 0.8),
        make_result("src/coherent.rs", "fn_one", ChunkKind::Function, 0.6),
        make_result("src/coherent.rs", "fn_two", ChunkKind::Function, 0.5),
        make_result("src/coherent.rs", "fn_three", ChunkKind::Function, 0.4),
    ];

    rerank_pipeline(&mut results, "coherent operations", 4);

    // coherent.rs should get a file coherence boost since multiple chunks match
    // The top result from coherent.rs should be boosted above isolated.rs
    let coherent_top = results
        .iter()
        .find(|r| r.file_path == "src/coherent.rs")
        .unwrap();
    assert!(
        coherent_top.rrf_score > 0.6,
        "coherent.rs top chunk should be boosted above its initial 0.6, got {}",
        coherent_top.rrf_score
    );
}

// ==========================================================================
// Scenario 6: Legacy/compat paths get penalized
// ==========================================================================

#[test]
fn scenario_legacy_compat_penalized() {
    let mut results = vec![
        make_result(
            "src/compat/old_api.rs",
            "handle_request",
            ChunkKind::Function,
            1.0,
        ),
        make_result(
            "src/legacy/v1_handler.rs",
            "process",
            ChunkKind::Function,
            0.95,
        ),
        make_result(
            "src/deprecated/auth.rs",
            "authenticate",
            ChunkKind::Function,
            0.90,
        ),
        make_result(
            "src/api/handler.rs",
            "handle_request",
            ChunkKind::Function,
            0.6,
        ),
    ];

    rerank_pipeline(&mut results, "handle request", 4);

    // Production code should rank above legacy/compat despite lower initial score
    assert_eq!(results[0].file_path, "src/api/handler.rs");
}

// ==========================================================================
// Scenario 7: .d.ts type stubs get mild penalty
// ==========================================================================

#[test]
fn scenario_type_stubs_mild_penalty() {
    let mut results = vec![
        make_result("src/types.d.ts", "AuthConfig", ChunkKind::Struct, 0.9),
        make_result("src/auth.ts", "AuthConfig", ChunkKind::Struct, 0.8),
    ];

    rerank_pipeline(&mut results, "AuthConfig", 2);

    // Both should still be present but auth.ts should rank above .d.ts
    assert_eq!(results[0].file_path, "src/auth.ts");
}

// ==========================================================================
// Scenario 8: Architecture query classification
// ==========================================================================

#[test]
fn scenario_architecture_query_weights() {
    assert_eq!(
        classify_query("how does authentication work"),
        QueryType::Architecture
    );
    assert_eq!(
        classify_query("where is the data flow"),
        QueryType::Architecture
    );

    let (bm25_w, dense_w) = resolve_weights(QueryType::Architecture);
    // Architecture queries should favor dense (semantic) over BM25 (lexical)
    assert!(dense_w > bm25_w);
}

// ==========================================================================
// Scenario 9: Mixed scenario - symbol in tests should still lose to prod definition
// ==========================================================================

#[test]
fn scenario_symbol_in_test_vs_prod() {
    let mut results = vec![
        make_result("tests/test_parser.rs", "Parser", ChunkKind::Struct, 0.9),
        make_result("src/parser/mod.rs", "Parser", ChunkKind::Struct, 0.5),
        make_result("src/parser/mod.rs", "parse", ChunkKind::Function, 0.4),
        make_result("examples/demo.rs", "use_parser", ChunkKind::Function, 0.8),
    ];

    rerank_pipeline(&mut results, "Parser", 4);

    // Production definition should win despite lower initial score
    // (definition boost + test penalty + example penalty on non-defining chunks)
    assert_eq!(results[0].file_path, "src/parser/mod.rs");
    assert_eq!(results[0].symbol_name, "Parser");
}

// ==========================================================================
// Scenario 10: Barrel/index files get moderate penalty
// ==========================================================================

#[test]
fn scenario_barrel_files_penalized() {
    let mut results = vec![
        make_result(
            "src/components/index.ts",
            "export_all",
            ChunkKind::Other,
            0.9,
        ),
        make_result("src/components/Button.ts", "Button", ChunkKind::Struct, 0.7),
        make_result(
            "src/utils/__init__.py",
            "module_init",
            ChunkKind::Other,
            0.85,
        ),
        make_result(
            "src/utils/helpers.py",
            "format_date",
            ChunkKind::Function,
            0.6,
        ),
    ];

    rerank_pipeline(&mut results, "button component", 4);

    // Actual implementation files should rank above barrel/index files
    let top_2_files: Vec<&str> = results
        .iter()
        .take(2)
        .map(|r| r.file_path.as_str())
        .collect();
    assert!(
        top_2_files.contains(&"src/components/Button.ts")
            || top_2_files.contains(&"src/utils/helpers.py"),
        "Implementation files should be in top 2, got: {top_2_files:?}",
    );
}

// ==========================================================================
// Scenario 11: Qualified symbol extraction
// ==========================================================================

#[test]
fn scenario_qualified_symbol_search() {
    let mut results = vec![
        make_result("src/auth/mod.rs", "authenticate", ChunkKind::Function, 0.8),
        make_result("src/auth/service.rs", "verify", ChunkKind::Function, 0.9),
        make_result("src/auth/service.rs", "AuthService", ChunkKind::Struct, 0.3),
    ];

    rerank_pipeline(&mut results, "auth::AuthService", 3);

    // AuthService definition should be boosted to top
    assert_eq!(results[0].symbol_name, "AuthService");
}

// ==========================================================================
// Scenario 12: Empty results don't panic
// ==========================================================================

#[test]
fn scenario_empty_results() {
    let mut results: Vec<HybridResult> = vec![];
    rerank_pipeline(&mut results, "anything", 10);
    assert!(results.is_empty());
}

// ==========================================================================
// Scenario 13: Single result passes through
// ==========================================================================

#[test]
fn scenario_single_result() {
    let mut results = vec![make_result("src/main.rs", "main", ChunkKind::Function, 1.0)];
    rerank_pipeline(&mut results, "main", 1);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].file_path, "src/main.rs");
}

// ==========================================================================
// Scenario 14: snake_case symbol recognition
// ==========================================================================

#[test]
fn scenario_snake_case_symbol() {
    assert_eq!(classify_query("get_user_by_id"), QueryType::Symbol);
    assert_eq!(classify_query("_private_field"), QueryType::Symbol);
    assert_eq!(classify_query("MAX_RETRY_COUNT"), QueryType::Symbol);
}

// ==========================================================================
// Scenario 15: Real-world multi-signal interaction
// ==========================================================================

#[test]
fn scenario_multi_signal_interaction() {
    // Simulates: searching for "BM25Index" in a codebase with:
    // - Definition in src/core/bm25_index.rs (low initial score)
    // - Usage in tests (high initial score)
    // - Usage in examples (medium score)
    // - Usage in production code (medium score)
    let mut results = vec![
        make_result(
            "tests/test_search.rs",
            "test_bm25",
            ChunkKind::Function,
            1.0,
        ),
        make_result(
            "examples/search_demo.rs",
            "demo_search",
            ChunkKind::Function,
            0.9,
        ),
        make_result(
            "src/tools/search.rs",
            "run_search",
            ChunkKind::Function,
            0.8,
        ),
        make_result(
            "src/tools/search.rs",
            "format_results",
            ChunkKind::Function,
            0.7,
        ),
        make_result(
            "src/core/bm25_index.rs",
            "BM25Index",
            ChunkKind::Struct,
            0.4,
        ),
        make_result(
            "src/core/bm25_index.rs",
            "search",
            ChunkKind::Function,
            0.35,
        ),
    ];

    rerank_pipeline(&mut results, "BM25Index", 4);

    // Expected: BM25Index definition rises to top (definition boost)
    // Test + Example files get noise penalty
    // Production usage stays in middle
    assert_eq!(
        results[0].file_path, "src/core/bm25_index.rs",
        "BM25Index definition should be #1"
    );
    assert_eq!(results[0].symbol_name, "BM25Index");

    // Production code (src/tools/search.rs) should be in top 2
    assert_eq!(
        results[1].file_path, "src/tools/search.rs",
        "Production usage should be #2"
    );

    // Test file should rank below example-free production code
    let test_pos = results
        .iter()
        .position(|r| r.file_path == "tests/test_search.rs");
    let example_pos = results
        .iter()
        .position(|r| r.file_path == "examples/search_demo.rs");
    // Both penalized paths should be below position 1
    assert!(
        test_pos.unwrap_or(99) > 1,
        "Test file should be below production code"
    );
    assert!(
        example_pos.unwrap_or(99) > 1,
        "Example file should be below production code"
    );
}
