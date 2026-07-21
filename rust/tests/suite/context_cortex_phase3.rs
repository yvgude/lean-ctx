//! Phase 3: Saliency Map + Info-Theoretic Ranking — Integration Tests
//!
//! Verifies the full Phase 3 implementation:
//!   1. ECS scoring ranks relevant chunks higher
//!   2. MIG selects diverse, non-redundant chunks
//!   3. Cross-source hints surface related external data
//!   4. Thompson Sampling provider bandit learns from feedback
//!   5. End-to-end: Provider → Consolidate → Saliency → MIG → Diverse output

use lean_ctx::core::bm25_index::ChunkKind;
use lean_ctx::core::cache::SessionCache;
use lean_ctx::core::consolidation::{apply_artifacts, consolidate};
use lean_ctx::core::content_chunk::ContentChunk;
use lean_ctx::core::cross_source_hints::{format_hints, hints_for_file};
use lean_ctx::core::graph_index::IndexEdge;
use lean_ctx::core::provider_bandit::ProviderBandit;
use lean_ctx::core::saliency::{EcsWeights, compute_ecs_scores, mig_select};

fn github_issue(id: &str, title: &str, content: &str, refs: Vec<&str>) -> ContentChunk {
    ContentChunk::from_provider(
        "github",
        "issues",
        id,
        title,
        ChunkKind::Issue,
        content.into(),
        refs.into_iter().map(String::from).collect(),
        Some(serde_json::json!({"state": "open", "labels": ["bug"]})),
    )
}

fn github_pr(id: &str, title: &str, content: &str, refs: Vec<&str>) -> ContentChunk {
    ContentChunk::from_provider(
        "github",
        "pull_requests",
        id,
        title,
        ChunkKind::PullRequest,
        content.into(),
        refs.into_iter().map(String::from).collect(),
        Some(serde_json::json!({"state": "open"})),
    )
}

// ---------------------------------------------------------------------------
// 1. ECS scoring
// ---------------------------------------------------------------------------

#[test]
fn ecs_ranks_task_relevant_chunks_first() {
    let chunks = vec![
        github_issue(
            "1",
            "Auth token bug",
            "JWT authentication token expiry is broken",
            vec!["src/auth.rs"],
        ),
        github_issue(
            "2",
            "UI button color",
            "The primary button color is slightly off on dark mode",
            vec![],
        ),
        github_issue(
            "3",
            "DB migration",
            "Need to add index on users.email column",
            vec!["src/db.rs"],
        ),
    ];

    let keywords = vec!["authentication".into(), "token".into(), "jwt".into()];
    let scores = compute_ecs_scores(&chunks, &keywords, &[0, 0, 0], &EcsWeights::default());

    assert!(
        scores[0].ecs_score > scores[1].ecs_score,
        "Auth issue should rank higher than UI issue"
    );
}

#[test]
fn ecs_boosts_graph_hubs() {
    let chunks = vec![
        github_issue("1", "Common file", "Changes to the auth module", vec![]),
        github_issue("2", "Rare file", "Changes to obscure util", vec![]),
    ];

    let edge_counts = vec![20, 1]; // First chunk's file has many edges
    let scores = compute_ecs_scores(&chunks, &[], &edge_counts, &EcsWeights::default());

    assert!(scores[0].graph_centrality > scores[1].graph_centrality);
}

#[test]
fn ecs_composite_score_respects_weights() {
    let chunks = vec![
        github_issue(
            "1",
            "Auth issue",
            "authentication token expiry broken",
            vec![],
        ),
        github_issue("2", "Hub file", "minor style change", vec![]),
    ];
    let keywords = vec!["authentication".into(), "token".into()];

    let task_heavy = EcsWeights {
        w_task: 0.9,
        w_graph: 0.05,
        w_density: 0.05,
    };
    let graph_heavy = EcsWeights {
        w_task: 0.05,
        w_graph: 0.9,
        w_density: 0.05,
    };

    // chunk 0: high task relevance, low graph edges
    // chunk 1: low task relevance, high graph edges
    let scores_task = compute_ecs_scores(&chunks, &keywords, &[1, 20], &task_heavy);
    let scores_graph = compute_ecs_scores(&chunks, &keywords, &[1, 20], &graph_heavy);

    // With task-heavy weights, chunk 0 (auth) should rank higher
    assert!(scores_task[0].final_score > scores_task[1].final_score);
    // With graph-heavy weights, chunk 1 (hub) should rank higher
    assert!(scores_graph[1].final_score > scores_graph[0].final_score);
}

