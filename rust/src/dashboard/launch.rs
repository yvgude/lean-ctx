// SPDX-License-Identifier: Apache-2.0
//! Which local Cockpit server belongs to this data dir, and how a new
//! `lean-ctx cockpit` decides between reusing, replacing and starting one.
//!
//! The server records itself in the launch record
//! (`core::cockpit_launch_record`) right after it bound its port
//! and removes the record when it ends.
//!
//! A start first looks at the record, then at the port: a healthy instance
//! of this data dir is reused; a hung, token-rejecting or outdated
//! background instance of this data dir is replaced (stopped by its
//! recorded PID, never by name); a port held by another program or another
//! LeanCTX data dir is skipped for the next free one unless the port was
//! asked for explicitly. [`plan`] is that decision, free of I/O.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::{Duration, Instant};

pub(crate) use crate::core::cockpit_launch_record::{
    DETACHED_ENV, LaunchRecord, read_record, remove_record_of, write_record,
};

/// How many ports after the default a start tries when it is taken.
pub(crate) const PORT_SCAN: u16 = 20;
/// One health probe: connect, request and read.
pub(crate) const PROBE_TIMEOUT: Duration = Duration::from_millis(400);

/// What answers on a port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Health {
    /// A LeanCTX Cockpit server that accepted this data dir's token; its
    /// engine version.
    Healthy { version: String },
    /// A LeanCTX Cockpit server that rejected the token: another data dir,
    /// or a pinned token this data dir does not hold.
    Rejected,
    /// Something else answers, or nothing answers in time.
    Foreign,
    /// Nothing listens.
    Closed,
}

