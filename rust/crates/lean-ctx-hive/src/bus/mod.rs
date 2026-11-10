// SPDX-License-Identifier: Apache-2.0
//! The cross-instance agent bus: the local bus (`ctx_agent`) of every
//! machine of an organization joined through Hive.
//!
//! One relay per machine runs in the daemon ([`spawn`]); it never runs in a
//! tool call. Offline, the local bus keeps working exactly as before; on
//! reconnect the relay catches up from its cursors and republishes what is
//! pending (Hive deduplicates by `client_msg_id`).

pub mod address;
pub mod control;
pub mod handoff;
pub mod leases;
pub mod messages_v2;
pub mod person_rules;
pub mod presence;
pub mod queen_kinds;
pub mod relay_v2;
pub mod task_events;
pub mod v2;
pub mod veto;
#[cfg(feature = "http-server")]
mod ws;

use crate::config::{HiveBusMode, HiveConfig};
use crate::state::{HiveStore, StateLock, lock_state_within};

/// How often the relay's process looks after CLI claims (lean-ctx #3).
const CLI_CLAIMS_PASS: std::time::Duration = std::time::Duration::from_secs(30);

/// One relay per machine (RB8 M11): two would overwrite each other's
/// cursors and send twice. The daemon's relay and `hive bus relay --for`
/// take `<data>/hive/relay.lock` without waiting; whoever does not get it
/// does not relay.
fn relay_lock(store: &HiveStore) -> Result<StateLock, crate::HiveError> {
    lock_state_within(store.data_dir(), "relay", std::time::Duration::ZERO).map_err(|_| {
        crate::HiveError::Local("another bus relay is already running on this machine".into())
    })
}

/// `[hive]` as it is now, when it lets the relay run (`bus` not `off`,
/// Hive not disabled). The relay reads it before every connect and on
/// every tick, so switching the bus off takes effect without a restart.
#[cfg(feature = "http-server")]
pub(crate) fn relay_config() -> Option<HiveConfig> {
    let config = lean_ctx::core::config::Config::load_arc().hive.clone();
    (config.bus != HiveBusMode::Off && config.enabled != Some(false)).then_some(config)
}

/// How long the daemon's relay waits before it asks for the relay lock
/// again while another relay (a foreground `hive bus relay --for`) holds it.
const LOCK_RETRY: std::time::Duration = std::time::Duration::from_secs(5);
/// The first restart after the daemon's relay crashed, doubled per crash in
/// a row, at most [`RESTART_MAX`]. A run that lasted [`RESTART_RESET`]
/// forgives the crashes before it.
const RESTART_FIRST: std::time::Duration = std::time::Duration::from_secs(5);
const RESTART_MAX: std::time::Duration = std::time::Duration::from_secs(300);
const RESTART_RESET: std::time::Duration = std::time::Duration::from_secs(600);

/// Where a [`Report`] line goes.
type Sink = std::sync::Arc<dyn Fn(&str) + Send + Sync>;

/// Where a relay says what it does. The foreground `hive bus relay`
/// prints it; the daemon's relay is quiet (its errors reach `hive status`).
/// A line equal to the one said last is not repeated, so a relay that
/// waits says why once, not on every tick.
#[derive(Clone, Default)]
pub struct Report {
    sink: Option<Sink>,
    last: std::sync::Arc<std::sync::Mutex<String>>,
}

impl Report {
    /// A relay that says nothing (the daemon's).
    pub fn quiet() -> Self {
        Self::default()
    }

    /// A relay that hands every line to `sink`.
    pub fn to(sink: impl Fn(&str) + Send + Sync + 'static) -> Self {
        Self {
            sink: Some(std::sync::Arc::new(sink)),
            last: std::sync::Arc::default(),
        }
    }

    #[cfg_attr(not(feature = "http-server"), allow(dead_code))]
    pub(crate) fn say(&self, line: &str) {
        let Some(sink) = &self.sink else { return };
        let mut last = self
            .last
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if *last != line {
            line.clone_into(&mut last);
            sink(line);
        }
    }
}

/// The text of a panic payload.
fn panic_text(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic".into())
}

