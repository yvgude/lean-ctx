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
// Only the socket loop (`ws`) uses it; gated the same way as `ws`.
#[cfg(feature = "http-server")]
mod frames;
pub mod handoff;
pub mod leases;
pub mod messages;
pub mod presence;
pub mod relay;
pub mod relay_v2;
pub mod v2;
pub mod veto;
#[cfg(feature = "http-server")]
mod ws;

use crate::core::hive::config::{HiveBusMode, HiveConfig};
use crate::core::hive::state::{HiveStore, StateLock, lock_state_within};

/// How often the relay's process looks after CLI claims (lean-ctx #3).
const CLI_CLAIMS_PASS: std::time::Duration = std::time::Duration::from_secs(30);

/// One relay per machine (RB8 M11): two would overwrite each other's
/// cursors and send twice. The daemon's relay and `hive bus relay --for`
/// take `<data>/hive/relay.lock` without waiting; whoever does not get it
/// does not relay.
fn relay_lock(store: &HiveStore) -> Result<StateLock, crate::core::hive::HiveError> {
    lock_state_within(store.data_dir(), "relay", std::time::Duration::ZERO).map_err(|_| {
        crate::core::hive::HiveError::Local(
            "another bus relay is already running on this machine".into(),
        )
    })
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
            let _relay = match relay_lock(&store) {
                Ok(lock) => lock,
                Err(error) => {
                    tracing::info!("hive bus: {error}");
                    return;
                }
            };
            // Engine start: the instance card (E44, SPEC-ENGINE §3.6). It
            // runs on this thread before the socket loop and never stops it.
            announce_instance(&store, &config);
            // The relay's process outlives CLI calls: it keeps the claims
            // `lean-ctx hive tasks claim` made for agents still at work.
            let _ = std::thread::Builder::new()
                .name("hive-cli-claims".into())
                .spawn(|| {
                    loop {
                        std::thread::sleep(CLI_CLAIMS_PASS);
                        crate::core::hive::tasks::keep_cli_claims();
                    }
                });
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            // A fatal stop is logged and recorded by the relay itself.
            #[cfg(feature = "http-server")]
            let _stopped = runtime.block_on(ws::run(store, config, None));
            #[cfg(not(feature = "http-server"))]
            drop((runtime, store, config));
        })
        .ok()
}

fn announce_instance(store: &HiveStore, config: &HiveConfig) {
    let result = (|| {
        let instance = store
            .instance()?
            .ok_or(crate::core::hive::HiveError::NotConnected)?;
        let root = crate::core::hive::root_cert::trust_root(store, config, &instance.url)?;
        let session = crate::core::hive::auth::Session::load(store.clone(), root)?;
        crate::core::hive::workers::register::announce_instance(&session)
    })();
    if let Err(error) = result {
        tracing::info!("hive instance card: {error}");
    }
}

/// Run the relay in the foreground for `duration` (`lean-ctx hive bus
/// relay --for <seconds>`), e.g. to watch it or in tests. When another
/// relay (the daemon's) already runs, it is asked to announce this
/// machine's agents now instead, and `Ok(false)` says so.
pub fn run_for(
    config: HiveConfig,
    duration: std::time::Duration,
) -> Result<bool, crate::core::hive::HiveError> {
    let store = HiveStore::open()?;
    let Ok(_relay) = relay_lock(&store) else {
        presence::request_announce();
        return Ok(false);
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| crate::core::hive::HiveError::Local(e.to_string()))?;
    // A relay that stopped for good (a revocation, a lapsed lease) says
    // why, in the words every other CLI path uses.
    #[cfg(feature = "http-server")]
    match runtime.block_on(ws::run(store, config, Some(duration))) {
        Some(error) => Err(error),
        None => Ok(true),
    }
    #[cfg(not(feature = "http-server"))]
    {
        drop((runtime, store, config, duration));
        Err(crate::core::hive::HiveError::Local(
            "the bus relay needs a build with the http-server feature".into(),
        ))
    }
}
