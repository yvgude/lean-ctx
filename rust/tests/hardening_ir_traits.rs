//! Integration tests for the hardening sprint:
//! - Context IR hot-path recording
//! - CompressionPattern trait interface
//! - ContextProvider trait interface
//! - CONTRACTS.md machine-checked KV block integrity

// =============================================================================
// Context IR: Hot-Path Recording
// =============================================================================

mod context_ir_hotpath {
    use lean_ctx::core::context_ir::{ContextIrSourceKindV1, ContextIrV1, RecordIrInput};
    use std::time::Duration;

    #[test]
    fn record_populates_all_fields() {
        let mut ir = ContextIrV1::new();
        assert_eq!(ir.items.len(), 0);
        assert_eq!(ir.next_seq, 1);

        ir.record(RecordIrInput {
            kind: ContextIrSourceKindV1::Read,
            tool: "ctx_read",
            client_name: Some("cursor".to_string()),
            agent_id: Some("agent_001".to_string()),
            path: Some("src/main.rs"),
            command: None,
            pattern: Some("full"),
            input_tokens: 1000,
            output_tokens: 200,
            duration: Duration::from_millis(42),
            content_excerpt: "fn main() { ... }",
        });

        assert_eq!(ir.items.len(), 1);
        assert_eq!(ir.next_seq, 2);

        let item = &ir.items[0];
        assert_eq!(item.seq, 1);
        assert_eq!(item.source.tool, "ctx_read");
        assert_eq!(item.input_tokens, 1000);
        assert_eq!(item.output_tokens, 200);
        assert!(item.duration_us > 0);
        assert!(item.compression_ratio < 1.0);
        assert!(!item.content_excerpt.is_empty());
    }

    #[test]
    fn record_shell_tool_with_command() {
        let mut ir = ContextIrV1::new();

        ir.record(RecordIrInput {
            kind: ContextIrSourceKindV1::Shell,
            tool: "ctx_shell",
            client_name: None,
            agent_id: None,
            path: None,
            command: Some("cargo test --lib"),
            pattern: None,
            input_tokens: 5000,
            output_tokens: 800,
            duration: Duration::from_millis(3200),
            content_excerpt: "test result: ok. 42 passed",
        });

        let item = &ir.items[0];
        assert!(matches!(item.source.kind, ContextIrSourceKindV1::Shell));
        assert!(item.source.command.is_some());
        assert_eq!(item.duration_us, 3_200_000);
    }

    #[test]
    fn record_search_tool() {
        let mut ir = ContextIrV1::new();

        ir.record(RecordIrInput {
            kind: ContextIrSourceKindV1::Search,
            tool: "ctx_search",
            client_name: None,
            agent_id: None,
            path: Some("rust/src/"),
            command: None,
            pattern: Some("fn compress"),
            input_tokens: 2000,
            output_tokens: 300,
            duration: Duration::from_millis(15),
            content_excerpt: "3 matches in 2 files",
        });

        let item = &ir.items[0];
        assert!(matches!(item.source.kind, ContextIrSourceKindV1::Search));
        assert_eq!(item.source.pattern.as_deref(), Some("fn compress"));
    }

    #[test]
    fn totals_accumulate_correctly() {
        let mut ir = ContextIrV1::new();

        for i in 0..5 {
            ir.record(RecordIrInput {
                kind: ContextIrSourceKindV1::Read,
                tool: "ctx_read",
                client_name: None,
                agent_id: None,
                path: Some("file.rs"),
                command: None,
                pattern: None,
                input_tokens: 1000 * (i + 1),
                output_tokens: 200 * (i + 1),
                duration: Duration::from_millis(10),
                content_excerpt: "x",
            });
        }

        assert_eq!(ir.items.len(), 5);
        assert_eq!(ir.totals.items_recorded, 5);
        assert_eq!(ir.totals.input_tokens, 1000 + 2000 + 3000 + 4000 + 5000);
        assert_eq!(ir.totals.output_tokens, 200 + 400 + 600 + 800 + 1000);
        assert_eq!(
            ir.totals.tokens_saved,
            (1000 - 200) + (2000 - 400) + (3000 - 600) + (4000 - 800) + (5000 - 1000)
        );
    }

