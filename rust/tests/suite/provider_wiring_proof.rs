//! Wiring proof tests — verify that all provider pipeline connections
//! are actually functional, not just defined.
//!
//! Each test proves a specific connection point in the architecture:
//! Provider → Consolidation → BM25/Graph/Knowledge/Cache → Tool output
//!
//! These tests catch "functional silos" — code that exists but isn't
//! actually connected to the runtime.

use lean_ctx::core::bm25_index::{BM25Index, ChunkKind};
use lean_ctx::core::cache::SessionCache;
use lean_ctx::core::consolidation;
use lean_ctx::core::content_chunk::ContentChunk;
use lean_ctx::core::cross_source_edges;
use lean_ctx::core::cross_source_hints;
use lean_ctx::core::graph_index::IndexEdge;
use lean_ctx::core::knowledge::ProjectKnowledge;
use lean_ctx::core::knowledge_provider_extract;
use lean_ctx::core::providers::ContextProvider;
use lean_ctx::core::providers::mcp_bridge::McpBridgeProvider;

// ===========================================================================
// PROOF 1: consolidate() produces ALL artifact types (not just some)
// ===========================================================================

#[test]
fn proof_consolidate_produces_all_four_artifact_types() {
    let chunks = vec![ContentChunk::from_provider(
        "github",
        "issues",
        "1",
        "Test bug",
        ChunkKind::Issue,
        "Bug in src/auth.rs".into(),
        vec!["src/auth.rs".into()],
        Some(serde_json::json!({"state": "open", "labels": ["bug"]})),
    )];

    let artifacts = consolidation::consolidate(&chunks);

    assert!(
        !artifacts.bm25_chunks.is_empty(),
        "WIRING BROKEN: consolidate() produces no BM25 chunks"
    );
    assert!(
        !artifacts.edges.is_empty(),
        "WIRING BROKEN: consolidate() produces no graph edges"
    );
    assert!(
        !artifacts.facts.is_empty(),
        "WIRING BROKEN: consolidate() produces no knowledge facts"
    );
    assert!(
        !artifacts.cache_entries.is_empty(),
        "WIRING BROKEN: consolidate() produces no cache entries"
    );
}

// ===========================================================================
// PROOF 2: apply_artifacts() actually writes to BM25 index
// ===========================================================================

#[test]
fn proof_apply_artifacts_writes_bm25() {
    let chunks = vec![ContentChunk::from_provider(
        "github",
        "issues",
        "42",
        "Authentication crash",
        ChunkKind::Issue,
        "Auth crashes on login".into(),
        vec![],
        Some(serde_json::json!({"state": "open", "labels": ["bug"]})),
    )];

    let artifacts = consolidation::consolidate(&chunks);
    let mut index = BM25Index::default();
    let before = index.external_chunk_count();

    consolidation::apply_artifacts(&artifacts, Some(&mut index), None, None);

    let after = index.external_chunk_count();
    assert!(
        after > before,
        "WIRING BROKEN: apply_artifacts did not write to BM25 (before={before}, after={after})"
    );

    let results = index.search("auth crashes login", 5);
    assert!(
        !results.is_empty(),
        "WIRING BROKEN: BM25 search finds nothing after apply_artifacts"
    );
}

// ===========================================================================
// PROOF 3: apply_artifacts() actually creates graph edges
// ===========================================================================

#[test]
fn proof_apply_artifacts_creates_graph_edges() {
    let chunks = vec![ContentChunk::from_provider(
        "github",
        "issues",
        "42",
        "Auth bug",
        ChunkKind::Issue,
        "Bug in src/auth.rs".into(),
        vec!["src/auth.rs".into()],
        Some(serde_json::json!({"state": "open", "labels": ["bug"]})),
    )];

    let artifacts = consolidation::consolidate(&chunks);
    let mut edges: Vec<IndexEdge> = Vec::new();

    consolidation::apply_artifacts(&artifacts, None, Some(&mut edges), None);

    assert!(
        !edges.is_empty(),
        "WIRING BROKEN: apply_artifacts did not create graph edges"
    );
    assert!(
        edges
            .iter()
            .any(|e| e.to == "src/auth.rs" || e.from == "src/auth.rs"),
        "WIRING BROKEN: edges don't reference the file from issue"
    );
}

