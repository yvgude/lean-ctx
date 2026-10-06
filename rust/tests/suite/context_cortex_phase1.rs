//! Phase 1: Cortical Column Infrastructure — Integration Tests
//!
//! Verifies the full Context Engine Phase 1 implementation:
//!   1. `ContentChunk` ↔ `CodeChunk` bidirectional conversion
//!   2. `ContentSource` serialization and tagging
//!   3. BM25 cross-source ingest pipeline
//!   4. Provider Registry lifecycle (register, discover, execute)
//!   5. Config-driven provider activation
//!   6. File reference extraction from freeform text
//!   7. `ChunkKind` extensions for external sources

use lean_ctx::core::bm25_index::{BM25Index, ChunkKind, CodeChunk};
use lean_ctx::core::content_chunk::{ContentChunk, ContentSource, extract_file_references};
use lean_ctx::core::providers::registry::{ProviderRegistry, global_registry, result_to_chunks};
use lean_ctx::core::providers::{ContextProvider, ProviderItem, ProviderParams, ProviderResult};
use std::sync::Arc;

#[test]
fn content_chunk_from_provider_has_correct_uri_scheme() {
    let cc = ContentChunk::from_provider(
        "github",
        "issues",
        "42",
        "Bug in auth",
        ChunkKind::Issue,
        "Token expiry broken".into(),
        vec!["src/auth.rs".into()],
        Some(serde_json::json!({"priority": "high"})),
    );

    assert_eq!(cc.file_path, "github://issues/42");
    assert_eq!(cc.symbol_name, "Bug in auth");
    assert_eq!(cc.kind, ChunkKind::Issue);
    assert!(cc.is_external());
    assert_eq!(cc.provider_id(), Some("github"));
    assert!(!cc.tokens.is_empty());
    assert!(cc.token_count > 0);
    assert_eq!(cc.references, vec!["src/auth.rs"]);
    assert!(cc.metadata.is_some());
}

#[test]
fn content_chunk_to_code_chunk_drops_extra_fields() {
    let cc = ContentChunk::from_provider(
        "jira",
        "tickets",
        "PROJ-100",
        "Perf regression",
        ChunkKind::Ticket,
        "Response time increased 3x".into(),
        vec!["src/handler.rs".into()],
        Some(serde_json::json!({"severity": "P1"})),
    );

    let code: CodeChunk = cc.into();
    assert_eq!(code.file_path, "jira://tickets/PROJ-100");
    assert_eq!(code.kind, ChunkKind::Ticket);
    assert!(code.token_count > 0);
}

#[test]
fn code_chunk_to_content_chunk_defaults_to_file_source() {
    let code = CodeChunk {
        file_path: "src/main.rs".into(),
        symbol_name: "main".into(),
        kind: ChunkKind::Function,
        start_line: 1,
        end_line: 10,
        content: "fn main() { println!(\"hello\"); }".into(),
        tokens: vec!["main".into(), "println".into()],
        token_count: 2,
    };

    let cc: ContentChunk = code.into();
    assert_eq!(cc.source, ContentSource::File);
    assert!(!cc.is_external());
    assert!(cc.references.is_empty());
    assert!(cc.metadata.is_none());
}

#[test]
fn content_source_file_serializes_correctly() {
    let src = ContentSource::File;
    let json = serde_json::to_string(&src).unwrap();
    assert!(json.contains("\"type\":\"file\""));

    let roundtrip: ContentSource = serde_json::from_str(&json).unwrap();
    assert_eq!(roundtrip, ContentSource::File);
}

#[test]
fn content_source_provider_serializes_with_all_fields() {
    let src = ContentSource::Provider {
        provider_id: "github".into(),
        resource_type: "pull_requests".into(),
    };
    let json = serde_json::to_string(&src).unwrap();
    assert!(json.contains("\"type\":\"provider\""));
    assert!(json.contains("\"provider_id\":\"github\""));
    assert!(json.contains("\"resource_type\":\"pull_requests\""));

    let roundtrip: ContentSource = serde_json::from_str(&json).unwrap();
    assert_eq!(roundtrip, src);
}

#[test]
fn content_source_shell_roundtrips() {
    let src = ContentSource::Shell {
        command: "cargo test".into(),
    };
    let json = serde_json::to_string(&src).unwrap();
    let roundtrip: ContentSource = serde_json::from_str(&json).unwrap();
    assert_eq!(roundtrip, src);
}