    #[test]
    fn prune_enforces_max_items_bound() {
        let mut ir = ContextIrV1::new();

        for i in 0..200 {
            ir.record(RecordIrInput {
                kind: ContextIrSourceKindV1::Other,
                tool: "ctx_control",
                client_name: None,
                agent_id: None,
                path: None,
                command: None,
                pattern: None,
                input_tokens: 10,
                output_tokens: 5,
                duration: Duration::from_micros(100),
                content_excerpt: &format!("item_{i}"),
            });
        }

        // MAX_ITEMS = 128
        assert!(ir.items.len() <= 128);
        assert_eq!(ir.totals.items_recorded, 200);
        // Oldest items were pruned, newest remain
        assert!(ir.items.last().unwrap().seq == 200);
    }

    #[test]
    fn compression_ratio_edge_cases() {
        let mut ir = ContextIrV1::new();

        // Zero input tokens -> ratio 1.0
        ir.record(RecordIrInput {
            kind: ContextIrSourceKindV1::Other,
            tool: "ctx_status",
            client_name: None,
            agent_id: None,
            path: None,
            command: None,
            pattern: None,
            input_tokens: 0,
            output_tokens: 50,
            duration: Duration::from_millis(1),
            content_excerpt: "status ok",
        });
        assert_eq!(ir.items[0].compression_ratio, 1.0);

        // Equal tokens -> ratio 1.0
        ir.record(RecordIrInput {
            kind: ContextIrSourceKindV1::Read,
            tool: "ctx_read",
            client_name: None,
            agent_id: None,
            path: Some("x.rs"),
            command: None,
            pattern: None,
            input_tokens: 500,
            output_tokens: 500,
            duration: Duration::from_millis(1),
            content_excerpt: "no compression",
        });
        assert_eq!(ir.items[1].compression_ratio, 1.0);
    }

    #[test]
    fn save_and_load_roundtrip() {
        let mut ir = ContextIrV1::new();
        ir.record(RecordIrInput {
            kind: ContextIrSourceKindV1::Read,
            tool: "ctx_read",
            client_name: Some("test".to_string()),
            agent_id: None,
            path: Some("roundtrip.rs"),
            command: None,
            pattern: Some("map"),
            input_tokens: 4000,
            output_tokens: 800,
            duration: Duration::from_millis(25),
            content_excerpt: "pub struct Foo {}",
        });

        // Serialize then deserialize
        let json = serde_json::to_string(&ir).unwrap();
        let loaded: ContextIrV1 = serde_json::from_str(&json).unwrap();

        assert_eq!(loaded.items.len(), 1);
        assert_eq!(loaded.totals.input_tokens, 4000);
        assert_eq!(loaded.totals.output_tokens, 800);
        assert_eq!(loaded.items[0].source.tool, "ctx_read");
    }
}

// =============================================================================
// CompressionPattern Trait Interface
// =============================================================================

mod compression_pattern_trait {
    use lean_ctx::core::patterns::{CompressionPattern, CompressionResult};

    struct MockCargoPattern;

    impl CompressionPattern for MockCargoPattern {
        fn name(&self) -> &'static str {
            "cargo"
        }

        fn version(&self) -> u32 {
            1
        }

        fn matches(&self, command: &str) -> bool {
            let cl = command.to_ascii_lowercase();
            cl.starts_with("cargo ")
        }

        fn compress(&self, _command: &str, output: &str) -> Option<String> {
            let lines: Vec<&str> = output.lines().collect();
            if lines.len() > 5 {
                Some(format!("[cargo: {} lines compressed]", lines.len()))
            } else {
                None
            }
        }

