// SPDX-License-Identifier: Apache-2.0
//! Quota and policy throttles (Release B, Cloud API §7.2 and §8.1).
//!
//! When Cloud answers `429 quota_exceeded` (monthly transfer or bus
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
//! - Personal slot cool-down (409 `registered_instances`, `cooldown_days`): until
//!   `reset_at` (the web recovery frees it sooner, P7);
//! - policy refusal (403) of a bus publish or a task claim:
//!   [`POLICY_HOLD_S`] per member and path, then one try (an admin may have
//!   changed the policy meanwhile), and at once when a newer policy is
//!   installed ([`policy_changed`]); a sync refusal holds only its draft
//!   (outbox, same wait, also ended by a newer policy) or its vault sync,
//!   never the other vaults;
//! - the subscription models gateway (mz6, [`Scope::Models`]): `429
//!   quota_exceeded` for `model_credit_usd_micros` holds like a monthly
//!   quota; `402 plan_required` (key `plan:models`, ended by a plan change)
//!   and `403 policy_denied` hold [`POLICY_HOLD_S`]; the proxy's `leanctx`
//!   provider answers locally meanwhile ([`models_hold_kind`]);
//! - other counted quotas (409 `private_vaults`, a slot without
//!   cool-down): no throttle; the refusal is shown and nothing retries it
//!   by itself;
//! - token mint rate limit (429 `rate_limited` at `/instances/{id}/token`):
//!   one hold for every process of this machine (they all mint with the
//!   same instance key, on the same server budget). It grows exponentially
//!   with each 429 in a row, from [`RATE_FIRST_S`] up to [`RATE_MAX_WAIT_S`],
//!   and never ends before the server's `Retry-After` (the floor, also
//!   above the cap). No caller passes it, a person's manual sync neither:
//!   the server counts refused attempts too. A minted token ends it.
//!
//! `lean-ctx cloud status` lists the active throttles ([`status_lines`]).

use std::collections::BTreeMap;

use lean_ctx_time::{OffsetDateTime, UtcDateTime};
use serde::{Deserialize, Serialize};

use super::CloudError;
use super::state::{CloudStore, load_or_quarantine, lock_state, write_private};

pub(crate) const THROTTLE_FILE: &str = "throttles.json";
/// The shortest wait of a throttle: one tick of a loop is never a retry.
pub const MIN_WAIT_S: i64 = 60;
/// The longest wait (a reset further out is shortened to this).
pub const MAX_WAIT_S: i64 = 32 * 86_400;
/// How long a policy refusal holds its path.
pub const POLICY_HOLD_S: i64 = 15 * 60;
/// A quota throttle lets one call through this often before it ends, so a
/// quota that was raised or lowered in Cloud is noticed (E2E-FINAL: sync
/// waited for the month's end after usage was lowered).
pub const QUOTA_PROBE_S: i64 = 3600;
/// The key of the token mint rate limit.
const TOKEN_KEY: &str = "rate:token";
/// The scope name the token mint hold shows in `cloud status`.
pub const TOKEN_SCOPE: &str = "token";
/// The first wait after a token mint 429 (a longer `Retry-After` wins).
pub const RATE_FIRST_S: i64 = 60;
/// The longest computed token mint wait; a longer `Retry-After` still wins.
pub const RATE_MAX_WAIT_S: i64 = 3600;
/// The longest server `Retry-After` the token mint hold honours (a day).
pub const RATE_FLOOR_MAX_S: i64 = 86_400;
/// How long an ended token mint hold is remembered, so that the next 429
/// within it doubles the wait instead of starting again at the first.
pub const RATE_MEMORY_S: i64 = 6 * 3600;

/// The engine paths a throttle can hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Scope {
    /// Knowledge sync, event tail and push, snapshots (transfer).
    Sync,
    /// Bus publishes (bus messages).
    Bus,
    /// Task claims, renewals and results.
    Tasks,
    /// A new engine registration (`cloud connect`).
    Connect,
    /// The `leanctx` provider's requests to the Cloud models gateway (mz6).
    Models,
}

impl Scope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sync => "sync",
            Self::Bus => "bus",
            Self::Tasks => "tasks",
            Self::Connect => "connect",
            Self::Models => "models",
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
        "registered_instances" => &[Scope::Connect],
        // mz6: the month's model credit, counted by the models gateway.
        "model_credit_usd_micros" => &[Scope::Models],
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
    /// The refusal, as `CloudError` shows it.
    pub line: String,
    /// A quota throttle's next probe ([`QUOTA_PROBE_S`]): from then one
    /// call may pass, and its success ends the throttle ([`passed`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probe_at: Option<String>,
    /// The token mint hold's 429s in a row (its backoff step); 0 elsewhere.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub strikes: u32,
}

