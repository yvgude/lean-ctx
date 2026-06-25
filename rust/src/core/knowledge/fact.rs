use chrono::{DateTime, Utc};

use super::types::{COGNITION_SYNTHESIS_SOURCE, FidelityScore, KnowledgeArchetype, KnowledgeFact};

impl KnowledgeFact {
    #[must_use]
    pub fn is_current(&self) -> bool {
        self.valid_until.is_none()
    }

    /// A synthesized observation: an entity-summary written by the cognition loop's
    /// synthesis step (#802), not a user-supplied finding. Recall surfaces these as
    /// orientation (a balanced boost, never absolute).
    #[must_use]
    pub fn is_synthesized_observation(&self) -> bool {
        self.archetype == KnowledgeArchetype::Observation
            && self.source_session == COGNITION_SYNTHESIS_SOURCE
    }

    /// Stable, intrinsic quality metric (0.0..1.0).
    ///
    /// Based only on confidence, confirmation count, and feedback balance.
    /// Deliberately excludes volatile signals (retrieval count, recency) to
    /// keep recall output deterministic. For display ordering use
    /// `salience_score()` which adds recency and category weighting.
    #[must_use]
    pub fn quality_score(&self) -> f32 {
        let confidence = self.confidence.clamp(0.0, 1.0);
        let confirmations_norm = (self.confirmation_count.min(5) as f32) / 5.0;
        let balance = self.feedback_up as i32 - self.feedback_down as i32;
        let feedback_effect = (balance as f32 / 4.0).tanh() * 0.1;

        // IMPORTANT: quality_score must be stable across repeated recall calls.
        // Retrieval signals (retrieval_count/last_retrieved) are persisted, but should not change
        // the displayed "quality" score, otherwise recall output becomes non-deterministic.
        (0.8 * confidence + 0.2 * confirmations_norm + feedback_effect).clamp(0.0, 1.0)
    }

    #[must_use]
    pub fn was_valid_at(&self, at: DateTime<Utc>) -> bool {
        let after_start = self.valid_from.is_none_or(|from| at >= from);
        let before_end = self.valid_until.is_none_or(|until| at <= until);
        after_start && before_end
    }

    /// Compute structural fidelity score (0.0 - 1.0).
    /// Based on: has source, confirmations, confidence, freshness, feedback.
    #[must_use]
    pub fn compute_structural_fidelity(&self) -> f64 {
        let mut score: f64 = 0.0;
        if !self.source_session.is_empty() && self.source_session != "unknown" {
            score += 0.2;
        }
        if self.confirmation_count >= 2 {
            score += 0.2;
        }
        if self.confidence > 0.7 {
            score += 0.2;
        }
        let days_since_confirmed = Utc::now()
            .signed_duration_since(self.last_confirmed)
            .num_days();
        if days_since_confirmed < 14 {
            score += 0.2;
        } else if days_since_confirmed < 30 {
            score += 0.1;
        }
        if self.feedback_up > self.feedback_down {
            score += 0.2;
        } else if self.feedback_up > 0 {
            score += 0.1;
        }
        score.min(1.0)
    }

    pub fn update_fidelity(&mut self) {
        let structural = self.compute_structural_fidelity();
        self.fidelity = Some(FidelityScore {
            structural,
            semantic: structural,
            computed_at: Utc::now(),
        });
    }
}
