// SPDX-License-Identifier: Apache-2.0
//! `lean-ctx cockpit` for a person on a terminal: reuse this data dir's
//! running Cockpit server or start one in the background, wait until it is
//! healthy (bounded), let it start the Cockpit runtime, open the browser at
//! the requested view and return the prompt. `lean-ctx cockpit stop` ends
//! the background server by its recorded PID.
//!
//! The bearer token travels only to the browser (in the URL handed to the
//! opener) — it is never printed, and a pinned `--auth-token` reaches the
//! server child through its environment, not its argv.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use crate::dashboard::cockpit::CockpitUnavailable;
use crate::dashboard::{self, launch};

/// How long a fresh server may take until it answers healthy.
const START_TIMEOUT: Duration = Duration::from_secs(20);
/// How long the first page may take: it starts the Cockpit runtime, whose
/// own handshake is bounded at 10 s.
const WARM_TIMEOUT: Duration = Duration::from_secs(15);

/// What opening the Cockpit came to.
#[derive(Debug)]
pub(crate) enum Outcome {
    /// The browser was sent to the Cockpit served on `port`.
    Opened { port: u16, base_path: String },
    /// The Cockpit runtime cannot be served; nothing was opened.
    Unavailable(CockpitUnavailable),
    /// The server could not be reused or started; one line why.
    Failed(String),
}

/// A person on a terminal: stdout and stderr are terminals and no CI runs.
pub(crate) fn interactive() -> bool {
    use std::io::IsTerminal as _;
    std::io::stdout().is_terminal()
        && std::io::stderr().is_terminal()
        && !crate::core::telemetry_consent::ci_detected(crate::core::host_env::var)
}

fn flag_value<'a>(rest: &'a [String], names: &[&str]) -> Option<&'a str> {
    rest.iter().find_map(|arg| {
        let (flag, value) = arg.split_once('=')?;
        names.contains(&flag).then_some(value)
    })
}

/// The server child's arguments: always foreground, never opening a
/// browser, on the planned port; the user's host, subpath, auth mode and
/// project carried over. The token is not among them (a token file's path
/// is: the child reads the file itself).
pub(crate) fn child_args(rest: &[String], port: u16) -> Vec<String> {
    let mut args = vec![
        "cockpit".to_string(),
        "--foreground".to_string(),
        "--no-open".to_string(),
        format!("--port={port}"),
    ];
    let mut iter = rest.iter();
    while let Some(arg) = iter.next() {
        let flag = arg.split_once('=').map_or(arg.as_str(), |(flag, _)| flag);
        match flag {
            "--host" | "-H" | "--base-path" | "--prefix" | "--auth" if arg.contains('=') => {
                args.push(arg.clone());
            }
            "--no-auth" => args.push(arg.clone()),
            "--project" | "--auth-token-file" if arg.contains('=') => args.push(arg.clone()),
            "--project" | "--auth-token-file" => {
                if let Some(value) = iter.next() {
                    args.push(format!("{flag}={value}"));
                }
            }
            _ => {}
        }
    }
    args
}

/// The normalized subpath a new server will serve under.
fn base_path_of(rest: &[String]) -> String {
    flag_value(rest, &["--base-path", "--prefix"])
        .map(str::to_string)
        .or_else(|| std::env::var("LEAN_CTX_DASHBOARD_BASE_PATH").ok())
        .map(|b| dashboard::base_path::normalize(&b))
        .unwrap_or_default()
}

