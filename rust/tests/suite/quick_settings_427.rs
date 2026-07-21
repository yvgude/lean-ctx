//! #427 Quick Settings endpoint — end-to-end persistence + auth.
//!
//! Boots the real dashboard as a subprocess against a fully isolated tempdir.
//! A fresh config leaves `proxy_enabled` unset, so `spawn_proxy_if_needed` is a
//! no-op and nothing on the developer's machine is touched. The test then drives
//! the actual `/api/settings` HTTP surface: GET returns the four switches, an
//! authenticated POST flips `terse_agent`, and the new value is shown to persist
//! both in the endpoint's own response (a fresh `Config::load`) and on disk in
//! `config.toml`. A wrong Bearer token is rejected with 401.
#![cfg(unix)]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const TOKEN: &str = "lctx_test_token_427";

/// Isolated dashboard process; killed on drop so no server outlives the test.
struct Dashboard {
    child: Child,
    port: u16,
    config: PathBuf,
    _root: tempfile::TempDir,
}

impl Drop for Dashboard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Grab a free loopback port by binding `:0` and immediately releasing it.
fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    let port = listener.local_addr().expect("local_addr").port();
    drop(listener);
    port
}

fn start_dashboard() -> Dashboard {
    let root = tempfile::tempdir().expect("tempdir");
    let home = root.path().join("home");
    let config = root.path().join("config");
    let data = root.path().join("data");
    let state = root.path().join("state");
    let cache = root.path().join("cache");
    for d in [&home, &config, &data, &state, &cache] {
        std::fs::create_dir_all(d).unwrap();
    }
    let port = free_port();
    let child = Command::new(env!("CARGO_BIN_EXE_lean-ctx"))
        .args([
            "dashboard",
            &format!("--port={port}"),
            &format!("--auth-token={TOKEN}"),
            "--open=none",
        ])
        .env("HOME", &home)
        .env("LEAN_CTX_CONFIG_DIR", &config)
        .env("LEAN_CTX_DATA_DIR", &data)
        .env("LEAN_CTX_STATE_DIR", &state)
        .env("LEAN_CTX_CACHE_DIR", &cache)
        // Never talk to (or start) the developer's daemon.
        .env("LEAN_CTX_HOOK_CHILD", "1")
        // Settings are env-overridable; strip any host values so the toggles
        // reflect config alone and the assertions are deterministic.
        .env_remove("LEAN_CTX_TOOL_PROFILE")
        .env_remove("LEAN_CTX_COMPRESSION")
        .env_remove("LEAN_CTX_TERSE_AGENT")
        .env_remove("LEAN_CTX_STRUCTURE_FIRST")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn lean-ctx dashboard");
    Dashboard {
        child,
        port,
        config,
        _root: root,
    }
}

/// Minimal HTTP/1.1 client returning `(status_code, body)`. Reads by
/// `Content-Length` when present, else to EOF, so it works whether or not the
/// server keeps the socket alive.
fn http(
    port: u16,
    method: &str,
    path: &str,
    token: &str,
    body: Option<&str>,
) -> Option<(u16, String)> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .ok()?;

    let mut req = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\
         Authorization: Bearer {token}\r\nConnection: close\r\n"
    );
    if let Some(b) = body {
        req.push_str("Content-Type: application/json\r\n");
        req.push_str(&format!("Content-Length: {}\r\n\r\n", b.len()));
        req.push_str(b);
    } else {
        req.push_str("\r\n");
    }
    stream.write_all(req.as_bytes()).ok()?;
    stream.flush().ok()?;

    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            break find_marker(&buf, b"\r\n\r\n")?;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(pos) = find_marker(&buf, b"\r\n\r\n") {
            break pos;
        }
    };

    let headers = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let status = headers
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|c| c.parse::<u16>().ok())?;
    let content_len = headers
        .lines()
        .find(|l| l.to_ascii_lowercase().starts_with("content-length:"))
        .and_then(|l| l.split_once(':').map(|x| x.1))
        .and_then(|v| v.trim().parse::<usize>().ok());

    let body_start = header_end + 4;
    match content_len {
        Some(cl) => {
            while buf.len() - body_start < cl {
                let n = stream.read(&mut chunk).unwrap_or(0);
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
            }
        }
        None => loop {
            let n = stream.read(&mut chunk).unwrap_or(0);
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
        },
    }
    Some((
        status,
        String::from_utf8_lossy(&buf[body_start..]).to_string(),
    ))
}

fn find_marker(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Poll `/api/settings` until the server answers 200, or fail loudly with the
/// child's stderr if it exited early (a bind clash or panic, not a flake).
fn wait_ready(dash: &mut Dashboard) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some((200, _)) = http(dash.port, "GET", "/api/settings", TOKEN, None) {
            return;
        }
        if let Ok(Some(status)) = dash.child.try_wait() {
            let mut err = String::new();
            if let Some(mut s) = dash.child.stderr.take() {
                let _ = s.read_to_string(&mut err);
            }
            panic!("dashboard exited early ({status}); stderr:\n{err}");
        }
        assert!(
            Instant::now() < deadline,
            "dashboard never became ready on port {}",
            dash.port
        );
        std::thread::sleep(Duration::from_millis(150));
    }
}

