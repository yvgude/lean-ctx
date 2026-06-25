//! End-to-end integration tests for the provider consolidation pipeline.
//!
//! Verifies that provider data (issues, PRs, DB schemas, wiki pages) flows
//! correctly through ALL stores: BM25 index, Graph index, Knowledge facts,
//! and Session cache — matching the production `apply_artifacts_to_stores` path.

use lean_ctx::core::bm25_index::{BM25Index, ChunkKind, bm25_search};
use lean_ctx::core::cache::SessionCache;
use lean_ctx::core::consolidation;
use lean_ctx::core::content_chunk::ContentChunk;
use lean_ctx::core::cross_source_edges;
use lean_ctx::core::cross_source_hints;
use lean_ctx::core::graph_index::IndexEdge;
use lean_ctx::core::knowledge::ProjectKnowledge;
use lean_ctx::core::knowledge_provider_extract;
use lean_ctx::core::memory_policy::MemoryPolicy;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn github_issue(id: &str, title: &str, labels: &[&str], refs: Vec<&str>) -> ContentChunk {
    ContentChunk::from_provider(
        "github",
        "issues",
        id,
        title,
        ChunkKind::Issue,
        format!("Body of issue {id}: {title}"),
        refs.into_iter().map(String::from).collect(),
        Some(serde_json::json!({"state": "open", "labels": labels})),
    )
}

fn github_pr(id: &str, title: &str, refs: Vec<&str>) -> ContentChunk {
    ContentChunk::from_provider(
        "github",
        "pull_requests",
        id,
        title,
        ChunkKind::PullRequest,
        format!("PR {id}: {title}"),
        refs.into_iter().map(String::from).collect(),
        Some(serde_json::json!({"state": "open"})),
    )
}

fn jira_ticket(id: &str, title: &str) -> ContentChunk {
    ContentChunk::from_provider(
        "jira",
        "issues",
        id,
        title,
        ChunkKind::Ticket,
        format!("Jira ticket {id}: {title}"),
        vec![],
        Some(serde_json::json!({"state": "In Progress", "labels": ["feature"]})),
    )
}

fn db_schema(table: &str) -> ContentChunk {
    ContentChunk::from_provider(
        "postgres",
        "schemas",
        table,
        &format!("public.{table}"),
        ChunkKind::DbSchema,
        format!("CREATE TABLE {table} (id serial PRIMARY KEY, name varchar)"),
        vec![],
        None,
    )
}

fn wiki_page(id: &str, title: &str, refs: Vec<&str>) -> ContentChunk {
    ContentChunk::from_provider(
        "confluence",
        "wikis",
        id,
        title,
        ChunkKind::WikiPage,
        format!("Documentation: {title}"),
        refs.into_iter().map(String::from).collect(),
        None,
    )
}

fn mcp_resource(bridge_name: &str, uri: &str, content: &str) -> ContentChunk {
    ContentChunk::from_provider(
        &format!("mcp:{bridge_name}"),
        "resource_content",
        uri,
        uri.rsplit('/').next().unwrap_or(uri),
        ChunkKind::ExternalOther,
        content.to_string(),
        vec![],
        None,
    )
}

// ---------------------------------------------------------------------------
// Scenario 1: Full pipeline — consolidate → apply to ALL stores
// ---------------------------------------------------------------------------

#[test]
fn scenario_full_pipeline_all_stores() {
    let chunks = vec![
        github_issue("42", "Auth token expiry bug", &["bug"], vec!["src/auth.rs"]),
        github_pr(
            "100",
            "Fix auth token lifetime",
            vec!["src/auth.rs", "src/config.rs"],
        ),
        db_schema("users"),
        wiki_page("arch-guide", "Architecture Guide", vec!["src/core/mod.rs"]),
    ];

    let artifacts = consolidation::consolidate(&chunks);

    // All artifact types should be populated
    assert!(
        !artifacts.bm25_chunks.is_empty(),
        "BM25 chunks should exist"
    );
    assert!(!artifacts.edges.is_empty(), "Graph edges should exist");
    assert!(!artifacts.facts.is_empty(), "Knowledge facts should exist");
    assert!(
        !artifacts.cache_entries.is_empty(),
        "Cache entries should exist"
    );

    // Apply to BM25
    let mut index = BM25Index::default();
    let mut edges: Vec<IndexEdge> = Vec::new();
    let mut cache = SessionCache::new();

    let result = consolidation::apply_artifacts(
        &artifacts,
        Some(&mut index),
        Some(&mut edges),
        Some(&mut cache),
    );

    assert!(result.chunks_indexed > 0, "Should index chunks into BM25");
    assert!(result.edges_created > 0, "Should create graph edges");
    assert!(result.facts_extracted > 0, "Should extract knowledge facts");
    assert!(
        result.cache_entries_stored > 0,
        "Should store cache entries"
    );
}

