// SPDX-License-Identifier: Apache-2.0
//! Background Hive work, started once at MCP startup.
//!
//! The scheduler never runs inside a tool call and never blocks one: it is a
//! low-priority thread that renews the lease when due (and, with later
//! packages, drains the outbox and pulls knowledge). Offline or a lapsed
//! lease only pauses Hive traffic; local tools are unaffected.

use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use chrono::Utc;

use super::HiveError;
use super::auth::Session;
use super::config::HiveConfig;
use super::state::HiveStore;
use lean_ctx_protocol::hive::lease::LeaseState;

/// Time between passes: `[hive] pull_interval_minutes` (SPEC-ENGINE §3.16).
fn tick_every(config: &HiveConfig) -> Duration {
    Duration::from_secs(u64::from(config.pull_interval()) * 60)
}

/// What one scheduler pass did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TickOutcome {
    Idle,
    Renewed,
    /// Nothing to do any more (not connected, disabled, lapsed): stop.
    Stop(String),
    /// A failure that may pass (Hive offline, a lease renewal refused);
    /// try again next tick.
    Retry(String),
}

/// One pass at local time `now`. A pass that stops (disabled, not
/// connected, revoked, lapsed, …) also stops every local recording for
/// Hive (K2 review 2, N1): nothing is collected while no pass runs.
pub fn tick(store: &HiveStore, config: &HiveConfig) -> TickOutcome {
    let outcome = tick_once(store, config);
    if matches!(outcome, TickOutcome::Stop(_)) {
        super::receipts::stop_local_recording(store.data_dir());
    }
    outcome
}

fn tick_once(store: &HiveStore, config: &HiveConfig) -> TickOutcome {
    if config.enabled == Some(false) {
        return TickOutcome::Stop("disabled by [hive] enabled = false".into());
    }
    let Ok(Some(instance)) = store.instance() else {
        return TickOutcome::Stop("not connected".into());
    };
    // A disconnect Hive has not confirmed yet: finish it, sync nothing.
    match super::disconnect::retry_pending(store, config, &instance) {
        None => {}
        Some(Ok(done)) => return TickOutcome::Stop(done),
        Some(Err(error)) if error.is_transient() => {
            return TickOutcome::Retry(format!("pending disconnect: {error}"));
        }
        Some(Err(error)) => {
            store.record_error(&format!("pending disconnect: {error}"));
            return TickOutcome::Stop(format!("pending disconnect: {error}"));
        }
    }
    // Upgrade duties of the first 3.12 start (local, once; §3.16).
    if let Err(error) = super::migrate::run(store, config) {
        store.record_error(&format!("upgrade: {error}"));
    }
    let session = super::root_cert::trust_root(store, config, &instance.url)
        .and_then(|root| Session::load(store.clone(), root));
    let mut session = match session {
        Ok(session) => session,
        Err(error) => {
            store.record_error(&error.to_string());
            return TickOutcome::Stop(error.to_string());
        }
    };
    let now = Utc::now();
    if session.lease.state_at(now) == LeaseState::Lapsed {
        store.record_error(&HiveError::LeaseLapsed.to_string());
        return TickOutcome::Stop(HiveError::LeaseLapsed.to_string());
    }
    let mut outcome = TickOutcome::Idle;
    if session.lease.renew_due(now) {
        match session.renew() {
            Ok(_) => {
                outcome = TickOutcome::Renewed;
                // A self-hosted Hive' root certificate is refreshed with the lease.
                if let Ok(client) = session.client() {
                    super::root_cert::refresh(store, &client, &instance.url);
                }
            }
            // A revoked instance or a lapsed lease ends the scheduler. Any
            // other refusal (a key this engine does not hold yet, a damaged
            // answer, Hive offline) is tried again next pass, one
            // `pull_interval` later, and the pass goes on under the lease
            // still in force: a scheduler that stopped here also stopped the
            // pulls, metrics and receipts until the next restart. The reason
            // stays in `hive status` until a renewal succeeds.
            Err(error @ (HiveError::Revoked(_) | HiveError::LeaseLapsed)) => {
                store.record_error(&error.to_string());
                return TickOutcome::Stop(error.to_string());
            }
            Err(error) => {
                store.record_error(&error.to_string());
                store.record_renew_error(Some(&error.to_string()));
                outcome = TickOutcome::Retry(error.to_string());
            }
        }
    }
    // A vault a checkout left (pulled link file, identity change) leaves
    // projects.json first, whatever pauses below: metrics and receipts
    // read the map too and are never filed under it.
    if let Err(error) = crate::resolution::load_current(store.data_dir()) {
        store.record_error(&format!("vault map: {error}"));
    }
    // A local screening rule that does not compile pauses every content
    // channel (fails closed) and says so each pass; the policy pull that
    // may bring fixed rules goes on.
    let screen_problem = super::knowledge::screening::Screen::for_installation().problem();
    if let Some(problem) = &screen_problem {
        store.record_error(problem);
    } else {
        if due(store, config, "knowledge", now) {
            crate::knowledge::pull::sync(&session, config, None);
        }
        for problem in crate::tasks::propose::drain(&session).problems {
            store.record_error(&format!("tasks: {problem}"));
        }
    }
    if due(store, config, "policy", now)
        && session
            .lease
            .lease
            .grants(lean_ctx_protocol::hive::lease::HiveRight::PolicyPull)
    {
        match crate::policy::pull(&session) {
            Ok(_) => crate::policy::note_pull(store.data_dir(), crate::policy::PulledVia::Poll),
            Err(error) => store.record_error(&format!("policy: {error}")),
        }
    }
    // Engine metrics v2 (hourly, per vault); held back until released.
    use crate::metrics_v2::upload::{self as hours, MetricsV2Run};
    match hours::run(&session, config, now, false) {
        Ok(MetricsV2Run::Sent { .. } | MetricsV2Run::Empty) => store.record_success(
            "metrics_v2",
            &now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        ),
        Ok(_) => {}
        Err(error) => store.record_error(&format!("instance metrics: {error}")),
    }
    match crate::receipts::run(&session, config, now) {
        Ok(crate::receipts::ReceiptsRun::Sent { accepted, .. }) if accepted > 0 => {
            store.record_success(
                "receipts",
                &now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            );
        }
        Ok(_) => {}
        Err(error) => store.record_error(&format!("receipts: {error}")),
    }
    outcome
}

