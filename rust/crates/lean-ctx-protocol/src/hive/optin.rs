// SPDX-License-Identifier: Apache-2.0

//! Engine opt-in (C0 1.9, section 4; owner decision O1).
//!
//! An engine's agents work on tasks only by *pulling* them, and only within
//! the opt-in its owner set on the web: which vaults and networks, which
//! topics, how much per day, and whether at all. Hive never starts an agent;
//! nothing here spawns anything.
//!
//! `GET /v1/me/optin` (instance token) → [`EngineOptinV1`]. The owner
//! changes it on the web; every change gets a new `optin_id`, and
//! `active: false` (the stop) ends every claim of that engine at once.
//! The engine may stop itself, never start: `PUT /v1/me/optin`
//! [`EngineOptinStopV1`] → [`EngineOptinV1`]; `active: true` there is
//! `403 forbidden_right` reason `owner_only`.
//!
//! Hive is the source of truth and enforces it on every claim:
//! - a claim carries the `optin_id` the engine last read (section 1
//!   `TaskClaimRequestV1.optin_id`, required since this section; without it
//!   `400 invalid_request` field `optin_id`);
//! - an older `optin_id` → `409 invalid_state` reason `optin_changed`: the
//!   engine reads the opt-in again before it pulls;
//! - `active: false`, or no opt-in → `409 invalid_state` reason
//!   `optin_inactive`;
//! - a vault or network outside the opt-in → `403 forbidden_right` reason
//!   `optin_scope`;
//! - the day's budget used up → `429 rate_limited` reason `daily_budget`,
//!   with `limit`, `used` (claims) and `retry_after_s` until 00:00 UTC.
//!
//! Usage counts per UTC day: a claim counts one task when it is granted; the
//! `usage` of task results (section 1) adds tokens and `usd_micros`. A claim
//! is refused once any set limit is reached; a running claim is never cut
//! short by the budget. A granted claim counts atomically: parallel claims
//! of one engine never exceed `daily_budget.tasks` together.
//!
//! An engine may apply stricter local caps (fewer tasks, a lower budget, a
//! narrower scope). It never widens what Hive allows.

use serde::{Deserialize, Serialize};

use super::messages::NetworkV1;
use super::{require_schema, validate_id};
use crate::{UtcTimestamp, ValidationError};

pub const ENGINE_OPTIN_SCHEMA: &str = "leanctx.hive.engine-optin/v1";
pub const ENGINE_OPTIN_STOP_SCHEMA: &str = "leanctx.hive.engine-optin-stop/v1";
pub const DEFAULT_DAILY_TASKS: u32 = 5;
pub const MAX_DAILY_TASKS: u32 = 100;
pub const MAX_OPTIN_VAULTS: usize = 200;
pub const MAX_OPTIN_NETWORKS: usize = 200;
pub const MAX_OPTIN_TOPICS: usize = 5;

/// Limits per UTC day. A limit that is absent is no limit; since Hive
/// task #19 (owner decision 23.10.) that holds for the task count too.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DailyBudgetV1 {
    /// Claims per day, 1-100 (default 5); `None`: no limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tasks: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usd_micros: Option<u64>,
}

impl Default for DailyBudgetV1 {
    fn default() -> Self {
        Self {
            tasks: Some(DEFAULT_DAILY_TASKS),
            tokens: None,
            usd_micros: None,
        }
    }
}

/// What the engine used today (UTC).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DailyUsageV1 {
    /// `YYYY-MM-DD`, UTC.
    pub day: String,
    pub tasks: u32,
    pub tokens: u64,
    pub usd_micros: u64,
}

/// `GET /v1/me/optin`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineOptinV1 {
    pub schema: String,
    /// Absent when the owner never opted this engine in (then `active` is
    /// false and every list is empty).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub optin_id: Option<String>,
    pub active: bool,
    /// Vaults (`vault_id`) whose tasks the agents may pull.
    #[serde(default)]
    pub vaults: Vec<String>,
    /// Networks whose questions and swarm subtasks they may pull
    /// (section 2 `NetworkV1`).
    #[serde(default)]
    pub networks: Vec<NetworkV1>,
    /// Swarm topics (2-32 of `a-z`, `0-9`, `-`).
    #[serde(default)]
    pub topics: Vec<String>,
    pub daily_budget: DailyBudgetV1,
    pub used_today: DailyUsageV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stopped_at: Option<UtcTimestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<UtcTimestamp>,
}

fn topic_ok(topic: &str) -> bool {
    (2..=32).contains(&topic.len())
        && topic
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn day_ok(day: &str) -> bool {
    let b = day.as_bytes();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && [0..4, 5..7, 8..10]
            .iter()
            .all(|r| b[r.clone()].iter().all(u8::is_ascii_digit))
}

impl EngineOptinV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        require_schema(&self.schema, ENGINE_OPTIN_SCHEMA)?;
        match &self.optin_id {
            Some(id) => validate_id(id, "optin_id")?,
            None if self.active
                || !self.vaults.is_empty()
                || !self.networks.is_empty()
                || !self.topics.is_empty() =>
            {
                return Err(ValidationError::new(
                    "an engine without opt-in is inactive with empty scope",
                ));
            }
            None => {}
        }
        if self.vaults.len() > MAX_OPTIN_VAULTS || self.networks.len() > MAX_OPTIN_NETWORKS {
            return Err(ValidationError::new(format!(
                "at most {MAX_OPTIN_VAULTS} vaults and {MAX_OPTIN_NETWORKS} networks"
            )));
        }
        for (i, vault) in self.vaults.iter().enumerate() {
            super::validate_vault_id(vault)?;
            if i > 0 && self.vaults[i - 1] >= *vault {
                return Err(ValidationError::new(
                    "vaults must be sorted without repeats",
                ));
            }
        }
        if self.topics.len() > MAX_OPTIN_TOPICS
            || !self.topics.iter().all(|t| topic_ok(t))
            || self.topics.windows(2).any(|pair| pair[0] >= pair[1])
        {
            return Err(ValidationError::new(
                "up to 5 topics of 2-32 [a-z0-9-], sorted without repeats",
            ));
        }
        if self
            .daily_budget
            .tasks
            .is_some_and(|t| !(1..=MAX_DAILY_TASKS).contains(&t))
            || self.daily_budget.tokens == Some(0)
        {
            return Err(ValidationError::new(
                "daily_budget is 1-100 tasks or none, and a non-zero token limit",
            ));
        }
        if !day_ok(&self.used_today.day) {
            return Err(ValidationError::new("used_today.day is YYYY-MM-DD"));
        }
        if self.active == self.stopped_at.is_some() && self.optin_id.is_some() {
            return Err(ValidationError::new(
                "stopped_at is set exactly while the opt-in is inactive",
            ));
        }
        Ok(())
    }
}

/// `PUT /v1/me/optin`: the engine's own stop.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineOptinStopV1 {
    pub schema: String,
    /// Always `false`; only the owner turns an opt-in on.
    pub active: bool,
}

impl EngineOptinStopV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        require_schema(&self.schema, ENGINE_OPTIN_STOP_SCHEMA)?;
        if self.active {
            return Err(ValidationError::new("an engine can only stop its opt-in"));
        }
        Ok(())
    }
}
