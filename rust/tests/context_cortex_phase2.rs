//! Phase 2: Hippocampal Consolidation Loop — Integration Tests
//!
//! Verifies the full consolidation pipeline:
//!   1. Cross-source edge creation + merge
//!   2. Knowledge auto-extraction from provider data
//!   3. Session cache integration for provider results
//!   4. End-to-end: Provider → Consolidate → All stores populated

use lean_ctx::core::bm25_index::{BM25Index, ChunkKind};
use lean_ctx::core::cache::SessionCache;
use lean_ctx::core::consolidation::{apply_artifacts, consolidate};
use lean_ctx::core::content_chunk::ContentChunk;
use lean_ctx::core::cross_source_edges::{
    EDGE_DOCUMENTS, EDGE_MENTIONS, EDGE_QUERIES, EDGE_RESOLVES, extract_cross_source_edges,
    merge_edges,
};
use lean_ctx::core::graph_index::IndexEdge;
use lean_ctx::core::knowledge_provider_extract::extract_facts;

// ---------------------------------------------------------------------------
// Helper: create provider chunks
// ---------------------------------------------------------------------------

fn github_issue(id: &str, title: &str, labels: &[&str], refs: Vec<&str>) -> ContentChunk {
    ContentChunk::from_provider(
        "github",
        "issues",
        id,
        title,
        ChunkKind::Issue,
        format!("Body of {title}"),
        refs.into_iter().map(String::from).collect(),
        Some(serde_json::json!({
            "state": "open",
            "author": "testuser",
            "labels": labels,
        })),
    )
}