/// The wait before restart number `crashes` (1 = the first) of a crashed
/// relay.
fn restart_delay(crashes: u32) -> std::time::Duration {
    RESTART_FIRST
        .saturating_mul(1u32 << crashes.saturating_sub(1).min(16))
        .min(RESTART_MAX)
}

/// Run the daemon's relay until it stops for good (`run_once` returns: a
/// revocation, a lapsed lease; the relay records those itself). A panic in
/// it, e.g. tokio's `spawn_blocking` when the OS refuses a thread (#252:
/// "OS can't spawn worker thread"), no longer ends the relay for the
/// daemon's lifetime: it is recorded for `hive status` and the relay starts
/// again after [`restart_delay`]. The relay lock stays held meanwhile, so
/// no second relay starts in between.
fn supervise(
    store: &HiveStore,
    mut run_once: impl FnMut(),
    mut sleep: impl FnMut(std::time::Duration),
) {
    let mut crashes: u32 = 0;
    loop {
        let started = std::time::Instant::now();
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(&mut run_once)) {
            Ok(()) => return,
            Err(payload) => {
                if started.elapsed() >= RESTART_RESET {
                    crashes = 0;
                }
                crashes = crashes.saturating_add(1);
                let delay = restart_delay(crashes);
                let line = format!(
                    "bus: the relay crashed ({}); it starts again in {} s",
                    panic_text(payload.as_ref()),
                    delay.as_secs()
                );
                tracing::warn!("hive {line}");
                store.record_error(&line);
                sleep(delay);
            }
        }
    }
}

/// Start the bus relay thread when this machine is connected, the bus is
/// not off and Hive is not disabled.
pub fn spawn(config: HiveConfig) -> Option<std::thread::JoinHandle<()>> {
    // The relay speaks WebSocket, which only an http-server build carries.
    if !cfg!(feature = "http-server") {
        return None;
    }
    if config.bus == HiveBusMode::Off || config.enabled == Some(false) {
        return None;
    }
    let store = HiveStore::open().ok()?;
    if !matches!(store.instance(), Ok(Some(_))) {
        return None;
    }
    std::thread::Builder::new()
        .name("hive-bus".into())
        .spawn(move || {
            // A foreground `hive bus relay --for` may hold the lock right
            // now (#252: one that ran while the daemon started left the
            // daemon without a relay for its whole life). Wait for it.
            let _relay = loop {
                match relay_lock(&store) {
                    Ok(lock) => break lock,
                    Err(error) => {
                        tracing::debug!("hive bus: {error}; asking again");
                        std::thread::sleep(LOCK_RETRY);
                    }
                }
            };
            // Engine start: the instance card (E44, SPEC-ENGINE §3.6). It
            // runs on this thread before the socket loop and never stops it.
            if let Err(error) = instance_card(&store, &config) {
                tracing::info!("hive instance card: {error}");
            }
            // The relay's process outlives CLI calls: it keeps the claims
            // `lean-ctx hive tasks claim` made for agents still at work, and
            // gives back the claims nobody works on any more (an ended
            // session's, an expired one), so others can take the task.
            let _ = std::thread::Builder::new()
                .name("hive-cli-claims".into())
                .spawn(|| {
                    loop {
                        std::thread::sleep(CLI_CLAIMS_PASS);
                        crate::tasks::keep_cli_claims();
                        crate::tasks::release_abandoned_claims();
                    }
                });
            let run_store = store.clone();
            supervise(
                &store,
                move || {
                    // A runtime the OS cannot build (no threads left) is a
                    // crash like any other: the supervisor tries again.
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap_or_else(|e| panic!("bus runtime: {e}"));
                    // A fatal stop is logged and recorded by the relay itself.
                    #[cfg(feature = "http-server")]
                    let _stopped = runtime.block_on(ws::run(
                        run_store.clone(),
                        config.clone(),
                        None,
                        Report::quiet(),
                    ));
                    #[cfg(not(feature = "http-server"))]
                    drop((runtime, &run_store, &config));
                },
                std::thread::sleep,
            );
        })
        .ok()
}

