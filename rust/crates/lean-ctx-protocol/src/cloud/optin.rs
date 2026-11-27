// SPDX-License-Identifier: Apache-2.0

//! Engine opt-in (C0 1.9, section 4; owner decision O1).
//!
//! An engine's agents work on tasks only by *pulling* them, and only within
//! the opt-in its owner set on the web: which vaults and networks, how
//! much per day, and whether at all. Cloud never starts an agent;
//! nothing here spawns anything.
//!
//! `GET /v1/me/optin` (instance token) → [`EngineOptinV1`]. The owner
//! changes it on the web; every change gets a new `optin_id`, and
//! `active: false` (the stop) ends every claim of that engine at once.
//! The engine may stop itself: `PUT /v1/me/optin` [`EngineOptinStopV1`] →
//! [`EngineOptinV1`]. It turns task pickup on only with the person at the
//! computer (rc4-16): `PUT /v1/me/optin` [`EngineOptinOnV1`], signed with
//! the app's bound opt-in key (no per-turn-on prompt) over [`OPTIN_SIGNING_DOMAIN`] ‖
//! canonical JSON of the body without `presence_signature`, plus
//! `instance_id`. Only the bound opt-in key signs it; there is no
//! presence-key fallback. The instance token alone never turns it on
//! (`403 forbidden_right` reason `person_unproven`); without a bound opt-in
//! key the engine refuses it before sending (`no_optin_key`: pair LeanCTX
//! Code with the computer first). Turning it off needs no key.
//!
//! Cloud is the source of truth and enforces it on every claim:
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
//! narrower scope). It never widens what Cloud allows.

use serde::{Deserialize, Serialize};

use super::{require_schema, validate_id};
use crate::{UtcTimestamp, ValidationError};

/// A network an engine may pull questions from: an org channel
/// (`org:<org_id>/vault:<vault_id>/<name>`). Any other kind does not decode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum NetworkV1 {
    Org { channel: String },
}

impl NetworkV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        match self {
            Self::Org { channel } => validate_channel(channel),
        }
    }
}

/// Validate `org:<org>/vault:<vault_id>/<name>`.
fn validate_channel(channel: &str) -> Result<(), ValidationError> {
    let mut parts = channel.splitn(3, '/');
    let org = parts.next().and_then(|part| part.strip_prefix("org:"));
    let vault = parts.next().and_then(|part| part.strip_prefix("vault:"));
    let name = parts.next();
    match (org, vault, name) {
        (Some(org), Some(vault), Some(name)) => {
            validate_id(org, "channel org")?;
            super::validate_vault_id(vault)?;
            validate_id(name, "channel name")
        }
        _ => Err(ValidationError::new(
            "channel must be org:<org>/vault:<vault>/<name>",
        )),
    }
}

pub const ENGINE_OPTIN_SCHEMA: &str = "leanctx.cloud.instance-optin/v1";
pub const ENGINE_OPTIN_STOP_SCHEMA: &str = "leanctx.cloud.instance-optin-stop/v1";
/// `PUT /v1/me/optin` with `active: true` (rc4-16).
pub const ENGINE_OPTIN_ON_SCHEMA: &str = "leanctx.cloud.instance-optin-on/v1";
/// The domain the person's presence signature of a turn-on is made under.
pub const OPTIN_SIGNING_DOMAIN: &str = "leanctx.cloud.optin.v1\n";
pub const DEFAULT_DAILY_TASKS: u32 = 5;
pub const MAX_DAILY_TASKS: u32 = 100;
pub const MAX_OPTIN_VAULTS: usize = 200;
pub const MAX_OPTIN_NETWORKS: usize = 200;

/// Limits per UTC day. A limit that is absent is no limit; since Cloud
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

impl DailyBudgetV1 {
    /// The budget task pickup starts with when it is turned on for the
    /// person's own vaults without a budget of theirs (owner decision rc3-72
    /// part 4): 3 tasks and 1M tokens a day, no cost limit.
    pub const AUTO: Self = Self {
        tasks: Some(3),
        tokens: Some(1_000_000),
        usd_micros: None,
    };

