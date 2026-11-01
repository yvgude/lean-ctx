//! `/api/telemetry` — anonymous telemetry heartbeat status and history.
//!
//! Telemetry is on by default. `enabled` says whether it is sent right now,
//! decided like the sender decides it (`blocked_by` names why not), and
//! `current_payload` is the exact batch the next send would carry
//! (`lean-ctx telemetry show`), with the installation ID masked. The route
//! only reads: it never creates the installation ID.

use serde_json::json;

pub(super) fn handle(
    path: &str,
    _query_str: &str,
    _method: &str,
    _body: &str,
) -> Option<(&'static str, &'static str, String)> {
    match path {
        "/api/telemetry" => Some(telemetry_status()),
        _ => None,
    }
}

fn telemetry_status() -> (&'static str, &'static str, String) {
    let cfg = crate::core::config::Config::load();

    // A read never creates the installation ID: `null` until the first send
    // (or `lean-ctx telemetry`) creates it.
    let masked_id = crate::core::installation_id::existing()
        .map(|id| crate::core::installation_id::masked(&id));

    let history = crate::core::telemetry_ledger::read_all();
    let total_sent = history.len();

    let history_entries: Vec<serde_json::Value> = history
        .iter()
        .rev()
        .take(100)
        .map(|r| {
            json!({
                "timestamp": r.timestamp,
                "version": r.version,
                "os": r.os,
                "arch": r.arch,
            })
        })
        .collect();

    let version_dist = compute_distribution(&history, |r| r.version.clone());
    let os_dist = compute_distribution(&history, |r| r.os.clone());
    let arch_dist = compute_distribution(&history, |r| r.arch.clone());

    let daily_counts = compute_daily_counts(&history);

    let blocked_by = send_blocker();
    let (current_payload, payload_error) = if blocked_by.is_some() {
        (serde_json::Value::Null, None)
    } else if masked_id.is_none() {
        (
            serde_json::Value::Null,
            Some("no installation ID yet: the first send creates it".to_string()),
        )
    } else {
        match crate::core::telemetry_aggregate::preview_daily_batch() {
            Ok(batch) => (masked_batch(&batch), None),
            Err(error) => (serde_json::Value::Null, Some(error)),
        }
    };

    let payload = json!({
        // Whether telemetry is sent right now, as the sender decides it.
        "enabled": blocked_by.is_none(),
        "blocked_by": blocked_by,
        "installation_id": masked_id,
        "last_heartbeat": cfg.telemetry.last_heartbeat,
        "total_sent": total_sent,
        "current_payload": current_payload,
        "payload_error": payload_error,
        "history": history_entries,
        "distributions": {
            "version": version_dist,
            "os": os_dist,
            "arch": arch_dist,
        },
        "daily_counts": daily_counts,
    });

    (
        "200 OK",
        "application/json",
        serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string()),
    )
}

/// Why telemetry is not sent right now, or `None` when it is: the sender's
/// authority (`TelemetryConfig::send_eligible` over the global config,
/// `DO_NOT_TRACK` and `LEAN_CTX_TELEMETRY`), then the CI rule. Fails closed
/// like the sender: an unreadable config is no consent.
fn send_blocker() -> Option<&'static str> {
    let Ok(config) = crate::core::config::Config::try_load_global() else {
        return Some("config_unreadable");
    };
    let do_not_track = crate::core::host_env::var("DO_NOT_TRACK");
    let env_override = crate::core::host_env::var("LEAN_CTX_TELEMETRY");
    let telemetry = &config.telemetry;
    if telemetry.send_eligible(do_not_track.as_deref(), env_override.as_deref()) {
        return crate::core::telemetry_consent::running_in_ci().then_some("ci");
    }
    Some(if telemetry.explicitly_disabled() {
        "preference"
    } else if crate::core::config::TelemetryConfig::environment_disables(
        do_not_track.as_deref(),
        env_override.as_deref(),
    ) {
        "environment"
    } else {
        "policy"
    })
}

