// SPDX-License-Identifier: Apache-2.0
//! BuiltinOutcomeTracker — captures accept/reject/partial signals.
//!
//! Records outcome feedback in a bounded buffer.
//! Replaces the legacy P3 version with canonical OCLA types from `types.rs`.

use std::collections::VecDeque;
use std::sync::Mutex;

use crate::core::ocla::traits::{OclaService, OutcomeTracker};
use crate::core::ocla::types::{OclaCapability, OclaCapabilityKind, OclaResult, Outcome};

const MAX_OUTCOMES: usize = 500;

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
    fn manifest(&self) -> crate::core::ocla::OclaResult<lean_ctx_protocol::CapabilityManifestV1> {
        crate::core::ocla::capability_fabric::builtin_manifest(&self.capability())
    }

    fn capability(&self) -> OclaCapability {
        OclaCapability::available(OclaCapabilityKind::OutcomeTracker)
    }
}

impl OutcomeTracker for BuiltinOutcomeTracker {
    fn record_outcome(&self, outcome: Outcome) -> OclaResult<()> {
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
                trace_id: "tr-unit".into(),
                task_id: None,
                parent_task_id: None,
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
        for index in 0..=MAX_OUTCOMES {
            let mut item = outcome(true);
            item.context.request_id = index.to_string();
            tracker.record_outcome(item).unwrap();
        }
        assert_eq!(tracker.recent(500).len(), MAX_OUTCOMES);
        assert_eq!(tracker.recent(MAX_OUTCOMES)[0].context.request_id, "1");
        assert_eq!(
            tracker
                .recent(MAX_OUTCOMES)
                .last()
                .unwrap()
                .context
                .request_id,
            MAX_OUTCOMES.to_string()
        );
    }

    #[test]
    fn concurrent_record_and_recent_are_panic_free() {
        let tracker = std::sync::Arc::new(BuiltinOutcomeTracker::new());
        let mut workers = Vec::new();

        for worker in 0..8 {
            let tracker = std::sync::Arc::clone(&tracker);
            workers.push(std::thread::spawn(move || {
                for index in 0..100 {
                    tracker
                        .record_outcome(outcome((worker + index) % 2 == 0))
                        .unwrap();
                    let recent = tracker.recent(32);
                    assert!(recent.len() <= 32);
                }
            }));
        }

        for worker in workers {
            worker.join().unwrap();
        }

        assert_eq!(tracker.recent(MAX_OUTCOMES).len(), MAX_OUTCOMES);
    }
}
