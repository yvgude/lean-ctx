// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::core::billing::Plan;
// Only the `#[cfg(unix)]` credential-permission tests still take the env lock
// directly; the plan-resolver tests use `isolated_data_dir()` (which locks
// internally). Gating the import keeps the Windows cross-compile warning-free.
#[cfg(unix)]
use crate::core::data_dir::test_env_lock;

#[test]
fn test_processes_never_reach_a_remote_cloud_endpoint() {
    for launched_by_cargo in [false, true] {
        for remote in [
            "https://api.leanctx.com",
            "http://10.0.0.1:8080",
            "http://localhost.evil.com",
            "http://127.0.0.1@api.leanctx.com",
            "ftp://127.0.0.1",
            "127.0.0.1:9",
        ] {
            assert!(
                !cloud_endpoint_allowed(remote, launched_by_cargo),
                "{remote}"
            );
        }
        for loopback in [
            "http://127.0.0.1:9",
            "http://127.3.2.1",
            "http://localhost:8088/prefix",
            "https://LOCALHOST",
            "http://[::1]:9",
        ] {
            assert!(
                cloud_endpoint_allowed(loopback, launched_by_cargo),
                "{loopback}"
            );
        }
    }
}

#[test]
fn api_url_redirects_test_processes_away_from_production() {
    let _lock = crate::core::data_dir::test_env_lock();
    let previous = std::env::var_os("LEAN_CTX_API_URL");

    // Every cloud call (feedback, stats, wrapped, telemetry) builds on api_url,
    // so the default and any remote override must land on the loopback sink.
    crate::test_env::remove_var("LEAN_CTX_API_URL");
    assert_eq!(api_url(), "http://127.0.0.1:9");
    crate::test_env::set_var("LEAN_CTX_API_URL", "https://api.leanctx.com");
    assert_eq!(api_url(), "http://127.0.0.1:9");
    crate::test_env::set_var("LEAN_CTX_API_URL", "http://127.0.0.1:8088");
    assert_eq!(api_url(), "http://127.0.0.1:8088");

    match previous {
        Some(value) => crate::test_env::set_var("LEAN_CTX_API_URL", value),
        None => crate::test_env::remove_var("LEAN_CTX_API_URL"),
    }
}

#[test]
fn cargo_test_processes_are_recognised_as_cargo_launched() {
    // Children spawned by integration tests inherit this, which is what keeps
    // the release binary under test from reaching production telemetry.
    assert!(launched_by_cargo());
}