/// Exchange the instance card (E44); its answer records which protocol
/// the enrolled Hive speaks, which the relay needs before it connects.
fn instance_card(store: &HiveStore, config: &HiveConfig) -> Result<(), crate::HiveError> {
    let instance = store.instance()?.ok_or(crate::HiveError::NotConnected)?;
    let root = crate::root_cert::trust_root(store, config, &instance.url)?;
    let session = crate::auth::Session::load(store.clone(), root)?;
    crate::workers::register::announce_instance(&session).map(drop)
}

/// Run the relay in the foreground for `duration` (`lean-ctx hive bus
/// relay --for <seconds>`), e.g. to watch it or in tests. When another
/// relay (the daemon's) already runs, nothing runs here and `Ok(false)`
/// says so: the caller then asks that relay to announce now. `report` hears
/// what the relay sends (each hello's workers, roles and buses) and why it
/// sends nothing, when it does not.
pub fn run_for(
    config: HiveConfig,
    duration: std::time::Duration,
    report: Report,
) -> Result<bool, crate::HiveError> {
    let store = HiveStore::open()?;
    // The caller asks the running relay to announce
    // (`presence::announce_and_wait`) and prints its answer.
    let Ok(_relay) = relay_lock(&store) else {
        return Ok(false);
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| crate::HiveError::Local(e.to_string()))?;
    // A relay that stopped for good (a revocation, a lapsed lease) says
    // why, in the words every other CLI path uses.
    #[cfg(feature = "http-server")]
    match runtime.block_on(ws::run(store, config, Some(duration), report)) {
        Some(error) => Err(error),
        None => Ok(true),
    }
    #[cfg(not(feature = "http-server"))]
    {
        drop((runtime, store, config, duration, report));
        Err(crate::HiveError::Local(
            "the bus relay needs a build with the http-server feature".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #252: on pounce the daemon's relay thread panicked once (tokio's
    /// `spawn_blocking` could not get a thread) and was gone for good, so
    /// no worker of that machine was on the bus any more and role-gated
    /// claims were refused. A crash is now recorded and the relay starts
    /// again, with a growing wait; a run that returns ends it.
    #[test]
    fn a_crashed_relay_is_recorded_and_started_again() {
        let dir = tempfile::tempdir().unwrap();
        let store = HiveStore::at(dir.path().join("c"), dir.path().join("d"));
        let mut runs = 0;
        let mut waits = Vec::new();
        supervise(
            &store,
            || {
                runs += 1;
                assert!(runs <= 3, "a run that returns ends the supervisor");
                // The first two runs crash the way a refused thread spawn does.
                assert!(
                    runs >= 3,
                    "OS can't spawn worker thread: Resource temporarily unavailable"
                );
            },
            |wait| waits.push(wait),
        );
        assert_eq!(runs, 3, "two crashes, then a run that returned");
        assert_eq!(
            waits,
            [
                std::time::Duration::from_secs(5),
                std::time::Duration::from_secs(10)
            ]
        );
        let error = store.activity().last_error.unwrap_or_default();
        assert!(
            error.contains("the relay crashed (OS can't spawn worker thread")
                && error.contains("starts again in 10 s"),
            "{error}"
        );
    }

    #[test]
    fn restarts_wait_longer_up_to_a_cap() {
        assert_eq!(restart_delay(1), RESTART_FIRST);
        assert_eq!(restart_delay(2), RESTART_FIRST * 2);
        assert_eq!(restart_delay(9), RESTART_MAX);
        assert_eq!(restart_delay(u32::MAX), RESTART_MAX);
    }

    /// The foreground relay says why it waits once, not on every tick.
    #[test]
    fn a_report_does_not_repeat_its_last_line() {
        let said = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let sink = said.clone();
        let report = Report::to(move |line| sink.lock().unwrap().push(line.to_owned()));
        report.say("waiting");
        report.say("waiting");
        report.say("connected");
        report.say("waiting");
        assert_eq!(*said.lock().unwrap(), ["waiting", "connected", "waiting"]);
        Report::quiet().say("nobody hears this");
    }
}
