// SPDX-License-Identifier: Apache-2.0

//! Fact states (C0 1.9, section 3).
//!
//! A fact is `current` until an engine finds a reason to doubt it: one of its
//! anchors no longer matches the code (`anchor_changed`), the symbol moved
//! to another path (`anchor_moved`), the anchored path or symbol is gone
//! (`anchor_missing`), or another fact contradicts it (`contradiction`).
//! The engine reports states with
//! `POST /v1/vaults/{vault_id}/facts/states` ([`FactStateReportV1`],
//! right `knowledge.read`); Hive answers [`FactStateAckV1`] and lists the
//! non-current states with the revision ([`KnowledgeRevisionV1::fact_states`]).
//! A report replaces the reporting engine's earlier states for that
//! revision; a fact is `current` again when no engine reports it.
//!
//! A sealed vault reports `counts` only: which facts are in doubt, and why,
//! is information too, so the states stay on the engines.
//!
//! A state never changes the document. Fixing a fact is a proposal; a
//! `contradiction` with no proposal after 24 hours becomes a task (origin
//! `contradiction`).
//!
//! [`KnowledgeRevisionV1::fact_states`]: super::knowledge::KnowledgeRevisionV1::fact_states

use serde::{Deserialize, Serialize};

use super::anchor::CodeAnchorV1;
use super::knowledge::fact_id_ok;
use super::require_schema;
use crate::{UtcTimestamp, ValidationError};

pub const FACT_STATE_REPORT_SCHEMA: &str = "leanctx.hive.fact-state-report/v1";
pub const FACT_STATE_ACK_SCHEMA: &str = "leanctx.hive.fact-state-ack/v1";
/// Facts per report (a document holds at most `MAX_FACTS`).
pub const MAX_FACT_STATES: usize = super::knowledge::MAX_FACTS;
pub const MAX_STATE_REASONS: usize = 4;
pub const MAX_CONTRADICTED_BY: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactStateKind {
    Current,
    ToVerify,
    Contradicted,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactStateReason {
    AnchorChanged,
    AnchorMoved,
    AnchorMissing,
    Contradiction,
    #[serde(other)]
    Unknown,
}

/// The state of one fact, as an engine reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FactStateV1 {
    pub fact: String,
    /// `to_verify` or `contradicted`; a report never sends `current`.
    pub state: FactStateKind,
    /// Sorted, without repeats.
    pub reasons: Vec<FactStateReason>,
    /// With `anchor_moved`: where the symbol is now.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub moved_to: Option<CodeAnchorV1>,
    /// With `contradiction`: the facts that contradict this one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contradicted_by: Vec<String>,
    pub since: UtcTimestamp,
}

impl FactStateV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        if !fact_id_ok(&self.fact) {
            return Err(ValidationError::new("fact id is malformed"));
        }
        if self.reasons.is_empty()
            || self.reasons.len() > MAX_STATE_REASONS
            || self.reasons.windows(2).any(|pair| pair[0] >= pair[1])
            || self.reasons.contains(&FactStateReason::Unknown)
        {
            return Err(ValidationError::new(
                "reasons are 1-4 known reasons, sorted without repeats",
            ));
        }
        let contradiction = self.reasons.contains(&FactStateReason::Contradiction);
        match self.state {
            FactStateKind::Contradicted if !contradiction => {
                return Err(ValidationError::new(
                    "a contradicted fact has the reason contradiction",
                ));
            }
            FactStateKind::ToVerify if contradiction => {
                return Err(ValidationError::new(
                    "a contradiction makes the fact contradicted",
                ));
            }
            FactStateKind::Current | FactStateKind::Unknown => {
                return Err(ValidationError::new(
                    "a report carries to_verify or contradicted",
                ));
            }
            _ => {}
        }
        match (
            self.reasons.contains(&FactStateReason::AnchorMoved),
            &self.moved_to,
        ) {
            (true, Some(anchor)) => anchor.validate()?,
            (false, None) => {}
            _ => {
                return Err(ValidationError::new(
                    "moved_to comes exactly with anchor_moved",
                ));
            }
        }
        if contradiction == self.contradicted_by.is_empty()
            || self.contradicted_by.len() > MAX_CONTRADICTED_BY
            || self
                .contradicted_by
                .iter()
                .any(|id| !fact_id_ok(id) || id == &self.fact)
            || self
                .contradicted_by
                .windows(2)
                .any(|pair| pair[0] >= pair[1])
        {
            return Err(ValidationError::new(
                "contradicted_by lists 1-16 other facts, sorted, exactly with contradiction",
            ));
        }
        Ok(())
    }
}

/// How many facts are in doubt (sealed vaults).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FactStateCountsV1 {
    pub to_verify: u64,
    pub contradicted: u64,
}

/// `POST /v1/vaults/{vault_id}/facts/states`: exactly one of
/// `states` (clear vaults; may be empty: nothing in doubt) and `counts`
/// (sealed vaults).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FactStateReportV1 {
    pub schema: String,
    /// The released revision the engine checked.
    pub revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub states: Option<Vec<FactStateV1>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub counts: Option<FactStateCountsV1>,
}

impl FactStateReportV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        require_schema(&self.schema, FACT_STATE_REPORT_SCHEMA)?;
        if self.revision == 0 {
            return Err(ValidationError::new("revision 0 has no facts"));
        }
        match (&self.states, &self.counts) {
            (Some(states), None) => {
                if states.len() > MAX_FACT_STATES {
                    return Err(ValidationError::new(format!(
                        "at most {MAX_FACT_STATES} states"
                    )));
                }
                for (i, state) in states.iter().enumerate() {
                    state.validate()?;
                    if i > 0 && states[i - 1].fact >= state.fact {
                        return Err(ValidationError::new(
                            "states must be sorted by fact without repeats",
                        ));
                    }
                }
                Ok(())
            }
            (None, Some(_)) => Ok(()),
            _ => Err(ValidationError::new(
                "a report carries exactly one of states and counts",
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactStateAckV1 {
    pub schema: String,
    pub revision: u64,
    /// Facts not `current` across all engines' reports for that revision.
    pub to_verify: u64,
    pub contradicted: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(reasons: Vec<FactStateReason>, kind: FactStateKind) -> FactStateV1 {
        FactStateV1 {
            fact: "retry.budget".into(),
            state: kind,
            reasons,
            moved_to: None,
            contradicted_by: Vec::new(),
            since: "2026-10-22T08:00:00Z".parse().expect("time"),
        }
    }

    #[test]
    fn states_follow_their_reasons() {
        assert!(
            state(
                vec![FactStateReason::AnchorChanged],
                FactStateKind::ToVerify
            )
            .validate()
            .is_ok()
        );
        let mut contradicted = state(
            vec![FactStateReason::Contradiction],
            FactStateKind::Contradicted,
        );
        assert!(
            contradicted.validate().is_err(),
            "names no contradicting fact"
        );
        contradicted.contradicted_by = vec!["retry.limit".into()];
        assert!(contradicted.validate().is_ok());
        assert!(
            state(
                vec![FactStateReason::Contradiction],
                FactStateKind::ToVerify
            )
            .validate()
            .is_err()
        );
        assert!(
            state(vec![FactStateReason::AnchorMoved], FactStateKind::ToVerify)
                .validate()
                .is_err(),
            "anchor_moved needs moved_to"
        );
        assert!(
            state(vec![FactStateReason::AnchorChanged], FactStateKind::Current)
                .validate()
                .is_err()
        );
    }
}
