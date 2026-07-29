//! In-memory quality tracking for model-routing decisions.

use std::collections::VecDeque;

const MAX_DECISIONS: usize = 1_000;
const QUALITY_THRESHOLD: f64 = 0.8;
const MIN_SCORED_OUTCOMES: usize = 20;

/// The model route selected for one request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RoutingDecision {
    pub decision_id: String,
    pub original_model: String,
    pub routed_model: String,
    pub reason: String,
    pub timestamp: String,
}

/// Measured result for a model route.
#[derive(Clone, Debug, PartialEq)]
pub struct RoutingOutcome {
    pub decision: RoutingDecision,
    pub quality_score: Option<f64>,
    pub tokens_saved: u64,
    pub latency_delta_ms: i64,
}

/// Bounded tracker for recent model-routing outcomes.
#[derive(Debug, Default)]
pub struct RoutingQualityTracker {
    outcomes: VecDeque<RoutingOutcome>,
}

impl RoutingQualityTracker {
    /// Creates an empty tracker.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records an outcome, retaining only the most recent 1000 outcomes.
    pub fn record(&mut self, outcome: RoutingOutcome) {
        if self.outcomes.len() == MAX_DECISIONS {
            self.outcomes.pop_front();
        }
        self.outcomes.push_back(RoutingOutcome {
            quality_score: sanitize_quality(outcome.quality_score),
            ..outcome
        });

        if self.should_fallback() {
            tracing::warn!(
                success_rate = self.success_rate(),
                "routing quality below threshold; suggest fallback"
            );
        }
    }

    /// Returns the fraction of recorded outcomes meeting the quality threshold.
    pub fn success_rate(&self) -> f64 {
        let scored = self.scored_outcomes();
        if scored == 0 {
            return 0.0;
        }

        self.outcomes
            .iter()
            .filter(|outcome| {
                outcome
                    .quality_score
                    .is_some_and(|score| score >= QUALITY_THRESHOLD)
            })
            .count() as f64
            / scored as f64
    }

    /// Returns the average token savings across recorded outcomes.
    pub fn average_savings(&self) -> f64 {
        if self.outcomes.is_empty() {
            return 0.0;
        }

        self.outcomes
            .iter()
            .map(|outcome| outcome.tokens_saved as f64)
            .sum::<f64>()
            / self.outcomes.len() as f64
    }

    /// Returns whether recent route quality should trigger a fallback.
    pub fn should_fallback(&self) -> bool {
        self.scored_outcomes() >= MIN_SCORED_OUTCOMES && self.success_rate() < QUALITY_THRESHOLD
    }

    fn scored_outcomes(&self) -> usize {
        self.outcomes
            .iter()
            .filter(|outcome| outcome.quality_score.is_some())
            .count()
    }
}

fn sanitize_quality(score: Option<f64>) -> Option<f64> {
    score.filter(|value| value.is_finite() && (0.0..=1.0).contains(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome(score: Option<f64>, tokens_saved: u64) -> RoutingOutcome {
        RoutingOutcome {
            decision: RoutingDecision {
                decision_id: "decision-test".into(),
                original_model: "expensive".into(),
                routed_model: "fast".into(),
                reason: "quality test".into(),
                timestamp: "2026-01-01T00:00:00Z".into(),
            },
            quality_score: score,
            tokens_saved,
            latency_delta_ms: -5,
        }
    }

    #[test]
    fn new_tracker_starts_empty() {
        let tracker = RoutingQualityTracker::new();

        assert_eq!(tracker.success_rate(), 0.0);
        assert_eq!(tracker.average_savings(), 0.0);
        assert!(!tracker.should_fallback());
    }

    #[test]
    fn mixed_outcomes_update_quality_and_savings() {
        let mut tracker = RoutingQualityTracker::new();
        tracker.record(outcome(Some(0.95), 100));
        tracker.record(outcome(Some(0.4), 20));
        tracker.record(outcome(None, 0));
        tracker.record(outcome(Some(0.8), 40));

        assert!((tracker.success_rate() - (2.0 / 3.0)).abs() < f64::EPSILON);
        assert!((tracker.average_savings() - 40.0).abs() < f64::EPSILON);
        assert!(!tracker.should_fallback());
    }

    #[test]
    fn ring_buffer_discards_oldest_outcome() {
        let mut tracker = RoutingQualityTracker::new();
        tracker.record(outcome(Some(0.0), 0));
        for _ in 0..MAX_DECISIONS {
            tracker.record(outcome(Some(1.0), 100));
        }

        assert!((tracker.success_rate() - 1.0).abs() < f64::EPSILON);
        assert!((tracker.average_savings() - 100.0).abs() < f64::EPSILON);
        assert!(!tracker.should_fallback());
    }

    #[test]
    fn fallback_requires_minimum_scored_samples() {
        let mut tracker = RoutingQualityTracker::new();
        for _ in 0..(MIN_SCORED_OUTCOMES - 1) {
            tracker.record(outcome(Some(0.0), 0));
        }
        assert!(!tracker.should_fallback());

        tracker.record(outcome(Some(0.0), 0));
        assert!(tracker.should_fallback());
    }

    #[test]
    fn invalid_and_absent_quality_do_not_count_as_failure() {
        let mut tracker = RoutingQualityTracker::new();
        tracker.record(outcome(None, 10));
        tracker.record(outcome(Some(f64::NAN), 10));
        tracker.record(outcome(Some(1.5), 10));

        assert_eq!(tracker.success_rate(), 0.0);
        assert!(!tracker.should_fallback());
    }
}
