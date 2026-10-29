#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::super::etpao::{EtpaoMetrics, EtpaoTracker};
    use super::super::knowledge_health;
    use super::super::types::{ContextReceiptV1, ReceiptOutcome};

    fn receipt(id: &str, delivered_tokens: usize, cache_hits: usize) -> ContextReceiptV1 {
        ContextReceiptV1 {
            receipt_id: format!("receipt:{id}"),
            plan_id: format!("plan:{id}"),
            task_id: None,
            delivered_tokens,
            cache_hits,
            cache_misses: usize::from(cache_hits == 0),
            outcome: ReceiptOutcome::Accepted,
            quality_signals: Vec::new(),
            feedback_attribution: HashMap::new(),
        }
    }

    #[test]
    fn etpao_improves_with_context_reuse() {
        let mut tracker = EtpaoTracker::default();
        for index in 0..5 {
            tracker.record_receipt(
                "chain",
                &receipt(&format!("uncached-{index}"), 1_000, 0),
                ReceiptOutcome::Accepted,
            );
        }
        let first: EtpaoMetrics = tracker.aggregate();
        let first_etpao = first.etpao();

        for index in 0..5 {
            tracker.record_receipt(
                "chain",
                &receipt(&format!("cached-{index}"), 100, 1),
                ReceiptOutcome::Accepted,
            );
        }
        let second_etpao = tracker.aggregate().etpao();

        assert!(second_etpao < first_etpao);
    }

    #[test]
    fn knowledge_health_detects_stale_facts() {
        let mut facts = vec![(true, false); 3];
        facts.extend([(false, false); 5]);
        facts.extend([(false, true); 2]);

        let report = knowledge_health::assess_health(&facts, 0, 0, 0, 0);

        assert!((report.freshness_score - 0.3).abs() < f64::EPSILON);
        assert!((report.stale_ratio - 0.7).abs() < f64::EPSILON);
        assert!((report.contradiction_rate - 0.2).abs() < f64::EPSILON);
    }
}
