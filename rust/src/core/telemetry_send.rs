// SPDX-License-Identifier: Apache-2.0
//! Sending the telemetry-v2 daily totals: on MCP exit, on tool-call ticks and
//! from the daemon's hourly loop, with the local send ledger.

use crate::core::config::Config;

/// Network budget for the send that runs as the MCP server exits.
pub const EXIT_TELEMETRY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);
const BACKGROUND_TELEMETRY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Whether this process may collect and send telemetry right now. Fails closed:
/// an unreadable config never counts as consent.
pub(crate) fn telemetry_send_eligible() -> bool {
    // In a test process only a thread that holds the test environment lock
    // knows whose data directory it resolves. The detached send of a tool
    // call's background tick runs on a thread of its own and resolved the
    // isolated directory of whichever test held the lock: it took that
    // test's aggregate lease, and that test's preview failed with "a send is
    // in progress" (25.10.).
    #[cfg(test)]
    if !crate::core::data_dir::holds_test_env_lock() {
        return false;
    }
    let Ok(config) = Config::try_load_global() else {
        return false;
    };
    let do_not_track = crate::core::host_env::var("DO_NOT_TRACK");
    let telemetry_override = crate::core::host_env::var("LEAN_CTX_TELEMETRY");
    config
        .telemetry
        .send_eligible(do_not_track.as_deref(), telemetry_override.as_deref())
}

/// Send the cumulative telemetry totals if the aggregate admits a send now.
/// Returns the acknowledged bucket. Admission (daily cap, spacing, "nothing
/// new") is decided under the aggregate lock, so callers may call freely.
pub fn send_telemetry(trigger: crate::core::telemetry_aggregate::SendTrigger) -> Option<String> {
    if !telemetry_send_eligible() {
        return None;
    }
    // Persist first so the counters survive even when no send is admitted.
    if let Err(error) = crate::core::telemetry_aggregate::persist_process_counters() {
        tracing::debug!("telemetry counters not persisted: {error}");
    }
    let lease = match crate::core::telemetry_aggregate::begin_send(trigger) {
        Ok(lease) => lease,
        Err(reason) => {
            tracing::debug!("telemetry send skipped: {reason}");
            return None;
        }
    };
    let batch = lease.batch().clone();
    let timeout = match trigger {
        crate::core::telemetry_aggregate::SendTrigger::Exit => EXIT_TELEMETRY_TIMEOUT,
        crate::core::telemetry_aggregate::SendTrigger::Periodic => BACKGROUND_TELEMETRY_TIMEOUT,
    };
    if let Err(error) = crate::telemetry_transport::telemetry_v2_batch_with_timeout(&batch, timeout)
    {
        tracing::debug!("telemetry send failed, batch kept for retry: {error}");
        return None;
    }
    let installation_id = batch.events.first()?.installation_id.clone();
    let payload = serde_json::to_vec(&batch).ok()?;
    use sha2::Digest;
    let record = crate::core::telemetry_ledger::HeartbeatRecord {
        timestamp: chrono::Utc::now().to_rfc3339(),
        installation_id,
        version: env!("CARGO_PKG_VERSION").to_string(),
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        schema_version: batch.schema_version,
        event_names: batch
            .events
            .iter()
            .map(|event| event.event.name().to_string())
            .collect(),
        payload_hash: hex::encode(sha2::Sha256::digest(payload)),
        endpoint: telemetry_ledger_endpoint(),
        status: "success".to_string(),
    };
    // Without a ledger entry the send stays pending and is retried with the
    // same bytes; the server replaces per-day rows, so that is harmless.
    if let Err(error) = crate::core::telemetry_ledger::append(&record) {
        tracing::debug!("telemetry ledger append failed: {error}");
        return None;
    }
    if let Err(error) = lease.commit() {
        tracing::debug!("telemetry acknowledgement failed: {error}");
        return None;
    }
    batch
        .events
        .first()
        .map(|event| event.timestamp_bucket.clone())
}

/// How often the daemon offers the day's totals. The aggregate's admission
/// (daily cap, growing spacing, "nothing new") decides whether a send happens.
#[cfg(feature = "http-server")]
const DAEMON_TELEMETRY_INTERVAL: std::time::Duration = std::time::Duration::from_hours(1);
/// Delay before the daemon's first offer so its startup stays light.
#[cfg(feature = "http-server")]
const DAEMON_TELEMETRY_INITIAL_DELAY: std::time::Duration = std::time::Duration::from_mins(2);