/// Whether the last success on `channel` is older than `pull_interval_minutes`.
fn due(store: &HiveStore, config: &HiveConfig, channel: &str, now: chrono::DateTime<Utc>) -> bool {
    let interval = chrono::Duration::minutes(i64::from(config.pull_interval()));
    store
        .activity()
        .last_success
        .get(channel)
        .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
        .is_none_or(|last| now - last.with_timezone(&Utc) >= interval)
}

/// One pending policy nudge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Nudge {
    /// The version the hint named; `None` (a reconnect): pull regardless.
    version: Option<u64>,
}

impl Nudge {
    /// Two nudges as one: a pull regardless wins, else the newer version.
    fn merge(self, other: Self) -> Self {
        Self {
            version: self.version.zip(other.version).map(|(a, b)| a.max(b)),
        }
    }
}

/// The pending policy nudge, and when the last nudged pull ran.
struct Nudges {
    policy: Option<Nudge>,
    last_run: Option<Instant>,
}

static NUDGES: Mutex<Nudges> = Mutex::new(Nudges {
    policy: None,
    last_run: None,
});
static NUDGE_WAKE: Condvar = Condvar::new();
/// At most one nudged policy pull per this interval (a burst of frames is
/// one pull).
const NUDGE_SPACING: Duration = Duration::from_secs(10);

fn nudges() -> MutexGuard<'static, Nudges> {
    NUDGES.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Ask the scheduler to pull the policy now: Hive announced `version`
/// (`policy.advanced`), or the bus reconnected (`None`). A hint only: the
/// pull verifies the signed bundle before anything applies, and acks it.
pub fn nudge_policy(version: Option<u64>) {
    {
        let mut pending = nudges();
        let nudge = Nudge { version };
        pending.policy = Some(pending.policy.map_or(nudge, |held| held.merge(nudge)));
    }
    NUDGE_WAKE.notify_all();
}

/// Wait until `deadline` or a policy nudge that may run (spaced by
/// [`NUDGE_SPACING`]). The nudge when nudged, `None` at `deadline`.
fn wait_nudge(deadline: Instant) -> Option<Nudge> {
    let mut pending = nudges();
    loop {
        let now = Instant::now();
        let until = match pending.policy {
            Some(nudge) => {
                let ready = pending.last_run.map_or(now, |last| last + NUDGE_SPACING);
                if ready <= now {
                    pending.policy = None;
                    pending.last_run = Some(now);
                    return Some(nudge);
                }
                ready.min(deadline)
            }
            None => deadline,
        };
        if until <= now {
            return None;
        }
        pending = NUDGE_WAKE
            .wait_timeout(pending, until - now)
            .unwrap_or_else(PoisonError::into_inner)
            .0;
    }
}

