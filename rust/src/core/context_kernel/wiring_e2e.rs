//! End-to-end conformance tests for request identity and client wiring.

#[cfg(test)]
mod tests {
    use super::super::client_profile::ProfileBuilder;
    use super::super::client_wiring;
    use super::super::coverage_class::{self, CoverageClass};
    use super::super::etpao_live::EtpaoLive;
    use super::super::identity::{CallerIdentity, CallerRole, IdentityLedger};
    use super::super::identity_resolver;
    use super::super::tool_surface::{ToolCategory, ToolSchema, ToolSurfaceOptimizer};

    fn headers(values: &[(&str, &str)]) -> Vec<(String, String)> {
        values
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect()
    }

    fn tool(index: usize, token_count: usize) -> ToolSchema {
        ToolSchema {
            name: format!("tool-{index}"),
            description: format!("Production tool number {index}"),
            parameters_json: r#"{"type":"object","properties":{}}"#.to_owned(),
            token_count,
            priority: u8::try_from(index).unwrap_or(u8::MAX),
            category: ToolCategory::Core,
        }
    }

    #[test]
    fn identity_to_ledger_pipeline() {
        let identity = CallerIdentity {
            user_id: Some("alice".to_owned()),
            team_id: Some("backend".to_owned()),
            role: CallerRole::Developer,
            ..CallerIdentity::default()
        };
        let mut ledger = IdentityLedger::new();

        for _ in 0..5 {
            ledger.record(&identity, 100, 25, true);
        }

        let attribution = ledger
            .attribution_for("alice")
            .expect("alice must have ledger attribution");
        assert_eq!(attribution.request_count, 5);
    }

    #[test]
    fn headers_to_identity_to_context() {
        let headers = headers(&[
            ("x-user-id", "bob"),
            ("x-team-id", "frontend"),
            ("x-cost-center", "eng-01"),
        ]);
        let identity = identity_resolver::resolve_with_defaults(&headers);
        let context = client_wiring::build_request_context(&headers, true, false, false);

        assert_eq!(identity, context.identity);
        assert_eq!(context.identity.user_id.as_deref(), Some("bob"));
        assert_eq!(context.identity.cost_center.as_deref(), Some("eng-01"));
    }

    #[test]
    fn full_inline_optimizes_tools() {
        let mut profile = ProfileBuilder::new("inline")
            .coverage(CoverageClass::FullInline)
            .build();
        profile.tool_budget.max_tools = 5;
        profile.tool_budget.max_schema_tokens = 10_000;
        let schemas = (0..15)
            .map(|index| tool(index, 100 + index * 10))
            .collect::<Vec<_>>();

        let reduction = ToolSurfaceOptimizer::from_profile(&profile).optimize(&schemas);

        assert!(reduction.reduced_count <= 5);
        assert!(reduction.tokens_saved > 0);
    }

    #[test]
    fn unmanaged_no_optimization() {
        let context = client_wiring::build_request_context(&[], false, false, false);

        assert!(!client_wiring::should_optimize(&context));
        assert_eq!(
            client_wiring::optimization_level(&context),
            client_wiring::OptimizationLevel::None
        );
    }

    #[test]
    fn etpao_tracks_per_identity() {
        let first = client_wiring::build_request_context(
            &headers(&[("x-client-id", "client-a")]),
            true,
            false,
            false,
        );
        let second = client_wiring::build_request_context(
            &headers(&[("x-client-id", "client-b")]),
            false,
            true,
            false,
        );
        let mut etpao = EtpaoLive::new();

        client_wiring::record_request_etpao(&mut etpao, &first, 800, 200);
        client_wiring::record_request_etpao(&mut etpao, &second, 600, 150);

        assert_eq!(etpao.request_count(), 2);
        assert!(etpao.etpao_for_client("client-a").is_some());
        assert!(etpao.etpao_for_client("client-b").is_some());
        assert_eq!(etpao.summary().total_tokens, 1_750);
    }

    #[test]
    fn tool_surface_savings_realistic() {
        let schemas = (0..20)
            .map(|index| tool(index, 100 + index * 20))
            .collect::<Vec<_>>();

        let reduction = ToolSurfaceOptimizer::new(5, 1_000).optimize(&schemas);

        assert!(reduction.reduced_count <= 5);
        assert!(reduction.reduced_tokens <= 1_000);
        assert!(reduction.savings_pct > 50.0);
    }

    #[test]
    fn end_to_end_request_lifecycle() {
        let headers = headers(&[
            ("x-client-id", "lifecycle-client"),
            ("x-user-id", "carol"),
            ("x-team-id", "platform"),
            ("x-cost-center", "eng-02"),
        ]);
        let identity = identity_resolver::resolve_with_defaults(&headers);
        let coverage = coverage_class::detect_coverage(true, false, false);
        let mut context = client_wiring::build_request_context(&headers, true, false, false);
        context.profile.tool_budget.max_tools = 3;
        context.profile.tool_budget.max_schema_tokens = 600;
        let schemas = (0..8)
            .map(|index| tool(index, 100 + index * 10))
            .collect::<Vec<_>>();
        let reduction = ToolSurfaceOptimizer::from_profile(&context.profile).optimize(&schemas);
        let mut etpao = EtpaoLive::new();
        let mut ledger = IdentityLedger::new();

        client_wiring::record_request_etpao(&mut etpao, &context, 1_000, 250);
        client_wiring::record_outcome_etpao(&mut etpao, &context, true, 0.95);
        ledger.record(&identity, 1_250, reduction.tokens_saved, true);

        assert_eq!(identity, context.identity);
        assert_eq!(coverage, CoverageClass::FullInline);
        assert_eq!(context.profile.coverage, coverage);
        assert!(context.broker_budget.context_tokens > 0);
        assert!(
            !reduction.selected_tools.is_empty(),
            "`reduction.selected_tools` is empty"
        );
        assert!(ledger.total_tokens() > 0);
        assert!(etpao.current_etpao() > 0.0);
    }
}