// ---------------------------------------------------------------------------
// Scenario 2: BM25 search finds provider data
// ---------------------------------------------------------------------------

#[test]
fn scenario_bm25_search_finds_provider_data() {
    let chunks = vec![
        github_issue(
            "42",
            "Authentication token expiry",
            &["bug"],
            vec!["src/auth.rs"],
        ),
        github_pr("100", "Fix login session timeout", vec!["src/session.rs"]),
    ];

    let artifacts = consolidation::consolidate(&chunks);
    let mut index = BM25Index::default();
    let _ = consolidation::apply_artifacts(&artifacts, Some(&mut index), None, None);

    assert_eq!(
        index.external_chunk_count(),
        2,
        "Should have 2 external chunks"
    );

    let results = bm25_search(&index, "authentication token", 10);
    assert!(
        !results.is_empty(),
        "BM25 should find 'authentication token'"
    );
    assert!(
        results[0].file_path.contains("://"),
        "Top result should be an external URI: {}",
        results[0].file_path
    );

    let results2 = bm25_search(&index, "login session", 10);
    assert!(!results2.is_empty(), "BM25 should find 'login session'");
}

// ---------------------------------------------------------------------------
// Scenario 3: Cross-source edges link issues to code files
// ---------------------------------------------------------------------------

#[test]
fn scenario_cross_source_edges_link_issues_to_files() {
    let chunks = vec![
        github_issue(
            "42",
            "Bug in auth handler",
            &["bug"],
            vec!["src/auth/handler.rs"],
        ),
        github_pr(
            "100",
            "Fix auth handler",
            vec!["src/auth/handler.rs", "src/config.rs"],
        ),
    ];

    let artifacts = consolidation::consolidate(&chunks);

    // Edges should reference src/auth/handler.rs
    let auth_edges: Vec<_> = artifacts
        .edges
        .iter()
        .filter(|e| e.to == "src/auth/handler.rs" || e.from == "src/auth/handler.rs")
        .collect();
    assert!(
        !auth_edges.is_empty(),
        "Should have edges linking to src/auth/handler.rs"
    );

    // Merge into graph and generate hints
    let mut all_edges: Vec<IndexEdge> = Vec::new();
    cross_source_edges::merge_edges(&mut all_edges, artifacts.edges);

    let hints = cross_source_hints::hints_for_file("src/auth/handler.rs", &all_edges, "/project");
    assert!(
        !hints.is_empty(),
        "Should generate hints for src/auth/handler.rs"
    );
    assert!(
        hints
            .iter()
            .any(|h| h.source_uri.contains("42") || h.source_uri.contains("100")),
        "Hints should reference issue #42 or PR #100: {:?}",
        hints.iter().map(|h| &h.source_uri).collect::<Vec<_>>()
    );
}

// ---------------------------------------------------------------------------
// Scenario 4: Knowledge facts extracted from all provider types
// ---------------------------------------------------------------------------

#[test]
fn scenario_knowledge_facts_from_all_providers() {
    let chunks = vec![
        github_issue("42", "Auth crash on login", &["bug"], vec!["src/auth.rs"]),
        github_issue("10", "Dark mode support", &["enhancement"], vec![]),
        github_pr("100", "Fix auth crash", vec!["src/auth.rs"]),
        jira_ticket("PROJ-123", "Implement caching layer"),
        db_schema("sessions"),
        wiki_page(
            "deploy-guide",
            "Deployment Guide",
            vec!["deploy/docker.yml"],
        ),
    ];

    let facts = knowledge_provider_extract::extract_facts(&chunks);

    // Bug issue → known_bugs
    assert!(
        facts.iter().any(|f| f.category == "known_bugs"),
        "Should have known_bugs fact"
    );

    // Enhancement → known_features
    assert!(
        facts.iter().any(|f| f.category == "known_features"),
        "Should have known_features fact"
    );

    // PR → recent_changes
    assert!(
        facts.iter().any(|f| f.category == "recent_changes"),
        "Should have recent_changes fact"
    );

    // Jira → known_issues (no bug/enhancement label match)
    assert!(
        facts
            .iter()
            .any(|f| f.category == "known_features" || f.category == "known_issues"),
        "Jira ticket should create a knowledge fact"
    );

    // DB schema → data_model
    assert!(
        facts.iter().any(|f| f.category == "data_model"),
        "DB schema should create data_model fact"
    );

    // Wiki → documentation
    assert!(
        facts.iter().any(|f| f.category == "documentation"),
        "Wiki page should create documentation fact"
    );

    // File mentions from issue refs
    assert!(
        facts
            .iter()
            .any(|f| f.category == "file_mentions" && f.key == "src/auth.rs"),
        "Should have file_mentions for src/auth.rs"
    );
}