// ===========================================================================
// PROOF 4: Graph edges produce cross-source hints for ctx_read
// ===========================================================================

#[test]
fn proof_graph_edges_produce_hints_for_ctx_read() {
    let chunks = vec![
        ContentChunk::from_provider(
            "github",
            "issues",
            "42",
            "Auth bug",
            ChunkKind::Issue,
            "Bug in src/auth.rs".into(),
            vec!["src/auth.rs".into()],
            Some(serde_json::json!({"state": "open", "labels": ["bug"]})),
        ),
        ContentChunk::from_provider(
            "github",
            "pull_requests",
            "100",
            "Fix auth",
            ChunkKind::PullRequest,
            "Fixes auth in src/auth.rs".into(),
            vec!["src/auth.rs".into()],
            Some(serde_json::json!({"state": "open"})),
        ),
    ];

    let artifacts = consolidation::consolidate(&chunks);
    let mut all_edges: Vec<IndexEdge> = Vec::new();
    cross_source_edges::merge_edges(&mut all_edges, artifacts.edges);

    let hints = cross_source_hints::hints_for_file("src/auth.rs", &all_edges, "/project");

    assert!(
        !hints.is_empty(),
        "WIRING BROKEN: hints_for_file returns nothing for a file with linked issues"
    );
    assert!(
        hints.iter().any(|h| h.source_uri.contains("42")),
        "WIRING BROKEN: hints don't contain issue #42"
    );
}

// ===========================================================================
// PROOF 5: Knowledge facts are extractable AND rememberable
// ===========================================================================

#[test]
fn proof_knowledge_facts_roundtrip() {
    let chunks = vec![ContentChunk::from_provider(
        "github",
        "issues",
        "42",
        "Critical auth bug",
        ChunkKind::Issue,
        "Authentication fails on token refresh".into(),
        vec!["src/auth.rs".into()],
        Some(serde_json::json!({"state": "open", "labels": ["bug", "critical"]})),
    )];

    let facts = knowledge_provider_extract::extract_facts(&chunks);
    assert!(
        !facts.is_empty(),
        "WIRING BROKEN: extract_facts produces no facts from provider chunks"
    );

    let mut knowledge = ProjectKnowledge::new("/tmp/wiring-proof-test");
    let policy = lean_ctx::core::memory_policy::MemoryPolicy::default();

    for fact in &facts {
        knowledge.remember(
            &fact.category,
            &fact.key,
            &fact.value,
            "test-session",
            fact.confidence,
            &policy,
        );
    }

    let recalled = knowledge.recall_by_category("known_bugs");
    assert!(
        !recalled.is_empty(),
        "WIRING BROKEN: knowledge.recall_by_category returns nothing after remember()"
    );
    assert!(
        recalled.iter().any(|f| f.value.contains("auth")),
        "WIRING BROKEN: recalled facts don't match what was remembered"
    );
}

// ===========================================================================
// PROOF 6: Session cache stores provider data with correct URIs
// ===========================================================================

#[test]
fn proof_cache_stores_provider_uris() {
    let chunks = vec![ContentChunk::from_provider(
        "github",
        "issues",
        "42",
        "Auth bug",
        ChunkKind::Issue,
        "Bug description".into(),
        vec![],
        Some(serde_json::json!({"state": "open"})),
    )];

    let artifacts = consolidation::consolidate(&chunks);
    let mut cache = SessionCache::new();

    consolidation::apply_artifacts(&artifacts, None, None, Some(&mut cache));

    let cached = cache.get("github://issues/42");
    assert!(
        cached.is_some(),
        "WIRING BROKEN: cache.get() returns None for provider URI after apply_artifacts"
    );
}

// ===========================================================================
// PROOF 7: BM25 search result formatting includes external attribution
// ===========================================================================

