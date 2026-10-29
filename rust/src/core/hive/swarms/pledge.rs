// SPDX-License-Identifier: Apache-2.0
//! The pledge of an account in a swarm (SPEC-ENGINE §3.14.1 = CONTRACT
//! §3.20.7, fixture `enums/pledge-limits.json`; USD in integer cents, R107).

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::hive::consent::ConsentError;
use crate::core::hive::consent::proof::pledge_digest;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Interactive,
    Unattended,
}

/// `{from "HH:MM", to "HH:MM", days [mon..sun], tz}`; `to < from` spans
/// midnight.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Window {
    pub from: String,
    pub to: String,
    pub days: Vec<String>,
    pub tz: String,
}

/// The six pledge fields (E24 body; `GET` adds `engine_approved`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pledge {
    pub mode: Mode,
    pub tasks_per_day: u32,
    pub tokens_per_day: Option<u64>,
    pub usd_cents_per_day: Option<u64>,
    pub window: Option<Window>,
    pub max_sessions: u32,
}

/// E24 `GET` answer.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ServerPledge {
    #[serde(flatten)]
    pub pledge: Pledge,
    #[serde(default)]
    pub engine_approved: Option<bool>,
}

pub const DAYS: [&str; 7] = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];

/// The embedded C0 limits; a test keeps it byte-equal to the fixture.
pub(crate) const LIMITS_JSON: &str = include_str!("pledge-limits.json");

impl Default for Pledge {
    /// The C0 defaults (interactive, 5 tasks, 1 session, no other limit).
    fn default() -> Self {
        Pledge {
            mode: Mode::Interactive,
            tasks_per_day: 5,
            tokens_per_day: None,
            usd_cents_per_day: None,
            window: None,
            max_sessions: 1,
        }
    }
}

impl Pledge {
    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }

    pub fn digest(&self) -> Result<String, ConsentError> {
        pledge_digest(&self.to_value())
    }

    /// Bounds of `pledge-limits.json`; `cap` is the swarm's
    /// `participant_task_cap` when known.
    pub fn check(&self, cap: Option<u32>) -> Result<(), String> {
        let limits: Value = serde_json::from_str(LIMITS_JSON).map_err(|e| e.to_string())?;
        let bound = |field: &str, key: &str| {
            limits["values"]
                .as_array()
                .and_then(|v| v.iter().find(|l| l["value"] == field))
                .and_then(|l| l[key].as_u64())
        };
        let within = |field: &str, value: u64| {
            bound(field, "min").is_none_or(|min| value >= min)
                && bound(field, "max").is_none_or(|max| value <= max)
        };
        if !within("tasks_per_day", u64::from(self.tasks_per_day))
            || cap.is_some_and(|c| self.tasks_per_day > c)
        {
            return Err("tasks_per_day".into());
        }
        if !within("max_sessions", u64::from(self.max_sessions)) {
            return Err("max_sessions".into());
        }
        if self
            .tokens_per_day
            .is_some_and(|t| !within("tokens_per_day", t))
        {
            return Err("tokens_per_day".into());
        }
        if self
            .usd_cents_per_day
            .is_some_and(|u| !within("usd_cents_per_day", u))
            || (self.mode == Mode::Unattended && self.usd_cents_per_day.is_none())
        {
            return Err("usd_cents_per_day".into());
        }
        if let Some(window) = &self.window {
            let hhmm = |t: &str| {
                t.len() == 5
                    && t.as_bytes()[2] == b':'
                    && t[..2].parse::<u8>().is_ok_and(|h| h < 24)
                    && t[3..].parse::<u8>().is_ok_and(|m| m < 60)
            };
            let mut seen = window.days.clone();
            seen.sort();
            seen.dedup();
            if !hhmm(&window.from)
                || !hhmm(&window.to)
                || window.days.is_empty()
                || seen.len() != window.days.len()
                || !window.days.iter().all(|d| DAYS.contains(&d.as_str()))
                || window.tz.is_empty()
            {
                return Err("window".into());
            }
        }
        Ok(())
    }

    /// Whether `other` loosens this pledge in any field (§3.14.1).
    pub fn loosened_by(&self, other: &Pledge) -> bool {
        crate::core::hive::consent::proof::loosens(&self.to_value(), &other.to_value())
    }

    /// Per field the tighter of the two (§3.14.1 "Looser server pledge"):
    /// `min` for the counts (`None` = unlimited), interactive before
    /// unattended, and the approved window when the two differ (the
    /// intersection is never wider than either; the approved one is the
    /// conservative choice until `pledge_change` is approved).
    pub fn tighter(&self, other: &Pledge) -> Pledge {
        let min_opt = |a: Option<u64>, b: Option<u64>| match (a, b) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (Some(x), None) | (None, Some(x)) => Some(x),
            (None, None) => None,
        };
        Pledge {
            mode: if self.mode == Mode::Interactive || other.mode == Mode::Interactive {
                Mode::Interactive
            } else {
                Mode::Unattended
            },
            tasks_per_day: self.tasks_per_day.min(other.tasks_per_day),
            tokens_per_day: min_opt(self.tokens_per_day, other.tokens_per_day),
            usd_cents_per_day: min_opt(self.usd_cents_per_day, other.usd_cents_per_day),
            window: match (&self.window, &other.window) {
                (Some(a), Some(b)) if a == b => Some(a.clone()),
                (Some(a), _) => Some(a.clone()),
                (None, b) => b.clone(),
            },
            max_sessions: self.max_sessions.min(other.max_sessions),
        }
    }
}