/// Spawn the daemon's telemetry loop (must run inside a Tokio runtime).
///
/// Without it, sends happen only inside an MCP server (tool-call ticks and
/// exit), so installs that use lean-ctx through hooks, the CLI or the proxy
/// were never counted. The daemon is the one long-lived process setup starts;
/// it also forwards the counters other processes persisted. Eligibility is
/// re-checked on every tick, so an opt-out takes effect without a restart.
#[cfg(feature = "http-server")]
pub(crate) fn spawn_daemon_telemetry() {
    tokio::spawn(async {
        tokio::time::sleep(DAEMON_TELEMETRY_INITIAL_DELAY).await;
        loop {
            let _ = tokio::task::spawn_blocking(daemon_telemetry_tick).await;
            tokio::time::sleep(DAEMON_TELEMETRY_INTERVAL).await;
        }
    });
}

#[cfg(feature = "http-server")]
fn daemon_telemetry_tick() {
    if !telemetry_send_eligible() {
        return;
    }
    if let Err(error) = crate::core::telemetry_aggregate::record_current_version() {
        tracing::debug!("telemetry version aggregate unavailable: {error}");
    }
    send_telemetry(crate::core::telemetry_aggregate::SendTrigger::Periodic);
}

/// The endpoint the send actually used: `api_url()`, so a test or
/// cargo-launched process records the loopback sink, not production.
fn telemetry_ledger_endpoint() -> String {
    sanitized_telemetry_endpoint(&crate::telemetry_transport::api_url())
}

fn sanitized_telemetry_endpoint(raw: &str) -> String {
    let Ok(mut url) = reqwest::Url::parse(raw) else {
        return "https://api.leanctx.com/api/telemetry/v2/batch".to_string();
    };
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return "https://api.leanctx.com/api/telemetry/v2/batch".to_string();
    }
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_query(None);
    url.set_fragment(None);
    url.set_path("/api/telemetry/v2/batch");
    url.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn telemetry_ledger_endpoint_removes_credentials_query_and_fragment() {
        assert_eq!(
            sanitized_telemetry_endpoint("https://user:secret@example.test/base?token=x#private"),
            "https://example.test/api/telemetry/v2/batch"
        );
        assert_eq!(
            sanitized_telemetry_endpoint("file:///private/path"),
            "https://api.leanctx.com/api/telemetry/v2/batch"
        );
    }

    #[test]
    fn telemetry_ledger_records_the_endpoint_the_send_used() {
        let _lock = crate::core::data_dir::test_env_lock();
        crate::test_env::set_var("LEAN_CTX_API_URL", "https://api.leanctx.com");
        let endpoint = telemetry_ledger_endpoint();
        crate::test_env::remove_var("LEAN_CTX_API_URL");
        // A test process sends to the loopback sink, so it records that.
        assert_eq!(endpoint, "http://127.0.0.1:9/api/telemetry/v2/batch");
    }

    #[test]
    #[serial_test::serial]
    fn daemon_tick_honours_an_explicit_opt_out() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        let config = Config::path().expect("config path");
        std::fs::create_dir_all(config.parent().expect("config dir")).expect("config dir");
        std::fs::write(&config, "[telemetry]\nenabled = false\n").expect("write opt-out");
        daemon_telemetry_tick();
        let state = crate::core::paths::state_dir().expect("state dir");
        assert!(!state.join("telemetry_v2_aggregate.json").exists());
        assert!(!state.join("telemetry_v2_one_shots.json").exists());
    }

    #[test]
    #[serial_test::serial]
    fn daemon_tick_attempts_a_send_for_a_default_install() {
        // No config file: the default-on install the daemon exists to cover.
        // The test guard sends to a discard port, so the batch stays pending,
        // which proves the tick reached the send path.
        let _iso = crate::core::data_dir::isolated_data_dir();
        daemon_telemetry_tick();
        let state = crate::core::paths::state_dir().expect("state dir");
        assert!(state.join("telemetry_v2_aggregate.json").exists());
    }

    /// A send from a thread without the test environment lock — the detached
    /// background tick of a tool call — resolves whichever data directory a
    /// locked test isolated at that moment, and its lease made that test's
    /// preview fail (25.10.). In a test process it sends nothing; the locked
    /// thread itself still reaches the send path (test above).
    #[test]
    #[serial_test::serial]
    fn a_thread_without_the_test_lock_never_sends() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        let sent = std::thread::spawn(|| {
            send_telemetry(crate::core::telemetry_aggregate::SendTrigger::Periodic)
        })
        .join()
        .expect("send thread");
        assert_eq!(sent, None);
        let state = crate::core::paths::state_dir().expect("state dir");
        assert!(!state.join("telemetry_v2_aggregate.json").exists());
        assert!(!state.join("telemetry_v2_one_shots.json").exists());
    }
}
