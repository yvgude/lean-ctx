// SPDX-License-Identifier: Apache-2.0
//! Background Hive work, started once at MCP startup.
//!
//! The scheduler never runs inside a tool call and never blocks one: it is a
//! low-priority thread that renews the lease when due (and, with later
//! packages, drains the outbox and pulls knowledge). Offline or a lapsed
//! lease only pauses Hive traffic; local tools are unaffected.

use std::time::Duration;

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
    /// A transient failure; try again next tick.
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
            Err(error) => {
                store.record_error(&error.to_string());
                return if error.is_transient() {
                    TickOutcome::Retry(error.to_string())
                } else {
                    TickOutcome::Stop(error.to_string())
                };
            }
        }
    }
    if due(store, config, "knowledge", now) {
        crate::core::hive::knowledge::pull::sync(&session, config, None);
    }
    for problem in crate::core::hive::tasks::propose::drain(&session).problems {
        store.record_error(&format!("tasks: {problem}"));
    }
    if due(store, config, "policy", now)
        && session
            .lease
            .lease
            .grants(lean_ctx_protocol::hive::lease::HiveRight::PolicyPull)
        && let Err(error) = crate::core::hive::policy::pull(&session)
    {
        store.record_error(&format!("policy: {error}"));
    }
    // The daily `POST /v1/metrics` upload is retired (it is always off,
    // `metrics::upload::RETIRED`); engine metrics v2 carry its figures.
    // Engine metrics v2 (hourly, per vault); held back until released.
    use crate::core::hive::metrics_v2::upload::{self as hours, MetricsV2Run};
    match hours::run(&session, config, now, false) {
        Ok(MetricsV2Run::Sent { .. } | MetricsV2Run::Empty) => store.record_success(
            "metrics_v2",
            &now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        ),
        Ok(_) => {}
        Err(error) => store.record_error(&format!("engine metrics: {error}")),
    }
    match crate::core::hive::receipts::run(&session, config, now) {
        Ok(crate::core::hive::receipts::ReceiptsRun::Sent { accepted, .. }) if accepted > 0 => {
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

/// Start the scheduler thread when this machine is connected and Hive is
/// not disabled. Returns `None` when there is nothing to schedule.
pub fn spawn(config: HiveConfig) -> Option<std::thread::JoinHandle<()>> {
    let store = HiveStore::open().ok()?;
    if config.enabled == Some(false) || !matches!(store.instance(), Ok(Some(_))) {
        // Disabled or not connected: nothing records for Hive (N1).
        super::receipts::stop_local_recording(store.data_dir());
        return None;
    }
    std::thread::Builder::new()
        .name("hive-scheduler".into())
        .spawn(move || {
            let mut config = config;
            loop {
                if let TickOutcome::Stop(_) = tick(&store, &config) {
                    return;
                }
                std::thread::sleep(tick_every(&config));
                // K2 review 3 (NEW-2): `config set hive.metrics_upload false`
                // or `hive.enabled false` applies on the next tick, not only
                // after a restart of this long-lived process.
                config = crate::core::config::Config::load().hive.clone();
            }
        })
        .ok()
}
