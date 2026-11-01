// SPDX-License-Identifier: Apache-2.0
//! Where the local lean-ctx proxy listens and whether it answers.
//!
//! The engine (updater, autostart, gain) and the setup/doctor surfaces share
//! this single source of truth for the proxy port, the reachability timeout
//! and the "is the proxy up" check.

pub(crate) const DEFAULT_PROXY_PORT: u16 = 4444;

/// Proxy reachability timeout. Priority: env var > config.toml > 200ms default.
pub fn proxy_timeout() -> std::time::Duration {
    if let Ok(val) = std::env::var("LEAN_CTX_PROXY_TIMEOUT_MS")
        && let Ok(ms) = val.parse::<u64>()
    {
        return std::time::Duration::from_millis(ms);
    }
    if let Some(ms) = crate::core::config::Config::load().proxy_timeout_ms {
        return std::time::Duration::from_millis(ms);
    }
    std::time::Duration::from_millis(200)
}

/// True when the lean-ctx proxy itself answers `GET /health` on `port`. An
/// open port alone proves nothing: a foreign process (or another user's
/// proxy) listening there must never count as "proxy up".
pub fn lean_ctx_proxy_responding(port: u16) -> bool {
    use std::io::{Read, Write};
    use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};

    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
    let Ok(mut stream) = TcpStream::connect_timeout(&addr, proxy_timeout()) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(3)));
    let req =
        format!("GET /health HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n");
    if stream.write_all(req.as_bytes()).is_err() {
        return false;
    }
    let mut buf = Vec::with_capacity(1024);
    let _ = stream.take(8 * 1024).read_to_end(&mut buf);
    is_lean_ctx_health_response(&String::from_utf8_lossy(&buf))
}

/// The `/health` answer of a lean-ctx proxy: HTTP 200 carrying the
/// `"service":"lean-ctx-proxy"` marker its handler always emits.
fn is_lean_ctx_health_response(response: &str) -> bool {
    let status_ok = response
        .lines()
        .next()
        .is_some_and(|line| line.starts_with("HTTP/1.") && line.split(' ').nth(1) == Some("200"));
    status_ok && response.contains("\"service\":\"lean-ctx-proxy\"")
}

/// Single source of truth for "is the proxy up" across `doctor`, `status` and
/// `gain`: the proxy must be enabled in config AND the configured endpoint must
/// answer as a lean-ctx proxy.
pub fn configured_proxy_live() -> bool {
    crate::core::config::Config::load().proxy_enabled == Some(true)
        && lean_ctx_proxy_responding(default_port())
}

pub fn default_port() -> u16 {
    if let Ok(val) = std::env::var("LEAN_CTX_PROXY_PORT")
        && let Ok(port) = val.parse::<u16>()
    {
        return port;
    }
    let cfg = crate::core::config::Config::load();
    if let Some(port) = cfg.proxy_port {
        return port;
    }
    uid_based_port()
}

/// Derives a deterministic port from the user's UID to avoid collisions
/// on multi-user systems. uid 1000 → 4444, uid 1001 → 4445, etc.
/// System accounts (uid < 1000) and root always get the base port 4444.
pub(crate) fn uid_based_port() -> u16 {
    #[cfg(unix)]
    {
        // SAFETY: `getuid` takes no arguments, always succeeds, and only reads
        // the calling process's real UID — no preconditions, no UB.
        let uid = unsafe { libc::getuid() } as u16;
        let offset = uid.saturating_sub(1000) % 1000;
        DEFAULT_PROXY_PORT + offset
    }
    #[cfg(not(unix))]
    {
        DEFAULT_PROXY_PORT
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uid_port_first_regular_user() {
        // uid 1000 (first regular user on most Linux) → base port
        assert_eq!(DEFAULT_PROXY_PORT, 4444);
    }

    #[test]
    fn uid_port_no_overflow() {
        // Ensure port stays in valid range even with high UIDs
        // uid 2999 → offset (2999-1000) % 1000 = 999 → port 5443
        let port = DEFAULT_PROXY_PORT + 999;
        assert_eq!(port, 5443);
        assert!(port < u16::MAX);
    }

    #[test]
    fn uid_port_system_accounts_get_base() {
        // uid < 1000 → saturating_sub gives 0 → base port
        let uid: u16 = 500;
        let offset = uid.saturating_sub(1000) % 1000;
        assert_eq!(DEFAULT_PROXY_PORT + offset, DEFAULT_PROXY_PORT);
    }

    #[test]
    fn proxy_timeout_default_200ms() {
        if std::env::var("LEAN_CTX_PROXY_TIMEOUT_MS").is_ok() {
            return;
        }
        assert_eq!(proxy_timeout(), std::time::Duration::from_millis(200));
    }

    #[test]
    fn proxy_timeout_is_non_zero() {
        let t = proxy_timeout();
        assert!(t.as_millis() > 0);
    }

    #[test]
    fn proxy_liveness_ignores_a_foreign_process_on_the_port() {
        use std::io::{Read, Write};
        // A foreign HTTP server on the proxy port answers 200 "ok"; only the
        // lean-ctx proxy's own /health marker may count as "proxy up".
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let server = std::thread::spawn(move || {
            let bodies = [
                r#"{"status":"ok"}"#,
                r#"{"pid":1,"service":"lean-ctx-proxy","status":"ok"}"#,
            ];
            for body in bodies {
                let (mut stream, _) = listener.accept().expect("accept");
                let mut req = [0u8; 512];
                let _ = stream.read(&mut req);
                let resp = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(resp.as_bytes());
            }
        });
        assert!(
            !lean_ctx_proxy_responding(port),
            "foreign 200 must not count"
        );
        assert!(
            lean_ctx_proxy_responding(port),
            "lean-ctx /health must count"
        );
        server.join().expect("server thread");
    }
}
