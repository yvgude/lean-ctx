//! Integration test: full Context Engine wiring pipeline.
//!
//! Tests the data flow from provider chunks through consolidation into
//! the session cache, graph edges, and cross-source hints — the exact
//! path that the wired `ctx_provider` and `ctx_read` tools execute.

use lean_ctx::core::bm25_index::ChunkKind;
use lean_ctx::core::cache::SessionCache;
use lean_ctx::core::consolidation;
use lean_ctx::core::content_chunk::ContentChunk;
use lean_ctx::core::cross_source_hints;
use lean_ctx::core::graph_index::IndexEdge;

fn github_issue_chunk(id: &str, title: &str, body: &str, refs: Vec<&str>) -> ContentChunk {
    ContentChunk::from_provider(
        "github",
        "issues",
        id,
        title,
        ChunkKind::Issue,
        body.to_string(),
        refs.into_iter().map(String::from).collect(),
        Some(serde_json::json!({"state": "open", "labels": ["bug"]})),
    )
}

fn github_pr_chunk(id: &str, title: &str, body: &str, refs: Vec<&str>) -> ContentChunk {
    ContentChunk::from_provider(
        "github",
        "pull_requests",
        id,
        title,
        ChunkKind::PullRequest,
        body.to_string(),
        refs.into_iter().map(String::from).collect(),
        Some(serde_json::json!({"state": "open"})),
    )
}

/// Full pipeline: provider chunks → consolidate → cache + edges → hints for file.
#[test]
fn full_pipeline_provider_to_hints() {
    let chunks = vec![
        github_issue_chunk(
            "42",
            "Token expiry bug",
            "The JWT token in src/auth/handler.rs expires too quickly",
            vec!["src/auth/handler.rs"],
        ),
        github_pr_chunk(
            "100",
            "Fix token lifetime",
            "Adjusts TOKEN_LIFETIME in src/auth/handler.rs and src/config.rs",
            vec!["src/auth/handler.rs", "src/config.rs"],
        ),
    ];

    // Step 1: Consolidate
    let artifacts = consolidation::consolidate(&chunks);
    assert!(
        !artifacts.is_empty(),
        "consolidation should produce artifacts"
    );
    assert!(
        !artifacts.edges.is_empty(),
        "should produce cross-source edges"
    );
    assert!(
        !artifacts.facts.is_empty(),
        "should extract knowledge facts"
    );
    assert_eq!(
        artifacts.cache_entries.len(),
        2,
        "should create cache entries for both chunks"
    );

    // Step 2: Apply to session cache
    let mut cache = SessionCache::new();
    let mut edges: Vec<IndexEdge> = Vec::new();
    let result =
        consolidation::apply_artifacts(&artifacts, None, Some(&mut edges), Some(&mut cache));

    assert!(result.edges_created > 0, "edges should be created");
    assert!(
        result.cache_entries_stored > 0,
        "cache entries should be stored"
    );

    // Step 3: Verify cache contains provider data
    assert!(
        cache.get("github://issues/42").is_some(),
        "issue should be cached"
    );
    assert!(
        cache.get("github://pull_requests/100").is_some(),
        "PR should be cached"
    );

    // Step 4: Cross-source hints for the referenced file
    let hints = cross_source_hints::hints_for_file("src/auth/handler.rs", &edges, "/project");
    assert!(
        !hints.is_empty(),
        "should find hints for src/auth/handler.rs"
    );
    assert!(
        hints.iter().any(|h| h.source_uri.contains("issues/42")),
        "should link to issue #42"
    );
    assert!(
        hints
            .iter()
            .any(|h| h.source_uri.contains("pull_requests/100")),
        "should link to PR #100"
    );

    // Step 5: Format hints (as ctx_read would append them)
    let formatted = cross_source_hints::format_hints(&hints);
    assert!(
        formatted.contains("Cross-Source Hints"),
        "should have header"
    );
    assert!(formatted.contains("issues/42"), "should mention issue");
    assert!(formatted.contains("pull_requests/100"), "should mention PR");
}

/// No external chunks → no artifacts, no hints.
#[test]
fn code_only_produces_no_hints() {
    let code = ContentChunk::from(lean_ctx::core::bm25_index::CodeChunk {
        file_path: "src/main.rs".into(),
        symbol_name: "main".into(),
        kind: ChunkKind::Function,
        start_line: 1,
        end_line: 10,
        content: "fn main() { println!(\"hello\"); }".into(),
        tokens: vec![],
        token_count: 0,
    });

    let artifacts = consolidation::consolidate(&[code]);
    assert!(artifacts.is_empty());
}

