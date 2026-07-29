//! Adapter between proxy routing events and OCLA quality tracking.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use crate::core::ocla::routing_quality::{RoutingDecision, RoutingOutcome, RoutingQualityTracker};

const MAX_PENDING_DECISIONS: usize = 1_000;
type PendingDecisions = HashMap<String, RoutingDecision>;
static NEXT_DECISION_ID: AtomicU64 = AtomicU64::new(1);
const FALLBACK_PROBE_INTERVAL: u64 = 20;

static GLOBAL_FEEDBACK: OnceLock<RoutingFeedback> = OnceLock::new();

/// Process-wide routing feedback collector used by the proxy router.
pub fn global_feedback() -> &'static RoutingFeedback {
    GLOBAL_FEEDBACK.get_or_init(RoutingFeedback::new)
}

/// Collects proxy routing decisions and their measured outcomes.
#[derive(Clone, Debug)]
pub struct RoutingFeedback {
    tracker: Arc<Mutex<RoutingQualityTracker>>,
    pending_decisions: Arc<Mutex<PendingDecisions>>,
    fallback_checks: Arc<AtomicU64>,
}

impl RoutingFeedback {
    /// Creates an empty routing feedback collector.
    pub fn new() -> Self {
        Self {
            tracker: Arc::new(Mutex::new(RoutingQualityTracker::new())),
            pending_decisions: Arc::new(Mutex::new(HashMap::new())),
            fallback_checks: Arc::new(AtomicU64::new(1)),
        }
    }

    /// Records a route selection until its measured outcome arrives.
    pub fn record_decision(&self, original: &str, routed: &str, reason: &str) -> String {
        let decision_id = format!("route-{}", NEXT_DECISION_ID.fetch_add(1, Ordering::Relaxed));
        let decision = RoutingDecision {
            decision_id: decision_id.clone(),
            original_model: original.to_string(),
            routed_model: routed.to_string(),
            reason: reason.to_string(),
            timestamp: chrono::Utc::now().to_rfc3339(),
        };
        let mut pending = self
            .pending_decisions
            .lock()
            .expect("routing feedback pending decision mutex poisoned");
        pending.insert(decision_id.clone(), decision);
        while pending.len() > MAX_PENDING_DECISIONS {
            let Some(evicted_key) = pending
                .iter()
                .min_by_key(|(_, decision)| &decision.timestamp)
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            pending.remove(&evicted_key);
        }
        decision_id
    }

    /// Records measured quality for a route and forwards it to the tracker.
    pub fn record_outcome_for_decision(
        &self,
        decision_id: &str,
        quality: Option<f64>,
        tokens_saved: u64,
        latency_delta_ms: i64,
    ) {
        let decision = {
            let mut pending = self
                .pending_decisions
                .lock()
                .expect("routing feedback pending decision mutex poisoned");
            pending.remove(decision_id)
        };
        if let Some(decision) = decision {
            self.tracker
                .lock()
                .expect("routing feedback tracker mutex poisoned")
                .record(RoutingOutcome {
                    decision,
                    quality_score: quality,
                    tokens_saved,
                    latency_delta_ms,
                });
        }
    }

    /// Records measured quality for a route and forwards it to the tracker.
    pub fn record_outcome(
        &self,
        original: &str,
        routed: &str,
        quality: Option<f64>,
        tokens_saved: u64,
        latency_delta_ms: i64,
    ) {
        let decision = RoutingDecision {
            decision_id: format!(
                "unmatched-{}",
                NEXT_DECISION_ID.fetch_add(1, Ordering::Relaxed)
            ),
            original_model: original.to_string(),
            routed_model: routed.to_string(),
            reason: "outcome without recorded decision".to_string(),
            timestamp: chrono::Utc::now().to_rfc3339(),
        };

        self.tracker
            .lock()
            .expect("routing feedback tracker mutex poisoned")
            .record(RoutingOutcome {
                decision,
                quality_score: quality,
                tokens_saved,
                latency_delta_ms,
            });
    }

    /// Returns whether tracked route quality warrants fallback.
    pub fn should_use_fallback(&self) -> bool {
        let should_fallback = self
            .tracker
            .lock()
            .expect("routing feedback tracker mutex poisoned")
            .should_fallback();
        should_fallback
            && !self
                .fallback_checks
                .fetch_add(1, Ordering::Relaxed)
                .is_multiple_of(FALLBACK_PROBE_INTERVAL)
    }

    /// Returns tracked success rate and average token savings.
    pub fn stats(&self) -> (f64, f64) {
        let tracker = self
            .tracker
            .lock()
            .expect("routing feedback tracker mutex poisoned");
        (tracker.success_rate(), tracker.average_savings())
    }
}

impl Default for RoutingFeedback {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_decision_until_matching_outcome() {
        let feedback = RoutingFeedback::new();

        let decision_id = feedback.record_decision("expensive", "fast", "token budget");
        assert_eq!(feedback.stats(), (0.0, 0.0));
        assert!(!feedback.should_use_fallback());

        let pending = feedback
            .pending_decisions
            .lock()
            .expect("test pending decision mutex poisoned");
        let decision = &pending[&decision_id];
        assert_eq!(decision.reason, "token budget");
    }

    #[test]
    fn records_successful_outcome_and_statistics() {
        let feedback = RoutingFeedback::new();

        let decision_id = feedback.record_decision("expensive", "fast", "token budget");
        feedback.record_outcome_for_decision(&decision_id, Some(0.95), 120, -10);

        assert_eq!(feedback.stats(), (1.0, 120.0));
        assert!(!feedback.should_use_fallback());
    }

    #[test]
    fn poor_outcome_triggers_fallback() {
        let feedback = RoutingFeedback::new();

        for _ in 0..20 {
            feedback.record_outcome("expensive", "fast", Some(0.4), 20, 15);
        }

        assert_eq!(feedback.stats(), (0.0, 20.0));
        assert!(feedback.should_use_fallback());
    }

    #[test]
    fn concurrent_same_pair_outcomes_match_by_decision_id() {
        let feedback = RoutingFeedback::new();
        let first = feedback.record_decision("expensive", "fast", "first");
        let second = feedback.record_decision("expensive", "fast", "second");

        feedback.record_outcome_for_decision(&second, Some(1.0), 20, 0);

        let pending = feedback
            .pending_decisions
            .lock()
            .expect("test pending decision mutex poisoned");
        assert!(pending.contains_key(&first));
        assert!(!pending.contains_key(&second));
        assert_eq!(feedback.stats(), (1.0, 20.0));
    }

    #[test]
    fn fallback_allows_periodic_recovery_probe() {
        let feedback = RoutingFeedback::new();
        for _ in 0..20 {
            feedback.record_outcome("expensive", "fast", Some(0.4), 20, 15);
        }

        let mut allowed_probe = false;
        for _ in 0..FALLBACK_PROBE_INTERVAL {
            allowed_probe |= !feedback.should_use_fallback();
        }

        assert!(allowed_probe);
    }
}