#[allow(clippy::trivially_copy_pass_by_ref)] // serde passes a reference
fn is_zero(n: &u32) -> bool {
    *n == 0
}

impl Throttle {
    fn holds(&self, scope: Scope) -> bool {
        self.scopes.iter().any(|s| s == scope.as_str())
    }

    fn active_at(&self, now: UtcDateTime) -> bool {
        OffsetDateTime::parse_from_rfc3339(&self.until).is_ok_and(|until| until > now)
    }

    /// Whether it is kept at `now`: while active, and a token mint hold
    /// also [`RATE_MEMORY_S`] after it ended (its backoff step counts on).
    /// A stale token mint hold ([`Self::token_stale`]) is not kept.
    fn kept_at(&self, key: &str, now: UtcDateTime) -> bool {
        if key == TOKEN_KEY {
            return !self.token_stale(now)
                && OffsetDateTime::parse_from_rfc3339(&self.until).is_ok_and(|until| {
                    until.to_utc() + lean_ctx_time::Duration::seconds(RATE_MEMORY_S) > now
                });
        }
        self.active_at(now)
    }

    /// Whether this token mint hold ends more than [`RATE_FLOOR_MAX_S`]
    /// after `now`: longer than any hold is written for, so it was written
    /// under another clock (a clock set back, or one that ran ahead when it
    /// was written) and holds nothing. Otherwise it would block every cloud
    /// call for as long as the clock jumped, and no mint could clear it.
    fn token_stale(&self, now: UtcDateTime) -> bool {
        OffsetDateTime::parse_from_rfc3339(&self.until)
            .is_ok_and(|until| (until.to_utc() - now).num_seconds() > RATE_FLOOR_MAX_S)
    }