#[test]
fn content_source_knowledge_roundtrips() {
    let src = ContentSource::Knowledge {
        category: "architecture".into(),
    };
    let json = serde_json::to_string(&src).unwrap();
    let roundtrip: ContentSource = serde_json::from_str(&json).unwrap();
    assert_eq!(roundtrip, src);
}

#[test]
fn bm25_ingest_content_chunks_increases_doc_count() {
    let mut index = BM25Index {
        chunks: Vec::new(),
        inverted: std::collections::HashMap::new(),
        avg_doc_len: 0.0,
        doc_count: 0,
        doc_freqs: std::collections::HashMap::new(),
        files: std::collections::HashMap::new(),
        dirs: std::collections::HashMap::new(),
        content_truncated: false,
        admission_policy: None,
    };

    let chunks = vec![
        ContentChunk::from_provider(
            "github",
            "issues",
            "1",
            "Auth bug",
            ChunkKind::Issue,
            "Authentication token expires too early in production".into(),
            vec!["src/auth.rs".into()],
            None,
        ),
        ContentChunk::from_provider(
            "github",
            "issues",
            "2",
            "DB timeout",
            ChunkKind::Issue,
            "PostgreSQL connection pool exhausted under load".into(),
            vec!["src/db/pool.rs".into()],
            None,
        ),
    ];

    let ingested = index.ingest_content_chunks(chunks);
    assert_eq!(ingested, 2);
    assert_eq!(index.doc_count, 2);
    assert_eq!(index.chunks.len(), 2);
    assert!(index.avg_doc_len > 0.0);
}

#[test]
fn bm25_search_finds_ingested_provider_chunks() {
    let mut index = BM25Index {
        chunks: Vec::new(),
        inverted: std::collections::HashMap::new(),
        avg_doc_len: 0.0,
        doc_count: 0,
        doc_freqs: std::collections::HashMap::new(),
        files: std::collections::HashMap::new(),
        dirs: std::collections::HashMap::new(),
        content_truncated: false,
        admission_policy: None,
    };

    index.ingest_content_chunks(vec![
        ContentChunk::from_provider(
            "github",
            "issues",
            "42",
            "Token expiry bug",
            ChunkKind::Issue,
            "JWT authentication token expires after 30 minutes instead of 24 hours".into(),
            vec![],
            None,
        ),
        ContentChunk::from_provider(
            "github",
            "issues",
            "43",
            "CSS layout broken",
            ChunkKind::Issue,
            "The sidebar CSS flexbox layout is broken on mobile screens".into(),
            vec![],
            None,
        ),
    ]);

    let results = index.search("authentication token expires", 5);
    assert!(!results.is_empty());
    assert_eq!(results[0].file_path, "github://issues/42");
}

#[test]
fn bm25_mixed_code_and_provider_chunks() {
    let mut index = BM25Index {
        chunks: Vec::new(),
        inverted: std::collections::HashMap::new(),
        avg_doc_len: 0.0,
        doc_count: 0,
        doc_freqs: std::collections::HashMap::new(),
        files: std::collections::HashMap::new(),
        dirs: std::collections::HashMap::new(),
        content_truncated: false,
        admission_policy: None,
    };

    index.ingest_content_chunks(vec![
        ContentChunk::from(CodeChunk {
            file_path: "src/auth.rs".into(),
            symbol_name: "validate_token".into(),
            kind: ChunkKind::Function,
            start_line: 10,
            end_line: 30,
            content: "fn validate_token(jwt: &str) -> Result<Claims, AuthError> { check_jwt_expiry(jwt) }".into(),
            tokens: vec![],
            token_count: 0,
        }),
        ContentChunk::from_provider(
            "github",
            "issues",
            "42",
            "Token validation fails",
            ChunkKind::Issue,
            "validate_token returns AuthError for valid tokens".into(),
            vec!["src/auth.rs".into()],
            None,
        ),
    ]);

    assert_eq!(index.chunks.len(), 2);
    assert_eq!(index.external_chunk_count(), 1);

    let has_code = index.chunks.iter().any(|c| c.file_path == "src/auth.rs");
    let has_issue = index
        .chunks
        .iter()
        .any(|c| c.file_path.contains("github://"));
    assert!(has_code);
    assert!(has_issue);
}

