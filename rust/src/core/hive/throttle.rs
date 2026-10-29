// SPDX-License-Identifier: Apache-2.0
//! Quota and policy throttles (Release B, Hive API §7.2 and §8.1).
//!
//! When Hive answers `429 quota_exceeded` (monthly transfer or bus
//! messages), `409 quota_exceeded` for a cooling-down Personal engine slot, or
//! `403 policy_denied`, the engine records a throttle in
//! `<data_dir>/throttles.json` and every path it applies to waits instead
//! of asking again: sync and pushes, bus publishes, task claims, connect.
//! The local work stays where it is (the replica's pending events, the bus
//! queue, the outbox) and goes once the throttle ends.
//!
//! Backoff rules (deterministic, never a busy loop):
//! - monthly quota (429): until `now + retry_after_s` (the body's value, or
//!   the `Retry-After` header), else `reset_at`, else the start of the next
//!   calendar month (UTC), the reset the contract names; at least
//!   [`MIN_WAIT_S`], at most [`MAX_WAIT_S`];
//! - Personal slot cool-down (409 `registered_engines`, `cooldown_days`): until
//!   `reset_at` (the web recovery frees it sooner, P7);
//! - policy refusal (403) of a bus publish or a task claim:
//!   [`POLICY_HOLD_S`] per member and path, then one try (an admin may have
//!   changed the policy meanwhile), and at once when a newer policy is
//!   installed ([`policy_changed`]); a sync refusal holds only its draft
//!   (outbox, same wait, also ended by a newer policy) or its vault sync,
//!   never the other vaults;
//! - other counted quotas (409 `private_vaults`, a slot without
//!   cool-down): no throttle; the refusal is shown and nothing retries it
//!   by itself.
//!
//! `lean-ctx hive status` lists the active throttles ([`status_lines`]).

use std::collections::BTreeMap;

use chrono::{DateTime, Datelike as _, TimeZone as _, Utc};
use serde::{Deserialize, Serialize};

use super::HiveError;
use super::state::{HiveStore, load_or_quarantine, lock_state, write_private};

pub(crate) const THROTTLE_FILE: &str = "throttles.json";
/// The shortest wait of a throttle: one tick of a loop is never a retry.
pub const MIN_WAIT_S: i64 = 60;
/// The longest wait (a reset further out is shortened to this).
pub const MAX_WAIT_S: i64 = 32 * 86_400;
/// How long a policy refusal holds its path.
pub const POLICY_HOLD_S: i64 = 15 * 60;
/// A quota throttle lets one call through this often before it ends, so a
/// quota that was raised or lowered in Hive is noticed (E2E-FINAL: sync
/// waited for the month's end after usage was lowered).
pub const QUOTA_PROBE_S: i64 = 3600;

/// The engine paths a throttle can hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Scope {
    /// Knowledge sync, event tail and push, snapshots (transfer).
    Sync,
    /// Bus publishes (bus messages).
    Bus,
    /// Task claims, renewals and results.
    Tasks,
    /// A new engine registration (`hive connect`).
    Connect,
}

impl Scope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sync => "sync",
            Self::Bus => "bus",
            Self::Tasks => "tasks",
            Self::Connect => "connect",
        }
    }
}

/// The paths a quota resource holds besides the one that met it.
fn resource_scopes(resource: &str) -> &'static [Scope] {
    match resource {
        // API §8.1: transfer counts the engine data routes (event push and
        // tail, snapshots and segments, knowledge, graphs, bus history).
        "transfer_bytes" => &[Scope::Sync],
        "bus_messages" => &[Scope::Bus],
        "registered_engines" => &[Scope::Connect],
        _ => &[],
    }
}

/// One recorded throttle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Throttle {
    /// Sorted scope names it holds.
    pub scopes: Vec<String>,
    /// When it ends, second precision `Z`.
    pub until: String,
    /// The refusal, as `HiveError` shows it.
    pub line: String,
    /// A quota throttle's next probe ([`QUOTA_PROBE_S`]): from then one
    /// call may pass, and its success ends the throttle ([`passed`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probe_at: Option<String>,
}

impl Throttle {
    fn holds(&self, scope: Scope) -> bool {
        self.scopes.iter().any(|s| s == scope.as_str())
    }

    fn active_at(&self, now: DateTime<Utc>) -> bool {
        DateTime::parse_from_rfc3339(&self.until).is_ok_and(|until| until > now)
    }