#[test]
fn telemetry_delete_response_requires_explicit_confirmation() {
    assert!(deletion_confirmed(200, r#"{"deleted":true}"#).unwrap());
    assert!(!deletion_confirmed(204, "").unwrap());
    assert!(!deletion_confirmed(200, r#"{"deleted":false}"#).unwrap());
    assert!(deletion_confirmed(200, "not-json").is_err());
}

struct TelemetryTestEnvironment(Vec<(&'static str, Option<std::ffi::OsString>)>);

impl TelemetryTestEnvironment {
    // Call only while isolated_data_dir holds the shared test environment lock.
    fn new(listener: &std::net::TcpListener) -> Self {
        let values = [
            "LEAN_CTX_API_URL",
            "DO_NOT_TRACK",
            "LEAN_CTX_TELEMETRY",
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "http_proxy",
            "https_proxy",
            "all_proxy",
        ]
        .into_iter()
        .map(|key| {
            let previous = std::env::var_os(key);
            crate::test_env::remove_var(key);
            (key, previous)
        })
        .collect();
        let guard = Self(values);
        crate::test_env::set_var(
            "LEAN_CTX_API_URL",
            format!("http://{}", listener.local_addr().unwrap()),
        );
        guard
    }
}

impl Drop for TelemetryTestEnvironment {
    fn drop(&mut self) {
        for (key, value) in &self.0 {
            match value {
                Some(value) => crate::test_env::set_var(key, value),
                None => crate::test_env::remove_var(key),
            }
        }
    }
}

fn telemetry_test_config(contents: &str) {
    let path = crate::core::config::Config::path().unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

const TELEMETRY_ALLOWED: &str = "[telemetry]\nenabled = true\n";

fn telemetry_test_batch() -> crate::core::telemetry_v2::TelemetryBatchV2 {
    use crate::core::telemetry_v2::{ClientFamily, DistributionChannel};
    crate::core::telemetry_aggregate::build_daily_heartbeat(
        "550e8400-e29b-41d4-a716-446655440000".into(),
        "a".repeat(64),
        "2026-03-01".into(),
        DistributionChannel::Unknown,
        ClientFamily::Other,
    )
    .unwrap()
}

fn receive_telemetry(listener: &std::net::TcpListener) -> (std::net::TcpStream, serde_json::Value) {
    use std::io::{BufRead, Read};
    listener.set_nonblocking(true).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    let mut stream = loop {
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(std::time::Instant::now() < deadline, "request not received");
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            Err(error) => panic!("local accept failed: {error}"),
        }
    };
    stream.set_nonblocking(false).unwrap();
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(3)))
        .unwrap();
    let mut reader = std::io::BufReader::new(&mut stream);
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    assert!(line.starts_with("POST /api/telemetry/v2/batch "));
    let mut length = None;
    loop {
        line.clear();
        assert!(reader.read_line(&mut line).unwrap() > 0);
        if line == "\r\n" {
            break;
        }
        if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            length = Some(value.trim().parse::<usize>().unwrap());
        }
    }
    let mut body = vec![0; length.unwrap()];
    reader.read_exact(&mut body).unwrap();
    (stream, serde_json::from_slice(&body).unwrap())
}

#[test]
fn telemetry_send_rechecks_config_and_environment_before_any_connection() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let _env = TelemetryTestEnvironment::new(&listener);
    let batch = telemetry_test_batch();
    telemetry_test_config(TELEMETRY_ALLOWED);
    let stale = crate::core::config::Config::try_load_global().unwrap();
    assert!(stale.telemetry.send_eligible(None, None));
    for config in [
        "[telemetry]\nenabled = false\n",
        "[telemetry]\nenabled = true\npreference = 'explicitly_disabled'\n",
    ] {
        telemetry_test_config(config);
        assert_eq!(
            telemetry_v2_batch(&batch).unwrap_err(),
            "Telemetry sending is disabled"
        );
    }
    telemetry_test_config(TELEMETRY_ALLOWED);
    for (key, value) in [("DO_NOT_TRACK", "1"), ("LEAN_CTX_TELEMETRY", "off")] {
        crate::test_env::set_var(key, value);
        assert_eq!(
            telemetry_v2_batch(&batch).unwrap_err(),
            "Telemetry sending is disabled"
        );
        crate::test_env::remove_var(key);
    }
    telemetry_test_config("[telemetry");
    assert_eq!(
        telemetry_v2_batch(&batch).unwrap_err(),
        "Telemetry configuration unavailable"
    );
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[test]
fn telemetry_send_posts_the_validated_batch_to_loopback() {
    use std::io::Write;
    let _iso = crate::core::data_dir::isolated_data_dir();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let _env = TelemetryTestEnvironment::new(&listener);
    telemetry_test_config(TELEMETRY_ALLOWED);
    let batch = telemetry_test_batch();
    let server = std::thread::spawn(move || {
        let (mut stream, body) = receive_telemetry(&listener);
        let response = r#"{"message":"accepted"}"#;
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",
            response.len()
        )
        .unwrap();
        body
    });
    let result = telemetry_v2_batch(&batch);
    let captured = server.join().unwrap();
    assert_eq!(result.unwrap(), "accepted");
    assert_eq!(captured, serde_json::to_value(batch).unwrap());
}

#[test]
fn a_notice_in_the_acknowledgement_waits_for_the_terminal() {
    use std::io::Write;
    let _iso = crate::core::data_dir::isolated_data_dir();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let _env = TelemetryTestEnvironment::new(&listener);
    telemetry_test_config(TELEMETRY_ALLOWED);
    let batch = telemetry_test_batch();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = receive_telemetry(&listener);
        let response = r#"{"message":"accepted","notices":[
            {"id":"6f1c2b9e-3d4a-4b5c-8d6e-7f8091a2b3c4","message":"Fix is in 3.11.3","link":"https://leanctx.com/changelog"},
            {"id":"7f1c2b9e-3d4a-4b5c-8d6e-7f8091a2b3c4","message":"phish","link":"https://evil.example/"}]}"#;
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",
            response.len()
        )
        .unwrap();
    });
    assert_eq!(telemetry_v2_batch(&batch).unwrap(), "accepted");
    server.join().unwrap();
    let pending = crate::core::telemetry_notices::pending();
    assert_eq!(pending.len(), 1, "{pending:?}");
    assert_eq!(pending[0].message, "Fix is in 3.11.3");
}