#[test]
fn bm25_external_chunk_count_accurate() {
    let mut index = BM25Index {
        chunks: Vec::new(),
        inverted: std::collections::HashMap::new(),
        avg_doc_len: 0.0,
        doc_count: 0,
        doc_freqs: std::collections::HashMap::new(),
        files: std::collections::HashMap::new(),
        dirs: std::collections::HashMap::new(),
        content_truncated: false,
        admission_policy: None,
    };

    index.ingest_content_chunks(vec![
        ContentChunk::from(CodeChunk {
            file_path: "src/lib.rs".into(),
            symbol_name: "lib".into(),
            kind: ChunkKind::Module,
            start_line: 1,
            end_line: 5,
            content: "pub mod core;".into(),
            tokens: vec![],
            token_count: 0,
        }),
        ContentChunk::from_provider(
            "github",
            "issues",
            "1",
            "Issue 1",
            ChunkKind::Issue,
            "body".into(),
            vec![],
            None,
        ),
        ContentChunk::from_provider(
            "jira",
            "tickets",
            "PROJ-1",
            "Ticket 1",
            ChunkKind::Ticket,
            "body".into(),
            vec![],
            None,
        ),
    ]);

    assert_eq!(index.external_chunk_count(), 2);
    assert_eq!(index.doc_count, 3);
}

#[test]
fn bm25_ingest_zero_chunks_is_noop() {
    let mut index = BM25Index {
        chunks: Vec::new(),
        inverted: std::collections::HashMap::new(),
        avg_doc_len: 0.0,
        doc_count: 0,
        doc_freqs: std::collections::HashMap::new(),
        files: std::collections::HashMap::new(),
        dirs: std::collections::HashMap::new(),
        content_truncated: false,
        admission_policy: None,
    };

    let ingested = index.ingest_content_chunks(Vec::<ContentChunk>::new());
    assert_eq!(ingested, 0);
    assert_eq!(index.doc_count, 0);
}

struct MockProvider {
    available: bool,
}

impl ContextProvider for MockProvider {
    fn id(&self) -> &'static str {
        "mock_test"
    }
    fn display_name(&self) -> &'static str {
        "Mock Test Provider"
    }
    fn supported_actions(&self) -> &[&str] {
        &["issues", "pull_requests"]
    }
    fn execute(&self, action: &str, params: &ProviderParams) -> Result<ProviderResult, String> {
        let limit = params.limit.unwrap_or(5);
        let state_filter = params.state.as_deref().unwrap_or("open");
        Ok(ProviderResult {
            provider: "mock_test".into(),
            resource_type: action.into(),
            items: (1..=limit)
                .map(|i| ProviderItem {
                    id: i.to_string(),
                    title: format!("Mock {action} #{i}"),
                    state: Some(state_filter.into()),
                    author: Some("tester".into()),
                    created_at: None,
                    updated_at: None,
                    url: Some(format!("https://example.com/{action}/{i}")),
                    labels: vec!["test".into()],
                    body: Some(format!("Body of {action} #{i} referencing src/main.rs")),
                    claims: vec![],
                })
                .collect(),
            total_count: Some(limit),
            truncated: false,
        })
    }
    fn is_available(&self) -> bool {
        self.available
    }
}

#[test]
fn registry_register_and_get() {
    let reg = ProviderRegistry::new();
    reg.register(Arc::new(MockProvider { available: true }));

    assert!(reg.get("mock_test").is_some());
    assert!(reg.get("nonexistent").is_none());
    assert_eq!(reg.provider_count(), 1);
}

#[test]
fn registry_execute_returns_result() {
    let reg = ProviderRegistry::new();
    reg.register(Arc::new(MockProvider { available: true }));

    let result = reg
        .execute(
            "mock_test",
            "issues",
            &ProviderParams {
                limit: Some(3),
                ..Default::default()
            },
        )
        .unwrap();

    assert_eq!(result.provider, "mock_test");
    assert_eq!(result.resource_type, "issues");
    assert_eq!(result.items.len(), 3);
}

#[test]
fn registry_execute_unavailable_provider_errors() {
    let reg = ProviderRegistry::new();
    reg.register(Arc::new(MockProvider { available: false }));

    let err = reg
        .execute("mock_test", "issues", &ProviderParams::default())
        .unwrap_err();
    assert!(err.contains("not available"));
}

#[test]
fn registry_execute_unsupported_action_errors() {
    let reg = ProviderRegistry::new();
    reg.register(Arc::new(MockProvider { available: true }));

    let err = reg
        .execute("mock_test", "wikis", &ProviderParams::default())
        .unwrap_err();
    assert!(err.contains("does not support"));
}