/// Open the Cockpit at `view` (a hash route; `None` = start page).
pub(crate) fn open(view: Option<&str>, rest: &[String]) -> Outcome {
    if let Err(reason) = crate::core::intelligence_runtime::cockpit::available_or_installed() {
        return Outcome::Unavailable(reason);
    }
    crate::core::layout_pin::heal();
    let requested = super::port_flag(rest).or_else(|| {
        std::env::var("LEAN_CTX_PORT")
            .ok()
            .and_then(|p| p.trim().parse().ok())
    });
    let (plan, record) = launch::plan_now(requested);
    let (port, base_path) = match plan {
        launch::Plan::Reuse {
            port,
            other_version,
        } => {
            if let Some(version) = other_version {
                eprintln!(
                    "\x1b[33m⚠\x1b[0m The running Cockpit server is version {version} (this is {}); \
                     restart it to update: lean-ctx cockpit stop, then lean-ctx cockpit",
                    env!("CARGO_PKG_VERSION")
                );
            }
            let base = record
                .filter(|r| r.port == port)
                .map_or_else(|| base_path_of(rest), |r| r.base_path);
            (port, base)
        }
        launch::Plan::Replace { port, .. } => {
            if let Some(record) = record
                && !launch::stop_instance(&record)
            {
                return Outcome::Failed(format!(
                    "The previous Cockpit server (PID {}) did not stop; stop it, then retry.",
                    record.pid
                ));
            }
            if let Err(message) = start_server(rest, port) {
                return Outcome::Failed(message);
            }
            (port, base_path_of(rest))
        }
        launch::Plan::Start { port, skipped } => {
            if let Some(skipped) = skipped {
                eprintln!(
                    "\x1b[2mPort {skipped} is taken (another program or LeanCTX data dir) — the Cockpit uses {port}.\x1b[0m"
                );
            }
            if let Err(message) = start_server(rest, port) {
                return Outcome::Failed(message);
            }
            (port, base_path_of(rest))
        }
        launch::Plan::PortTaken { port, by } => {
            return Outcome::Failed(launch::port_taken_message(port, &by));
        }
    };
    if let Some(reason) = warm(port, &base_path) {
        return Outcome::Unavailable(reason);
    }
    let token = dashboard::load_saved_token();
    let url = dashboard::launch_url(port, &base_path, token.as_deref(), view);
    dashboard::open_dashboard_url(&url, dashboard::resolve_open_mode(None));
    Outcome::Opened { port, base_path }
}

/// Start the server in the background on `port` and wait until it answers
/// healthy. `Err` is the one line to show.
fn start_server(rest: &[String], port: u16) -> Result<(), String> {
    let exe = std::env::current_exe()
        .map_err(|e| format!("Cannot locate the lean-ctx binary to start the Cockpit: {e}"))?;
    let mut command = std::process::Command::new(exe);
    command
        .args(child_args(rest, port))
        .env(launch::DETACHED_ENV, "1")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    if let Some(token) = flag_value(rest, &["--auth-token", "--token"]) {
        command.env("LEAN_CTX_HTTP_TOKEN", token);
    }
    // Its own process group: closing this terminal (or the shell's job
    // control hanging up its jobs) does not take the server down.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        command.process_group(0);
    }
    let mut child = crate::ipc::process::spawn_detached(&mut command)
        .map_err(|e| format!("Could not start the Cockpit server: {e}"))?;
    let healthy = || {
        matches!(
            launch::probe("127.0.0.1", port, dashboard::load_saved_token().as_deref()),
            launch::Health::Healthy { .. }
        )
    };
    let deadline = Instant::now() + START_TIMEOUT;
    loop {
        if healthy() {
            return Ok(());
        }
        if let Ok(Some(status)) = child.try_wait() {
            // It may have found an instance that started meanwhile.
            return if healthy() {
                Ok(())
            } else {
                Err(format!(
                    "The Cockpit server ended at start ({status}). See why: lean-ctx cockpit --foreground"
                ))
            };
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "The Cockpit server (PID {}) did not answer within {} s. See why: lean-ctx cockpit stop, then lean-ctx cockpit --foreground",
                child.id(),
                START_TIMEOUT.as_secs()
            ));
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

/// The reason code in the server's Cockpit-unavailable page, if `page` is one.
pub(crate) fn unavailable_reason(page: &str) -> Option<CockpitUnavailable> {
    let code = page.split("data-reason=\"").nth(1)?.split('"').next()?;
    [
        CockpitUnavailable::NotInstalled,
        CockpitUnavailable::VerificationFailed,
        CockpitUnavailable::StartFailed,
        CockpitUnavailable::Disabled,
        CockpitUnavailable::SourceBuild,
    ]
    .into_iter()
    .find(|reason| reason.as_str() == code)
}