// ---------------------------------------------------------------------------
// 2. MIG selection
// ---------------------------------------------------------------------------

#[test]
fn mig_avoids_redundant_chunks() {
    let chunks = vec![
        github_issue(
            "1",
            "Auth bug A",
            "JWT authentication token expiry broken in production",
            vec![],
        ),
        github_issue(
            "2",
            "Auth bug B",
            "JWT authentication token expiry broken in staging",
            vec![],
        ),
        github_issue(
            "3",
            "DB timeout",
            "PostgreSQL connection pool exhausted under heavy load",
            vec![],
        ),
    ];

    let keywords = vec!["authentication".into(), "database".into()];
    let scores = compute_ecs_scores(&chunks, &keywords, &[0, 0, 0], &EcsWeights::default());

    let selected = mig_select(&scores, &chunks, 2, 0.6);
    assert_eq!(selected.len(), 2);

    // Should NOT select both auth issues (they're near-duplicates).
    assert!(
        !(selected.contains(&0) && selected.contains(&1)),
        "MIG should not select two nearly identical auth issues"
    );
}

#[test]
fn mig_with_zero_lambda_is_pure_relevance() {
    let chunks = vec![
        github_issue("1", "Top ranked", "authentication token expiry", vec![]),
        github_issue("2", "Second", "authentication problem", vec![]),
        github_issue("3", "Third", "minor CSS issue", vec![]),
    ];

    let keywords = vec!["authentication".into(), "token".into()];
    let scores = compute_ecs_scores(&chunks, &keywords, &[0, 0, 0], &EcsWeights::default());

    let selected = mig_select(&scores, &chunks, 2, 0.0); // lambda=0: pure relevance
    assert_eq!(selected[0], 0); // Highest score first
}

#[test]
fn mig_handles_single_chunk() {
    let chunks = vec![github_issue("1", "Only one", "content", vec![])];
    let scores = compute_ecs_scores(&chunks, &[], &[0], &EcsWeights::default());

    let selected = mig_select(&scores, &chunks, 5, 0.6);
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0], 0);
}

// ---------------------------------------------------------------------------
// 3. Cross-source hints
// ---------------------------------------------------------------------------

#[test]
fn hints_surface_related_issues_for_code_file() {
    let edges = vec![
        IndexEdge {
            from: "src/auth.rs".into(),
            to: "github://issues/42".into(),
            kind: "mentions".into(),
            weight: 1.0,
        },
        IndexEdge {
            from: "github://pull_requests/100".into(),
            to: "src/auth.rs".into(),
            kind: "resolves".into(),
            weight: 1.5,
        },
    ];

    let hints = hints_for_file("src/auth.rs", &edges, "/project");
    assert_eq!(hints.len(), 2);
    assert!(hints.iter().any(|h| h.source_uri.contains("issues/42")));
    assert!(
        hints
            .iter()
            .any(|h| h.source_uri.contains("pull_requests/100"))
    );
}

#[test]
fn hints_sorted_by_weight() {
    let edges = vec![
        IndexEdge {
            from: "src/auth.rs".into(),
            to: "github://issues/1".into(),
            kind: "mentions".into(),
            weight: 0.5,
        },
        IndexEdge {
            from: "src/auth.rs".into(),
            to: "github://issues/2".into(),
            kind: "mentions".into(),
            weight: 2.0,
        },
    ];

    let hints = hints_for_file("src/auth.rs", &edges, "/project");
    assert_eq!(hints[0].source_uri, "github://issues/2");
    assert_eq!(hints[1].source_uri, "github://issues/1");
}

#[test]
fn hints_format_is_human_readable() {
    let edges = vec![IndexEdge {
        from: "src/auth.rs".into(),
        to: "github://issues/42".into(),
        kind: "mentions".into(),
        weight: 1.0,
    }];

    let hints = hints_for_file("src/auth.rs", &edges, "/project");
    let formatted = format_hints(&hints);

    assert!(formatted.contains("Cross-Source Hints"));
    assert!(formatted.contains("github://issues/42"));
    assert!(formatted.contains("[mentions]"));
}

// ---------------------------------------------------------------------------
// 4. Thompson Sampling provider bandit
// ---------------------------------------------------------------------------

