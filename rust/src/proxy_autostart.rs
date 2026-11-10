#[cfg(any(target_os = "macos", target_os = "linux"))]
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
#[cfg(any(target_os = "macos", target_os = "linux"))]
use std::path::PathBuf;
#[cfg(any(target_os = "macos", target_os = "linux"))]
use std::time::{Duration, Instant};

#[cfg(target_os = "macos")]
const PLIST_LABEL: &str = "com.leanctx.proxy";
#[cfg(target_os = "linux")]
const SYSTEMD_SERVICE: &str = "lean-ctx-proxy";

/// PID the proxy on this instance's configured port reports in `/health`,
/// on every platform. Used by the lifecycle commands to stop the proxy this
/// instance owns — never by process name (#2176). The caller still gates the
/// PID by command line and process identity before signalling it.
pub(crate) fn recorded_proxy_pid() -> Option<u32> {
    let port = crate::core::proxy_endpoint::default_port();
    // The account's default port belongs to the default install. Another
    // instance (directory override, sandbox HOME) owns a proxy only on a port
    // it configured itself; otherwise `stop` from a scratch data dir stopped
    // the user's real proxy (5 Nov 2026).
    if !crate::daemon_autostart::controls_user_services()
        && port == crate::core::proxy_endpoint::uid_based_port()
    {
        return None;
    }
    health_proxy_pid(port)
}

/// PID a lean-ctx proxy on `port` reports in `/health`. Only an answer that
/// identifies itself as the lean-ctx proxy counts (#2176 review #5); the PID
/// is still only a candidate — `ipc::process` gates it by command line,
/// executable and user (the caller's uid / token SID) before any signal.
#[must_use]
pub fn health_proxy_pid(port: u16) -> Option<u32> {
    let response = ureq::get(&format!("http://127.0.0.1:{port}/health"))
        .config()
        .timeout_global(Some(std::time::Duration::from_millis(500)))
        .build()
        .call()
        .ok()?;
    let body = response.into_body().read_to_string().ok()?;
    proxy_pid_from_health(&body)
}

fn proxy_pid_from_health(body: &str) -> Option<u32> {
    let health: serde_json::Value = serde_json::from_str(body).ok()?;
    if health.get("status").and_then(serde_json::Value::as_str) != Some("ok")
        || !health_identifies_lean_ctx_proxy(body)
    {
        return None;
    }
    let pid = u32::try_from(health.get("pid")?.as_u64()?).ok()?;
    (pid > 0).then_some(pid)
}

fn health_identifies_lean_ctx_proxy(body: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|health| health.get("service")?.as_str().map(str::to_owned))
        .is_some_and(|service| service == "lean-ctx-proxy")
}