        fn prefixes(&self) -> &[&str] {
            &["cargo "]
        }
    }

    #[test]
    fn trait_matches_correct_commands() {
        let pattern = MockCargoPattern;
        assert!(pattern.matches("cargo test"));
        assert!(pattern.matches("cargo build --release"));
        assert!(pattern.matches("CARGO clippy")); // case insensitive
        assert!(!pattern.matches("npm test"));
        assert!(!pattern.matches("git status"));
    }

    #[test]
    fn trait_compress_returns_shorter_output() {
        let pattern = MockCargoPattern;
        let long_output = "line1\nline2\nline3\nline4\nline5\nline6\nline7\n";
        let result = pattern.compress("cargo test", long_output);
        assert!(result.is_some());
        assert!(result.unwrap().len() < long_output.len());
    }

    #[test]
    fn trait_compress_returns_none_for_short_output() {
        let pattern = MockCargoPattern;
        let short_output = "ok";
        let result = pattern.compress("cargo test", short_output);
        assert!(result.is_none());
    }

    #[test]
    fn trait_default_version() {
        struct MinimalPattern;
        impl CompressionPattern for MinimalPattern {
            fn name(&self) -> &'static str {
                "minimal"
            }
            fn matches(&self, _cmd: &str) -> bool {
                true
            }
            fn compress(&self, _cmd: &str, _output: &str) -> Option<String> {
                None
            }
            fn prefixes(&self) -> &[&str] {
                &["*"]
            }
        }
        let p = MinimalPattern;
        assert_eq!(p.version(), 1); // default
    }

    #[test]
    fn compression_result_struct_usage() {
        let result = CompressionResult {
            pattern_name: "docker".to_string(),
            pattern_version: 2,
            input_tokens: 5000,
            output_tokens: 800,
            compressed: "[docker: 3 services, 2 volumes]".to_string(),
        };
        assert_eq!(result.pattern_name, "docker");
        assert!(result.output_tokens < result.input_tokens);
    }

    #[test]
    fn multiple_patterns_dispatch() {
        struct GitPattern;
        impl CompressionPattern for GitPattern {
            fn name(&self) -> &'static str {
                "git"
            }
            fn matches(&self, cmd: &str) -> bool {
                cmd.to_ascii_lowercase().starts_with("git ")
            }
            fn compress(&self, _cmd: &str, output: &str) -> Option<String> {
                Some(format!("[git: {} lines]", output.lines().count()))
            }
            fn prefixes(&self) -> &[&str] {
                &["git "]
            }
        }

        let patterns: Vec<Box<dyn CompressionPattern>> =
            vec![Box::new(MockCargoPattern), Box::new(GitPattern)];

        let cmd = "git status";
        let output = "On branch main\nnothing to commit";

        let matched = patterns.iter().find(|p| p.matches(cmd));
        assert!(matched.is_some());
        assert_eq!(matched.unwrap().name(), "git");

        let compressed = matched.unwrap().compress(cmd, output);
        assert!(compressed.is_some());
    }
}

// =============================================================================
// ContextProvider Trait Interface
// =============================================================================

mod context_provider_trait {
    use lean_ctx::core::providers::{
        ContextPacket, ContextProvider, ProviderItem, ProviderParams, ProviderResult,
    };

    struct MockGitLabProvider {
        available: bool,
    }

    impl ContextProvider for MockGitLabProvider {
        fn id(&self) -> &'static str {
            "gitlab"
        }

