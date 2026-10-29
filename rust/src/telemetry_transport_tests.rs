// SPDX-License-Identifier: Apache-2.0

use super::*;

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