#[test]
fn registry_execute_unknown_provider_errors() {
    let reg = ProviderRegistry::new();
    let err = reg
        .execute("ghost", "issues", &ProviderParams::default())
        .unwrap_err();
    assert!(err.contains("not registered"));
}

#[test]
fn registry_discover_lists_all_providers() {
    let reg = ProviderRegistry::new();
    reg.register(Arc::new(MockProvider { available: true }));

    let infos = reg.discover();
    assert_eq!(infos.len(), 1);
    assert_eq!(infos[0].id, "mock_test");
    assert!(infos[0].available);
    assert!(infos[0].actions.contains(&"issues".to_string()));
}

#[test]
fn registry_available_provider_ids_filters_unavailable() {
    let reg = ProviderRegistry::new();
    reg.register(Arc::new(MockProvider { available: false }));

    assert!(reg.available_provider_ids().is_empty());
    assert_eq!(reg.provider_count(), 1);
}

#[test]
fn registry_execute_as_chunks_produces_content_chunks() {
    let reg = ProviderRegistry::new();
    reg.register(Arc::new(MockProvider { available: true }));

    let chunks = reg
        .execute_as_chunks(
            "mock_test",
            "issues",
            &ProviderParams {
                limit: Some(2),
                ..Default::default()
            },
        )
        .unwrap();

    assert_eq!(chunks.len(), 2);
    assert!(chunks[0].is_external());
    assert_eq!(chunks[0].provider_id(), Some("mock_test"));
    assert_eq!(chunks[0].kind, ChunkKind::Issue);
    assert!(chunks[0].file_path.contains("mock_test://issues/"));
}

#[test]
fn result_to_chunks_maps_all_resource_types() {
    let types_and_kinds = vec![
        ("issues", ChunkKind::Issue),
        ("pull_requests", ChunkKind::PullRequest),
        ("merge_requests", ChunkKind::PullRequest),
        ("wikis", ChunkKind::WikiPage),
        ("schemas", ChunkKind::DbSchema),
        ("endpoints", ChunkKind::ApiEndpoint),
        ("tickets", ChunkKind::Ticket),
        ("unknown_type", ChunkKind::ExternalOther),
    ];

    for (resource_type, expected_kind) in types_and_kinds {
        let result = ProviderResult {
            provider: "test".into(),
            resource_type: resource_type.into(),
            items: vec![ProviderItem {
                id: "1".into(),
                title: "Test".into(),
                state: Some("open".into()),
                author: None,
                created_at: None,
                updated_at: None,
                url: None,
                labels: vec![],
                body: None,
                claims: vec![],
            }],
            total_count: Some(1),
            truncated: false,
        };

        let chunks = result_to_chunks(&result);
        assert_eq!(
            chunks[0].kind, expected_kind,
            "resource_type '{resource_type}' should map to {expected_kind:?}"
        );
    }
}

#[test]
fn result_to_chunks_extracts_file_references_from_body() {
    let result = ProviderResult {
        provider: "github".into(),
        resource_type: "issues".into(),
        items: vec![ProviderItem {
            id: "99".into(),
            title: "Bug in auth module".into(),
            state: Some("open".into()),
            author: Some("dev".into()),
            created_at: None,
            updated_at: None,
            url: None,
            labels: vec![],
            body: Some("The bug is in src/auth/handler.rs and affects lib/utils.ts.".into()),
            claims: vec![],
        }],
        total_count: Some(1),
        truncated: false,
    };

    let chunks = result_to_chunks(&result);
    assert!(
        chunks[0]
            .references
            .contains(&"src/auth/handler.rs".to_string())
    );
    assert!(chunks[0].references.contains(&"lib/utils.ts".to_string()));
}

#[test]
fn result_to_chunks_preserves_metadata() {
    let result = ProviderResult {
        provider: "github".into(),
        resource_type: "issues".into(),
        items: vec![ProviderItem {
            id: "1".into(),
            title: "Test".into(),
            state: Some("closed".into()),
            author: Some("user1".into()),
            created_at: Some("2026-01-01".into()),
            updated_at: Some("2026-01-02".into()),
            url: Some("https://github.com/o/r/issues/1".into()),
            labels: vec!["bug".into(), "p1".into()],
            body: None,
            claims: vec![],
        }],
        total_count: Some(1),
        truncated: false,
    };

    let chunks = result_to_chunks(&result);
    let meta = chunks[0].metadata.as_ref().unwrap();
    assert_eq!(meta["state"], "closed");
    assert_eq!(meta["author"], "user1");
    assert_eq!(meta["labels"][0], "bug");
    assert_eq!(meta["labels"][1], "p1");
}

