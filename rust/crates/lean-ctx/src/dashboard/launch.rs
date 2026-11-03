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

pub(crate) use lean_ctx::core::cockpit_launch_record::{
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
    /// Something else answers, or the connect failed in a way that cannot
    /// tell.
    Foreign,
    /// Nothing listens: refused, or (on loopback) no answer in time.
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
        Err(error) if no_listener(&address, error.kind()) => return Health::Closed,
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

/// A failed connect to `address` that means "nothing listens there".
///
/// A refusal always does. On loopback a timeout does too (#2174): Windows
/// answers a connect to a closed loopback port only after retrying the SYN
/// (about 1–2 s), far beyond [`PROBE_TIMEOUT`], while a real listener is
/// accepted by the kernel at once. Read as "something else holds it", every
/// free port on Windows looked taken. A timeout to a non-loopback address
/// stays inconclusive (a filtering firewall looks the same).
fn no_listener(address: &SocketAddr, kind: std::io::ErrorKind) -> bool {
    match kind {
        std::io::ErrorKind::ConnectionRefused => true,
        std::io::ErrorKind::TimedOut => address.ip().is_loopback(),
        _ => false,
    }
}

/// What a connect to one loopback address found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Answer {
    /// A listener accepted the connection.
    Listener,
    /// Refused, or no answer in time on loopback: nothing listens.
    Nothing,
    /// Any other failure: cannot tell.
    Unknown,
}

fn connect_answer(address: &SocketAddr) -> Answer {
    match TcpStream::connect_timeout(address, PROBE_TIMEOUT) {
        Ok(_) => Answer::Listener,
        Err(error) if no_listener(address, error.kind()) => Answer::Nothing,
        Err(_) => Answer::Unknown,
    }
}

/// `port` is free for a new Cockpit server on loopback: this process can
/// bind it on 127.0.0.1 (and on ::1 where IPv6 exists), and nothing answers
/// a connect on either — so the browser's `localhost` cannot reach another
/// program instead of the new server.
///
/// The bind is what decides whether the port is usable at all; it is also
/// what `lean-ctx doctor` checks, so both now agree. It catches what a connect
/// cannot: a port Windows reserves (an excluded port range answers nothing
/// but refuses the bind). The connect still matters: on macOS and Windows a
/// bind to 127.0.0.1 can succeed beside another program's wildcard
/// (`0.0.0.0`) listener, which then answers `localhost`.
pub(crate) fn port_free(port: u16) -> bool {
    port_free_with(
        port,
        |address| std::net::TcpListener::bind(address).map(drop),
        connect_answer,
    )
}

/// [`port_free`] with its I/O injected: `bind` tries to bind an address,
/// `answer` connects to one.
pub(crate) fn port_free_with(
    port: u16,
    bind: impl Fn(SocketAddr) -> std::io::Result<()>,
    answer: impl Fn(&SocketAddr) -> Answer,
) -> bool {
    let v4 = probe_address("127.0.0.1", port);
    let v6 = probe_address("::1", port);
    if bind(v4).is_err() {
        return false;
    }
    // A machine without IPv6 loopback cannot bind ::1 at all; that does not
    // make the port taken. Any other refusal (in use, access denied) does.
    let v6_usable = match bind(v6) {
        Ok(()) => true,
        Err(error) if ipv6_absent(&error) => false,
        Err(_) => return false,
    };
    answer(&v4) == Answer::Nothing && (!v6_usable || answer(&v6) == Answer::Nothing)
}

/// The bind error a host without an IPv6 loopback reports for `::1`.
fn ipv6_absent(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::AddrNotAvailable | std::io::ErrorKind::Unsupported
    ) || (AF_NOT_SUPPORTED != 0 && error.raw_os_error() == Some(AF_NOT_SUPPORTED))
}