    /// The error a held path answers instead of calling Hive.
    #[must_use]
    pub fn refusal(&self) -> HiveError {
        HiveError::Refused(format!(
            "waiting for Hive until {}: {}",
            self.until, self.line
        ))
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Throttles {
    /// Key (`quota:<resource>` or `policy:<member>:<scope>`) → throttle.
    #[serde(default)]
    items: BTreeMap<String, Throttle>,
}

fn stamp(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// 00:00 UTC on the first day of the month after `now`'s.
fn next_month(now: DateTime<Utc>) -> DateTime<Utc> {
    let (year, month) = if now.month() == 12 {
        (now.year() + 1, 1)
    } else {
        (now.year(), now.month() + 1)
    };
    Utc.with_ymd_and_hms(year, month, 1, 0, 0, 0)
        .single()
        .unwrap_or(now + chrono::Duration::days(31))
}

/// `until` itself when it is [`MIN_WAIT_S`]..=[`MAX_WAIT_S`] away (so a
/// `reset_at` is kept to the second), else the nearer bound.
fn clamp(now: DateTime<Utc>, until: DateTime<Utc>) -> DateTime<Utc> {
    let wait = (until - now).num_seconds();
    if (MIN_WAIT_S..=MAX_WAIT_S).contains(&wait) {
        until
    } else {
        now + chrono::Duration::seconds(wait.clamp(MIN_WAIT_S, MAX_WAIT_S))
    }
}

/// The throttle `error` sets at `now`, if it sets one: on the paths its
/// quota resource holds plus `scope`, the path that met it (`None`: an
/// HTTP call of unknown path, so only the resource's paths; a policy
/// refusal then sets nothing).
#[must_use]
pub fn of_error(
    scope: Option<Scope>,
    error: &HiveError,
    now: DateTime<Utc>,
) -> Option<(String, Throttle)> {
    let (key, scopes, until) = match error {
        HiveError::QuotaExceeded(q) if q.waits() => {
            let until = match error.retry_after_at(now) {
                // Clamped before the arithmetic: a huge `retry_after_s`
                // (up to u64::MAX) must not overflow chrono (a panic).
                Some(seconds) => {
                    let wait = i64::try_from(seconds)
                        .unwrap_or(MAX_WAIT_S)
                        .clamp(MIN_WAIT_S, MAX_WAIT_S);
                    now + chrono::Duration::seconds(wait)
                }
                None => next_month(now),
            };
            (
                format!("quota:{}", q.resource),
                resource_scopes(&q.resource),
                clamp(now, until),
            )
        }
        HiveError::QuotaExceeded(q) if q.cooling_slot() => {
            let reset = DateTime::parse_from_rfc3339(q.reset_at.as_deref()?)
                .ok()?
                .with_timezone(&Utc);
            (
                format!("quota:{}", q.resource),
                resource_scopes(&q.resource),
                clamp(now, reset),
            )
        }
        HiveError::PolicyDenied(d) => (
            format!(
                "policy:{}:{}",
                d.member.as_deref().unwrap_or("unknown"),
                scope?.as_str()
            ),
            &[][..],
            now + chrono::Duration::seconds(POLICY_HOLD_S),
        ),
        _ => return None,
    };
    let mut names: Vec<String> = scope
        .into_iter()
        .chain(scopes.iter().copied())
        .map(|s| s.as_str().to_string())
        .collect();
    names.sort();
    names.dedup();
    if names.is_empty() {
        return None;
    }
    let probe = now + chrono::Duration::seconds(QUOTA_PROBE_S);
    let probe_at = (key.starts_with("quota:") && until > probe).then(|| stamp(probe));
    Some((
        key,
        Throttle {
            scopes: names,
            until: stamp(until),
            line: error.to_string(),
            probe_at,
        },
    ))
}

fn read(store: &HiveStore) -> Throttles {
    load_or_quarantine(&store.data_dir().join(THROTTLE_FILE), |_: &Throttles| true)
        .ok()
        .flatten()
        .unwrap_or_default()
}

fn change(store: &HiveStore, edit: impl FnOnce(&mut Throttles)) -> Result<(), HiveError> {
    let dir = store.data_dir();
    let _lock = lock_state(dir, THROTTLE_FILE)?;
    let mut throttles = read(store);
    edit(&mut throttles);
    let bytes = serde_json::to_vec(&throttles).map_err(|e| HiveError::Local(e.to_string()))?;
    write_private(dir, THROTTLE_FILE, &bytes)
}

/// Record the throttle `error` sets on `scope` (expired ones are dropped
/// on the way) and pass the error on unchanged.
pub fn note(store: &HiveStore, scope: Scope, error: HiveError, now: DateTime<Utc>) -> HiveError {
    note_on(store, Some(scope), error, now)
}

/// [`note`] for an answer of any engine route: a quota holds the paths its
/// resource names (every authenticated call passes here, `Session`).
pub fn note_on(
    store: &HiveStore,
    scope: Option<Scope>,
    error: HiveError,
    now: DateTime<Utc>,
) -> HiveError {
    if let Some((key, throttle)) = of_error(scope, &error, now)
        && let Err(e) = change(store, |t| {
            t.items.retain(|_, v| v.active_at(now));
            t.items.insert(key, throttle);
        })
    {
        tracing::warn!("hive: could not record a throttle: {e}");
    }
    error
}

/// The active throttle holding `scope` at `now` (the one ending last).
#[must_use]
pub fn holding(store: &HiveStore, scope: Scope, now: DateTime<Utc>) -> Option<Throttle> {
    read(store)
        .items
        .into_values()
        .filter(|t| t.holds(scope) && t.active_at(now))
        .max_by(|a, b| a.until.cmp(&b.until))
}

/// `Err` with the throttle's refusal while `scope` is held. When only
/// quota throttles hold it and their probe is due, one call passes (the
/// next probe is an hour later): a success ends them ([`passed`]), a new
/// refusal records them again.
pub fn check(store: &HiveStore, scope: Scope, now: DateTime<Utc>) -> Result<(), HiveError> {
    let Some(held) = holding(store, scope, now) else {
        return Ok(());
    };
    let due = |t: &Throttle| {
        t.probe_at
            .as_deref()
            .and_then(|p| DateTime::parse_from_rfc3339(p).ok())
            .is_some_and(|p| p <= now)
    };
    let throttles = read(store);
    let active: Vec<(&String, &Throttle)> = throttles
        .items
        .iter()
        .filter(|(_, t)| t.holds(scope) && t.active_at(now))
        .collect();
    if !active
        .iter()
        .all(|(k, t)| k.starts_with("quota:") && due(t))
    {
        return Err(held.refusal());
    }
    let next = stamp(now + chrono::Duration::seconds(QUOTA_PROBE_S));
    let keys: Vec<String> = active.iter().map(|(k, _)| (*k).clone()).collect();
    let _ = change(store, |t| {
        for k in &keys {
            if let Some(v) = t.items.get_mut(k) {
                v.probe_at = Some(next.clone());
            }
        }
    });
    Ok(())
}

/// Whether a quota throttle (not only a policy hold) holds `scope` now.
#[must_use]
pub fn quota_holds(store: &HiveStore, scope: Scope, now: DateTime<Utc>) -> bool {
    read(store)
        .items
        .iter()
        .any(|(k, t)| k.starts_with("quota:") && t.holds(scope) && t.active_at(now))
}

/// The quota is back (a successful call, or an answer that shows it):
/// every quota throttle holding `scope` ends.
pub fn quota_lifted(store: &HiveStore, scope: Scope) {
    if read(store)
        .items
        .iter()
        .any(|(k, t)| k.starts_with("quota:") && t.holds(scope))
    {
        let _ = change(store, |t| {
            t.items
                .retain(|k, v| !(k.starts_with("quota:") && v.holds(scope)));
        });
    }
}

/// A successful call on `scope`: its policy holds and quota throttles are
/// over (a probe that passed, or a quota raised in Hive meanwhile).
pub fn passed(store: &HiveStore, scope: Scope) {
    let suffix = format!(":{}", scope.as_str());
    if read(store)
        .items
        .keys()
        .any(|k| k.starts_with("policy:") && k.ends_with(&suffix))
    {
        let _ = change(store, |t| {
            t.items
                .retain(|k, _| !(k.starts_with("policy:") && k.ends_with(&suffix)));
        });
    }
    quota_lifted(store, scope);
}

/// The workspace's plan changed (a newer lease): every quota throttle was
/// the old plan's answer and ends; policy holds stay.
pub fn plan_changed(store: &HiveStore) {
    if read(store).items.keys().any(|k| k.starts_with("quota:")) {
        let _ = change(store, |t| t.items.retain(|k, _| !k.starts_with("quota:")));
    }
}

/// A newer policy is in force: every policy hold is over, on every path
/// (each was the old policy's answer), and so is every outbox item's
/// policy wait (a refused knowledge draft); quota throttles stay.
pub fn policy_changed(store: &HiveStore) {
    if read(store).items.keys().any(|k| k.starts_with("policy:")) {
        let _ = change(store, |t| t.items.retain(|k, _| !k.starts_with("policy:")));
    }
    super::outbox::Outbox::at(store.data_dir()).release_policy_holds();
}

/// A new enrolment starts without the old connect throttle.
pub fn clear_connect(store: &HiveStore) {
    let _ = change(store, |t| {
        t.items.retain(|_, v| !v.holds(Scope::Connect));
    });
}

/// `hive status` lines of every active throttle, sorted by key (stable).
#[must_use]
pub fn status_lines(store: &HiveStore, now: DateTime<Utc>) -> Vec<String> {
    read(store)
        .items
        .values()
        .filter(|t| t.active_at(now))
        .map(|t| {
            format!(
                "Throttled:    {} until {}: {}",
                t.scopes.join(", "),
                t.until,
                t.line
            )
        })
        .collect()
}

#[cfg(test)]
#[path = "throttle_tests.rs"]
mod tests;