#[test]
fn extract_refs_handles_backtick_paths() {
    let text = "Check `src/auth/handler.rs` for the fix";
    let refs = extract_file_references(text);
    assert!(refs.contains(&"src/auth/handler.rs".to_string()));
}

#[test]
fn extract_refs_handles_bracket_paths() {
    let text = "See [src/config/mod.rs] for config schema";
    let refs = extract_file_references(text);
    assert!(refs.contains(&"src/config/mod.rs".to_string()));
}

#[test]
fn extract_refs_ignores_email_addresses() {
    let text = "Contact user@example.com/foo.rs";
    let refs = extract_file_references(text);
    assert!(refs.is_empty());
}

#[test]
fn extract_refs_handles_multiple_extensions() {
    let text = "Files: src/app.tsx and lib/handler.go and tests/main_test.py";
    let refs = extract_file_references(text);
    assert!(refs.contains(&"src/app.tsx".to_string()));
    assert!(refs.contains(&"lib/handler.go".to_string()));
    assert!(refs.contains(&"tests/main_test.py".to_string()));
}

#[test]
fn chunk_kind_serialization_roundtrip() {
    let kinds = vec![
        ChunkKind::Issue,
        ChunkKind::PullRequest,
        ChunkKind::WikiPage,
        ChunkKind::DbSchema,
        ChunkKind::ApiEndpoint,
        ChunkKind::Ticket,
        ChunkKind::ExternalOther,
    ];

    for kind in kinds {
        let json = serde_json::to_string(&kind).unwrap();
        let roundtrip: ChunkKind = serde_json::from_str(&json).unwrap();
        assert_eq!(roundtrip, kind, "ChunkKind {kind:?} should roundtrip");
    }
}

#[test]
fn providers_config_defaults_are_enabled() {
    let cfg = lean_ctx::core::config::ProvidersConfig::default();
    assert!(cfg.enabled);
    assert!(cfg.github.enabled);
    assert!(cfg.gitlab.enabled);
    assert!(cfg.auto_index);
    assert_eq!(cfg.cache_ttl_secs, 120);
}

#[test]
fn providers_config_deserializes_from_toml() {
    let toml = r#"
        enabled = true
        auto_index = true
        cache_ttl_secs = 60

        [github]
        enabled = false

        [gitlab]
        enabled = true
        api_url = "https://gitlab.internal.com"
    "#;

    let cfg: lean_ctx::core::config::ProvidersConfig = toml::from_str(toml).unwrap();
    assert!(cfg.enabled);
    assert!(cfg.auto_index);
    assert_eq!(cfg.cache_ttl_secs, 60);
    assert!(!cfg.github.enabled);
    assert!(cfg.gitlab.enabled);
    assert_eq!(
        cfg.gitlab.api_url.as_deref(),
        Some("https://gitlab.internal.com")
    );
}

#[test]
fn end_to_end_provider_to_bm25_search() {
    let reg = ProviderRegistry::new();
    reg.register(Arc::new(MockProvider { available: true }));

    let chunks = reg
        .execute_as_chunks(
            "mock_test",
            "issues",
            &ProviderParams {
                limit: Some(5),
                ..Default::default()
            },
        )
        .unwrap();

    let mut index = BM25Index {
        chunks: Vec::new(),
        inverted: std::collections::HashMap::new(),
        avg_doc_len: 0.0,
        doc_count: 0,
        doc_freqs: std::collections::HashMap::new(),
        files: std::collections::HashMap::new(),
        dirs: std::collections::HashMap::new(),
        content_truncated: false,
        admission_policy: None,
    };

    let ingested = index.ingest_content_chunks(chunks);
    assert_eq!(ingested, 5);

    let results = index.search("Mock issues", 10);
    assert!(!results.is_empty());

    for result in &results {
        assert!(result.file_path.starts_with("mock_test://"));
    }
}

#[test]
fn global_registry_is_singleton() {
    let r1 = global_registry();
    let r2 = global_registry();
    assert!(std::ptr::eq(r1, r2));
}
