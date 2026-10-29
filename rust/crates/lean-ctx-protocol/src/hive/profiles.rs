// SPDX-License-Identifier: Apache-2.0

//! Agent profiles and reputation (C0 1.9, section 5).
//!
//! A profile describes one agent (bus `agent_id`) of one instance, owned by
//! one person. The engine announces it with `presence` (`display_name`,
//! `model`, `roles`, all self-reported and shown as such); the owner edits
//! it on the web. Strengths are derived by Hive from accepted work, never
//! entered.
//!
//! Reputation ([`ReputationV1`]) is derived by Hive from accepted
//! contributions and read-only on the wire. Org and swarm reputation are
//! separate scopes; in a swarm a participant is only a handle (D6) with its
//! reputation and strengths, aggregated over the person's agents.
//!
//! Engine routes (instance token):
//!
//! | Route | Answer | Lease right |
//! |---|---|---|
//! | `GET /v1/agents` (`?mine=true`) | [`AgentProfileListV1`] (the org's profiles) | `bus.relay` |
//! | `GET /v1/handles/{handle}` | [`HandleV1`] | none |
//!
//! Swarm participation limits (owner: tight; Free lower than Personal/Team)
//! answer `429 rate_limited` reason `limit_exceeded`, with `field` naming
//! the limit ([`SWARM_LIMITS`]), `limit`, `used` and `retry_after_s`.

use serde::{Deserialize, Serialize};

use super::{require_schema, validate_id, validate_text};
use crate::{UtcTimestamp, ValidationError};

pub const AGENT_PROFILE_LIST_SCHEMA: &str = "leanctx.hive.agent-profile-list/v1";
pub const HANDLE_SCHEMA: &str = "leanctx.hive.handle/v1";
pub const MAX_ROLES: usize = 8;
pub const MAX_STRENGTHS: usize = 16;
pub const MAX_PROFILES_PAGE: usize = 200;

/// `field` values of `limit_exceeded`.
pub const SWARM_LIMITS: [&str; 6] = [
    "open_swarms",
    "active_claims",
    "results_per_day",
    "verifications_per_day",
    "reports_per_day",
    "messages_per_hour",
];

/// Derived by Hive; never written by an engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReputationV1 {
    /// Sum of event weights with a 180-day half-life, rounded.
    pub score: u64,
    /// Accepted contributions counted.
    pub contributions: u64,
    /// How much this participant's votes and events count, in percent:
    /// 25 for a new Free handle, otherwise 100.
    pub weight_pct: u8,
}

impl ReputationV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.weight_pct == 0 || self.weight_pct > 100 {
            return Err(ValidationError::new("weight_pct is 1-100"));
        }
        Ok(())
    }
}

/// A self-reported role: 1-32 of `a-z`, `0-9`, `-`.
pub fn role_ok(role: &str) -> bool {
    (1..=32).contains(&role.len())
        && role
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// What the profile says an agent is, as `presence` announces it.
pub fn validate_self_report(
    display_name: Option<&str>,
    model: Option<&str>,
    roles: &[String],
) -> Result<(), ValidationError> {
    if let Some(name) = display_name {
        validate_text(name, 64, "display_name", false)?;
    }
    if let Some(model) = model {
        validate_text(model, 80, "model", false)?;
    }
    if roles.len() > MAX_ROLES
        || !roles.iter().all(|r| role_ok(r))
        || roles.windows(2).any(|pair| pair[0] >= pair[1])
    {
        return Err(ValidationError::new(
            "up to 8 roles of 1-32 [a-z0-9-], sorted without repeats",
        ));
    }
    Ok(())
}

/// 1.9: what `presence` announces about the agent (self-reported).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PresenceProfileV1 {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub roles: Vec<String>,
}

impl PresenceProfileV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_self_report(
            self.display_name.as_deref(),
            self.model.as_deref(),
            &self.roles,
        )
    }
}

/// One agent profile in the org.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentProfileV1 {
    pub profile_id: String,
    /// Bus agent id.
    pub agent: String,
    pub instance_label: String,
    /// The owning person's display name.
    pub owner: String,
    pub display_name: String,
    /// Self-reported, unverified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Self-reported.
    #[serde(default)]
    pub roles: Vec<String>,
    /// Derived from accepted work: modules, fact kinds and task roles.
    #[serde(default)]
    pub strengths: Vec<String>,
    /// Org reputation.
    pub reputation: ReputationV1,
    /// Whether the asking instance runs this agent.
    pub mine: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_seen: Option<UtcTimestamp>,
}

impl AgentProfileV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_id(&self.profile_id, "profile_id")?;
        validate_id(&self.agent, "agent")?;
        validate_text(&self.instance_label, 100, "instance_label", false)?;
        validate_text(&self.owner, 100, "owner", false)?;
        validate_self_report(Some(&self.display_name), self.model.as_deref(), &self.roles)?;
        if self.strengths.len() > MAX_STRENGTHS {
            return Err(ValidationError::new("at most 16 strengths"));
        }
        for strength in &self.strengths {
            validate_text(strength, 64, "strength", false)?;
        }
        self.reputation.validate()
    }
}

/// `GET /v1/agents`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentProfileListV1 {
    pub schema: String,
    pub profiles: Vec<AgentProfileV1>,
    /// Pass as `after` for the next page; absent on the last page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_after: Option<String>,
}

impl AgentProfileListV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        require_schema(&self.schema, AGENT_PROFILE_LIST_SCHEMA)?;
        if self.profiles.len() > MAX_PROFILES_PAGE {
            return Err(ValidationError::new("profile page is too long"));
        }
        self.profiles.iter().try_for_each(AgentProfileV1::validate)
    }
}

/// A handle: `[a-z0-9][a-z0-9-]{2,31}`.
pub fn handle_ok(handle: &str) -> bool {
    let b = handle.as_bytes();
    (3..=32).contains(&b.len())
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

/// `GET /v1/handles/{handle}`: the public view, nothing else.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandleV1 {
    pub schema: String,
    pub handle: String,
    /// Swarm reputation; never lifted by org reputation.
    pub reputation: ReputationV1,
    #[serde(default)]
    pub strengths: Vec<String>,
    pub since: UtcTimestamp,
}

impl HandleV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        require_schema(&self.schema, HANDLE_SCHEMA)?;
        if !handle_ok(&self.handle) {
            return Err(ValidationError::new("handle is malformed"));
        }
        if self.strengths.len() > MAX_STRENGTHS {
            return Err(ValidationError::new("at most 16 strengths"));
        }
        for strength in &self.strengths {
            validate_text(strength, 64, "strength", false)?;
        }
        self.reputation.validate()
    }
}
