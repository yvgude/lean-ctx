//! End-to-End Scenario Tests — Realistic Context Engine Usage
//!
//! Tests the full pipeline with realistic multi-source data:
//!
//!   Scenario 1: Bug Investigation
//!     Agent task: "Fix the JWT token expiry bug"
//!     Sources: GitHub issues, PRs, code files, DB schema
//!     Verifies: correct ranking, dedup, hints, preload predictions
//!
//!   Scenario 2: Feature Development
//!     Agent task: "Add user avatar upload feature"
//!     Sources: Jira tickets, wiki docs, code, DB schema
//!     Verifies: cross-source graph, knowledge extraction, budget allocation
//!
//!   Scenario 3: Code Review
//!     Agent task: "Review pull requests for the auth module"
//!     Sources: GitHub PRs, related issues, code context
//!     Verifies: PR-to-issue linking, file hints, bandit learning

use lean_ctx::core::active_inference::predict_preloads;
use lean_ctx::core::bm25_index::{BM25Index, ChunkKind, CodeChunk};
use lean_ctx::core::cache::SessionCache;
use lean_ctx::core::consolidation::{apply_artifacts, consolidate};
use lean_ctx::core::content_chunk::ContentChunk;
use lean_ctx::core::cross_source_hints::{format_hints, hints_for_file};
use lean_ctx::core::free_energy_budget::{ColumnBudgetRequest, allocate_budget, free_energy};
use lean_ctx::core::graph_index::IndexEdge;
use lean_ctx::core::knowledge_provider_extract::extract_facts;
use lean_ctx::core::provider_bandit::ProviderBandit;
use lean_ctx::core::saliency::{EcsWeights, compute_ecs_scores, mig_select};

// ---------------------------------------------------------------------------
// Helpers: realistic data generators
// ---------------------------------------------------------------------------

fn code_chunk(path: &str, symbol: &str, content: &str, kind: ChunkKind) -> ContentChunk {
    ContentChunk::from(CodeChunk {
        file_path: path.into(),
        symbol_name: symbol.into(),
        kind,
        start_line: 1,
        end_line: 20,
        content: content.into(),
        tokens: vec![],
        token_count: 0,
    })
}

fn github_issue(
    id: &str,
    title: &str,
    body: &str,
    labels: &[&str],
    refs: Vec<&str>,
) -> ContentChunk {
    ContentChunk::from_provider(
        "github",
        "issues",
        id,
        title,
        ChunkKind::Issue,
        body.into(),
        refs.into_iter().map(String::from).collect(),
        Some(serde_json::json!({"state": "open", "author": "dev", "labels": labels})),
    )
}

fn github_pr(id: &str, title: &str, body: &str, refs: Vec<&str>) -> ContentChunk {
    ContentChunk::from_provider(
        "github",
        "pull_requests",
        id,
        title,
        ChunkKind::PullRequest,
        body.into(),
        refs.into_iter().map(String::from).collect(),
        Some(serde_json::json!({"state": "open", "author": "dev"})),
    )
}

fn jira_ticket(
    id: &str,
    title: &str,
    body: &str,
    labels: &[&str],
    refs: Vec<&str>,
) -> ContentChunk {
    ContentChunk::from_provider(
        "jira",
        "issues",
        id,
        title,
        ChunkKind::Ticket,
        body.into(),
        refs.into_iter().map(String::from).collect(),
        Some(serde_json::json!({"state": "In Progress", "labels": labels})),
    )
}

fn wiki_page(id: &str, title: &str, body: &str, refs: Vec<&str>) -> ContentChunk {
    ContentChunk::from_provider(
        "confluence",
        "wikis",
        id,
        title,
        ChunkKind::WikiPage,
        body.into(),
        refs.into_iter().map(String::from).collect(),
        None,
    )
}

fn db_schema(table: &str, columns: &str) -> ContentChunk {
    ContentChunk::from_provider(
        "postgres",
        "schemas",
        table,
        &format!("public.{table}"),
        ChunkKind::DbSchema,
        format!("CREATE TABLE {table} ({columns})"),
        vec![],
        None,
    )
}

// ---------------------------------------------------------------------------
// Scenario 1: Bug Investigation
// ---------------------------------------------------------------------------