/// The lean-ctx proxy of this user listening on `port`, gated like every
/// other lifecycle target: `/health` names it, and the PID passes the
/// command-line, executable, user and identity checks.
#[must_use]
pub fn proxy_on_port(port: u16) -> Option<crate::ipc::process::ServiceProcess> {
    use crate::ipc::process::{RecordedPid, ServiceKind, ServiceSweep, find_service_processes};
    let pid = health_proxy_pid(port)?;
    find_service_processes(
        &[RecordedPid::bare(pid)],
        ServiceSweep::Only(ServiceKind::Proxy),
    )
    .into_iter()
    .next()
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn port_is_open(port: u16) -> bool {
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
    TcpStream::connect_timeout(&addr, Duration::from_millis(150)).is_ok()
}

/// Stop a proxy already owning `port` before a managed service is started.
///
/// Returns `false` when the listener cannot be positively identified as a
/// lean-ctx proxy or when it does not release the port. Managed startup must
/// fail closed in either case; otherwise launchd/systemd would enter a restart
/// loop on `EADDRINUSE`.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn release_proxy_port(port: u16, quiet: bool) -> bool {
    if !port_is_open(port) {
        return true;
    }

    // The account's default port belongs to the default install: another
    // instance never signals the proxy listening there, whatever `/health`
    // reports (the same rule as `recorded_proxy_pid`, 6 Nov 2026).
    if !crate::daemon_autostart::controls_user_services()
        && port == crate::core::proxy_endpoint::uid_based_port()
    {
        if !quiet {
            eprintln!(
                "  Refusing managed proxy startup: port {port} belongs to the default install."
            );
        }
        return false;
    }

    // #2176 review: the `/health` PID is signalled only while it is this
    // user's lean-ctx proxy (command line, executable, identity); never this
    // process or its ancestors. Anything else fails closed.
    let Some(proxy) = proxy_on_port(port) else {
        if !quiet {
            eprintln!(
                "  Refusing managed proxy startup: port {port} is occupied by an unidentified service."
            );
        }
        return false;
    };
    let pid = proxy.pid;

    proxy.signal(crate::ipc::process::StopSignal::Terminate);
    let graceful_deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < graceful_deadline {
        if !proxy.is_running() && !port_is_open(port) {
            if !quiet {
                eprintln!(
                    "  Handed port {port} from standalone proxy PID {pid} to managed service."
                );
            }
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    if proxy.is_running() {
        proxy.signal(crate::ipc::process::StopSignal::Kill);
    }
    let force_deadline = Instant::now() + Duration::from_secs(1);
    while Instant::now() < force_deadline {
        if !port_is_open(port) {
            if !quiet {
                eprintln!(
                    "  Handed port {port} from standalone proxy PID {pid} to managed service."
                );
            }
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    if !quiet {
        eprintln!("  Refusing managed proxy startup: port {port} was not released by PID {pid}.");
    }
    false
}

pub fn install(port: u16, quiet: bool) -> bool {
    let binary = find_binary();
    install_with_binary(&binary, port, quiet)
}

/// Install (or rewrite) and start the proxy autostart service. Refused for
/// another instance (a directory override or a sandbox HOME): the service
/// manager addresses the unit by name, so the stop, port hand-off, unit
/// rewrite and `enable --now` would act on the default install's proxy
/// (6 Nov 2026, see [`crate::daemon_autostart::controls_user_services`]).
pub fn install_with_binary(binary: &str, port: u16, quiet: bool) -> bool {
    if !crate::daemon_autostart::controls_user_services() {
        if !quiet {
            eprintln!("  {}", crate::daemon_autostart::not_this_instance_message());
        }
        return false;
    }
    if binary.is_empty() {
        if !quiet {
            tracing::error!("Cannot find lean-ctx binary for autostart");
        }
        return false;
    }

    #[cfg(target_os = "macos")]
    {
        install_launchagent(binary, port, quiet)
    }

    #[cfg(target_os = "linux")]
    {
        install_systemd(binary, port, quiet)
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (binary, quiet);
        println!("  Autostart not supported on this platform");
        println!("  Run manually: lean-ctx proxy start --port={port}");
        false
    }
}

/// Stop the proxy's autostart service. A no-op for another instance (a
/// directory override or a sandbox HOME): the service manager addresses the
/// unit by name, and it belongs to the default install (5 Nov 2026, see
/// [`crate::daemon_autostart::controls_user_services`]).
pub fn stop() {
    if !crate::daemon_autostart::controls_user_services() {
        return;
    }
    #[cfg(target_os = "macos")]
    {
        let plist_path = launchagent_path();
        if plist_path.exists() {
            crate::core::launchd::bootout(PLIST_LABEL, &plist_path);
        }
    }

    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("systemctl")
            .args(["--user", "stop", SYSTEMD_SERVICE])
            .output();
    }
}

pub fn start() -> bool {
    start_on_port(crate::core::proxy_endpoint::default_port())
}

/// Start the installed manager after atomically taking ownership of `port`.
/// This also handles the case where an IDE/dashboard spawned a standalone
/// proxy while the manager was temporarily unloaded.
pub fn start_on_port(port: u16) -> bool {
    if !crate::daemon_autostart::controls_user_services() {
        return false;
    }
    #[cfg(target_os = "macos")]
    {
        let plist_path = launchagent_path();
        if plist_path.exists() {
            crate::core::launchd::bootout(PLIST_LABEL, &plist_path);
            if !release_proxy_port(port, false) {
                return false;
            }
            return crate::core::launchd::bootstrap(PLIST_LABEL, &plist_path);
        }
        false
    }

    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("systemctl")
            .args(["--user", "stop", SYSTEMD_SERVICE])
            .output();
        if release_proxy_port(port, false) {
            std::process::Command::new("systemctl")
                .args(["--user", "start", SYSTEMD_SERVICE])
                .status()
                .is_ok_and(|status| status.success())
        } else {
            false
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = port;
        false
    }
}

/// Stop, disable and remove the proxy autostart service. A no-op for another
/// instance, like [`install_with_binary`].
pub fn uninstall(quiet: bool) {
    if !crate::daemon_autostart::controls_user_services() {
        if !quiet {
            println!("  {}", crate::daemon_autostart::not_this_instance_message());
        }
        return;
    }
    #[cfg(target_os = "macos")]
    uninstall_launchagent(quiet);

    #[cfg(target_os = "linux")]
    uninstall_systemd(quiet);
}

/// Whether this platform has a proxy-autostart backend (LaunchAgent on macOS,
/// systemd user service on Linux). Windows and other targets have none, so a
/// missing autostart there must not be treated as a failure by `doctor` (#416).
pub fn is_supported() -> bool {
    cfg!(any(target_os = "macos", target_os = "linux"))
}

/// Returns true if the proxy autostart is installed (plist/systemd service file exists).
/// Always false for another instance: the service is not its own.
pub fn is_installed() -> bool {
    if !crate::daemon_autostart::controls_user_services() {
        return false;
    }
    #[cfg(target_os = "macos")]
    {
        launchagent_path().exists()
    }
    #[cfg(target_os = "linux")]
    {
        systemd_path().exists()
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        false
    }
}

/// Returns true when the installed manager is actively responsible for the proxy.
/// Callers must not spawn a second foreground proxy while this is true.
/// Always false for another instance (no `systemctl`/`launchctl` query).
pub fn is_loaded() -> bool {
    if !crate::daemon_autostart::controls_user_services() {
        return false;
    }
    #[cfg(target_os = "macos")]
    {
        is_installed() && crate::core::launchd::is_loaded(PLIST_LABEL)
    }
    #[cfg(target_os = "linux")]
    {
        is_installed()
            && std::process::Command::new("systemctl")
                .args(["--user", "is-active", "--quiet", SYSTEMD_SERVICE])
                .status()
                .is_ok_and(|status| status.success())
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        false
    }
}

pub fn status() {
    if !crate::daemon_autostart::controls_user_services() {
        println!("  {}", crate::daemon_autostart::not_this_instance_message());
        return;
    }
    #[cfg(target_os = "macos")]
    {
        let plist_path = launchagent_path();
        if plist_path.exists() {
            println!("  LaunchAgent: installed at {}", plist_path.display());
            if crate::core::launchd::is_loaded(PLIST_LABEL) {
                println!("  Status: loaded");
            } else {
                println!("  Status: not loaded (run: lean-ctx proxy start)");
            }
        } else {
            println!("  LaunchAgent: not installed");
        }
    }

    #[cfg(target_os = "linux")]
    {
        let service_path = systemd_path();
        if service_path.exists() {
            println!("  systemd user service: installed");
            let output = std::process::Command::new("systemctl")
                .args(["--user", "is-active", SYSTEMD_SERVICE])
                .output();
            match output {
                Ok(o) => {
                    let state = String::from_utf8_lossy(&o.stdout).trim().to_string();
                    println!("  Status: {state}");
                }
                Err(_) => println!("  Status: unknown"),
            }
        } else {
            println!("  systemd service: not installed");
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        println!("  Autostart not available on this platform");
    }
}

#[cfg(target_os = "macos")]
fn launchagent_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("Library/LaunchAgents")
        .join(format!("{PLIST_LABEL}.plist"))
}

#[cfg(target_os = "macos")]
fn install_launchagent(binary: &str, port: u16, quiet: bool) -> bool {
    let plist_dir = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("Library/LaunchAgents");
    let _ = std::fs::create_dir_all(&plist_dir);

    let plist_path = plist_dir.join(format!("{PLIST_LABEL}.plist"));
    // Stop the registered job first, then take over any standalone proxy an
    // IDE/dashboard may have spawned while launchd was unloaded. Starting the
    // new job with an occupied port causes an unconditional KeepAlive loop.
    crate::core::launchd::bootout(PLIST_LABEL, &plist_path);
    if !release_proxy_port(port, quiet) {
        return false;
    }
    // GH #439: proxy logs are STATE — resolve through the typed dir so a
    // post-split install writes to $XDG_STATE_HOME/lean-ctx/logs instead of a
    // re-created ~/.lean-ctx. Legacy single-dir installs still resolve here.
    let log_dir = crate::core::paths::state_dir()
        .unwrap_or_else(|_| std::env::temp_dir().join("lean-ctx"))
        .join("logs");
    let _ = std::fs::create_dir_all(&log_dir);

    // #356: wrap the launchd invocation in a deny-~/Documents seatbelt sandbox
    // so the proxy (a TCC-standalone process) can never trip the privacy prompt.
    let port_arg = format!("--port={port}");
    let program_args = crate::core::tcc_guard_sandbox::program_args_xml(
        &crate::core::tcc_guard_sandbox::wrap_launchd_args(binary, &["proxy", "start", &port_arg]),
        "        ",
    );

    // #449: pin the directory layout. A launchd-spawned proxy inherits only
    // launchd's minimal environment (no HOME, no XDG vars), so it resolves a
    // *different* config/data dir than the CLI that installed it — it never sees
    // the user's config.toml edits (live-upstream reload reads nothing) and
    // derives a mismatched session token. Bake the exact dirs this CLI resolves
    // into the plist so the managed proxy always agrees with the CLI.
    let env_vars = crate::core::tcc_guard_sandbox::pinned_layout_env_xml();

    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{PLIST_LABEL}</string>
    <key>ProgramArguments</key>
    <array>
{program_args}
    </array>
{env_vars}    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>{stdout}</string>
    <key>StandardErrorPath</key>
    <string>{stderr}</string>
</dict>
</plist>"#,
        stdout = log_dir.join("proxy.stdout.log").display(),
        stderr = log_dir.join("proxy.stderr.log").display(),
    );

    let _ = std::fs::write(&plist_path, &plist);

    let ok = crate::core::launchd::bootstrap(PLIST_LABEL, &plist_path);

    if !quiet {
        if ok {
            println!("  Installed LaunchAgent: {}", plist_path.display());
            println!("  Proxy will start on login and restart if stopped");
        } else {
            println!("  Created LaunchAgent at {}", plist_path.display());
            println!("  Load reported a problem; check: launchctl print {PLIST_LABEL}");
        }
    }
    ok
}

#[cfg(target_os = "macos")]
fn uninstall_launchagent(quiet: bool) {
    let plist_path = launchagent_path();
    if !plist_path.exists() {
        if !quiet {
            println!("  LaunchAgent not installed, nothing to remove");
        }
        return;
    }

    crate::core::launchd::bootout(PLIST_LABEL, &plist_path);

    let _ = std::fs::remove_file(&plist_path);
    if !quiet {
        println!("  Removed LaunchAgent: {}", plist_path.display());
    }
}

#[cfg(target_os = "linux")]
fn systemd_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(".config/systemd/user")
        .join(format!("{SYSTEMD_SERVICE}.service"))
}

#[cfg(target_os = "linux")]
fn install_systemd(binary: &str, port: u16, quiet: bool) -> bool {
    let service_dir = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(".config/systemd/user");
    let _ = std::fs::create_dir_all(&service_dir);

    let service_path = service_dir.join(format!("{SYSTEMD_SERVICE}.service"));

    let _ = std::process::Command::new("systemctl")
        .args(["--user", "stop", SYSTEMD_SERVICE])
        .output();
    if !release_proxy_port(port, quiet) {
        return false;
    }

    let unit = format!(
        r"[Unit]
Description=lean-ctx API Proxy
After=network.target
StartLimitIntervalSec=300
StartLimitBurst=5

[Service]
Type=simple
ExecStart={binary} proxy start --port={port}
Restart=on-failure
RestartSec=5
StandardOutput=journal
StandardError=journal
Environment=RUST_LOG=info

[Install]
WantedBy=default.target
"
    );

    let _ = std::fs::write(&service_path, &unit);

    let _ = std::process::Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .output();

    let result = std::process::Command::new("systemctl")
        .args(["--user", "enable", "--now", SYSTEMD_SERVICE])
        .output();

    if !quiet {
        match &result {
            Ok(o) if o.status.success() => {
                println!("  Installed systemd user service: {SYSTEMD_SERVICE}");
                println!("  Proxy will start on login and restart if stopped");
            }
            Ok(o) => {
                let err = String::from_utf8_lossy(&o.stderr);
                println!("  Created service file but enable failed: {err}");
            }
            Err(e) => {
                println!("  Created service file at {}", service_path.display());
                println!("  Could not enable: {e}");
            }
        }
    }
    result.is_ok_and(|output| output.status.success())
}

#[cfg(target_os = "linux")]
fn uninstall_systemd(quiet: bool) {
    let service_path = systemd_path();
    if !service_path.exists() {
        if !quiet {
            println!("  systemd service not installed, nothing to remove");
        }
        return;
    }

    let _ = std::process::Command::new("systemctl")
        .args(["--user", "stop", SYSTEMD_SERVICE])
        .output();
    let _ = std::process::Command::new("systemctl")
        .args(["--user", "disable", SYSTEMD_SERVICE])
        .output();
    let _ = std::fs::remove_file(&service_path);
    let _ = std::process::Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .output();

    if !quiet {
        println!("  Removed systemd service: {SYSTEMD_SERVICE}");
    }
}

pub fn find_binary() -> String {
    crate::core::portable_binary::resolve_portable_binary()
}

#[cfg(test)]
mod tests {
    use super::{health_identifies_lean_ctx_proxy, proxy_pid_from_health};
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    use super::{port_is_open, release_proxy_port};
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    use std::io::{Read, Write};
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    use std::net::TcpListener;

    #[test]
    fn health_pid_parser_accepts_only_well_formed_health() {
        assert_eq!(
            proxy_pid_from_health(r#"{"status":"ok","service":"lean-ctx-proxy","pid":4242}"#),
            Some(4242)
        );
        // #2176 review #5: an answer that does not identify the lean-ctx
        // proxy names no candidate.
        assert_eq!(proxy_pid_from_health(r#"{"status":"ok","pid":4242}"#), None);
        assert_eq!(
            proxy_pid_from_health(r#"{"status":"ok","service":"other","pid":4242}"#),
            None
        );
        assert_eq!(
            proxy_pid_from_health(r#"{"status":"busy","service":"lean-ctx-proxy","pid":4242}"#),
            None
        );
        assert_eq!(
            proxy_pid_from_health(r#"{"status":"ok","service":"lean-ctx-proxy","pid":0}"#),
            None
        );
        assert_eq!(proxy_pid_from_health(r#"{"status":"ok"}"#), None);
        assert_eq!(proxy_pid_from_health("not json"), None);
        assert!(health_identifies_lean_ctx_proxy(
            r#"{"status":"ok","service":"lean-ctx-proxy","pid":4242}"#
        ));
        assert!(!health_identifies_lean_ctx_proxy(
            r#"{"status":"ok","pid":4242}"#
        ));
    }

    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn managed_handoff_fails_closed_for_unidentified_listener() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(1)))
                    .unwrap();
                let mut request = [0_u8; 512];
                if stream.read(&mut request).unwrap_or(0) > 0 {
                    let body = r#"{"status":"ok"}"#;
                    write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .unwrap();
                }
            }
        });

        assert!(!release_proxy_port(port, true));
        server.join().unwrap();
    }

    #[cfg(unix)]
    struct ChildGuard(std::process::Child);

    #[cfg(unix)]
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    /// #2176 review: a listener that claims to be the lean-ctx proxy in
    /// `/health` but is not a lean-ctx `proxy start` process (here the test
    /// binary) is never signalled; the hand-off fails closed.
    #[test]
    #[cfg(unix)]
    fn managed_handoff_never_signals_an_unverified_health_pid() {
        let reservation = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = reservation.local_addr().unwrap().port();
        drop(reservation);

        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "proxy_autostart::tests::managed_handoff_test_child",
                "--nocapture",
            ])
            .env("LEAN_CTX_HANDOFF_TEST_PORT", port.to_string())
            .spawn()
            .unwrap();
        let mut child = ChildGuard(child);

        let ready_deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while !port_is_open(port) && std::time::Instant::now() < ready_deadline {
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        assert!(port_is_open(port), "test proxy did not bind port {port}");
        assert!(!release_proxy_port(port, true));
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "an unverified /health PID must never be signalled"
        );
        assert!(port_is_open(port));
    }

    #[test]
    #[ignore = "helper process for managed_handoff_never_signals_an_unverified_health_pid"]
    #[cfg(unix)]
    fn managed_handoff_test_child() {
        let port: u16 = std::env::var("LEAN_CTX_HANDOFF_TEST_PORT")
            .unwrap()
            .parse()
            .unwrap();
        let listener = TcpListener::bind(("127.0.0.1", port)).unwrap();
        loop {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(1)))
                .unwrap();
            let mut request = [0_u8; 512];
            if stream.read(&mut request).unwrap_or(0) > 0 {
                let body = format!(
                    r#"{{"status":"ok","service":"lean-ctx-proxy","pid":{}}}"#,
                    std::process::id()
                );
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
        }
    }
}
