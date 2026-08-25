//! End-to-end conformance tests for the MCP-to-kernel bridge.

#[cfg(test)]
mod tests {
    use std::sync::{Mutex, MutexGuard};

    use super::super::client_wiring::OptimizationLevel;
    use super::super::coverage_class::CoverageClass;
    use super::super::mcp_bridge::{self, McpCallData, McpClientInfo};
    use super::super::mcp_coverage;
    use super::super::mcp_schema_opt::{self, SchemaBudget, SchemaEntry};

    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn isolated_test() -> MutexGuard<'static, ()> {
        let guard = match TEST_LOCK.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        mcp_bridge::reset_mcp_state();
        guard
    }

    fn cursor() -> McpClientInfo {
        McpClientInfo {
            client_name: "cursor".to_owned(),
            supports_roots: true,
            supports_sampling: true,
            tool_count: 15,
        }
    }

    fn call(tool: &str, output_tokens: usize) -> McpCallData {
        McpCallData {
            tool_name: tool.to_owned(),
            input_tokens: 1_000,
            output_tokens,
            is_retry: false,
            call_number: 1,
        }
    }

    #[test]
    fn full_mcp_lifecycle() {
        let _guard = isolated_test();
        let context = mcp_bridge::process_mcp_context(&cursor());
        assert_eq!(context.coverage, CoverageClass::FullInline);

        for output_tokens in [100, 125, 150] {
            mcp_bridge::record_mcp_call(&call("ctx_read", output_tokens));
        }

        assert_eq!(mcp_bridge::mcp_etpao(), 0.0);
        let summary = mcp_bridge::mcp_summary();
        assert_eq!(summary.total_calls, 3);
        assert_eq!(summary.total_output_tokens, 375);
    }

    #[test]
    fn schema_compression_saves_tokens() {
        let _guard = isolated_test();
        let schemas: Vec<SchemaEntry> = (0..10)
            .map(|index| SchemaEntry {
                name: format!("tool-{index}"),
                description: "x".repeat(2000),
                param_count: 5,
                estimated_tokens: 600,
                essential: false,
            })
            .collect();
        let budget = SchemaBudget {
            max_total_tokens: 3_000,
            max_per_tool_tokens: 200,
        };

        let result = mcp_schema_opt::optimize_schemas(&schemas, &budget);

        assert!(result.tokens_after < result.tokens_before);
        assert!(result.compressed_count > 0);
    }

    #[test]
    fn cursor_full_pipeline() {
        let _guard = isolated_test();
        assert_eq!(
            mcp_coverage::detect_mcp_coverage("cursor", true, true),
            CoverageClass::FullInline
        );
        assert_eq!(
            mcp_coverage::mcp_client_profile("cursor").context_window,
            200_000
        );
        assert_eq!(
            mcp_coverage::mcp_optimization_level("cursor"),
            OptimizationLevel::Full
        );
    }

    #[test]
    fn vscode_pipeline() {
        let _guard = isolated_test();
        assert_eq!(
            mcp_coverage::detect_mcp_coverage("vscode", true, true),
            CoverageClass::ContextControlled
        );
        assert_eq!(
            mcp_coverage::mcp_client_profile("vscode").context_window,
            128_000
        );
        assert_eq!(
            mcp_coverage::mcp_optimization_level("vscode"),
            OptimizationLevel::Partial
        );
    }

    #[test]
    fn request_metrics_track_mcp_calls_without_inventing_etpao() {
        let _guard = isolated_test();
        for index in 0..10 {
            mcp_bridge::record_mcp_call(&call("ctx_read", 50 + index));
        }

        let summary = mcp_bridge::mcp_summary();
        assert_eq!(mcp_bridge::mcp_etpao(), 0.0);
        assert_eq!(summary.accepted_calls, 0);
        assert_eq!(summary.total_calls, 10);
    }

    #[test]
    fn coverage_affects_schema_budget() {
        let _guard = isolated_test();
        let full = mcp_schema_opt::budget_for_coverage(CoverageClass::FullInline);
        let controlled = mcp_schema_opt::budget_for_coverage(CoverageClass::ContextControlled);
        let observe = mcp_schema_opt::budget_for_coverage(CoverageClass::ObserveOnly);

        assert!(full.max_total_tokens > observe.max_total_tokens);
        assert_eq!(full.max_total_tokens, 12_000);
        assert_eq!(controlled.max_total_tokens, 8_000);
        assert_eq!(observe.max_total_tokens, 4_000);
    }

    #[test]
    fn end_to_end_identity_to_request_metrics() {
        let _guard = isolated_test();
        let _context = mcp_bridge::process_mcp_context(&cursor());

        for index in 0..5 {
            mcp_bridge::record_mcp_call(&call("ctx_read", 100 + index));
        }

        let summary = mcp_bridge::mcp_summary();
        assert_eq!(summary.total_calls, 5);
        assert_eq!(summary.total_input_tokens, 5_000);
        assert_eq!(summary.total_output_tokens, 510);
    }
}