/// Whether a nudge for `requested` needs a pull with `installed` in force.
fn nudge_needed(requested: Option<u64>, installed: Option<u64>) -> bool {
    match requested {
        None => true,
        Some(version) => installed.is_none_or(|have| have < version),
    }
}

/// The Hive session of this connected machine under a lease still in
/// force, for work off the tick (nudged pulls, background sync). The tick
/// alone renews the lease and finishes a pending disconnect.
pub(crate) fn connected_session(
    store: &HiveStore,
    config: &HiveConfig,
) -> Result<Session, HiveError> {
    if config.enabled == Some(false) {
        return Err(HiveError::NotConnected);
    }
    let instance = store.instance()?.ok_or(HiveError::NotConnected)?;
    let root = super::root_cert::trust_root(store, config, &instance.url)?;
    let session = Session::load(store.clone(), root)?;
    if session.lease.state_at(Utc::now()) == LeaseState::Lapsed {
        return Err(HiveError::LeaseLapsed);
    }
    Ok(session)
}

/// The nudged pull: verify, apply and ack the policy now (the same
/// [`crate::policy::pull`] the tick runs), outside the tick.
fn policy_now(store: &HiveStore, config: &HiveConfig, requested: Option<u64>) {
    let installed = crate::policy::load_state(store.data_dir())
        .effective
        .map(|effective| effective.version);
    if config.enabled == Some(false) || !nudge_needed(requested, installed) {
        return;
    }
    let session = match connected_session(store, config) {
        Ok(session) => session,
        Err(error) => {
            store.record_error(&format!("policy nudge: {error}"));
            return;
        }
    };
    if !session
        .lease
        .lease
        .grants(lean_ctx_protocol::hive::lease::HiveRight::PolicyPull)
    {
        return;
    }
    match crate::policy::pull(&session) {
        Ok(_) => crate::policy::note_pull(store.data_dir(), crate::policy::PulledVia::Nudge),
        Err(error) => store.record_error(&format!("policy: {error}")),
    }
}

/// Start the scheduler thread when this machine is connected and Hive is
/// not disabled. Returns `None` when there is nothing to schedule.
pub fn spawn(config: HiveConfig) -> Option<std::thread::JoinHandle<()>> {
    let store = HiveStore::open().ok()?;
    if config.enabled == Some(false) || !matches!(store.instance(), Ok(Some(_))) {
        // Disabled or not connected: nothing records for Hive (N1).
        super::receipts::stop_local_recording(store.data_dir());
        return None;
    }
    // Vault sync in the background, beside the scheduler (gap 2 A).
    super::sync::auto::spawn(store.clone(), config.clone());
    std::thread::Builder::new()
        .name("hive-scheduler".into())
        .spawn(move || {
            let mut config = config;
            loop {
                if let TickOutcome::Stop(_) = tick(&store, &config) {
                    super::sync::auto::stop();
                    return;
                }
                // Sleep until the next tick; a policy nudge (bus frame
                // `policy.advanced`, a reconnect) pulls the policy at once.
                let next = Instant::now() + tick_every(&config);
                while let Some(nudge) = wait_nudge(next) {
                    policy_now(&store, &config, nudge.version);
                }
                // K2 review 3 (NEW-2): `config set hive.metrics_upload false`
                // or `hive.enabled false` applies on the next tick, not only
                // after a restart of this long-lived process.
                config = lean_ctx::core::config::Config::load().hive.clone();
            }
        })
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_nudge_pulls_only_a_newer_version_or_after_a_reconnect() {
        assert!(nudge_needed(None, Some(9)));
        assert!(nudge_needed(Some(3), None));
        assert!(nudge_needed(Some(4), Some(3)));
        assert!(!nudge_needed(Some(3), Some(3)));
        assert!(!nudge_needed(Some(2), Some(3)));
    }

    /// One test owns the process-wide nudge state (nextest runs each test
    /// in its own process).
    #[test]
    fn nudges_coalesce_and_are_spaced() {
        let soon = || Instant::now() + Duration::from_millis(50);
        assert_eq!(wait_nudge(soon()), None, "no nudge: the deadline");
        nudge_policy(Some(4));
        nudge_policy(None);
        nudge_policy(Some(6));
        assert_eq!(
            wait_nudge(soon()),
            Some(Nudge { version: None }),
            "one pull for the burst; the reconnect pulls regardless"
        );
        // The next nudge within the spacing waits past a short deadline.
        nudge_policy(Some(7));
        assert_eq!(wait_nudge(soon()), None);
        nudges().last_run = Instant::now().checked_sub(NUDGE_SPACING);
        assert_eq!(wait_nudge(soon()), Some(Nudge { version: Some(7) }));
    }
}