// ---------------------------------------------------------------------------
// Scenario 5: Knowledge remember + recall roundtrip
// ---------------------------------------------------------------------------

#[test]
fn scenario_knowledge_remember_recall_roundtrip() {
    let chunks = vec![
        github_issue(
            "42",
            "Token expiry bug",
            &["bug", "critical"],
            vec!["src/auth.rs"],
        ),
        db_schema("api_keys"),
    ];

    let artifacts = consolidation::consolidate(&chunks);
    let policy = MemoryPolicy::default();
    let mut knowledge = ProjectKnowledge::new("/tmp/provider-pipeline-test");
    let session_id = "test-session-1";

    for fact in &artifacts.facts {
        knowledge.remember(
            &fact.category,
            &fact.key,
            &fact.value,
            session_id,
            fact.confidence,
            &policy,
        );
    }

    // Recall should find the bug by category
    let recalled = knowledge.recall_by_category("known_bugs");
    assert!(!recalled.is_empty(), "Should recall known_bugs facts");
    assert!(
        recalled.iter().any(|f| f.value.contains("Token expiry")),
        "Should recall the token expiry bug"
    );

    // Recall data_model
    let data_facts = knowledge.recall_by_category("data_model");
    assert!(!data_facts.is_empty(), "Should recall data_model facts");
}

// ---------------------------------------------------------------------------
// Scenario 6: Session cache stores provider URIs
// ---------------------------------------------------------------------------

#[test]
fn scenario_cache_stores_provider_uris() {
    let chunks = vec![
        github_issue("42", "Auth bug", &["bug"], vec![]),
        github_pr("100", "Fix auth", vec![]),
        mcp_resource(
            "knowledge-base",
            "docs://api/v2",
            "API v2 documentation content",
        ),
    ];

    let artifacts = consolidation::consolidate(&chunks);
    let mut cache = SessionCache::new();

    consolidation::apply_artifacts(&artifacts, None, None, Some(&mut cache));

    // Each external chunk should be cached under its URI
    assert!(
        cache.get("github://issues/42").is_some(),
        "Issue should be cached at github://issues/42"
    );
    assert!(
        cache.get("github://pull_requests/100").is_some(),
        "PR should be cached at github://pull_requests/100"
    );
    // All 3 external chunks should be cached
    assert!(
        cache.total_cached_tokens() > 0,
        "Cache should contain entries from all 3 provider chunks"
    );
}

// ---------------------------------------------------------------------------
// Scenario 7: MCP Bridge unique IDs
// ---------------------------------------------------------------------------

#[test]
fn scenario_mcp_bridge_unique_ids() {
    use lean_ctx::core::providers::ContextProvider;
    use lean_ctx::core::providers::mcp_bridge::McpBridgeProvider;

    let kb = McpBridgeProvider::new("knowledge-base", "http://localhost:8080");
    let gh = McpBridgeProvider::new("github-issues", "http://localhost:9090");
    let empty = McpBridgeProvider::new("offline", "");

    assert_eq!(kb.id(), "mcp:knowledge-base");
    assert_eq!(gh.id(), "mcp:github-issues");
    assert_eq!(empty.id(), "mcp:offline");
    assert_ne!(kb.id(), gh.id());

    assert!(kb.is_available());
    assert!(gh.is_available());
    assert!(!empty.is_available());

    assert!(kb.supported_actions().contains(&"resources"));
    assert!(kb.supported_actions().contains(&"read_resource"));
    assert!(kb.supported_actions().contains(&"tools"));

    // Stdio bridges are available but return clear error on execute
    let stdio = McpBridgeProvider::new_stdio("local-kb", "npx", &["my-server".into()]);
    assert_eq!(stdio.id(), "mcp:local-kb");
    assert!(stdio.is_available());
    let result = stdio.execute(
        "resources",
        &lean_ctx::core::providers::ProviderParams::default(),
    );
    assert!(result.is_err());
    assert!(
        result.unwrap_err().contains("stdio transport"),
        "Should mention stdio transport limitation"
    );
}