#[test]
fn scenario_bug_investigation_full_pipeline() {
    // === DATA SOURCES ===
    let chunks = vec![
        // Code files
        code_chunk(
            "src/auth/jwt.rs",
            "validate_token",
            "pub fn validate_token(jwt: &str) -> Result<Claims, AuthError> { \
             let decoded = decode_jwt(jwt)?; check_expiry(&decoded.exp)?; Ok(decoded.claims) }",
            ChunkKind::Function,
        ),
        code_chunk(
            "src/auth/jwt.rs",
            "check_expiry",
            "fn check_expiry(exp: &u64) -> Result<(), AuthError> { \
             let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs(); \
             if now > *exp { Err(AuthError::Expired) } else { Ok(()) } }",
            ChunkKind::Function,
        ),
        code_chunk(
            "src/api/middleware.rs",
            "auth_middleware",
            "pub async fn auth_middleware(req: Request) -> Result<Request, Response> { \
             let token = req.header(\"Authorization\")?; validate_token(token)?; Ok(req) }",
            ChunkKind::Function,
        ),
        // GitHub issues
        github_issue(
            "142",
            "JWT tokens expire after 30 minutes instead of 24 hours",
            "Users report being logged out after 30 minutes. The token expiry is set \
             in src/auth/jwt.rs but the value seems to use minutes instead of seconds.",
            &["bug", "p1", "authentication"],
            vec!["src/auth/jwt.rs"],
        ),
        github_issue(
            "143",
            "Rate limiter triggers too aggressively",
            "The rate limiter in src/api/ratelimit.rs blocks legitimate users after 10 requests.",
            &["bug", "p2"],
            vec!["src/api/ratelimit.rs"],
        ),
        // GitHub PR
        github_pr(
            "87",
            "Fix JWT token expiry calculation",
            "Changes the expiry calculation from minutes to seconds. \
             Fixes #142. Modified src/auth/jwt.rs.",
            vec!["src/auth/jwt.rs"],
        ),
        // DB schema
        db_schema(
            "sessions",
            "id SERIAL PRIMARY KEY, user_id INT, token TEXT, expires_at TIMESTAMP",
        ),
    ];

    // === 1. CONSOLIDATION ===
    let artifacts = consolidate(&chunks);
    let mut bm25 = BM25Index {
        chunks: Vec::new(),
        inverted: std::collections::HashMap::new(),
        avg_doc_len: 0.0,
        doc_count: 0,
        doc_freqs: std::collections::HashMap::new(),
        files: std::collections::HashMap::new(),
        content_truncated: false,
    };
    let mut edges: Vec<IndexEdge> = Vec::new();
    let mut cache = SessionCache::new();
    let result = apply_artifacts(
        &artifacts,
        Some(&mut bm25),
        Some(&mut edges),
        Some(&mut cache),
    );

    assert!(result.chunks_indexed > 0, "BM25 should have indexed chunks");
    assert!(
        result.edges_created > 0,
        "Graph should have cross-source edges"
    );
    assert!(
        result.facts_extracted > 0,
        "Knowledge facts should be extracted"
    );
    assert!(
        result.cache_entries_stored > 0,
        "Session cache should be populated"
    );

    // === 2. BM25 SEARCH ===
    let search_results = bm25.search("JWT token expiry authentication", 10);
    assert!(!search_results.is_empty(), "Search should find results");
    // The JWT issue or auth code should rank high
    let top_paths: Vec<&str> = search_results
        .iter()
        .take(3)
        .map(|r| r.file_path.as_str())
        .collect();
    let has_jwt_related = top_paths
        .iter()
        .any(|p| p.contains("jwt") || p.contains("issues/142"));
    assert!(
        has_jwt_related,
        "Top results should include JWT-related content: {top_paths:?}"
    );

    // === 3. CROSS-SOURCE EDGES ===
    assert!(
        edges
            .iter()
            .any(|e| e.to == "src/auth/jwt.rs" || e.from == "src/auth/jwt.rs"),
        "jwt.rs should be connected via cross-source edges"
    );

    // === 4. SALIENCY + MIG ===
    let task_keywords = vec![
        "jwt".into(),
        "token".into(),
        "expiry".into(),
        "authentication".into(),
    ];
    let edge_counts: Vec<usize> = chunks
        .iter()
        .map(|c| {
            edges
                .iter()
                .filter(|e| e.from == c.file_path || e.to == c.file_path)
                .count()
        })
        .collect();
    let scores = compute_ecs_scores(
        &chunks,
        &task_keywords,
        &edge_counts,
        &EcsWeights::default(),
    );

    // JWT-related chunks should have highest saliency
    let jwt_scores: Vec<(usize, f64)> = scores
        .iter()
        .filter(|s| {
            chunks[s.chunk_idx].file_path.contains("jwt")
                || chunks[s.chunk_idx].symbol_name.contains("JWT")
        })
        .map(|s| (s.chunk_idx, s.ecs_score))
        .collect();
    let other_max = scores
        .iter()
        .filter(|s| {
            !chunks[s.chunk_idx].file_path.contains("jwt")
                && !chunks[s.chunk_idx].symbol_name.contains("JWT")
        })
        .map(|s| s.ecs_score)
        .fold(0.0f64, f64::max);

    assert!(!jwt_scores.is_empty(), "JWT chunks should have scores");
    let jwt_max = jwt_scores.iter().map(|(_, s)| *s).fold(0.0f64, f64::max);
    assert!(
        jwt_max >= other_max,
        "JWT chunks should score >= non-JWT: jwt={jwt_max:.3} other={other_max:.3}"
    );

    // MIG should select diverse chunks (not just all JWT)
    let selected = mig_select(&scores, &chunks, 4, 0.6);
    assert_eq!(selected.len(), 4);
    let jwt_count = selected
        .iter()
        .filter(|&&i| chunks[i].file_path.contains("jwt"))
        .count();
    assert!(
        jwt_count <= 2,
        "MIG should diversify, not only pick JWT chunks"
    );

    // === 5. CROSS-SOURCE HINTS ===
    let hints = hints_for_file("src/auth/jwt.rs", &edges, "/project");
    assert!(!hints.is_empty(), "jwt.rs should have cross-source hints");
    let formatted = format_hints(&hints);
    assert!(
        formatted.contains("Cross-Source Hints"),
        "Hints should be formatted"
    );

    // === 6. KNOWLEDGE EXTRACTION ===
    let facts = extract_facts(&chunks);
    assert!(
        facts.iter().any(|f| f.category == "known_bugs"),
        "Should extract known_bugs"
    );
    assert!(
        facts.iter().any(|f| f.category == "recent_changes"),
        "Should extract recent_changes from PR"
    );
    assert!(
        facts.iter().any(|f| f.category == "data_model"),
        "Should extract data_model from DB schema"
    );

    // === 7. SESSION CACHE ===
    assert!(
        cache.get("github://issues/142").is_some(),
        "Issue 142 should be cached"
    );
    assert!(
        cache.get("github://pull_requests/87").is_some(),
        "PR 87 should be cached"
    );

    // === 8. ACTIVE INFERENCE ===
    let mut bandit = ProviderBandit::new();
    let providers = vec!["github".into(), "postgres".into()];
    let predictions = predict_preloads(
        "Fix the JWT token expiry bug in authentication",
        &providers,
        &mut bandit,
        5,
    );
    assert!(
        !predictions.is_empty(),
        "Should predict preloads for bug fix task"
    );
    assert!(
        predictions.iter().any(|p| p.provider_id == "github"),
        "Should predict GitHub"
    );

    // === 9. FREE ENERGY BUDGET ===
    let budget_requests = vec![
        ColumnBudgetRequest {
            column_id: "filesystem".into(),
            saliency_score: 0.8,
            estimated_tokens: 5000,
            minimum_tokens: 1000,
        },
        ColumnBudgetRequest {
            column_id: "github".into(),
            saliency_score: 0.9,
            estimated_tokens: 2000,
            minimum_tokens: 500,
        },
        ColumnBudgetRequest {
            column_id: "postgres".into(),
            saliency_score: 0.3,
            estimated_tokens: 1000,
            minimum_tokens: 200,
        },
    ];
    let allocs = allocate_budget(8000, &budget_requests, 0.05);
    assert_eq!(allocs.len(), 3);
    let fe = free_energy(&budget_requests, &allocs);
    assert!(fe >= 0.0, "Free energy should be non-negative");

    // GitHub (highest saliency/cost ratio) should get good allocation
    let gh_alloc = allocs.iter().find(|a| a.column_id == "github").unwrap();
    let pg_alloc = allocs.iter().find(|a| a.column_id == "postgres").unwrap();
    assert!(
        gh_alloc.allocated_tokens > pg_alloc.allocated_tokens,
        "GitHub should get more budget than Postgres"
    );
}