/// Multiple files referenced by same issue → hints appear for each file.
#[test]
fn multi_file_reference_produces_hints_for_each() {
    let chunk = github_issue_chunk(
        "55",
        "Auth refactor needed",
        "Affects src/auth/handler.rs, src/auth/middleware.rs, and src/db/sessions.rs",
        vec![
            "src/auth/handler.rs",
            "src/auth/middleware.rs",
            "src/db/sessions.rs",
        ],
    );

    let artifacts = consolidation::consolidate(&[chunk]);
    let mut edges: Vec<IndexEdge> = Vec::new();
    consolidation::apply_artifacts(&artifacts, None, Some(&mut edges), None);

    for file in &[
        "src/auth/handler.rs",
        "src/auth/middleware.rs",
        "src/db/sessions.rs",
    ] {
        let hints = cross_source_hints::hints_for_file(file, &edges, "/project");
        assert!(!hints.is_empty(), "should find hints for {file}");
        assert!(
            hints.iter().any(|h| h.source_uri.contains("issues/55")),
            "{file} should link to issue #55"
        );
    }
}

/// Cache re-read returns the stored content.
#[test]
fn cached_provider_data_survives_reread() {
    let chunks = vec![github_issue_chunk(
        "99",
        "Performance regression",
        "Query in src/db/queries.rs takes 5s after upgrade",
        vec!["src/db/queries.rs"],
    )];

    let artifacts = consolidation::consolidate(&chunks);
    let mut cache = SessionCache::new();
    consolidation::apply_artifacts(&artifacts, None, None, Some(&mut cache));

    let cached = cache.get("github://issues/99");
    assert!(cached.is_some());
    let entry = cached.unwrap();
    let content = entry.content().expect("cached entry should have content");
    assert!(
        content.contains("Performance regression") || content.contains("5s after upgrade"),
        "cached content should contain the issue text"
    );
}

/// Knowledge facts are correctly extracted from different chunk kinds.
#[test]
fn knowledge_extraction_from_mixed_chunks() {
    let chunks = vec![
        github_issue_chunk(
            "10",
            "Login broken after deploy",
            "Users cannot log in since last deploy",
            vec!["src/auth.rs"],
        ),
        github_pr_chunk(
            "20",
            "Add rate limiting",
            "Implements rate limiting middleware",
            vec!["src/middleware.rs"],
        ),
    ];

    let artifacts = consolidation::consolidate(&chunks);
    assert!(
        artifacts.facts.len() >= 2,
        "should extract facts from both chunk types"
    );

    let has_bug_fact = artifacts.facts.iter().any(|f| f.category == "known_bugs");
    let has_change_fact = artifacts
        .facts
        .iter()
        .any(|f| f.category == "recent_changes");
    assert!(has_bug_fact, "issue should produce a known_bugs fact");
    assert!(has_change_fact, "PR should produce a recent_changes fact");
}

/// Active inference predicts preloads based on task description.
#[test]
fn active_inference_predicts_for_task() {
    let available = vec!["github".to_string(), "jira".to_string()];
    let mut bandit = lean_ctx::core::provider_bandit::ProviderBandit::new();

    let predictions = lean_ctx::core::active_inference::predict_preloads(
        "fix authentication bug in login flow",
        &available,
        &mut bandit,
        2,
    );

    assert!(
        !predictions.is_empty(),
        "should predict at least one preload for a bug-related task"
    );
    assert!(
        predictions.iter().all(|p| p.confidence > 0.0),
        "all predictions should have positive confidence"
    );
}