#[test]
fn settings_endpoint_persists_and_enforces_auth() {
    let mut dash = start_dashboard();
    wait_ready(&mut dash);

    // GET exposes all four switches.
    let (status, body) =
        http(dash.port, "GET", "/api/settings", TOKEN, None).expect("GET /api/settings");
    assert_eq!(status, 200, "GET should be 200; body: {body}");
    for key in [
        "compression_level",
        "tool_profile",
        "structure_first",
        "terse_agent",
    ] {
        assert!(body.contains(key), "settings payload missing {key}: {body}");
    }

    // POST flips terse_agent and echoes the fresh state from the source of truth.
    let (status, body) = http(
        dash.port,
        "POST",
        "/api/settings",
        TOKEN,
        Some(r#"{"key":"terse_agent","value":"ultra"}"#),
    )
    .expect("POST /api/settings");
    assert_eq!(status, 200, "POST should persist; body: {body}");
    assert!(
        body.contains(r#""value":"ultra""#),
        "POST echo must show the new value; body: {body}"
    );

    // It survives a fresh GET (re-read from config), proving real persistence.
    let (_status, body) =
        http(dash.port, "GET", "/api/settings", TOKEN, None).expect("GET after POST");
    assert!(
        body.contains(r#""value":"ultra""#),
        "value must persist across reads; body: {body}"
    );

    // …and it actually landed in config.toml on disk.
    let cfg = std::fs::read_to_string(dash.config.join("config.toml")).unwrap_or_default();
    assert!(
        cfg.contains("terse_agent") && cfg.to_lowercase().contains("ultra"),
        "config.toml must record terse_agent=ultra; got:\n{cfg}"
    );

    // Auth gate: a wrong Bearer token is rejected with 401.
    let (status, _body) =
        http(dash.port, "GET", "/api/settings", "wrong-token", None).expect("GET with bad token");
    assert_eq!(status, 401, "a bad Bearer token must be 401");
}

/// GH #431: selecting "Lean" (unpin) must not snap back to "Power". A fresh
/// config is unpinned, so the default reads as `lean`; pinning `power` then
/// `lean` must round-trip to `lean`, not the internally-effective `power`.
#[test]
fn tool_profile_lean_does_not_snap_to_power() {
    let mut dash = start_dashboard();
    wait_ready(&mut dash);

    // Fresh config is unpinned → the toggle shows "lean", never "power".
    let (_status, body) =
        http(dash.port, "GET", "/api/settings", TOKEN, None).expect("GET /api/settings");
    assert!(
        body.contains(r#""value":"lean""#),
        "unpinned default must read as lean; body: {body}"
    );
    assert!(
        !body.contains(r#""value":"power""#),
        "unpinned default must not read as power; body: {body}"
    );

    // Pin power and confirm it sticks.
    let (status, body) = http(
        dash.port,
        "POST",
        "/api/settings",
        TOKEN,
        Some(r#"{"key":"tool_profile","value":"power"}"#),
    )
    .expect("POST power");
    assert_eq!(status, 200, "pinning power should succeed; body: {body}");
    assert!(
        body.contains(r#""value":"power""#),
        "power pin must be echoed; body: {body}"
    );

    // Unpin via "lean" — the regression: this used to echo "power".
    let (status, body) = http(
        dash.port,
        "POST",
        "/api/settings",
        TOKEN,
        Some(r#"{"key":"tool_profile","value":"lean"}"#),
    )
    .expect("POST lean");
    assert_eq!(
        status, 200,
        "unpinning via lean should succeed; body: {body}"
    );
    assert!(
        body.contains(r#""value":"lean""#),
        "lean must round-trip to lean, not power; body: {body}"
    );
    assert!(
        !body.contains(r#""value":"power""#),
        "lean must not snap back to power; body: {body}"
    );

    // And it stays lean across a fresh read (no re-pin, no stale WARN).
    let (_status, body) =
        http(dash.port, "GET", "/api/settings", TOKEN, None).expect("GET after lean");
    assert!(
        body.contains(r#""value":"lean""#),
        "lean must persist; body: {body}"
    );
}

#[test]
fn settings_rejects_unknown_key() {
    let mut dash = start_dashboard();
    wait_ready(&mut dash);

    // A non-allow-listed key must never reach the config writer.
    let (status, body) = http(
        dash.port,
        "POST",
        "/api/settings",
        TOKEN,
        Some(r#"{"key":"proxy.anthropic_upstream","value":"http://evil"}"#),
    )
    .expect("POST unknown key");
    assert_eq!(status, 400, "unknown key must be rejected; body: {body}");
}