fn github_pr(id: &str, title: &str, refs: Vec<&str>) -> ContentChunk {
    ContentChunk::from_provider(
        "github",
        "pull_requests",
        id,
        title,
        ChunkKind::PullRequest,
        format!("PR body: {title}"),
        refs.into_iter().map(String::from).collect(),
        Some(serde_json::json!({"state": "open"})),
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

fn db_schema(table: &str, refs: Vec<&str>) -> ContentChunk {
    ContentChunk::from_provider(
        "postgres",
        "schemas",
        table,
        &format!("public.{table}"),
        ChunkKind::DbSchema,
        format!("CREATE TABLE {table} (id serial PRIMARY KEY)"),
        refs.into_iter().map(String::from).collect(),
        None,
    )
}

// ---------------------------------------------------------------------------
// 1. Cross-source edges
// ---------------------------------------------------------------------------

#[test]
fn issue_creates_bidirectional_mentions_edges() {
    let chunks = vec![github_issue(
        "42",
        "Auth crash",
        &["bug"],
        vec!["src/auth.rs"],
    )];
    let edges = extract_cross_source_edges(&chunks);

    let forward: Vec<_> = edges
        .iter()
        .filter(|e| e.from.contains("issues/42") && e.to == "src/auth.rs")
        .collect();
    let reverse: Vec<_> = edges
        .iter()
        .filter(|e| e.from == "src/auth.rs" && e.to.contains("issues/42"))
        .collect();

    assert_eq!(forward.len(), 1);
    assert_eq!(reverse.len(), 1);
    assert_eq!(forward[0].kind, EDGE_MENTIONS);
    assert_eq!(reverse[0].kind, "mentioned_in");
}

#[test]
fn pr_creates_resolves_edges_with_high_weight() {
    let chunks = vec![github_pr("100", "Fix auth", vec!["src/auth.rs"])];
    let edges = extract_cross_source_edges(&chunks);

    let resolves: Vec<_> = edges.iter().filter(|e| e.kind == EDGE_RESOLVES).collect();
    assert_eq!(resolves.len(), 1);
    assert_eq!(resolves[0].weight, 1.5);
}

#[test]
fn wiki_creates_documents_edges() {
    let chunks = vec![wiki_page(
        "auth-guide",
        "Auth Guide",
        vec!["src/auth/mod.rs"],
    )];
    let edges = extract_cross_source_edges(&chunks);

    assert!(edges.iter().any(|e| e.kind == EDGE_DOCUMENTS));
}

#[test]
fn db_schema_creates_queries_edges() {
    let chunks = vec![db_schema("users", vec!["src/db/users.rs"])];
    let edges = extract_cross_source_edges(&chunks);

    assert!(edges.iter().any(|e| e.kind == EDGE_QUERIES));
    let query_edge = edges.iter().find(|e| e.kind == EDGE_QUERIES).unwrap();
    assert_eq!(query_edge.weight, 1.2);
}

#[test]
fn hub_detection_multiple_sources_point_to_same_file() {
    let chunks = vec![
        github_issue("1", "Bug in auth", &["bug"], vec!["src/auth.rs"]),
        github_issue("2", "Feature for auth", &["feature"], vec!["src/auth.rs"]),
        github_pr("10", "Fix auth", vec!["src/auth.rs"]),
        wiki_page("auth-doc", "Auth Docs", vec!["src/auth.rs"]),
    ];

    let edges = extract_cross_source_edges(&chunks);
    let incoming_to_auth = edges.iter().filter(|e| e.to == "src/auth.rs").count();
    assert_eq!(incoming_to_auth, 4);
}

#[test]
fn merge_edges_dedup_and_weight_upgrade() {
    let mut existing = vec![IndexEdge {
        from: "github://issues/1".into(),
        to: "src/auth.rs".into(),
        kind: EDGE_MENTIONS.into(),
        weight: 0.5,
    }];

    let new = vec![
        IndexEdge {
            from: "github://issues/1".into(),
            to: "src/auth.rs".into(),
            kind: EDGE_MENTIONS.into(),
            weight: 2.0,
        },
        IndexEdge {
            from: "github://issues/2".into(),
            to: "src/db.rs".into(),
            kind: EDGE_MENTIONS.into(),
            weight: 1.0,
        },
    ];

    let added = merge_edges(&mut existing, new);
    assert_eq!(added, 1);
    assert_eq!(existing.len(), 2);
    assert_eq!(
        existing
            .iter()
            .find(|e| e.to == "src/auth.rs")
            .unwrap()
            .weight,
        2.0
    );
}

// ---------------------------------------------------------------------------
// 2. Knowledge extraction
// ---------------------------------------------------------------------------

#[test]
fn bug_issue_extracts_known_bugs_fact() {
    let chunks = vec![github_issue(
        "42",
        "Token expiry crash",
        &["bug"],
        vec!["src/auth.rs"],
    )];
    let facts = extract_facts(&chunks);

    let bug = facts.iter().find(|f| f.category == "known_bugs");
    assert!(bug.is_some());
    let bug = bug.unwrap();
    assert!(bug.key.contains("42"));
    assert!(bug.value.contains("Token expiry crash"));
    assert_eq!(bug.confidence, 0.9);
}

#[test]
fn pr_extracts_recent_changes_and_changed_files() {
    let chunks = vec![github_pr("100", "Fix token lifetime", vec!["src/auth.rs"])];
    let facts = extract_facts(&chunks);

    assert!(facts.iter().any(|f| f.category == "recent_changes"));
    let changed = facts
        .iter()
        .find(|f| f.category == "changed_files" && f.key == "src/auth.rs");
    assert!(changed.is_some());
    assert!(changed.unwrap().confidence >= 0.9);
}

#[test]
fn wiki_extracts_documentation_facts() {
    let chunks = vec![wiki_page("api-guide", "API Guide", vec!["src/api/mod.rs"])];
    let facts = extract_facts(&chunks);

    assert!(facts.iter().any(|f| f.category == "documentation"));
    assert!(facts.iter().any(|f| f.category == "documented_files"));
}

#[test]
fn db_extracts_data_model_facts() {
    let chunks = vec![db_schema("sessions", vec![])];
    let facts = extract_facts(&chunks);

    assert_eq!(facts.len(), 1);
    assert_eq!(facts[0].category, "data_model");
    assert_eq!(facts[0].confidence, 0.95);
}

// ---------------------------------------------------------------------------
// 3. Session cache integration
// ---------------------------------------------------------------------------

#[test]
fn consolidation_stores_in_session_cache() {
    let chunks = vec![
        github_issue("42", "Auth bug", &["bug"], vec!["src/auth.rs"]),
        github_pr("100", "Fix auth", vec!["src/auth.rs"]),
    ];

    let artifacts = consolidate(&chunks);
    let mut cache = SessionCache::new();

    apply_artifacts(&artifacts, None, None, Some(&mut cache));

    assert!(cache.get("github://issues/42").is_some());
    assert!(cache.get("github://pull_requests/100").is_some());
}

#[test]
fn cached_provider_result_has_correct_content() {
    let chunks = vec![github_issue("42", "Auth bug", &["bug"], vec![])];
    let artifacts = consolidate(&chunks);
    let mut cache = SessionCache::new();

    apply_artifacts(&artifacts, None, None, Some(&mut cache));

    let entry = cache.get("github://issues/42").unwrap();
    let content = entry.content().unwrap();
    assert!(content.contains("Auth bug"));
}

// ---------------------------------------------------------------------------
// 4. End-to-end consolidation
// ---------------------------------------------------------------------------

#[test]
fn end_to_end_consolidation_populates_all_stores() {
    let chunks = vec![
        github_issue(
            "42",
            "Auth token crash",
            &["bug", "p1"],
            vec!["src/auth.rs"],
        ),
        github_pr(
            "100",
            "Fix auth expiry",
            vec!["src/auth.rs", "src/token.rs"],
        ),
        wiki_page("auth-doc", "Auth Architecture", vec!["src/auth/mod.rs"]),
        db_schema("sessions", vec!["src/db/session.rs"]),
    ];

    let artifacts = consolidate(&chunks);

    assert!(!artifacts.is_empty());
    assert_eq!(artifacts.bm25_chunks.len(), 4);
    assert!(!artifacts.edges.is_empty());
    assert!(!artifacts.facts.is_empty());
    assert_eq!(artifacts.cache_entries.len(), 4);

    let mut index = BM25Index {
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
        Some(&mut index),
        Some(&mut edges),
        Some(&mut cache),
    );

    // BM25
    assert_eq!(result.chunks_indexed, 4);
    assert_eq!(index.doc_count, 4);
    assert_eq!(index.external_chunk_count(), 4);

    // Graph edges
    assert!(result.edges_created > 0);
    assert!(edges.iter().any(|e| e.to == "src/auth.rs"));
    assert!(edges.iter().any(|e| e.kind == EDGE_RESOLVES));
    assert!(edges.iter().any(|e| e.kind == EDGE_DOCUMENTS));
    assert!(edges.iter().any(|e| e.kind == EDGE_QUERIES));

    // Knowledge facts
    assert!(result.facts_extracted > 0);
    let facts = &artifacts.facts;
    assert!(facts.iter().any(|f| f.category == "known_bugs"));
    assert!(facts.iter().any(|f| f.category == "recent_changes"));
    assert!(facts.iter().any(|f| f.category == "documentation"));
    assert!(facts.iter().any(|f| f.category == "data_model"));

    // Session cache
    assert_eq!(result.cache_entries_stored, 4);
    assert!(cache.get("github://issues/42").is_some());
    assert!(cache.get("github://pull_requests/100").is_some());
    assert!(cache.get("confluence://wikis/auth-doc").is_some());
    assert!(cache.get("postgres://schemas/sessions").is_some());
}

#[test]
fn consolidation_summary_is_accurate() {
    let chunks = vec![
        github_issue("1", "Bug", &["bug"], vec!["src/a.rs", "src/b.rs"]),
        github_pr("10", "Fix", vec!["src/a.rs"]),
    ];

    let artifacts = consolidate(&chunks);
    let summary = artifacts.summary();

    assert_eq!(summary.chunks_indexed, 2);
    assert!(summary.edges_created > 0);
    assert!(summary.facts_extracted > 0);
    assert_eq!(summary.cache_entries_stored, 2);
}

#[test]
fn consolidation_with_only_code_chunks_is_noop() {
    let code = ContentChunk::from(lean_ctx::core::bm25_index::CodeChunk {
        file_path: "src/main.rs".into(),
        symbol_name: "main".into(),
        kind: ChunkKind::Function,
        start_line: 1,
        end_line: 5,
        content: "fn main() {}".into(),
        tokens: vec![],
        token_count: 0,
    });

    let artifacts = consolidate(&[code]);
    assert!(artifacts.edges.is_empty());
    assert!(artifacts.facts.is_empty());
    assert!(artifacts.cache_entries.is_empty());
}

#[test]
fn bm25_search_finds_consolidated_provider_data() {
    let chunks = vec![github_issue(
        "42",
        "Authentication token expired",
        &["bug"],
        vec!["src/auth.rs"],
    )];

    let artifacts = consolidate(&chunks);
    let mut index = BM25Index {
        chunks: Vec::new(),
        inverted: std::collections::HashMap::new(),
        avg_doc_len: 0.0,
        doc_count: 0,
        doc_freqs: std::collections::HashMap::new(),
        files: std::collections::HashMap::new(),
        content_truncated: false,
    };

    apply_artifacts(&artifacts, Some(&mut index), None, None);

    let results = index.search("authentication token", 5);
    assert!(!results.is_empty());
    assert!(results[0].file_path.contains("github://issues/42"));
}