/// `EAFNOSUPPORT` / `WSAEAFNOSUPPORT`: the address family is unavailable.
#[cfg(unix)]
const AF_NOT_SUPPORTED: i32 = libc::EAFNOSUPPORT;
#[cfg(windows)]
const AF_NOT_SUPPORTED: i32 = 10047;
#[cfg(not(any(unix, windows)))]
const AF_NOT_SUPPORTED: i32 = 0;

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
/// grace period. Never by name. The PID is signalled only while it is still
/// this user's lean-ctx process the record names (its recorded identity, or
/// the `cockpit --foreground` command line where the platform records no
/// identity), and every signal is bound to that process (#2176 review).
/// True when it is gone.
pub(crate) fn stop_instance(record: &LaunchRecord) -> bool {
    use lean_ctx::ipc::process::{self, RecordedPid, ServiceKind, ServiceSweep, StopSignal};
    if !record.process_alive() {
        remove_record_of(record.pid);
        return true;
    }
    let target = match record.identity() {
        Some(identity) => process::recorded_process(record.pid, identity, ServiceKind::Cockpit),
        None => process::find_service_processes(
            &[RecordedPid::bare(record.pid)],
            ServiceSweep::Only(ServiceKind::Cockpit),
        )
        .into_iter()
        .next(),
    };
    let Some(target) = target else {
        // Alive, but not provably the recorded Cockpit: never signalled.
        return false;
    };
    target.signal(StopSignal::Terminate);
    let gone = |within: Duration| {
        let deadline = Instant::now() + within;
        while Instant::now() < deadline {
            if !target.is_running() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        !target.is_running()
    };
    let stopped = gone(Duration::from_secs(5)) || {
        target.signal(StopSignal::Kill);
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
        // `Closed` here: nothing answers on 127.0.0.1, yet the port is not
        // free (a listener on ::1 only, or a bind the system refuses).
        by if requested.is_some() => Plan::PortTaken { port: base, by },
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

/// One line for a port another program or data dir holds, or the system
/// does not let this process bind.
pub(crate) fn port_taken_message(port: u16, by: &Health) -> String {
    let who = match by {
        Health::Rejected => {
            "is used by another LeanCTX Cockpit (another data dir or a pinned token)"
        }
        // Nothing answers on 127.0.0.1, but the bind was refused.
        Health::Closed => {
            "cannot be used (held on IPv6 by another program, or reserved by the system)"
        }
        _ => "is used by another program",
    };
    format!("Port {port} {who} — choose another one: lean-ctx cockpit --port <N>")
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
        let refused = port_taken_message(3333, &Health::Closed);
        assert!(refused.contains("cannot be used") && refused.contains("--port"));
        assert!(!refused.contains("is used by another program"));
    }

    fn bind_ok(_: SocketAddr) -> std::io::Result<()> {
        Ok(())
    }

    fn bind_err(kind: std::io::ErrorKind) -> impl Fn(SocketAddr) -> std::io::Result<()> {
        move |_| Err(std::io::Error::from(kind))
    }

    /// #2174: on Windows a connect to a closed loopback port is not refused
    /// within the probe timeout, it times out. That is "nothing listens".
    #[test]
    fn a_loopback_timeout_means_nothing_listens() {
        use std::io::ErrorKind::{ConnectionRefused, PermissionDenied, TimedOut};
        let v4 = probe_address("127.0.0.1", 3333);
        let v6 = probe_address("::1", 3333);
        assert!(no_listener(&v4, TimedOut));
        assert!(no_listener(&v6, TimedOut));
        assert!(no_listener(&v4, ConnectionRefused));
        assert!(!no_listener(&v4, PermissionDenied));
        // Off loopback a timeout can be a filtering firewall: inconclusive.
        let remote = probe_address("192.0.2.10", 3333);
        assert!(!no_listener(&remote, TimedOut));
        assert!(no_listener(&remote, ConnectionRefused));
    }

    #[test]
    fn a_bindable_port_nobody_answers_is_free() {
        assert!(port_free_with(3333, bind_ok, |_| Answer::Nothing));
        // The Windows case end to end: every port used to read as taken.
        let windows_free = |port| port_free_with(port, bind_ok, |_| Answer::Nothing);
        assert!((3333..=3353).all(windows_free));
    }

    #[test]
    fn a_port_the_system_refuses_to_bind_is_not_free() {
        use std::io::ErrorKind::{AddrInUse, PermissionDenied};
        // Windows excluded port ranges refuse the bind with access denied.
        assert!(!port_free_with(3333, bind_err(PermissionDenied), |_| {
            Answer::Nothing
        }));
        assert!(!port_free_with(3333, bind_err(AddrInUse), |_| {
            Answer::Nothing
        }));
        // Held on ::1 only.
        let v6_in_use = |address: SocketAddr| {
            if address.is_ipv6() {
                Err(std::io::Error::from(AddrInUse))
            } else {
                Ok(())
            }
        };
        assert!(!port_free_with(3333, v6_in_use, |_| Answer::Nothing));
    }

    #[test]
    fn a_host_without_ipv6_still_has_free_ports() {
        let no_v6 = |kind: std::io::ErrorKind| {
            move |address: SocketAddr| {
                if address.is_ipv6() {
                    Err(std::io::Error::from(kind))
                } else {
                    Ok(())
                }
            }
        };
        // ::1 is never asked when IPv6 is absent.
        let v4_only = |address: &SocketAddr| {
            assert!(address.is_ipv4(), "probed {address} without IPv6");
            Answer::Nothing
        };
        assert!(port_free_with(
            3333,
            no_v6(std::io::ErrorKind::AddrNotAvailable),
            v4_only
        ));
        assert!(port_free_with(
            3333,
            no_v6(std::io::ErrorKind::Unsupported),
            v4_only
        ));
        if AF_NOT_SUPPORTED != 0 {
            let afno = |address: SocketAddr| {
                if address.is_ipv6() {
                    Err(std::io::Error::from_raw_os_error(AF_NOT_SUPPORTED))
                } else {
                    Ok(())
                }
            };
            assert!(port_free_with(3333, afno, v4_only));
        }
        // An unrelated uncategorized failure is not "no IPv6".
        let other = |address: SocketAddr| {
            if address.is_ipv6() {
                Err(std::io::Error::other("boom"))
            } else {
                Ok(())
            }
        };
        assert!(!port_free_with(3333, other, |_| Answer::Nothing));
    }

    #[test]
    fn a_port_something_answers_on_is_not_free() {
        // A wildcard listener of another program: our 127.0.0.1 bind can
        // succeed beside it (macOS, Windows), but it answers `localhost`.
        assert!(!port_free_with(3333, bind_ok, |_| Answer::Listener));
        let v6_listener = |address: &SocketAddr| {
            if address.is_ipv6() {
                Answer::Listener
            } else {
                Answer::Nothing
            }
        };
        assert!(!port_free_with(3333, bind_ok, v6_listener));
        assert!(!port_free_with(3333, bind_ok, |_| Answer::Unknown));
    }

    #[test]
    fn an_explicit_port_the_system_refuses_says_so() {
        assert_eq!(
            plan(None, Some(4339), 3333, nothing_listens, |_| false),
            Plan::PortTaken {
                port: 4339,
                by: Health::Closed
            }
        );
    }

    /// Real sockets: a listening port is taken, and the probe sees it.
    #[test]
    fn a_real_listener_is_detected() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
        let port = listener.local_addr().expect("addr").port();
        assert!(!port_free(port));
        assert_eq!(
            connect_answer(&probe_address("127.0.0.1", port)),
            Answer::Listener
        );
        drop(listener);
        // macOS can answer the first connect after the close with neither a
        // refusal nor a listener; the port settles to closed within moments.
        let closed = (0..40).any(|_| {
            let health = probe("127.0.0.1", port, None);
            if health != Health::Closed {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            health == Health::Closed
        });
        assert!(closed, "a dropped listener's port never probed as closed");
    }
}