// ---------------------------------------------------------------------------
// Scenario 2: Feature Development
// ---------------------------------------------------------------------------

#[test]
fn scenario_feature_development_cross_source() {
    let chunks = vec![
        // Existing code
        code_chunk(
            "src/models/user.rs",
            "User",
            "pub struct User { id: i64, email: String, name: String }",
            ChunkKind::Struct,
        ),
        code_chunk(
            "src/api/users.rs",
            "get_user",
            "pub async fn get_user(id: i64) -> Result<User, ApiError>",
            ChunkKind::Function,
        ),
        // Jira ticket
        jira_ticket(
            "PROJ-42",
            "Add user avatar upload feature",
            "As a user, I want to upload an avatar image. Must support JPEG/PNG. \
             Store in S3. Update src/models/user.rs to add avatar_url field.",
            &["feature", "user-profile"],
            vec!["src/models/user.rs", "src/api/users.rs"],
        ),
        // Wiki documentation
        wiki_page(
            "file-upload-guide",
            "File Upload Architecture",
            "Our file upload system uses presigned S3 URLs. See src/storage/s3.rs for the implementation.",
            vec!["src/storage/s3.rs"],
        ),
        // DB schema
        db_schema(
            "users",
            "id SERIAL PRIMARY KEY, email VARCHAR(255), name VARCHAR(100), avatar_url TEXT",
        ),
    ];

    // Consolidate
    let artifacts = consolidate(&chunks);
    let mut edges: Vec<IndexEdge> = Vec::new();
    apply_artifacts(&artifacts, None, Some(&mut edges), None);

    // Cross-source edges should connect the Jira ticket to code files
    assert!(
        edges.iter().any(|e| e.to == "src/models/user.rs"),
        "Jira ticket should create edges to user model"
    );
    assert!(
        edges.iter().any(|e| e.to == "src/api/users.rs"),
        "Jira ticket should create edges to user API"
    );

    // Knowledge extraction
    let facts = extract_facts(&chunks);
    assert!(
        facts.iter().any(|f| f.category == "known_features"),
        "Feature ticket should create known_features fact"
    );
    assert!(
        facts.iter().any(|f| f.category == "documentation"),
        "Wiki page should create documentation fact"
    );
    assert!(
        facts.iter().any(|f| f.category == "data_model"),
        "DB schema should create data_model fact"
    );

    // Hints for user.rs should include the Jira ticket
    let hints = hints_for_file("src/models/user.rs", &edges, "/project");
    assert!(!hints.is_empty(), "user.rs should have hints from Jira");
}