// ---------------------------------------------------------------------------
// Scenario 8: BM25 search result formatting for external vs code chunks
// ---------------------------------------------------------------------------

#[test]
fn scenario_search_result_formatting_external_vs_code() {
    use lean_ctx::core::bm25_index::{SearchResult, format_search_results};

    let external_result = SearchResult {
        chunk_idx: 0,
        score: 0.85,
        file_path: "github://issues/42".to_string(),
        symbol_name: "Auth token expiry bug".to_string(),
        kind: ChunkKind::Issue,
        start_line: 0,
        end_line: 0,
        snippet: "Token expires too early".to_string(),
    };

    let code_result = SearchResult {
        chunk_idx: 1,
        score: 0.75,
        file_path: "src/auth.rs".to_string(),
        symbol_name: "validate_token".to_string(),
        kind: ChunkKind::Function,
        start_line: 42,
        end_line: 60,
        snippet: "fn validate_token() { ... }".to_string(),
    };

    // Compact mode
    let compact = format_search_results(&[external_result.clone(), code_result.clone()], true);
    assert!(
        compact.contains("[Issue]"),
        "Compact format should show [Issue] for external: {compact}"
    );
    assert!(
        compact.contains("github://issues/42"),
        "Compact format should show URI"
    );
    assert!(
        compact.contains("src/auth.rs:42-60"),
        "Compact format should show line range for code: {compact}"
    );

    // Full mode
    let full = format_search_results(&[external_result, code_result], false);
    assert!(
        full.contains("[Issue]"),
        "Full format should show [Issue] for external: {full}"
    );
    assert!(
        full.contains("src/auth.rs :: validate_token"),
        "Full format should show symbol for code: {full}"
    );
}

// ---------------------------------------------------------------------------
// Scenario 9: Mixed provider data — multi-source consolidation
// ---------------------------------------------------------------------------

#[test]
fn scenario_multi_source_consolidation() {
    let chunks = vec![
        github_issue("1", "Login fails", &["bug"], vec!["src/auth.rs"]),
        jira_ticket("PROJ-50", "Add OAuth2 support"),
        db_schema("oauth_tokens"),
        wiki_page(
            "oauth-guide",
            "OAuth2 Integration Guide",
            vec!["src/oauth.rs"],
        ),
        mcp_resource(
            "team-kb",
            "kb://runbooks/auth",
            "Auth troubleshooting runbook",
        ),
    ];

    let artifacts = consolidation::consolidate(&chunks);

    // All 5 sources should produce chunks
    assert_eq!(
        artifacts
            .bm25_chunks
            .iter()
            .filter(|c| c.is_external())
            .count(),
        5,
        "All 5 external chunks should be present"
    );

    // Apply to all stores
    let mut index = BM25Index::default();
    let mut edges: Vec<IndexEdge> = Vec::new();
    let mut cache = SessionCache::new();

    let result = consolidation::apply_artifacts(
        &artifacts,
        Some(&mut index),
        Some(&mut edges),
        Some(&mut cache),
    );

    assert_eq!(result.chunks_indexed, 5, "Should index all 5 chunks");
    assert!(result.cache_entries_stored >= 5, "Should cache all entries");

    // BM25 should find cross-source matches
    let auth_results = bm25_search(&index, "authentication login OAuth", 10);
    assert!(
        auth_results.len() >= 2,
        "Should find multiple auth-related results across sources: got {}",
        auth_results.len()
    );
}

// ---------------------------------------------------------------------------
// Scenario 10: Config auto_index default is true
// ---------------------------------------------------------------------------

#[test]
fn scenario_auto_index_default_is_true() {
    let cfg = lean_ctx::core::config::Config::default();
    assert!(
        cfg.providers.auto_index,
        "providers.auto_index should default to true"
    );
    assert!(
        cfg.providers.enabled,
        "providers.enabled should default to true"
    );
}