        fn display_name(&self) -> &'static str {
            "GitLab"
        }

        fn supported_actions(&self) -> &[&str] {
            &["issues", "mrs", "pipelines"]
        }

        fn execute(&self, action: &str, params: &ProviderParams) -> Result<ProviderResult, String> {
            if !self.available {
                return Err("GitLab token not configured".to_string());
            }
            match action {
                "issues" => Ok(ProviderResult {
                    provider: "gitlab".to_string(),
                    resource_type: "issues".to_string(),
                    items: vec![ProviderItem {
                        id: "42".to_string(),
                        title: "Fix context leak".to_string(),
                        state: Some("opened".to_string()),
                        author: Some("dev".to_string()),
                        created_at: Some("2026-05-19".to_string()),
                        updated_at: None,
                        url: Some("https://gitlab.com/issues/42".to_string()),
                        labels: vec!["bug".to_string()],
                        body: params.query.clone(),
                        claims: vec![],
                    }],
                    total_count: Some(1),
                    truncated: false,
                }),
                _ => Err(format!("unsupported action: {action}")),
            }
        }

        fn cache_ttl_secs(&self) -> u64 {
            60
        }

        fn is_available(&self) -> bool {
            self.available
        }
    }

    #[test]
    fn provider_reports_availability() {
        let available = MockGitLabProvider { available: true };
        let unavailable = MockGitLabProvider { available: false };

        assert!(available.is_available());
        assert!(!unavailable.is_available());
    }

    #[test]
    fn provider_execute_returns_structured_result() {
        let provider = MockGitLabProvider { available: true };
        let params = ProviderParams {
            project: Some("lean-ctx".to_string()),
            state: Some("opened".to_string()),
            limit: Some(10),
            query: None,
            id: None,
        };

        let result = provider.execute("issues", &params).unwrap();
        assert_eq!(result.provider, "gitlab");
        assert_eq!(result.items.len(), 1);
        assert_eq!(result.items[0].title, "Fix context leak");
        assert!(!result.truncated);
    }

    #[test]
    fn provider_execute_fails_when_unavailable() {
        let provider = MockGitLabProvider { available: false };
        let params = ProviderParams::default();

        let err = provider.execute("issues", &params).unwrap_err();
        assert!(err.contains("not configured"));
    }

    #[test]
    fn provider_execute_fails_on_unknown_action() {
        let provider = MockGitLabProvider { available: true };
        let params = ProviderParams::default();

        let err = provider.execute("unknown_action", &params).unwrap_err();
        assert!(err.contains("unsupported"));
    }

    #[test]
    fn provider_supported_actions_lists_capabilities() {
        let provider = MockGitLabProvider { available: true };
        let actions = provider.supported_actions();
        assert!(actions.contains(&"issues"));
        assert!(actions.contains(&"mrs"));
        assert!(actions.contains(&"pipelines"));
        assert!(!actions.contains(&"unknown"));
    }

    #[test]
    fn provider_default_cache_ttl() {
        struct MinimalProvider;
        impl ContextProvider for MinimalProvider {
            fn id(&self) -> &'static str {
                "test"
            }
            fn display_name(&self) -> &'static str {
                "Test"
            }
            fn supported_actions(&self) -> &[&str] {
                &[]
            }
            fn execute(&self, _: &str, _: &ProviderParams) -> Result<ProviderResult, String> {
                Err("not impl".to_string())
            }
            fn is_available(&self) -> bool {
                false
            }
        }
        let p = MinimalProvider;
        assert_eq!(p.cache_ttl_secs(), 120); // default from trait
        assert!(p.requires_auth()); // default from trait
    }

    #[test]
    fn context_packet_construction() {
        let packet = ContextPacket {
            provider_id: "gitlab".to_string(),
            action: "issues".to_string(),
            items: vec![ProviderItem {
                id: "1".to_string(),
                title: "test".to_string(),
                state: None,
                author: None,
                created_at: None,
                updated_at: None,
                url: None,
                labels: vec![],
                body: None,
                claims: vec![],
            }],
            token_count_raw: 500,
            token_count_compressed: 120,
            cache_hit: true,
        };

        assert_eq!(packet.provider_id, "gitlab");
        assert!(packet.cache_hit);
        assert!(packet.token_count_compressed < packet.token_count_raw);
    }

    #[test]
    fn provider_registry_dispatch() {
        let providers: Vec<Box<dyn ContextProvider>> =
            vec![Box::new(MockGitLabProvider { available: true })];

        let target_id = "gitlab";
        let found = providers.iter().find(|p| p.id() == target_id);
        assert!(found.is_some());
        assert_eq!(found.unwrap().display_name(), "GitLab");
    }
}