/// Load the start page once, so the server starts the Cockpit runtime
/// before the browser arrives; the reason when it shows the unavailable
/// page instead. A slow or failed load is left to the browser.
fn warm(port: u16, base_path: &str) -> Option<CockpitUnavailable> {
    let address = launch::probe_address("127.0.0.1", port);
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(2)).ok()?;
    let _ = stream.set_read_timeout(Some(WARM_TIMEOUT));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
    let request = format!(
        "GET {base_path}/ HTTP/1.1\r\nHost: localhost:{port}\r\nAccept: text/html\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).ok()?;
    let mut page = Vec::new();
    let mut buf = [0u8; 8192];
    while page.len() < 64 * 1024 {
        match stream.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => page.extend_from_slice(&buf[..n]),
        }
    }
    unavailable_reason(&String::from_utf8_lossy(&page))
}

/// The one line after a successful open; never the token.
pub(crate) fn opened_line(view: Option<&str>, port: u16, base_path: &str) -> String {
    match view.filter(|v| !v.is_empty()) {
        Some(view) => format!("Opened the Cockpit \u{2192} #{view}"),
        None => format!("Opened the Cockpit \u{2192} http://localhost:{port}{base_path}/"),
    }
}

/// The one line when the Cockpit runtime cannot be served.
pub(crate) fn unavailable_line(reason: CockpitUnavailable) -> &'static str {
    match reason {
        CockpitUnavailable::NotInstalled | CockpitUnavailable::SourceBuild => {
            "Cockpit not installed \u{2014} lean-ctx cockpit install"
        }
        other => other.message(),
    }
}

/// `lean-ctx cockpit stop`: end this data dir's background server by its
/// recorded PID. The exit code.
pub(crate) fn stop() -> i32 {
    let Some(record) = launch::read_record() else {
        println!("No Cockpit server is recorded for this data dir.");
        return 0;
    };
    if !record.process_alive() {
        launch::remove_record_of(record.pid);
        println!(
            "The recorded Cockpit server (PID {}) is no longer running.",
            record.pid
        );
        return 0;
    }
    if !record.detached {
        eprintln!(
            "The Cockpit server on port {} runs in a terminal (PID {}) — stop it there with Ctrl+C.",
            record.port, record.pid
        );
        return 1;
    }
    if launch::stop_instance(&record) {
        println!(
            "Stopped the Cockpit server (PID {}, port {}).",
            record.pid, record.port
        );
        0
    } else {
        eprintln!(
            "Could not stop the Cockpit server (PID {}); it is still running.",
            record.pid
        );
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn the_child_runs_in_the_foreground_on_the_planned_port_without_the_token() {
        let child = child_args(
            &args(&[
                "--host=127.0.0.1",
                "--base-path=/c",
                "--auth-token=secret-value",
                "--token=other",
                "--no-auth",
                "--project",
                "/work/repo",
                "--port",
                "9999",
                "--vscode",
                "--auth-token-file",
                "/run/token",
            ]),
            4100,
        );
        assert_eq!(
            child,
            args(&[
                "cockpit",
                "--foreground",
                "--no-open",
                "--port=4100",
                "--host=127.0.0.1",
                "--base-path=/c",
                "--no-auth",
                "--project=/work/repo",
                "--auth-token-file=/run/token",
            ])
        );
        assert!(!child.iter().any(|a| a.contains("secret")));
    }

    #[test]
    fn the_unavailable_page_reason_is_read_back() {
        for reason in [
            CockpitUnavailable::NotInstalled,
            CockpitUnavailable::StartFailed,
            CockpitUnavailable::SourceBuild,
        ] {
            let page = crate::dashboard::cockpit::unavailable_page(reason);
            assert_eq!(unavailable_reason(&page), Some(reason));
        }
        assert_eq!(unavailable_reason("<html>the Cockpit app</html>"), None);
    }

    #[test]
    fn lines_name_the_route_or_the_fix_and_never_a_token() {
        assert_eq!(
            opened_line(Some("savings"), 3333, ""),
            "Opened the Cockpit \u{2192} #savings"
        );
        assert_eq!(
            opened_line(None, 3334, "/c"),
            "Opened the Cockpit \u{2192} http://localhost:3334/c/"
        );
        assert_eq!(
            unavailable_line(CockpitUnavailable::SourceBuild),
            "Cockpit not installed \u{2014} lean-ctx cockpit install"
        );
        assert!(unavailable_line(CockpitUnavailable::Disabled).contains("cockpit install"));
    }
}