#[test]
fn bandit_learns_provider_preference() {
    let mut bandit = ProviderBandit::new();

    // Train: github is always good for bugfix, jira is bad
    for _ in 0..30 {
        bandit.update("bugfix", "github", true);
        bandit.update("bugfix", "jira", false);
    }

    let gh = bandit.estimated_probability("bugfix", "github");
    let jira = bandit.estimated_probability("bugfix", "jira");
    assert!(gh > 0.85);
    assert!(jira < 0.15);
}

#[test]
fn bandit_independent_per_task_type() {
    let mut bandit = ProviderBandit::new();

    // github is good for bugfix, jira is good for feature
    for _ in 0..20 {
        bandit.update("bugfix", "github", true);
        bandit.update("bugfix", "jira", false);
        bandit.update("feature", "jira", true);
        bandit.update("feature", "github", false);
    }

    assert!(bandit.estimated_probability("bugfix", "github") > 0.7);
    assert!(bandit.estimated_probability("feature", "jira") > 0.7);
}

#[test]
fn bandit_selects_from_trained_distribution() {
    let mut bandit = ProviderBandit::new();
    let providers = vec!["github".into(), "jira".into()];

    for _ in 0..50 {
        bandit.update("bugfix", "github", true);
        bandit.update("bugfix", "jira", false);
    }

    let mut github_count = 0;
    for _ in 0..100 {
        let selected = bandit.select_provider("bugfix", &providers).unwrap();
        if selected == "github" {
            github_count += 1;
        }
    }
    assert!(
        github_count > 85,
        "github should be selected >85% of the time, got {github_count}"
    );
}

// ---------------------------------------------------------------------------
// 5. End-to-end: Provider → Consolidate → Saliency → MIG
// ---------------------------------------------------------------------------

#[test]
fn end_to_end_saliency_pipeline() {
    // 1. Create provider chunks
    let chunks = vec![
        github_issue(
            "42",
            "Auth token crash",
            "JWT authentication token expiry broken in production env",
            vec!["src/auth.rs"],
        ),
        github_issue(
            "43",
            "Auth token crash (dup)",
            "JWT authentication token expiry broken in staging env",
            vec!["src/auth.rs"],
        ),
        github_issue(
            "44",
            "DB pool exhaust",
            "PostgreSQL connection pool timeout under load",
            vec!["src/db/pool.rs"],
        ),
        github_pr(
            "100",
            "Fix auth tokens",
            "Fixes JWT token lifetime calculation",
            vec!["src/auth.rs"],
        ),
        github_issue(
            "45",
            "CSS dark mode",
            "Button color wrong in dark mode theme",
            vec!["src/ui/theme.css"],
        ),
    ];

    // 2. Consolidate
    let artifacts = consolidate(&chunks);
    let mut index = lean_ctx::core::bm25_index::BM25Index {
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

    apply_artifacts(
        &artifacts,
        Some(&mut index),
        Some(&mut edges),
        Some(&mut cache),
    );

    assert_eq!(index.doc_count, 5);
    assert!(!edges.is_empty());

    // 3. Compute saliency scores
    let edge_counts: Vec<usize> = chunks
        .iter()
        .map(|c| {
            edges
                .iter()
                .filter(|e| e.from == c.file_path || e.to == c.file_path)
                .count()
        })
        .collect();

    let keywords = vec!["authentication".into(), "token".into(), "jwt".into()];
    let scores = compute_ecs_scores(&chunks, &keywords, &edge_counts, &EcsWeights::default());

    // Auth issues should score highest
    let auth_scores: Vec<f64> = scores
        .iter()
        .filter(|s| chunks[s.chunk_idx].symbol_name.contains("Auth token"))
        .map(|s| s.ecs_score)
        .collect();
    let css_score = scores
        .iter()
        .find(|s| chunks[s.chunk_idx].symbol_name.contains("CSS"))
        .unwrap()
        .ecs_score;

    assert!(auth_scores.iter().all(|&s| s > css_score));

    // 4. MIG select diverse top-3
    let selected = mig_select(&scores, &chunks, 3, 0.6);
    assert_eq!(selected.len(), 3);

    // Should not select both duplicate auth issues
    let auth_crash_count = selected
        .iter()
        .filter(|&&i| chunks[i].symbol_name.contains("Auth token crash"))
        .count();
    assert!(
        auth_crash_count <= 1,
        "MIG should deduplicate near-identical auth crash issues"
    );

    // 5. Cross-source hints for auth.rs
    let hints = hints_for_file("src/auth.rs", &edges, "/project");
    assert!(!hints.is_empty());
}