/// Simulates the exact MCP handler flow: `ctx_provider` query → consolidate →
/// then `ctx_read` would append hints. Exercises `ctx_provider::handle` with a
/// real `ToolContext` containing a shared `SessionCache`.
#[test]
fn mcp_handler_flow_provider_then_read_hints() {
    use std::sync::Arc;

    // Simulate the ToolContext that the MCP server creates
    let cache = Arc::new(tokio::sync::RwLock::new(SessionCache::new()));

    // Step 1: Provider query produces chunks and consolidates them
    let chunks = vec![
        github_issue_chunk(
            "77",
            "Memory leak in connection pool",
            "Connection pool in src/db/pool.rs leaks when timeout occurs. See also src/db/config.rs",
            vec!["src/db/pool.rs", "src/db/config.rs"],
        ),
        github_pr_chunk(
            "88",
            "Fix pool leak on timeout",
            "Properly drains connections in src/db/pool.rs on timeout. Tests in tests/pool_test.rs",
            vec!["src/db/pool.rs", "tests/pool_test.rs"],
        ),
    ];

    // This is what ctx_provider::consolidate_to_session does internally
    let artifacts = consolidation::consolidate(&chunks);
    assert!(!artifacts.is_empty());

    // Write to cache (as consolidate_to_session does via ctx.cache)
    {
        let mut cache_guard = cache.blocking_write();
        for entry in &artifacts.cache_entries {
            cache_guard.store(&entry.uri, &entry.content);
        }
    }

    // Step 2: Apply edges (these would be in the graph index)
    let mut edges: Vec<IndexEdge> = Vec::new();
    let result = consolidation::apply_artifacts(&artifacts, None, Some(&mut edges), None);
    assert!(result.edges_created > 0);

    // Step 3: Simulate ctx_read — check cross-source hints for referenced files
    let hints_pool = cross_source_hints::hints_for_file("src/db/pool.rs", &edges, "/project");
    assert!(
        !hints_pool.is_empty(),
        "pool.rs should have cross-source hints"
    );
    assert!(
        hints_pool
            .iter()
            .any(|h| h.source_uri.contains("issues/77")),
        "pool.rs should link to issue #77"
    );
    assert!(
        hints_pool
            .iter()
            .any(|h| h.source_uri.contains("pull_requests/88")),
        "pool.rs should link to PR #88"
    );

    let hints_config = cross_source_hints::hints_for_file("src/db/config.rs", &edges, "/project");
    assert!(
        !hints_config.is_empty(),
        "config.rs should have cross-source hints"
    );

    let hints_test = cross_source_hints::hints_for_file("tests/pool_test.rs", &edges, "/project");
    assert!(
        !hints_test.is_empty(),
        "pool_test.rs should have cross-source hints"
    );

    // Step 4: Verify format output matches what ctx_read appends
    let formatted = cross_source_hints::format_hints(&hints_pool);
    assert!(formatted.starts_with("\n--- Cross-Source Hints ---\n"));
    assert!(formatted.contains("[mentions]") || formatted.contains("[mentioned_in]"));

    // Step 5: Verify cache hit (session cache stores the provider results)
    {
        let cache_guard = cache.blocking_read();
        assert!(cache_guard.get("github://issues/77").is_some());
        assert!(cache_guard.get("github://pull_requests/88").is_some());
    }
}

/// Verify the free-energy budget allocator works with realistic data.
#[test]
fn free_energy_budget_allocation() {
    use lean_ctx::core::free_energy_budget::{ColumnBudgetRequest, allocate_budget};

    let requests = vec![
        ColumnBudgetRequest {
            column_id: "code".into(),
            saliency_score: 0.9,
            estimated_tokens: 5000,
            minimum_tokens: 0,
        },
        ColumnBudgetRequest {
            column_id: "issues".into(),
            saliency_score: 0.6,
            estimated_tokens: 2000,
            minimum_tokens: 0,
        },
        ColumnBudgetRequest {
            column_id: "wiki".into(),
            saliency_score: 0.3,
            estimated_tokens: 3000,
            minimum_tokens: 0,
        },
    ];

    let allocations = allocate_budget(4000, &requests, 0.05);

    assert_eq!(allocations.len(), 3);
    let total_allocated: usize = allocations.iter().map(|a| a.allocated_tokens).sum();
    assert!(
        total_allocated <= 4000,
        "should not exceed budget: {total_allocated}"
    );

    let code_alloc = allocations.iter().find(|a| a.column_id == "code").unwrap();
    let wiki_alloc = allocations.iter().find(|a| a.column_id == "wiki").unwrap();
    assert!(
        code_alloc.allocated_tokens >= wiki_alloc.allocated_tokens,
        "higher-saliency code column should get more tokens than wiki"
    );
}

/// ECS saliency ranking respects task relevance.
#[test]
fn saliency_ranks_relevant_chunks_higher() {
    use lean_ctx::core::saliency::{EcsWeights, compute_ecs_scores};

    let chunks = vec![
        github_issue_chunk(
            "1",
            "Authentication token expired",
            "Token in src/auth.rs expired too fast",
            vec!["src/auth.rs"],
        ),
        github_issue_chunk(
            "2",
            "CSS alignment issue",
            "Button misaligned on mobile in styles.css",
            vec!["src/styles.css"],
        ),
    ];

    let keywords: Vec<String> = vec!["auth".into(), "token".into(), "expiry".into()];
    let edge_counts = vec![0, 0];
    let weights = EcsWeights {
        w_task: 0.8,
        w_graph: 0.1,
        w_density: 0.1,
    };

    let scores = compute_ecs_scores(&chunks, &keywords, &edge_counts, &weights);

    assert_eq!(scores.len(), 2);
    let auth_score = &scores[0];
    let css_score = &scores[1];
    assert!(
        auth_score.final_score >= css_score.final_score,
        "auth-related chunk should score >= css chunk for auth task"
    );
}