#[test]
fn proof_bm25_search_attributes_external_results() {
    use lean_ctx::core::bm25_index::{SearchResult, format_search_results};

    let results = vec![SearchResult {
        chunk_idx: 0,
        score: 0.9,
        file_path: "github://issues/42".to_string(),
        symbol_name: "Auth token expiry".to_string(),
        kind: ChunkKind::Issue,
        start_line: 0,
        end_line: 0,
        snippet: "Token expires too fast".to_string(),
    }];

    let formatted = format_search_results(&results, true);
    assert!(
        formatted.contains("[Issue]"),
        "WIRING BROKEN: format_search_results doesn't show [Issue] for external results: {formatted}"
    );
    assert!(
        formatted.contains("github://"),
        "WIRING BROKEN: format_search_results doesn't show provider URI: {formatted}"
    );
}

// ===========================================================================
// PROOF 8: MCP Bridge registration — HTTP and stdio both register
// ===========================================================================

#[test]
fn proof_mcp_bridge_http_registers_correctly() {
    let bridge = McpBridgeProvider::new("my-kb", "http://localhost:8080");
    assert_eq!(bridge.id(), "mcp:my-kb");
    assert!(bridge.is_available());
    assert!(bridge.supported_actions().contains(&"resources"));
    assert!(bridge.supported_actions().contains(&"read_resource"));
}

#[test]
fn proof_mcp_bridge_stdio_registers_correctly() {
    let bridge = McpBridgeProvider::new_stdio("local-server", "npx", &["my-mcp".into()]);
    assert_eq!(bridge.id(), "mcp:local-server");
    assert!(bridge.is_available());

    let result = bridge.execute(
        "resources",
        &lean_ctx::core::providers::ProviderParams::default(),
    );
    assert!(
        result.is_err(),
        "Stdio bridge should error with clear message"
    );
    let err = result.unwrap_err();
    assert!(
        err.contains("stdio transport"),
        "WIRING BROKEN: stdio error message unclear: {err}"
    );
}

// ===========================================================================
// PROOF 9: Provider init registers from config
// ===========================================================================

#[test]
fn proof_init_registers_builtin_providers() {
    lean_ctx::core::providers::init::init_builtin_providers();
    let reg = lean_ctx::core::providers::registry::global_registry();

    assert!(
        reg.get("github").is_some(),
        "WIRING BROKEN: GitHub provider not registered"
    );
    assert!(
        reg.get("gitlab").is_some(),
        "WIRING BROKEN: GitLab provider not registered"
    );
    assert!(
        reg.get("jira").is_some(),
        "WIRING BROKEN: Jira provider not registered"
    );
    assert!(
        reg.get("postgres").is_some(),
        "WIRING BROKEN: Postgres provider not registered"
    );
}

// ===========================================================================
// PROOF 10: Config auto_index controls indexing behavior
// ===========================================================================

#[test]
fn proof_auto_index_config_default() {
    let cfg = lean_ctx::core::config::Config::default();
    assert!(
        cfg.providers.auto_index,
        "WIRING BROKEN: auto_index should default to true"
    );
    assert!(
        cfg.providers.enabled,
        "WIRING BROKEN: providers.enabled should default to true"
    );
}

// ===========================================================================
// PROOF 11: End-to-end — provider data searchable after full pipeline
// ===========================================================================