// ---------------------------------------------------------------------------
// Scenario 3: Code Review with Bandit Learning
// ---------------------------------------------------------------------------

#[test]
fn scenario_code_review_bandit_learns() {
    let chunks = vec![
        github_pr(
            "200",
            "Refactor auth middleware",
            "Simplifies the auth middleware. Removes dead code from src/auth/middleware.rs.",
            vec!["src/auth/middleware.rs"],
        ),
        github_pr(
            "201",
            "Update rate limiter",
            "Changes rate limiting algorithm in src/api/ratelimit.rs.",
            vec!["src/api/ratelimit.rs"],
        ),
        github_issue(
            "150",
            "Middleware is too complex",
            "The auth middleware in src/auth/middleware.rs has too many branches.",
            &["tech-debt"],
            vec!["src/auth/middleware.rs"],
        ),
    ];

    // Consolidate
    let artifacts = consolidate(&chunks);
    let mut edges: Vec<IndexEdge> = Vec::new();
    apply_artifacts(&artifacts, None, Some(&mut edges), None);

    // PR #200 should link to Issue #150 via shared file reference
    let middleware_edges: Vec<_> = edges
        .iter()
        .filter(|e| e.from.contains("middleware") || e.to.contains("middleware"))
        .collect();
    assert!(
        !middleware_edges.is_empty(),
        "Middleware should have cross-source edges"
    );

    // Bandit learning cycle
    let mut bandit = ProviderBandit::new();
    let providers = vec!["github".into(), "jira".into()];

    // Simulate: GitHub is useful for code review, Jira is not
    for _ in 0..15 {
        bandit.update("review", "github", true);
        bandit.update("review", "jira", false);
    }

    // Selection should strongly prefer github for review tasks
    let mut gh_count = 0;
    for _ in 0..50 {
        let selected = bandit.select_provider("review", &providers).unwrap();
        if selected == "github" {
            gh_count += 1;
        }
    }
    assert!(
        gh_count > 40,
        "Bandit should prefer GitHub for review after training: {gh_count}/50"
    );

    // Active inference should predict GitHub issues/PRs for review task
    let predictions = predict_preloads(
        "Review the open pull requests for auth module",
        &providers,
        &mut bandit,
        5,
    );
    assert!(
        predictions
            .iter()
            .any(|p| p.provider_id == "github" && p.action == "pull_requests"),
        "Should predict GitHub PRs for review task"
    );
}

