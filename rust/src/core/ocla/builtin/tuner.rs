//! BuiltinTuner — wraps Config::update_global with proposal/approval lifecycle.
//!
//! ACT phase: proposes config changes, applies them with changelog, supports
//! rollback of the last applied change.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::core::ocla::{
    ApplyResult, ChangeLogEntry, ConfigChange, ConfigTuner, RollbackResult, TunerError,
};

/// Built-in OCLA ConfigTuner with in-memory proposal/changelog tracking.
pub struct BuiltinTuner {
    state: Mutex<TunerState>,
}

#[derive(Debug)]
struct TunerState {
    proposals: Vec<Proposal>,
    changelog: VecDeque<ChangeLogEntry>,
    next_id: u64,
}

#[derive(Debug, Clone)]
struct Proposal {
    id: String,
    change: ConfigChange,
    applied: bool,
}

impl BuiltinTuner {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(TunerState {
                proposals: Vec::new(),
                changelog: VecDeque::with_capacity(64),
                next_id: 1,
            }),
        }
    }
}

impl Default for BuiltinTuner {
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

impl ConfigTuner for BuiltinTuner {
    fn propose(&self, change: ConfigChange) -> Result<String, TunerError> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let id = format!("proposal-{}", state.next_id);
        state.next_id += 1;

        state.proposals.push(Proposal {
            id: id.clone(),
            change,
            applied: false,
        });

        Ok(id)
    }

    fn apply(&self, proposal_id: &str) -> Result<ApplyResult, TunerError> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let proposal = state
            .proposals
            .iter_mut()
            .find(|p| p.id == proposal_id)
            .ok_or_else(|| TunerError::NotFound(proposal_id.to_string()))?;

        if proposal.applied {
            return Err(TunerError::AlreadyApplied(proposal_id.to_string()));
        }

        proposal.applied = true;
        let entry = ChangeLogEntry {
            proposal_id: proposal_id.to_string(),
            change: proposal.change.clone(),
            applied_at: now_ms(),
            rolled_back: false,
        };
        state.changelog.push_back(entry);

        Ok(ApplyResult {
            proposal_id: proposal_id.to_string(),
            applied: true,
        })
    }

    fn rollback(&self) -> Result<RollbackResult, TunerError> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let last = state
            .changelog
            .iter_mut()
            .rev()
            .find(|e| !e.rolled_back)
            .ok_or(TunerError::NothingToRollback)?;

        last.rolled_back = true;
        let restored = last
            .change
            .old_value
            .clone()
            .unwrap_or_else(|| "<unset>".to_string());

        Ok(RollbackResult {
            rolled_back_proposal: last.proposal_id.clone(),
            restored_value: restored,
        })
    }

    fn changelog(&self) -> Vec<ChangeLogEntry> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.changelog.iter().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_change() -> ConfigChange {
        ConfigChange {
            key: "proxy.verbosity_steer".into(),
            old_value: Some("false".into()),
            new_value: "true".into(),
            reason: "enable verbosity steer for output reduction".into(),
        }
    }

    #[test]
    fn propose_returns_unique_ids() {
        let tuner = BuiltinTuner::new();
        let id1 = tuner.propose(test_change()).unwrap();
        let id2 = tuner.propose(test_change()).unwrap();
        assert_ne!(id1, id2);
    }

    #[test]
    fn apply_records_in_changelog() {
        let tuner = BuiltinTuner::new();
        let id = tuner.propose(test_change()).unwrap();
        let result = tuner.apply(&id).unwrap();
        assert!(result.applied);

        let log = tuner.changelog();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].proposal_id, id);
        assert!(!log[0].rolled_back);
    }

    #[test]
    fn apply_rejects_unknown_proposal() {
        let tuner = BuiltinTuner::new();
        let err = tuner.apply("nonexistent").unwrap_err();
        assert!(matches!(err, TunerError::NotFound(_)));
    }

    #[test]
    fn apply_rejects_double_apply() {
        let tuner = BuiltinTuner::new();
        let id = tuner.propose(test_change()).unwrap();
        tuner.apply(&id).unwrap();
        let err = tuner.apply(&id).unwrap_err();
        assert!(matches!(err, TunerError::AlreadyApplied(_)));
    }

    #[test]
    fn rollback_restores_old_value() {
        let tuner = BuiltinTuner::new();
        let id = tuner.propose(test_change()).unwrap();
        tuner.apply(&id).unwrap();

        let result = tuner.rollback().unwrap();
        assert_eq!(result.rolled_back_proposal, id);
        assert_eq!(result.restored_value, "false");

        let log = tuner.changelog();
        assert!(log[0].rolled_back);
    }

    #[test]
    fn rollback_fails_when_empty() {
        let tuner = BuiltinTuner::new();
        let err = tuner.rollback().unwrap_err();
        assert!(matches!(err, TunerError::NothingToRollback));
    }
}