#[test]
fn proof_end_to_end_provider_data_searchable() {
    let chunks = vec![
        ContentChunk::from_provider(
            "github",
            "issues",
            "99",
            "Memory leak in websocket handler",
            ChunkKind::Issue,
            "WebSocket connections leak memory when client disconnects abruptly".into(),
            vec!["src/websocket.rs".into()],
            Some(serde_json::json!({"state": "open", "labels": ["bug", "performance"]})),
        ),
        ContentChunk::from_provider(
            "jira",
            "issues",
            "PERF-42",
            "Optimize database connection pooling",
            ChunkKind::Ticket,
            "Connection pool exhaustion under load".into(),
            vec![],
            Some(serde_json::json!({"state": "In Progress", "labels": ["performance"]})),
        ),
    ];

    let artifacts = consolidation::consolidate(&chunks);

    let mut index = BM25Index::default();
    let mut edges: Vec<IndexEdge> = Vec::new();
    let mut cache = SessionCache::new();

    let result = consolidation::apply_artifacts(
        &artifacts,
        Some(&mut index),
        Some(&mut edges),
        Some(&mut cache),
    );

    // Search should find the websocket issue
    let ws_results = index.search("websocket memory leak", 5);
    assert!(
        !ws_results.is_empty(),
        "WIRING BROKEN: BM25 can't find 'websocket memory leak' after full pipeline"
    );

    // Search should find the Jira ticket
    let db_results = index.search("database connection pool", 5);
    assert!(
        !db_results.is_empty(),
        "WIRING BROKEN: BM25 can't find 'database connection pool' after full pipeline"
    );

    // Cross-source hints should work for the websocket file
    let hints = cross_source_hints::hints_for_file("src/websocket.rs", &edges, "/project");
    assert!(
        !hints.is_empty(),
        "WIRING BROKEN: No cross-source hints for src/websocket.rs after full pipeline"
    );

    // Knowledge should have extracted facts
    let mut knowledge = ProjectKnowledge::new("/tmp/wiring-e2e-test");
    let policy = lean_ctx::core::memory_policy::MemoryPolicy::default();
    for fact in &artifacts.facts {
        knowledge.remember(
            &fact.category,
            &fact.key,
            &fact.value,
            "test",
            fact.confidence,
            &policy,
        );
    }
    let bugs = knowledge.recall_by_category("known_bugs");
    assert!(
        !bugs.is_empty(),
        "WIRING BROKEN: No known_bugs facts after full pipeline"
    );

    // Cache should have entries
    assert!(
        result.cache_entries_stored > 0,
        "WIRING BROKEN: No cache entries after full pipeline"
    );

    // Stats should be non-zero across ALL stores
    assert!(
        result.chunks_indexed > 0,
        "WIRING BROKEN: chunks_indexed is 0"
    );
    assert!(
        result.edges_created > 0,
        "WIRING BROKEN: edges_created is 0"
    );
    assert!(
        result.facts_extracted > 0,
        "WIRING BROKEN: facts_extracted is 0"
    );
}

// ===========================================================================
// PROOF 12: Multiple provider types produce distinct knowledge categories
// ===========================================================================

#[test]
fn proof_distinct_knowledge_categories_per_provider_type() {
    let chunks = vec![
        ContentChunk::from_provider(
            "github",
            "issues",
            "1",
            "Bug: crash on startup",
            ChunkKind::Issue,
            "App crashes".into(),
            vec![],
            Some(serde_json::json!({"state": "open", "labels": ["bug"]})),
        ),
        ContentChunk::from_provider(
            "github",
            "issues",
            "2",
            "Feature: dark mode",
            ChunkKind::Issue,
            "Add dark mode".into(),
            vec![],
            Some(serde_json::json!({"state": "open", "labels": ["enhancement"]})),
        ),
        ContentChunk::from_provider(
            "github",
            "pull_requests",
            "10",
            "Fix crash",
            ChunkKind::PullRequest,
            "Fixes the crash".into(),
            vec![],
            Some(serde_json::json!({"state": "merged"})),
        ),
        ContentChunk::from_provider(
            "postgres",
            "schemas",
            "users",
            "public.users",
            ChunkKind::DbSchema,
            "CREATE TABLE users (id serial)".into(),
            vec![],
            None,
        ),
        ContentChunk::from_provider(
            "confluence",
            "wikis",
            "deploy",
            "Deploy Guide",
            ChunkKind::WikiPage,
            "How to deploy".into(),
            vec![],
            None,
        ),
    ];

    let facts = knowledge_provider_extract::extract_facts(&chunks);
    let categories: std::collections::HashSet<&str> =
        facts.iter().map(|f| f.category.as_str()).collect();

    assert!(
        categories.contains("known_bugs"),
        "Missing known_bugs category"
    );
    assert!(
        categories.contains("known_features"),
        "Missing known_features category"
    );
    assert!(
        categories.contains("recent_changes"),
        "Missing recent_changes category"
    );
    assert!(
        categories.contains("data_model"),
        "Missing data_model category"
    );
    assert!(
        categories.contains("documentation"),
        "Missing documentation category"
    );
}