#[test]
fn telemetry_send_times_out_when_the_gateway_stalls() {
    assert_telemetry_gateway_timeout(false);
}

#[test]
fn telemetry_send_times_out_after_receiving_response_headers() {
    assert_telemetry_gateway_timeout(true);
}

fn assert_telemetry_gateway_timeout(send_headers: bool) {
    use std::io::Write;
    let _iso = crate::core::data_dir::isolated_data_dir();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let _env = TelemetryTestEnvironment::new(&listener);
    telemetry_test_config(TELEMETRY_ALLOWED);
    let (release, wait) = std::sync::mpsc::channel::<()>();
    let server = std::thread::spawn(move || {
        let (mut stream, body) = receive_telemetry(&listener);
        if send_headers {
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1024\r\nConnection: close\r\n\r\n{")
                .unwrap();
        }
        let _ = wait.recv_timeout(std::time::Duration::from_secs(3));
        body
    });
    let batch = telemetry_test_batch();
    let started = std::time::Instant::now();
    let result = telemetry_v2_batch_with_timeout(&batch, std::time::Duration::from_millis(300));
    let elapsed = started.elapsed();
    let _ = release.send(());
    let captured = server.join().unwrap();
    let error = result.unwrap_err().to_ascii_lowercase();
    assert!(
        error.contains("timeout") || error.contains("timed out"),
        "{error}"
    );
    assert!(elapsed < std::time::Duration::from_secs(2), "{elapsed:?}");
    assert_eq!(captured, serde_json::to_value(batch).unwrap());
}

#[test]
fn checkout_response_requires_credential_free_absolute_https_url() {
    assert_eq!(
        parse_checkout_url(&serde_json::json!({
            "url": "https://checkout.stripe.com/c/pay/test_session?prefilled_email=x"
        }))
        .unwrap(),
        "https://checkout.stripe.com/c/pay/test_session?prefilled_email=x"
    );
    for response in [
        serde_json::json!({}),
        serde_json::json!({ "url": null }),
        serde_json::json!({ "url": "" }),
        serde_json::json!({ "url": "/checkout/session" }),
        serde_json::json!({ "url": "http://checkout.stripe.com/session" }),
        serde_json::json!({ "url": "https://user:secret@example.com/session" }),
        serde_json::json!({ "url": "https://example.com/a\nb" }),
        serde_json::json!({ "url": "https://example.com/a\rb" }),
        serde_json::json!({ "url": "https://example.com/a\u{001b}]0;owned" }),
    ] {
        assert!(
            parse_checkout_url(&response).is_err(),
            "accepted {response}"
        );
    }
}

#[test]
fn existing_card_publish_response_carries_recovery_challenge() {
    let card: PublishedCard = serde_json::from_value(serde_json::json!({
        "id": "card-1",
        "url": "https://leanctx.com/w/card-1",
        "edit_token_challenge": "nonce-1",
        "challenge_expires_in_secs": 300
    }))
    .unwrap();
    assert!(card.edit_token.is_none());
    assert_eq!(card.edit_token_challenge.as_deref(), Some("nonce-1"));
    assert_eq!(card.challenge_expires_in_secs, Some(300));
    assert!(!card.account_claimed);
}

#[test]
fn grace_window_boundaries_are_inclusive_and_skew_safe() {
    let now = 1_000_000_000;
    let day = 86_400;
    assert_eq!(plan_within_grace(now, now, 14), (true, 0));
    // Exactly at the edge stays valid (inclusive).
    assert_eq!(plan_within_grace(now - 14 * day, now, 14), (true, 14));
    // One day past → expired.
    assert_eq!(plan_within_grace(now - 15 * day, now, 14), (false, 15));
    // Clock skew (future timestamp) is clamped to age 0, never negative.
    assert_eq!(plan_within_grace(now + day, now, 14), (true, 0));
}

#[test]
fn plan_cache_roundtrips_through_json() {
    let c = PlanCache {
        plan: "pro".into(),
        verified_at: 42,
    };
    let back: PlanCache = serde_json::from_str(&serde_json::to_string(&c).unwrap()).unwrap();
    assert_eq!(back.plan, "pro");
    assert_eq!(back.verified_at, 42);
}

#[test]
fn cloud_access_handles_canonical_and_legacy_plan_values() {
    for value in [
        "pro",
        "team",
        "business",
        "biz",
        "enterprise",
        "ent",
        "cloud",
    ] {
        assert!(plan_value_has_cloud_access(value), "{value}");
    }
    for value in ["community", "free", "supporter", "sponsor", "unknown"] {
        assert!(!plan_value_has_cloud_access(value), "{value}");
    }
}

#[test]
fn cached_resolve_grants_within_grace_then_expires_to_community() {
    // Isolate all dirs (config + cache) so the resolver reads only the cache
    // this test writes, not a developer's real plan cache.
    let _iso = crate::core::data_dir::isolated_data_dir();

    // A fresh save is served from cache, within grace, at full plan.
    save_plan("pro").unwrap();
    let eff = resolve_effective_plan_cached();
    assert_eq!(eff.plan, Plan::Pro);
    assert_eq!(eff.source, PlanSource::Cached);

    // Backdate beyond grace → paid entitlements fail closed to Community.
    let stale = PlanCache {
        plan: "pro".into(),
        verified_at: now_unix() - (PLAN_GRACE_DAYS + 1) * 86_400,
    };
    std::fs::write(plan_cache_path(), serde_json::to_string(&stale).unwrap()).unwrap();
    let eff = resolve_effective_plan_cached();
    assert_eq!(eff.plan, Plan::Community);
    assert_eq!(eff.source, PlanSource::Expired);
}

#[test]
fn no_cache_resolves_to_community_none() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    let eff = resolve_effective_plan_cached();
    assert_eq!(eff.plan, Plan::Community);
    assert_eq!(eff.source, PlanSource::None);
}