    /// 1-100 tasks or none, and a non-zero token limit or none.
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self
            .tasks
            .is_some_and(|t| !(1..=MAX_DAILY_TASKS).contains(&t))
            || self.tokens == Some(0)
        {
            return Err(ValidationError::new(
                "daily_budget is 1-100 tasks or none, and a non-zero token limit",
            ));
        }
        Ok(())
    }
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
    /// Networks whose questions they may pull ([`NetworkV1`]).
    #[serde(default)]
    pub networks: Vec<NetworkV1>,
    pub daily_budget: DailyBudgetV1,
    pub used_today: DailyUsageV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stopped_at: Option<UtcTimestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<UtcTimestamp>,
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
            None if self.active || !self.vaults.is_empty() || !self.networks.is_empty() => {
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
        self.daily_budget.validate()?;
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
    /// Always `false`; turning on is [`EngineOptinOnV1`].
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

/// `PUT /v1/me/optin`: task pickup turned on from the computer, with the
/// person at it (rc4-16). `presence_signature` is the signature of the
/// app's bound opt-in key only (never the presence key; the field keeps its
/// name on the wire) (base64url, no padding) over [`OPTIN_SIGNING_DOMAIN`] ‖
/// canonical JSON of the body without it, plus `"instance_id"` (the
/// instance UUID, lowercase with hyphens). Cloud refuses a `prev_optin_id`
/// that is not the opt-in's current one (`409 invalid_state` reason
/// `optin_changed`), so a signature works at most once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineOptinOnV1 {
    pub schema: String,
    /// Always `true`.
    pub active: bool,
    /// The `optin_id` `GET /v1/me/optin` showed when the person signed;
    /// `null` only while there is none. Always sent.
    pub prev_optin_id: Option<String>,
    /// Vaults (project ids); left out means none (it replaces the list).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vaults: Option<Vec<String>>,
    pub daily_budget: DailyBudgetV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presence_signature: Option<String>,
}

impl EngineOptinOnV1 {
    pub fn validate(&self) -> Result<(), ValidationError> {
        require_schema(&self.schema, ENGINE_OPTIN_ON_SCHEMA)?;
        if !self.active {
            return Err(ValidationError::new(
                "a turn-on is active; a stop is the stop schema",
            ));
        }
        if let Some(id) = &self.prev_optin_id {
            validate_id(id, "prev_optin_id")?;
        }
        let vaults = self.vaults.as_deref().unwrap_or_default();
        if vaults.len() > MAX_OPTIN_VAULTS {
            return Err(ValidationError::new(format!(
                "at most {MAX_OPTIN_VAULTS} vaults"
            )));
        }
        for vault in vaults {
            super::validate_vault_id(vault)?;
        }
        self.daily_budget.validate()
    }
}

#[cfg(test)]
mod on_tests {
    use super::*;

    fn on() -> EngineOptinOnV1 {
        EngineOptinOnV1 {
            schema: ENGINE_OPTIN_ON_SCHEMA.into(),
            active: true,
            prev_optin_id: None,
            vaults: Some(vec!["0f3c2b6a8d4e4a7b9c105e2f6a7b8c01".into()]),
            daily_budget: DailyBudgetV1::AUTO,
            presence_signature: None,
        }
    }

    #[test]
    fn pickup_turn_on_body_keeps_a_null_prev_optin_id_and_the_auto_budget() {
        let body = serde_json::to_value(on()).unwrap();
        assert!(body["prev_optin_id"].is_null());
        assert!(body.as_object().unwrap().contains_key("prev_optin_id"));
        assert!(!body.as_object().unwrap().contains_key("presence_signature"));
        assert_eq!(
            body["daily_budget"],
            serde_json::json!({"tasks": 3, "tokens": 1_000_000})
        );
        on().validate().unwrap();
    }

    #[test]
    fn pickup_turn_on_refuses_a_stop_a_bad_vault_and_a_bad_budget() {
        let mut stop = on();
        stop.active = false;
        assert!(stop.validate().is_err());
        let mut vault = on();
        vault.vaults = Some(vec!["notes".into()]);
        assert!(vault.validate().is_err());
        let mut budget = on();
        budget.daily_budget.tasks = Some(0);
        assert!(budget.validate().is_err());
        let extra = serde_json::json!({
            "schema": ENGINE_OPTIN_ON_SCHEMA, "active": true, "prev_optin_id": null,
            "daily_budget": {"tasks": 3}, "networks": []
        });
        assert!(serde_json::from_value::<EngineOptinOnV1>(extra).is_err());
    }
}