// =============================================================================
// Contracts KV Block Integrity
// =============================================================================

mod contracts_integrity {
    #[test]
    fn contracts_kv_block_parseable() {
        let content = include_str!("../../CONTRACTS.md");
        let begin_marker = "<!-- leanctx-contracts-kv:begin -->";
        let end_marker = "<!-- leanctx-contracts-kv:end -->";

        let start = content.find(begin_marker).expect("KV begin marker missing");
        let end = content.find(end_marker).expect("KV end marker missing");
        assert!(start < end);

        let kv_block = &content[start + begin_marker.len()..end];
        let lines: Vec<&str> = kv_block.lines().filter(|l| !l.trim().is_empty()).collect();

        assert!(
            lines.len() >= 15,
            "expected at least 15 contracts, found {}",
            lines.len()
        );

        for line in &lines {
            let parts: Vec<&str> = line.split('=').collect();
            assert_eq!(parts.len(), 2, "malformed KV line: {line}");
            assert!(
                parts[0].starts_with("leanctx.contract."),
                "KV key missing prefix: {line}"
            );
            let version: u32 = parts[1]
                .trim()
                .parse()
                .unwrap_or_else(|_| panic!("non-integer version in: {line}"));
            assert!(version >= 1, "version must be >= 1: {line}");
        }
    }

    #[test]
    fn contracts_has_protocol_family_structure() {
        let content = include_str!("../../CONTRACTS.md");

        assert!(
            content.contains("## Core Context Contracts"),
            "missing Core section"
        );
        assert!(
            content.contains("## Runtime Contracts"),
            "missing Runtime section"
        );
        assert!(
            content.contains("## Memory & Collaboration Contracts"),
            "missing Memory section"
        );
        assert!(
            content.contains("## Extension Contracts"),
            "missing Extension section"
        );
        assert!(
            content.contains("## Transport Contracts"),
            "missing Transport section"
        );
    }

    #[test]
    fn architecture_references_ir_in_hotpath() {
        let content = include_str!("../../ARCHITECTURE.md");

        assert!(
            content.contains("IRRecord"),
            "ARCHITECTURE.md should reference IR recording in flow"
        );
        // Derive the count from the registry SSOT so the doc check can never
        // drift again: adding/removing a tool updates this automatically.
        let expected = format!(
            "{} trait-based tools",
            lean_ctx::server::registry::tool_count()
        );
        assert!(
            content.contains(&expected),
            "ARCHITECTURE.md should reference the current registry count ({expected})"
        );
        assert!(
            !content.contains("pipeline_stages.rs"),
            "ARCHITECTURE.md should NOT reference non-existent pipeline_stages.rs"
        );
        assert!(
            !content.contains("DispatchRead"),
            "ARCHITECTURE.md should NOT reference non-existent DispatchRead"
        );
    }

    /// Guards the load-bearing invariant behind #348: in MCP stdio mode `stdout`
    /// is the JSON-RPC transport, so every diagnostic MUST go to `stderr`. A stray
    /// switch to stdout would silently corrupt every MCP client's protocol stream.
    /// `tracing-subscriber` does not expose the runtime writer, so we assert on the
    /// logging single-source-of-truth instead.
    #[test]
    fn mcp_logging_targets_stderr_never_stdout() {
        let logging = include_str!("../src/core/logging.rs");
        assert!(
            logging.matches("with_writer(std::io::stderr)").count() >= 2,
            "both init_logging and init_mcp_logging must pin the tracing writer to stderr"
        );
        assert!(
            !logging.contains("std::io::stdout"),
            "logging must never route tracing to stdout — it corrupts the MCP JSON-RPC channel (#348)"
        );
    }
}
