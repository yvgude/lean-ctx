//! BuiltinOutcomeTracker — explicit accept/reject signal capture.
//!
//! OBSERVE phase (Stufe 1): captures explicit user feedback signals
//! (accept/reject/partial). Emits OutcomeRecorded events to OclaBus.
//!
//! Stufe 2 (implicit heuristics) is deferred to a later MR:
//! - "Edit nach Read = accepted"
//! - "Kompilierungsfehler nach Generate = rejected"

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::core::ocla::{Outcome, OutcomeRecord, OutcomeTracker};
use crate::core::ocla_bus::{self, OclaEvent};

/// Built-in OCLA OutcomeTracker with per-session outcome history.
pub struct BuiltinOutcomeTracker {
    state: Mutex<TrackerState>,
    next_id: AtomicU64,
}

#[derive(Debug, Default)]
struct TrackerState {
    sessions: HashMap<String, VecDeque<OutcomeRecord>>,
}

const MAX_OUTCOMES_PER_SESSION: usize = 256;

impl BuiltinOutcomeTracker {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(TrackerState::default()),
            next_id: AtomicU64::new(1),
        }
    }
}

impl Default for BuiltinOutcomeTracker {
    fn default() -> Self {
        Self::new()
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

impl OutcomeTracker for BuiltinOutcomeTracker {
    fn record(&self, outcome: Outcome) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);

        let record = OutcomeRecord {
            id,
            outcome: outcome.clone(),
            timestamp_ms: now_ms(),
        };

        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let session = state
            .sessions
            .entry(outcome.session_id.clone())
            .or_insert_with(|| VecDeque::with_capacity(MAX_OUTCOMES_PER_SESSION));

        if session.len() >= MAX_OUTCOMES_PER_SESSION {
            session.pop_front();
        }
        session.push_back(record);

        ocla_bus::emit(OclaEvent::OutcomeRecorded {
            session_id: outcome.session_id,
            accepted: outcome.accepted,
            implicit: outcome.implicit,
        });

        id
    }

    fn recent(&self, session_id: &str, limit: usize) -> Vec<OutcomeRecord> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        state
            .sessions
            .get(session_id)
            .map(|s| {
                let start = s.len().saturating_sub(limit);
                s.iter().skip(start).cloned().collect()
            })
            .unwrap_or_default()
    }

    fn acceptance_rate(&self, session_id: &str) -> Option<f64> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let session = state.sessions.get(session_id)?;
        if session.is_empty() {
            return None;
        }

        let accepted = session.iter().filter(|r| r.outcome.accepted).count();
        #[allow(clippy::cast_precision_loss)]
        Some(accepted as f64 / session.len() as f64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn accept(session: &str) -> Outcome {
        Outcome {
            session_id: session.into(),
            accepted: true,
            tool: Some("ctx_read".into()),
            implicit: false,
        }
    }

    fn reject(session: &str) -> Outcome {
        Outcome {
            session_id: session.into(),
            accepted: false,
            tool: None,
            implicit: false,
        }
    }

    #[test]
    fn record_returns_unique_ids() {
        let tracker = BuiltinOutcomeTracker::new();
        let id1 = tracker.record(accept("s1"));
        let id2 = tracker.record(accept("s1"));
        assert_ne!(id1, id2);
    }

    #[test]
    fn recent_returns_latest() {
        let tracker = BuiltinOutcomeTracker::new();
        tracker.record(accept("s1"));
        tracker.record(reject("s1"));
        tracker.record(accept("s1"));

        let recent = tracker.recent("s1", 2);
        assert_eq!(recent.len(), 2);
        assert!(!recent[0].outcome.accepted);
        assert!(recent[1].outcome.accepted);
    }

    #[test]
    fn acceptance_rate_calculation() {
        let tracker = BuiltinOutcomeTracker::new();
        tracker.record(accept("s1"));
        tracker.record(accept("s1"));
        tracker.record(reject("s1"));

        let rate = tracker.acceptance_rate("s1").unwrap();
        assert!((rate - 2.0 / 3.0).abs() < 0.01);
    }

    #[test]
    fn acceptance_rate_none_for_unknown_session() {
        let tracker = BuiltinOutcomeTracker::new();
        assert_eq!(tracker.acceptance_rate("nonexistent"), None);
    }

    #[test]
    fn sessions_are_isolated() {
        let tracker = BuiltinOutcomeTracker::new();
        tracker.record(accept("s1"));
        tracker.record(reject("s2"));

        assert_eq!(tracker.acceptance_rate("s1"), Some(1.0));
        assert_eq!(tracker.acceptance_rate("s2"), Some(0.0));
    }

    #[test]
    fn bounded_capacity() {
        let tracker = BuiltinOutcomeTracker::new();
        for _ in 0..300 {
            tracker.record(accept("s1"));
        }
        let recent = tracker.recent("s1", 500);
        assert_eq!(recent.len(), MAX_OUTCOMES_PER_SESSION);
    }
}