    /// The error a held path answers instead of calling Cloud.
    #[must_use]
    pub fn refusal(&self) -> CloudError {
        CloudError::Refused(format!(
            "waiting for LeanCTX Cloud until {}: {}",
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

fn stamp(at: UtcDateTime) -> String {
    at.to_rfc3339_opts(lean_ctx_time::SecondsFormat::Secs, true)
}

/// 00:00 UTC on the first day of the month after `now`'s.
fn next_month(now: UtcDateTime) -> UtcDateTime {
    let (year, month) = if now.month() == 12 {
        (now.year() + 1, 1)
    } else {
        (now.year(), now.month() + 1)
    };
    UtcDateTime::with_ymd_and_hms(year, month, 1, 0, 0, 0)
        .unwrap_or(now + lean_ctx_time::Duration::days(31))
}

/// `until` itself when it is [`MIN_WAIT_S`]..=[`MAX_WAIT_S`] away (so a
/// `reset_at` is kept to the second), else the nearer bound.
fn clamp(now: UtcDateTime, until: UtcDateTime) -> UtcDateTime {
    let wait = (until - now).num_seconds();
    if (MIN_WAIT_S..=MAX_WAIT_S).contains(&wait) {
        until
    } else {
        now + lean_ctx_time::Duration::seconds(wait.clamp(MIN_WAIT_S, MAX_WAIT_S))
    }
}

/// The throttle `error` sets at `now`, if it sets one: on the paths its
/// quota resource holds plus `scope`, the path that met it (`None`: an
/// HTTP call of unknown path, so only the resource's paths; a policy
/// refusal then sets nothing).
#[must_use]
pub fn of_error(
    scope: Option<Scope>,
    error: &CloudError,
    now: UtcDateTime,
) -> Option<(String, Throttle)> {
    let (key, scopes, until) = match error {
        CloudError::QuotaExceeded(q) if q.waits() => {
            let until = match error.retry_after_at(now) {
                // Clamped before the arithmetic: a huge `retry_after_s`
                // (up to u64::MAX) must not overflow the time type (a panic).
                Some(seconds) => {
                    let wait = i64::try_from(seconds)
                        .unwrap_or(MAX_WAIT_S)
                        .clamp(MIN_WAIT_S, MAX_WAIT_S);
                    now + lean_ctx_time::Duration::seconds(wait)
                }
                None => next_month(now),
            };
            (
                format!("quota:{}", q.resource),
                resource_scopes(&q.resource),
                clamp(now, until),
            )
        }
        CloudError::QuotaExceeded(q) if q.cooling_slot() => {
            let reset = OffsetDateTime::parse_from_rfc3339(q.reset_at.as_deref()?)
                .ok()?
                .to_utc();
            (
                format!("quota:{}", q.resource),
                resource_scopes(&q.resource),
                clamp(now, reset),
            )
        }
        CloudError::PolicyDenied(d) => (
            format!(
                "policy:{}:{}",
                d.member.as_deref().unwrap_or("unknown"),
                scope?.as_str()
            ),
            &[][..],
            now + lean_ctx_time::Duration::seconds(POLICY_HOLD_S),
        ),
        // A gateway `402 plan_required` (the plan lost the subscription
        // models): the models path waits like a policy hold; a newer lease
        // ends it at once ([`plan_changed`]). Elsewhere a 402 sets nothing.
        CloudError::PlanRequired(_) if scope == Some(Scope::Models) => (
            "plan:models".to_string(),
            &[][..],
            now + lean_ctx_time::Duration::seconds(POLICY_HOLD_S),
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
    let probe = now + lean_ctx_time::Duration::seconds(QUOTA_PROBE_S);
    let probe_at = (key.starts_with("quota:") && until > probe).then(|| stamp(probe));
    Some((
        key,
        Throttle {
            scopes: names,
            until: stamp(until),
            line: error.to_string(),
            probe_at,
            strikes: 0,
        },
    ))
}

fn read(store: &CloudStore) -> Throttles {
    load_or_quarantine(&store.data_dir().join(THROTTLE_FILE), |_: &Throttles| true)
        .ok()
        .flatten()
        .unwrap_or_default()
}

fn change(store: &CloudStore, edit: impl FnOnce(&mut Throttles)) -> Result<(), CloudError> {
    let dir = store.data_dir();
    let _lock = lock_state(dir, THROTTLE_FILE)?;
    let mut throttles = read(store);
    edit(&mut throttles);
    let bytes = serde_json::to_vec(&throttles).map_err(|e| CloudError::Local(e.to_string()))?;
    write_private(dir, THROTTLE_FILE, &bytes)
}

/// Record the throttle `error` sets on `scope` (expired ones are dropped
/// on the way) and pass the error on unchanged.
pub fn note(store: &CloudStore, scope: Scope, error: CloudError, now: UtcDateTime) -> CloudError {
    note_on(store, Some(scope), error, now)
}

/// [`note`] for an answer of any engine route: a quota holds the paths its
/// resource names (every authenticated call passes here, `Session`).
pub fn note_on(
    store: &CloudStore,
    scope: Option<Scope>,
    error: CloudError,
    now: UtcDateTime,
) -> CloudError {
    if let Some((key, throttle)) = of_error(scope, &error, now)
        && let Err(e) = change(store, |t| {
            t.items.retain(|k, v| v.kept_at(k, now));
            t.items.insert(key, throttle);
        })
    {
        tracing::warn!("cloud: could not record a throttle: {e}");
    }
    error
}

/// The token mint wait after the `strikes`-th 429 in a row (`strikes >= 1`)
/// with the server's `Retry-After` `floor`: doubling from [`RATE_FIRST_S`]
/// up to [`RATE_MAX_WAIT_S`], never shorter than the floor (which is kept
/// above the cap too, up to [`RATE_FLOOR_MAX_S`]: a broken or hostile
/// answer cannot stop every call for weeks).
#[must_use]
pub fn rate_wait(strikes: u32, floor: Option<u64>) -> i64 {
    let backoff = RATE_FIRST_S
        .saturating_mul(1i64 << strikes.saturating_sub(1).min(16))
        .min(RATE_MAX_WAIT_S);
    let floor = floor.map_or(0, |s| {
        i64::try_from(s)
            .unwrap_or(RATE_FLOOR_MAX_S)
            .min(RATE_FLOOR_MAX_S)
    });
    backoff.max(floor).max(1)
}

/// Record a `429 rate_limited` of the token mint: every process of this
/// machine waits [`rate_wait`] before it mints again. Any other error is
/// left alone. Returns the error unchanged.
pub fn note_token_limit(store: &CloudStore, error: CloudError, now: UtcDateTime) -> CloudError {
    let CloudError::RateLimited { retry_after } = &error else {
        return error;
    };
    let floor = *retry_after;
    let line = error.to_string();
    if let Err(e) = change(store, |t| {
        let strikes = t
            .items
            .get(TOKEN_KEY)
            .filter(|prev| prev.kept_at(TOKEN_KEY, now))
            .map_or(0, |prev| prev.strikes)
            .saturating_add(1);
        let until = now + lean_ctx_time::Duration::seconds(rate_wait(strikes, floor));
        t.items.retain(|k, v| v.kept_at(k, now));
        t.items.insert(
            TOKEN_KEY.to_string(),
            Throttle {
                scopes: vec![TOKEN_SCOPE.to_string()],
                until: stamp(until),
                line,
                probe_at: None,
                strikes,
            },
        );
    }) {
        tracing::warn!("cloud: could not record the token rate limit: {e}");
    }
    error
}

/// While the token mint is held at `now`: the `429 rate_limited` a mint
/// answers instead of calling the cloud, with the seconds left as `retry_after`
/// (so every caller's backoff waits at least as long). A stale hold (more
/// than [`RATE_FLOOR_MAX_S`] left) holds nothing; the next change drops it.
#[must_use]
pub fn token_hold(store: &CloudStore, now: UtcDateTime) -> Option<CloudError> {
    let held = read(store).items.remove(TOKEN_KEY)?;
    let until = OffsetDateTime::parse_from_rfc3339(&held.until).ok()?;
    let left = (until.to_utc() - now).num_seconds();
    (left > 0 && left <= RATE_FLOOR_MAX_S).then(|| CloudError::RateLimited {
        retry_after: u64::try_from(left).ok(),
    })
}

/// A token was minted: the token mint hold and its backoff step end.
pub fn token_minted(store: &CloudStore) {
    if read(store).items.contains_key(TOKEN_KEY) {
        let _ = change(store, |t| {
            t.items.remove(TOKEN_KEY);
        });
    }
}

/// The active throttle holding `scope` at `now` (the one ending last).
#[must_use]
pub fn holding(store: &CloudStore, scope: Scope, now: UtcDateTime) -> Option<Throttle> {
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
pub fn check(store: &CloudStore, scope: Scope, now: UtcDateTime) -> Result<(), CloudError> {
    let Some(held) = holding(store, scope, now) else {
        return Ok(());
    };
    let due = |t: &Throttle| {
        t.probe_at
            .as_deref()
            .and_then(|p| OffsetDateTime::parse_from_rfc3339(p).ok())
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
    let next = stamp(now + lean_ctx_time::Duration::seconds(QUOTA_PROBE_S));
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
pub fn quota_holds(store: &CloudStore, scope: Scope, now: UtcDateTime) -> bool {
    read(store)
        .items
        .iter()
        .any(|(k, t)| k.starts_with("quota:") && t.holds(scope) && t.active_at(now))
}

/// What holds the models path (mz6), for the local answer's status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoldKind {
    Quota,
    Plan,
    Policy,
}

/// The kind of the throttle ending last among those holding
/// [`Scope::Models`] at `now` (a policy hold when none is a quota or plan).
#[must_use]
pub fn models_hold_kind(store: &CloudStore, now: UtcDateTime) -> HoldKind {
    read(store)
        .items
        .iter()
        .filter(|(_, t)| t.holds(Scope::Models) && t.active_at(now))
        .max_by(|a, b| a.1.until.cmp(&b.1.until))
        .map_or(HoldKind::Policy, |(k, _)| {
            if k.starts_with("quota:") {
                HoldKind::Quota
            } else if k.starts_with("plan:") {
                HoldKind::Plan
            } else {
                HoldKind::Policy
            }
        })
}

/// The quota is back (a successful call, or an answer that shows it):
/// every quota throttle holding `scope` ends.
pub fn quota_lifted(store: &CloudStore, scope: Scope) {
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
/// over (a probe that passed, or a quota raised in Cloud meanwhile).
pub fn passed(store: &CloudStore, scope: Scope) {
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
/// the old plan's answer and ends, and so does a plan hold of the models
/// path; policy holds stay.
pub fn plan_changed(store: &CloudStore) {
    let old_plan = |k: &str| k.starts_with("quota:") || k.starts_with("plan:");
    if read(store).items.keys().any(|k| old_plan(k)) {
        let _ = change(store, |t| t.items.retain(|k, _| !old_plan(k)));
    }
}

/// A newer policy is in force: every policy hold is over, on every path
/// (each was the old policy's answer), and so is every outbox item's
/// policy wait (a refused knowledge draft); quota throttles stay.
pub fn policy_changed(store: &CloudStore) {
    if read(store).items.keys().any(|k| k.starts_with("policy:")) {
        let _ = change(store, |t| t.items.retain(|k, _| !k.starts_with("policy:")));
    }
    super::outbox::Outbox::at(store.data_dir()).release_policy_holds();
}

/// A new enrolment starts without the old connect throttle.
pub fn clear_connect(store: &CloudStore) {
    let _ = change(store, |t| {
        t.items.retain(|_, v| !v.holds(Scope::Connect));
    });
}

/// `cloud status` lines of every active throttle, sorted by key (stable).
#[must_use]
pub fn status_lines(store: &CloudStore, now: UtcDateTime) -> Vec<String> {
    read(store)
        .items
        .iter()
        .filter(|(key, t)| t.active_at(now) && !(key.as_str() == TOKEN_KEY && t.token_stale(now)))
        .map(|(_, t)| {
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
