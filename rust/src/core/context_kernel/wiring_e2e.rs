//! End-to-end conformance tests for request identity and client wiring.

#[cfg(test)]
mod tests {
    use super::super::client_wiring;
    use super::super::coverage_class::{self, CoverageClass};
    use super::super::etpao_live::EtpaoLive;
    use super::super::identity::{CallerIdentity, CallerRole, IdentityLedger};
    use super::super::identity_resolver;

    fn headers(values: &[(&str, &str)]) -> Vec<(String, String)> {
        values
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect()
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
    fn end_to_end_request_lifecycle() {
        let headers = headers(&[
            ("x-client-id", "lifecycle-client"),
            ("x-user-id", "carol"),
            ("x-team-id", "platform"),
            ("x-cost-center", "eng-02"),
        ]);
        let identity = identity_resolver::resolve_with_defaults(&headers);
        let coverage = coverage_class::detect_coverage(true, false, false);
        let context = client_wiring::build_request_context(&headers, true, false, false);
        let mut etpao = EtpaoLive::new();
        let mut ledger = IdentityLedger::new();

        client_wiring::record_request_etpao(&mut etpao, &context, 1_000, 250);
        client_wiring::record_outcome_etpao(&mut etpao, &context, true, 0.95);
        ledger.record(&identity, 1_250, 0, true);

        assert_eq!(identity, context.identity);
        assert_eq!(coverage, CoverageClass::FullInline);
        assert_eq!(context.profile.coverage, coverage);
        assert!(context.broker_budget.context_tokens > 0);
        assert!(ledger.total_tokens() > 0);
        assert!(etpao.current_etpao() > 0.0);
    }
}