/// Classify the raw response to `GET /api/version`.
pub(crate) fn classify_response(response: &str) -> Health {
    let status_ok = response.starts_with("HTTP/1.1 200") || response.starts_with("HTTP/1.0 200");
    let ours = response.contains(r#""current":"#)
        && response.contains(r#""latest":"#)
        && response.contains(r#""update_available":"#);
    if status_ok && ours {
        let version = response
            .split(r#""current":""#)
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .unwrap_or_default()
            .to_string();
        return Health::Healthy { version };
    }
    if response.starts_with("HTTP/1.1 401") && response.contains(r#"{"error":"unauthorized"}"#) {
        return Health::Rejected;
    }
    Health::Foreign
}

/// The loopback address that reaches a server bound to `host:port`.
pub(crate) fn probe_address(host: &str, port: u16) -> SocketAddr {
    let ip = match host.trim_matches(|c| c == '[' || c == ']') {
        "::1" | "::" => std::net::IpAddr::V6(std::net::Ipv6Addr::LOCALHOST),
        other => other
            .parse::<std::net::IpAddr>()
            .ok()
            .filter(|ip| !ip.is_unspecified())
            .unwrap_or(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)),
    };
    SocketAddr::new(ip, port)
}

/// Probe `GET /api/version` on `host:port` with `token`.
pub(crate) fn probe(host: &str, port: u16, token: Option<&str>) -> Health {
    let address = probe_address(host, port);
    let mut stream = match TcpStream::connect_timeout(&address, PROBE_TIMEOUT) {
        Ok(stream) => stream,
        Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => {
            return Health::Closed;
        }
        Err(_) => return Health::Foreign,
    };
    let _ = stream.set_read_timeout(Some(PROBE_TIMEOUT));
    let _ = stream.set_write_timeout(Some(PROBE_TIMEOUT));
    let auth = token
        .filter(|t| !t.is_empty())
        .map(|t| format!("Authorization: Bearer {t}\r\n"))
        .unwrap_or_default();
    let request =
        format!("GET /api/version HTTP/1.1\r\nHost: localhost\r\n{auth}Connection: close\r\n\r\n");
    if stream.write_all(request.as_bytes()).is_err() {
        return Health::Foreign;
    }
    // Read until the peer closes, bounded so a rogue peer can't stream forever.
    let mut response = Vec::new();
    let mut buf = [0u8; 1024];
    while response.len() < 8 * 1024 {
        match stream.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => response.extend_from_slice(&buf[..n]),
        }
    }
    classify_response(&String::from_utf8_lossy(&response))
}

/// `probe`, retried for `patience` while it does not answer healthy: a
/// server that just started or is busy gets a moment before it counts as
/// hung.
pub(crate) fn probe_patiently(
    host: &str,
    port: u16,
    token: impl Fn() -> Option<String>,
    patience: Duration,
) -> Health {
    let deadline = Instant::now() + patience;
    loop {
        let health = probe(host, port, token().as_deref());
        if matches!(health, Health::Healthy { .. }) || Instant::now() >= deadline {
            return health;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Nothing listens on `port`, on IPv4 or IPv6 loopback — so the browser's
/// `localhost` cannot reach another program instead of the new server.
pub(crate) fn port_free(port: u16) -> bool {
    let v4 = probe_address("127.0.0.1", port);
    let v6 = probe_address("::1", port);
    let refused = |address: &SocketAddr| match TcpStream::connect_timeout(address, PROBE_TIMEOUT) {
        Ok(_) => false,
        Err(error) => !matches!(error.kind(), std::io::ErrorKind::TimedOut),
    };
    refused(&v4) && refused(&v6)
}

/// This data dir's live recorded instance, probed with a little patience.
/// A record whose process is gone is removed.
pub(crate) fn observe(record: Option<&LaunchRecord>) -> Option<RecordView> {
    let record = record?;
    if !record.process_alive() {
        remove_record_of(record.pid);
        return None;
    }
    let health = probe_patiently(
        &record.host,
        record.port,
        super::load_saved_token,
        Duration::from_secs(2),
    );
    Some(RecordView {
        pid: record.pid,
        port: record.port,
        detached: record.detached,
        health,
    })
}

/// [`plan`] for this data dir now, with the record it saw.
pub(crate) fn plan_now(requested: Option<u16>) -> (Plan, Option<LaunchRecord>) {
    let record = read_record();
    let view = observe(record.as_ref());
    let token = super::load_saved_token();
    let decided = plan(
        view.as_ref(),
        requested,
        super::DEFAULT_PORT,
        |port| probe("127.0.0.1", port, token.as_deref()),
        port_free,
    );
    (decided, record.filter(|_| view.is_some()))
}

/// Stop a recorded instance by its PID — SIGTERM, then a kill after a
/// grace period. Never by name; a PID whose process identity changed is
/// left alone. True when it is gone.
pub(crate) fn stop_instance(record: &LaunchRecord) -> bool {
    use crate::ipc::process::{force_kill, is_alive, terminate_gracefully};
    if !record.process_alive() {
        remove_record_of(record.pid);
        return true;
    }
    let _ = terminate_gracefully(record.pid);
    let gone = |within: Duration| {
        let deadline = Instant::now() + within;
        while Instant::now() < deadline {
            if !is_alive(record.pid) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        !is_alive(record.pid)
    };
    let stopped = gone(Duration::from_secs(5)) || {
        let _ = force_kill(record.pid);
        gone(Duration::from_secs(2))
    };
    if stopped {
        remove_record_of(record.pid);
    }
    stopped
}

/// What a start knows about the recorded instance of this data dir.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RecordView {
    pub(crate) pid: u32,
    pub(crate) port: u16,
    pub(crate) detached: bool,
    pub(crate) health: Health,
}

/// What a start does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Plan {
    /// Open the healthy instance on `port`; `other_version` when it runs a
    /// different engine version than this one (a foreground instance is
    /// never stopped for that).
    Reuse {
        port: u16,
        other_version: Option<String>,
    },
    /// Stop this data dir's background instance (`pid`), then start on
    /// `port`.
    Replace { pid: u32, port: u16 },
    /// Start on `port`; `skipped` names the taken port it moved past.
    Start { port: u16, skipped: Option<u16> },
    /// The explicitly requested `port` (or every scanned one) is taken.
    PortTaken { port: u16, by: Health },
}

/// Decide a start. `record` is this data dir's live recorded instance (a
/// dead one is `None`); `requested` an explicit `--port`/`LEAN_CTX_PORT`;
/// `port_health` probes a port; `free` says whether nothing listens there.
pub(crate) fn plan(
    record: Option<&RecordView>,
    requested: Option<u16>,
    default_port: u16,
    port_health: impl Fn(u16) -> Health,
    free: impl Fn(u16) -> bool,
) -> Plan {
    let ours = env!("CARGO_PKG_VERSION");
    if let Some(record) = record
        && requested.is_none_or(|port| port == record.port)
    {
        match &record.health {
            Health::Healthy { version } if version == ours || !record.detached => {
                return Plan::Reuse {
                    port: record.port,
                    other_version: (version != ours).then(|| version.clone()),
                };
            }
            // Outdated, hung or token-rejecting: ours to restart.
            _ if record.detached => {
                return Plan::Replace {
                    pid: record.pid,
                    port: record.port,
                };
            }
            // Someone's foreground instance that does not answer: leave it
            // and pick a port below.
            _ => {}
        }
    }
    let base = requested.unwrap_or(default_port);
    let skip_foreground = record
        .filter(|record| !record.detached && !matches!(record.health, Health::Healthy { .. }))
        .map(|record| record.port);
    let health = if skip_foreground == Some(base) {
        Health::Foreign
    } else {
        port_health(base)
    };
    match health {
        Health::Healthy { version } => Plan::Reuse {
            port: base,
            other_version: (version != ours).then_some(version),
        },
        Health::Closed if free(base) => Plan::Start {
            port: base,
            skipped: None,
        },
        by if requested.is_some() => Plan::PortTaken {
            port: base,
            by: if by == Health::Closed {
                Health::Foreign
            } else {
                by
            },
        },
        by => (1..=PORT_SCAN)
            .filter_map(|step| base.checked_add(step))
            .find(|port| Some(*port) != skip_foreground && free(*port))
            .map_or(
                Plan::PortTaken {
                    port: base,
                    by: by.clone(),
                },
                |port| Plan::Start {
                    port,
                    skipped: Some(base),
                },
            ),
    }
}

/// One line for a port another program or data dir holds.
pub(crate) fn port_taken_message(port: u16, by: &Health) -> String {
    let who = match by {
        Health::Rejected => "another LeanCTX Cockpit (another data dir or a pinned token)",
        _ => "another program",
    };
    format!("Port {port} is used by {who} — choose another one: lean-ctx cockpit --port <N>")
}

#[cfg(test)]
mod tests {
    use super::*;

    const OURS: &str = env!("CARGO_PKG_VERSION");

    fn healthy(version: &str) -> Health {
        Health::Healthy {
            version: version.to_string(),
        }
    }

    fn view(detached: bool, health: Health) -> RecordView {
        RecordView {
            pid: 42,
            port: 3333,
            detached,
            health,
        }
    }

    fn nothing_listens(_: u16) -> Health {
        Health::Closed
    }

    #[test]
    fn version_response_is_classified_without_trusting_lookalikes() {
        let ok = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n\
             {{\"current\":\"{OURS}\",\"latest\":\"{OURS}\",\"update_available\":false}}"
        );
        assert_eq!(classify_response(&ok), healthy(OURS));
        assert_eq!(
            classify_response(
                "HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Bearer\r\n\r\n{\"error\":\"unauthorized\"}"
            ),
            Health::Rejected
        );
        assert_eq!(
            classify_response("HTTP/1.1 200 OK\r\n\r\n<html>other app</html>"),
            Health::Foreign
        );
        assert_eq!(
            classify_response("HTTP/1.1 401 Unauthorized\r\n\r\n"),
            Health::Foreign
        );
        assert_eq!(classify_response(""), Health::Foreign);
    }

    #[test]
    fn a_healthy_instance_of_this_data_dir_is_reused() {
        let record = view(true, healthy(OURS));
        assert_eq!(
            plan(Some(&record), None, 3333, nothing_listens, |_| true),
            Plan::Reuse {
                port: 3333,
                other_version: None
            }
        );
        // Without a record, a port that accepts our token is ours as well.
        assert_eq!(
            plan(None, None, 3333, |_| healthy(OURS), |_| false),
            Plan::Reuse {
                port: 3333,
                other_version: None
            }
        );
    }

    #[test]
    fn an_outdated_or_hung_background_instance_is_replaced_but_a_foreground_one_never() {
        for health in [healthy("0.0.1"), Health::Rejected, Health::Foreign] {
            assert_eq!(
                plan(
                    Some(&view(true, health.clone())),
                    None,
                    3333,
                    nothing_listens,
                    |_| true
                ),
                Plan::Replace {
                    pid: 42,
                    port: 3333
                },
                "{health:?}"
            );
        }
        assert_eq!(
            plan(
                Some(&view(false, healthy("0.0.1"))),
                None,
                3333,
                nothing_listens,
                |_| true
            ),
            Plan::Reuse {
                port: 3333,
                other_version: Some("0.0.1".into())
            }
        );
        // A hung foreground instance keeps its port; the start moves on.
        assert_eq!(
            plan(
                Some(&view(false, Health::Foreign)),
                None,
                3333,
                nothing_listens,
                |p| p != 3333
            ),
            Plan::Start {
                port: 3334,
                skipped: Some(3333)
            }
        );
    }

    #[test]
    fn a_taken_default_port_moves_to_the_next_free_one() {
        for by in [Health::Foreign, Health::Rejected] {
            let by_ref = by.clone();
            assert_eq!(
                plan(
                    None,
                    None,
                    3333,
                    move |p| if p == 3333 {
                        by_ref.clone()
                    } else {
                        Health::Closed
                    },
                    |p| p > 3334
                ),
                Plan::Start {
                    port: 3335,
                    skipped: Some(3333)
                },
                "{by:?}"
            );
        }
        // IPv6-only listener: refused on 127.0.0.1 but not free.
        assert_eq!(
            plan(None, None, 3333, nothing_listens, |p| p != 3333),
            Plan::Start {
                port: 3334,
                skipped: Some(3333)
            }
        );
        assert_eq!(
            plan(None, None, 3333, |_| Health::Foreign, |_| false),
            Plan::PortTaken {
                port: 3333,
                by: Health::Foreign
            }
        );
    }

    #[test]
    fn an_explicit_port_is_never_swapped_silently() {
        assert_eq!(
            plan(None, Some(4000), 3333, |_| Health::Rejected, |_| true),
            Plan::PortTaken {
                port: 4000,
                by: Health::Rejected
            }
        );
        assert_eq!(
            plan(None, Some(4000), 3333, nothing_listens, |_| true),
            Plan::Start {
                port: 4000,
                skipped: None
            }
        );
        // A recorded instance on another port is left alone.
        assert_eq!(
            plan(
                Some(&view(true, healthy(OURS))),
                Some(4000),
                3333,
                nothing_listens,
                |_| true
            ),
            Plan::Start {
                port: 4000,
                skipped: None
            }
        );
    }

    #[test]
    fn probe_addresses_use_loopback_for_wildcards_and_keep_ipv6() {
        assert_eq!(probe_address("0.0.0.0", 1).to_string(), "127.0.0.1:1");
        assert_eq!(probe_address("localhost", 1).to_string(), "127.0.0.1:1");
        assert_eq!(probe_address("::1", 1).to_string(), "[::1]:1");
        assert_eq!(probe_address("[::]", 1).to_string(), "[::1]:1");
    }

    #[test]
    fn port_taken_names_the_fix() {
        let line = port_taken_message(3333, &Health::Rejected);
        assert!(line.contains("another LeanCTX Cockpit") && line.contains("--port"));
        assert!(port_taken_message(3333, &Health::Foreign).contains("another program"));
    }
}
