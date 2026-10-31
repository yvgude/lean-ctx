//! End-to-end conformance tests for the client intelligence layer.

#[cfg(test)]
mod tests {
    use super::super::client_profile::ProfileBuilder;
    use super::super::coverage_class::{self, CoverageClass};
    use super::super::etpao_live::{EtpaoLive, OutcomeMetrics, RequestMetrics};

    fn request(client_id: &str, tokens: usize, coverage_class: CoverageClass) -> RequestMetrics {
        RequestMetrics {
            input_tokens: tokens,
            output_tokens: 0,
            reasoning_tokens: 0,
            schema_tokens: 0,
            cache_write_tokens: 0,
            retry_count: 0,
            client_id: client_id.to_owned(),
            coverage_class,
        }
    }

    fn outcome(client_id: &str, accepted: bool) -> OutcomeMetrics {
        OutcomeMetrics {
            accepted,
            quality_score: 1.0,
            first_pass: accepted,
            client_id: client_id.to_owned(),
        }
    }

    #[test]
    fn coverage_detection_full_inline() {
        assert_eq!(
            coverage_class::detect_coverage(true, false, false),
            CoverageClass::FullInline
        );
    }

    #[test]
    fn coverage_detection_mcp_only() {
        assert_eq!(
            coverage_class::detect_coverage(false, true, false),
            CoverageClass::ContextControlled
        );
    }

    #[test]
    fn profile_builder_defaults() {
        let profile = ProfileBuilder::new("test").build();
        assert_eq!(profile.client_id, "test");
        assert!(profile.context_window > 0);
    }

    #[test]
    fn etpao_records_and_computes() {
        let mut etpao = EtpaoLive::new();
        for index in 0..10 {
            let client_id = format!("client-{index}");
            etpao.record_request(request(&client_id, 1_000, CoverageClass::FullInline));
            if index < 8 {
                etpao.record_outcome(outcome(&client_id, true));
            }
        }

        assert!((etpao.current_etpao() - 1_250.0).abs() < f64::EPSILON);
    }

    #[test]
    fn etpao_by_coverage_class() {
        let mut etpao = EtpaoLive::new();
        etpao.record_request(request("inline", 1_000, CoverageClass::FullInline));
        etpao.record_request(request(
            "controlled",
            1_000,
            CoverageClass::ContextControlled,
        ));
        etpao.record_outcome(outcome("inline", true));
        etpao.record_outcome(outcome("controlled", true));

        assert_eq!(etpao.summary().by_coverage_class.len(), 2);
    }

    #[test]
    fn full_pipeline_profile_to_etpao() {
        let coverage = coverage_class::detect_coverage(true, false, false);
        let mut etpao = EtpaoLive::new();
        etpao.record_request(request("pipeline", 500, coverage));
        etpao.record_outcome(outcome("pipeline", true));

        assert!(etpao.summary().etpao > 0.0);
    }
}
