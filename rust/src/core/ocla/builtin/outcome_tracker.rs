//! BuiltinOutcomeTracker — captures accept/reject/partial signals.
//!
//! Records outcome feedback and emits OutcomeRecorded events to OclaBus.
//! Replaces the legacy P3 version with canonical OCLA types from `types.rs`.

use std::collections::VecDeque;
use std::sync::Mutex;

use crate::core::ocla::traits::{OclaService, OutcomeTracker};
use crate::core::ocla::types::{OclaCapability, OclaCapabilityKind, OclaResult, Outcome};
use crate::core::ocla_bus::{self, OclaEvent};

const MAX_OUTCOMES: usize = 256;

pub struct BuiltinOutcomeTracker {
    outcomes: Mutex<VecDeque<Outcome>>,
}

impl BuiltinOutcomeTracker {
    pub fn new() -> Self {
        Self {
            outcomes: Mutex::new(VecDeque::with_capacity(MAX_OUTCOMES)),
        }
    }

    pub fn recent(&self, limit: usize) -> Vec<Outcome> {
        let state = self
            .outcomes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let start = state.len().saturating_sub(limit);
        state.iter().skip(start).cloned().collect()
    }
}

impl Default for BuiltinOutcomeTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl OclaService for BuiltinOutcomeTracker {
    fn capability(&self) -> OclaCapability {
        OclaCapability::available(OclaCapabilityKind::OutcomeTracker)
    }
}

impl OutcomeTracker for BuiltinOutcomeTracker {
    fn record_outcome(&self, outcome: Outcome) -> OclaResult<()> {
        let accepted = outcome.accepted.unwrap_or(false);

        ocla_bus::emit(OclaEvent::OutcomeRecorded {
            session_id: outcome.context.session_id.clone(),
            accepted,
            implicit: false,
        });

        let mut state = self
            .outcomes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        if state.len() >= MAX_OUTCOMES {
            state.pop_front();
        }
        state.push_back(outcome);

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::ocla::types::OclaRequestContext;

    fn outcome(accepted: bool) -> Outcome {
        Outcome {
            context: OclaRequestContext {
                request_id: "r1".into(),
                session_id: "s1".into(),
                agent_id: "agent-test".into(),
                content_ref: "ref:test".into(),
                tenant_id: None,
            },
            accepted: Some(accepted),
            quality_score_milli: None,
            outcome_ref: None,
        }
    }

    #[test]
    fn records_and_retrieves() {
        let tracker = BuiltinOutcomeTracker::new();
        tracker.record_outcome(outcome(true)).unwrap();
        tracker.record_outcome(outcome(false)).unwrap();

        let recent = tracker.recent(10);
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].accepted, Some(true));
    }

    #[test]
    fn bounded_capacity() {
        let tracker = BuiltinOutcomeTracker::new();
        for _ in 0..300 {
            tracker.record_outcome(outcome(true)).unwrap();
        }
        assert_eq!(tracker.recent(500).len(), MAX_OUTCOMES);
    }
}