// ---------------------------------------------------------------------------
// Scenario 4: Full Budget Optimization
// ---------------------------------------------------------------------------

#[test]
fn scenario_budget_optimization_under_constraint() {
    // Simulate 3 columns competing for a tight 4000-token budget
    let requests = vec![
        ColumnBudgetRequest {
            column_id: "filesystem".into(),
            saliency_score: 0.7,
            estimated_tokens: 3000,
            minimum_tokens: 500,
        },
        ColumnBudgetRequest {
            column_id: "github_issues".into(),
            saliency_score: 0.95,
            estimated_tokens: 1500,
            minimum_tokens: 300,
        },
        ColumnBudgetRequest {
            column_id: "db_schemas".into(),
            saliency_score: 0.2,
            estimated_tokens: 500,
            minimum_tokens: 100,
        },
    ];

    let allocs = allocate_budget(4000, &requests, 0.05);

    // All columns should get at least their minimum
    for (alloc, req) in allocs.iter().zip(requests.iter()) {
        assert!(
            alloc.allocated_tokens >= req.minimum_tokens,
            "{} got {} tokens, minimum was {}",
            alloc.column_id,
            alloc.allocated_tokens,
            req.minimum_tokens
        );
    }

    // GitHub issues has highest saliency/cost ratio (0.95/1500 = 0.000633)
    // should get proportionally more than DB schemas (0.2/500 = 0.0004)
    let gh = allocs
        .iter()
        .find(|a| a.column_id == "github_issues")
        .unwrap();
    let db = allocs.iter().find(|a| a.column_id == "db_schemas").unwrap();
    assert!(
        gh.allocated_tokens > db.allocated_tokens,
        "GitHub should get more budget than DB: {} vs {}",
        gh.allocated_tokens,
        db.allocated_tokens
    );

    // Free energy should be > 0 since we can't satisfy all requests
    let fe = free_energy(&requests, &allocs);
    assert!(fe > 0.0, "Free energy should be positive under constraint");
    assert!(
        fe < 1.0,
        "Free energy should be < 1.0 (we allocated something)"
    );
}

// ---------------------------------------------------------------------------
// Scenario 5: MIG Dedup with Near-Duplicate External Sources
// ---------------------------------------------------------------------------

#[test]
fn scenario_dedup_duplicate_issues_from_different_sources() {
    // Same bug reported in GitHub AND Jira (common in real orgs)
    let chunks = vec![
        github_issue(
            "100",
            "Auth token expires too early",
            "JWT authentication tokens expire after 30 minutes instead of 24 hours in production",
            &["bug"],
            vec!["src/auth/jwt.rs"],
        ),
        jira_ticket(
            "PROJ-50",
            "Authentication token expiry broken",
            "JWT authentication tokens expire after 30 minutes instead of the expected 24 hours",
            &["bug", "defect"],
            vec!["src/auth/jwt.rs"],
        ),
        github_issue(
            "101",
            "Homepage loads slowly",
            "The main page takes 5 seconds to load due to unoptimized database queries",
            &["performance"],
            vec!["src/api/home.rs"],
        ),
    ];

    let keywords = vec!["authentication".into(), "token".into(), "expiry".into()];
    let scores = compute_ecs_scores(&chunks, &keywords, &[0, 0, 0], &EcsWeights::default());

    // MIG should detect the duplicates and pick only one auth issue
    let selected = mig_select(&scores, &chunks, 2, 0.6);
    assert_eq!(selected.len(), 2);

    let auth_count = selected
        .iter()
        .filter(|&&i| {
            chunks[i].symbol_name.contains("Auth")
                || chunks[i].symbol_name.contains("Authentication")
        })
        .count();
    assert!(
        auth_count <= 1,
        "MIG should deduplicate near-identical auth issues from GitHub and Jira"
    );

    // Should include the homepage issue for diversity
    assert!(
        selected
            .iter()
            .any(|&i| chunks[i].symbol_name.contains("Homepage")),
        "Should include the diverse homepage issue"
    );
}
