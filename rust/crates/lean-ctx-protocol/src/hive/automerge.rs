// SPDX-License-Identifier: Apache-2.0

//! Auto-merge by confirmation threshold (C0 1.9, section 7; owner O2, D1,
//! X3, X-D1/X-D2).
//!
//! The vault owner sets [`AutoMergeV1`] on the web (`VaultV1.auto_merge`).
//! Without it a person merges every proposal. With it, independent
//! confirmations merge a proposal once `confirmations` are reached:
//! - `distinct_sessions` (Personal, default 1): sessions on the owner's other
//!   instances. D1 holds per instance (lead decision, RB7): the instance that
//!   wrote the proposal, or claimed the task and submitted its result, never
//!   counts toward its own result, whichever of its sessions confirms; such a
//!   confirmation is `409 invalid_state` reason `reviewer_not_independent`
//!   (fixture `error.reviewer_own_instance`). A session counts
//!   only if Hive saw it from an instance of the same owner before the
//!   proposal was created (an earlier proposal or task result carrying that
//!   `session_ref`), and only while the confirming instance is live on the
//!   bus: a `session_ref` is chosen by the engine, so Hive never counts one
//!   it has not seen before (R3-C1);
//! - `independent_owners` (Team, default 1; swarms): agents of other owners
//!   or people; never the author's own instances (X3).
//!
//! An engine confirms (or objects to) a proposal with
//! `POST /v1/vaults/{vault_id}/proposals/{number}/confirmations`
//! [`ProposalConfirmV1`] → [`ProposalConfirmAckV1`] (right
//! `knowledge.draft`). A confirmation that may not count is
//! `409 invalid_state` reason `reviewer_not_independent`; one session counts
//! once. An objection stops the threshold: a person decides then.
//!
//! When the threshold is reached, Hive merges a clear proposal itself
//! (`decided_by` threshold). A sealed proposal becomes `ready`: Hive cannot
//! read it, so the next engine with `knowledge.merge` merges it like a
//! person's merge (`merged_proposals`). A threshold merge in Team and swarms
//! starts `quarantined` (section 3); in Personal it is released at once (X-D2).
//! A person can always merge or reject by hand.

use serde::{Deserialize, Serialize};

use super::knowledge::ProposalState;
use super::{require_schema, validate_id, validate_text};
use crate::ValidationError;

pub const PROPOSAL_CONFIRM_SCHEMA: &str = "leanctx.hive.proposal-confirm/v1";
pub const PROPOSAL_CONFIRM_ACK_SCHEMA: &str = "leanctx.hive.proposal-confirm-ack/v1";
pub const MAX_CONFIRMATIONS: u32 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfirmationCount {
    /// Personal: distinct sessions on the owner's instances.
    DistinctSessions,
    /// Team and swarms: other owners' agents or people.
    IndependentOwners,
    #[serde(other)]
    Unknown,
}

/// `VaultV1.auto_merge`: absent means people merge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AutoMergeV1 {
    /// 1-5.
    pub confirmations: u32,
    pub counts: ConfirmationCount,
}

impl AutoMergeV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        if !(1..=MAX_CONFIRMATIONS).contains(&self.confirmations)
            || self.counts == ConfirmationCount::Unknown
        {
            return Err(ValidationError::new(
                "auto_merge is 1-5 confirmations of a known kind",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfirmVerdict {
    /// The proposal is right as written.
    Confirm,
    /// Something is wrong; a person decides.
    Object,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProposalConfirmV1 {
    pub schema: String,
    /// The confirming session (opaque, as in drafts).
    pub session_ref: String,
    pub verdict: ConfirmVerdict,
    /// Required with `object`, in the clear only (a sealed vault sends
    /// none: people read the reason in their engine).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl ProposalConfirmV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        require_schema(&self.schema, PROPOSAL_CONFIRM_SCHEMA)?;
        validate_id(&self.session_ref, "session_ref")?;
        if self.verdict == ConfirmVerdict::Unknown {
            return Err(ValidationError::new("verdict is confirm or object"));
        }
        if let Some(reason) = &self.reason {
            validate_text(reason, 500, "reason", true)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProposalConfirmAckV1 {
    pub schema: String,
    pub number: u64,
    /// Counted confirmations so far.
    pub confirmations: u32,
    /// The threshold (`AutoMergeV1.confirmations`).
    pub required: u32,
    /// `open`, `ready` (sealed, waiting for an engine merge) or `merged`.
    pub state: ProposalState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merged_revision: Option<u64>,
    /// An objection is pending: no threshold merge until a person decides.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub objected: bool,
}

impl ProposalConfirmAckV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        require_schema(&self.schema, PROPOSAL_CONFIRM_ACK_SCHEMA)?;
        if (self.state == ProposalState::Merged) != self.merged_revision.is_some() {
            return Err(ValidationError::new(
                "merged_revision comes exactly with a merged proposal",
            ));
        }
        Ok(())
    }
}