/// The batch the next send would carry, with the installation ID masked and
/// the deletion-token hash left out, as everywhere in the dashboard.
fn masked_batch(batch: &crate::core::telemetry_v2::TelemetryBatchV2) -> serde_json::Value {
    let mut value = serde_json::to_value(batch).unwrap_or_default();
    if let Some(obj) = value.as_object_mut() {
        obj.remove("deletion_token_hash");
    }
    if let Some(events) = value
        .get_mut("events")
        .and_then(serde_json::Value::as_array_mut)
    {
        for event in events {
            if let Some(id) = event.get("installation_id").and_then(|id| id.as_str()) {
                let masked = crate::core::installation_id::masked(id);
                event["installation_id"] = masked.into();
            }
        }
    }
    value
}

fn compute_distribution(
    history: &[crate::core::telemetry_ledger::HeartbeatRecord],
    key_fn: impl Fn(&crate::core::telemetry_ledger::HeartbeatRecord) -> String,
) -> Vec<serde_json::Value> {
    let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for record in history {
        *counts.entry(key_fn(record)).or_default() += 1;
    }
    let mut entries: Vec<_> = counts.into_iter().collect();
    entries.sort_by_key(|e| std::cmp::Reverse(e.1));
    entries
        .into_iter()
        .map(|(label, count)| json!({"label": label, "count": count}))
        .collect()
}

fn compute_daily_counts(
    history: &[crate::core::telemetry_ledger::HeartbeatRecord],
) -> Vec<serde_json::Value> {
    let mut counts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for record in history {
        let date = record.timestamp.get(..10).unwrap_or(&record.timestamp);
        *counts.entry(date.to_string()).or_default() += 1;
    }
    counts
        .into_iter()
        .map(|(date, count)| json!({"date": date, "count": count}))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sets (or clears) consent variables for one test and restores them.
    struct ConsentEnv(Vec<(&'static str, Option<std::ffi::OsString>)>);

    impl ConsentEnv {
        fn set(pairs: &[(&'static str, Option<&str>)]) -> Self {
            let previous = pairs
                .iter()
                .map(|(key, value)| {
                    let old = std::env::var_os(key);
                    match value {
                        Some(value) => crate::test_env::set_var(key, value),
                        None => crate::test_env::remove_var(key),
                    }
                    (*key, old)
                })
                .collect();
            Self(previous)
        }
    }

    impl Drop for ConsentEnv {
        fn drop(&mut self) {
            for (key, value) in &self.0 {
                match value {
                    Some(value) => crate::test_env::set_var(key, value),
                    None => crate::test_env::remove_var(key),
                }
            }
        }
    }

    fn status() -> serde_json::Value {
        let (status, _, body) = handle("/api/telemetry", "", "GET", "").unwrap();
        assert_eq!(status, "200 OK");
        serde_json::from_str(&body).unwrap()
    }

    #[test]
    fn the_environment_opt_outs_turn_the_status_off_like_the_sender() {
        let _data = crate::core::data_dir::isolated_data_dir();
        for opt_out in [
            [("DO_NOT_TRACK", Some("1")), ("LEAN_CTX_TELEMETRY", None)],
            [("DO_NOT_TRACK", None), ("LEAN_CTX_TELEMETRY", Some("off"))],
        ] {
            let _env = ConsentEnv::set(&opt_out);
            assert!(!crate::core::telemetry_send::telemetry_send_eligible());
            let v = status();
            assert_eq!(v["enabled"], false, "{opt_out:?}");
            assert_eq!(v["blocked_by"], "environment", "{opt_out:?}");
            assert!(v["current_payload"].is_null());
        }
    }

    #[test]
    fn the_preview_is_the_batch_the_sender_would_send_with_the_id_masked() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _env = ConsentEnv::set(&[("DO_NOT_TRACK", None), ("LEAN_CTX_TELEMETRY", None)]);
        let id = crate::core::installation_id::get_or_create().unwrap();
        let v = status();
        if crate::core::telemetry_consent::running_in_ci() {
            assert_eq!(v["blocked_by"], "ci");
            return;
        }
        assert_eq!(v["enabled"], true, "{v}");
        let batch = crate::core::telemetry_aggregate::preview_daily_batch().unwrap();
        let preview = &v["current_payload"];
        assert_eq!(preview["schema_version"], batch.schema_version);
        assert_eq!(
            preview["events"].as_array().map(Vec::len),
            Some(batch.events.len())
        );
        assert_eq!(
            preview["events"][0]["installation_id"],
            crate::core::installation_id::masked(&id)
        );
        assert!(preview.get("deletion_token_hash").is_none());
        assert!(!v.to_string().contains(&id), "the full ID never leaves");
    }
}