// P0-2 (#414): credentials must be owner-only on disk.
#[cfg(unix)]
#[test]
fn credentials_are_written_owner_only_and_atomic() {
    use std::os::unix::fs::PermissionsExt;
    let _env = test_env_lock();
    let tmp = tempfile::tempdir().unwrap();
    crate::test_env::set_var("LEAN_CTX_DATA_DIR", tmp.path());

    save_credentials("sk-test-key", "user-1", "a@b.c").unwrap();

    let path = credentials_path();
    let mode = std::fs::metadata(&path).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600, "credentials.json must be 0o600");

    let dir_mode = std::fs::metadata(config_dir())
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(
        dir_mode & 0o077,
        0,
        "cloud dir must not be group/world accessible"
    );

    // No tmp file leftovers from the atomic write.
    let leftovers: Vec<_> = std::fs::read_dir(config_dir())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().contains(".tmp."))
        .collect();
    assert!(leftovers.is_empty(), "atomic write must not leak tmp files");

    crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
}

// P0-2 (#414): pre-existing world-readable credentials are tightened on load.
#[cfg(unix)]
#[test]
fn loose_credential_permissions_are_tightened_on_load() {
    use std::os::unix::fs::PermissionsExt;
    let _env = test_env_lock();
    let tmp = tempfile::tempdir().unwrap();
    crate::test_env::set_var("LEAN_CTX_DATA_DIR", tmp.path());

    std::fs::create_dir_all(config_dir()).unwrap();
    let path = credentials_path();
    std::fs::write(&path, "{}").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

    let _ = load_credentials();

    let mode = std::fs::metadata(&path).unwrap().permissions().mode();
    assert_eq!(
        mode & 0o777,
        0o600,
        "legacy file must be tightened to 0o600"
    );

    crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
}

#[test]
fn legacy_plan_txt_is_migrated_but_treated_as_stale() {
    let _iso = crate::core::data_dir::isolated_data_dir();
    // Only the legacy flat file exists (no timestamp) → past grace until refresh.
    std::fs::create_dir_all(config_dir()).unwrap();
    std::fs::write(config_dir().join("plan.txt"), "team").unwrap();
    let cache = cached_plan().unwrap();
    assert_eq!(cache.plan, "team");
    assert_eq!(cache.verified_at, 0);
    assert_eq!(resolve_effective_plan_cached().source, PlanSource::Expired);
}
