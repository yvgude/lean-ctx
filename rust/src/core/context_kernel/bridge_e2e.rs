//! End-to-end conformance tests for the proxy-to-kernel bridge.

#[cfg(test)]
mod tests {
    use std::sync::MutexGuard;

    fn isolated() -> MutexGuard<'static, ()> {
        let guard = crate::core::context_kernel::kernel_config::KERNEL_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        crate::core::context_kernel::proxy_bridge::reset_state();
        crate::core::context_kernel::mcp_bridge::reset_mcp_state();
        guard
    }
    use super::super::accounting_fix;
    use super::super::coverage_class::CoverageClass;
    use super::super::identity::CallerRole;
    use super::super::proxy_bridge::{self, ProxyRequestData};

    fn request_for(user_id: &str) -> ProxyRequestData {
        ProxyRequestData {
            headers: vec![("x-user-id".to_owned(), user_id.to_owned())],
            input_tokens: 1_000,
            output_tokens: 200,
            tokens_saved: 300,
            request_count: 1,
            ..ProxyRequestData::default()
        }
    }

    #[test]
    fn full_proxy_lifecycle() {
        let _guard = isolated();
        let request = ProxyRequestData {
            headers: vec![
                ("x-user-id".to_owned(), "alice".to_owned()),
                ("x-team-id".to_owned(), "backend".to_owned()),
            ],
            input_tokens: 1_000,
            output_tokens: 200,
            tokens_saved: 300,
            request_count: 1,
            ..ProxyRequestData::default()
        };

        let result = proxy_bridge::process_proxy_request(&request);

        assert_eq!(result.identity.user_id.as_deref(), Some("alice"));
        assert_eq!(result.identity.role, CallerRole::Developer);
        assert_eq!(result.coverage, CoverageClass::FullInline);
        assert!(result.is_addressable);
    }

    #[test]
    fn proxy_records_request_without_inventing_etpao() {
        let _guard = isolated();
        for _ in 0..5 {
            let _ = proxy_bridge::process_proxy_request(&request_for("alice"));
        }

        let summary = proxy_bridge::etpao_summary();
        assert_eq!(proxy_bridge::current_etpao(), 0.0);
        assert!(summary.total_tokens > 0);
        assert_eq!(summary.accepted_outcomes, 0);
    }

    #[test]
    fn proxy_records_identity_ledger() {
        let _guard = isolated();
        for _ in 0..3 {
            let _ = proxy_bridge::process_proxy_request(&request_for("bob"));
        }

        assert!(proxy_bridge::identity_summary().total_users >= 1);
    }

    #[test]
    fn honest_accounting_detects_phantom() {
        let _guard = isolated();
        let accounting = accounting_fix::account_proxy_request(1_000, 300, 200, 50);

        assert!(accounting.phantom_savings_pct > 0.0);
        assert!(accounting_fix::format_proxy_accounting(&accounting).contains("phantom"));
    }

    #[test]
    fn end_to_end_identity_to_request_metrics() {
        let _guard = isolated();
        for index in 0..10 {
            let user = format!("user-{}", index % 3);
            let _ = proxy_bridge::process_proxy_request(&request_for(&user));
        }

        assert!(proxy_bridge::identity_summary().total_users >= 3);
        let summary = proxy_bridge::etpao_summary();
        assert!(summary.total_tokens > 0);
        assert_eq!(summary.accepted_outcomes, 0);
        assert_eq!(summary.etpao, 0.0);
    }

    #[test]
    fn unevaluated_outcome_remains_unknown() {
        let _guard = isolated();
        let request = ProxyRequestData {
            is_retry: true,
            request_count: 3,
            ..request_for("alice")
        };

        let result = proxy_bridge::process_proxy_request(&request);

        assert_eq!(
            result.outcome_signal.outcome,
            super::super::types::ReceiptOutcome::Unknown
        );
    }
}
