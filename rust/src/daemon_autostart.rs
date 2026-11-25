#[cfg(any(target_os = "macos", target_os = "linux"))]
use std::path::PathBuf;

#[cfg(target_os = "macos")]
const PLIST_LABEL: &str = "com.leanctx.daemon";
#[cfg(target_os = "linux")]
const SYSTEMD_SERVICE: &str = "lean-ctx-daemon";

/// Whether this platform has a daemon autostart service (LaunchAgent on
/// macOS, systemd user unit on Linux). Windows has none: the daemon starts
/// through `lean-ctx daemon start` or on demand from an editor (#2176).
#[must_use]
pub const fn is_supported() -> bool {
    cfg!(any(target_os = "macos", target_os = "linux"))
}

/// What `lean-ctx daemon enable|disable` reports where no autostart service
/// exists — one line, never followed by a success claim (#2176).
#[must_use]
pub fn unsupported_message() -> &'static str {
    "Daemon autostart is not available on this platform. \
     Start the daemon with `lean-ctx daemon start` (editors also start it on demand)."
}

/// Platform service unit name (`systemctl --user <name>` / `launchctl print gui/$UID/<name>`).
/// `None` on platforms without autostart support.
pub fn service_name() -> Option<&'static str> {
    #[cfg(target_os = "macos")]
    {
        Some(PLIST_LABEL)
    }
    #[cfg(target_os = "linux")]
    {
        Some(SYSTEMD_SERVICE)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        None
    }
}

/// Path of the autostart service file lean-ctx writes on `daemon enable`
/// (LaunchAgent plist on macOS, systemd user unit on Linux), regardless of
/// whether it currently exists. `None` on unsupported platforms.
pub fn service_file_path() -> Option<std::path::PathBuf> {
    #[cfg(target_os = "macos")]
    {
        Some(launchagent_path())
    }
    #[cfg(target_os = "linux")]
    {
        Some(systemd_path())
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        None
    }
}

/// Whether this process may drive the user-wide autostart services (the
/// systemd user unit, the LaunchAgent). They run the daemon of the default
/// install: the account's own HOME and no directory override. The service
/// manager addresses a unit by name (`systemctl --user stop
/// lean-ctx-daemon`), whatever data dir the caller has, so a process with
/// `LEAN_CTX_DATA_DIR` (or another directory override) or a sandbox HOME is
/// another instance and never stops, starts, installs or removes them. On
/// 5 Nov 2026 `serve --stop` with a scratch `LEAN_CTX_DATA_DIR` stopped the
/// user's real daemon through exactly that call.
#[must_use]
pub fn controls_user_services() -> bool {
    services_belong_to_instance(
        crate::core::paths::dir_override_active(),
        dirs::home_dir().as_deref(),
        crate::core::config::shell_policy::real_user_home().as_deref(),
    )
}

/// [`controls_user_services`] as a pure function of its inputs.
fn services_belong_to_instance(
    override_active: bool,
    home: Option<&std::path::Path>,
    account_home: Option<&std::path::Path>,
) -> bool {
    if override_active {
        return false;
    }
    let (Some(home), Some(account_home)) = (home, account_home) else {
        return false;
    };
    home == account_home
        || std::fs::canonicalize(home)
            .ok()
            .zip(std::fs::canonicalize(account_home).ok())
            .is_some_and(|(a, b)| a == b)
}

/// What a command prints when it leaves a user-wide service alone because it
/// belongs to another instance (see [`controls_user_services`]): the daemon
/// and proxy autostart units and the auto-update schedule (LaunchAgent,
/// systemd timer, crontab entry, scheduled task).
#[must_use]
pub fn not_this_instance_message() -> &'static str {
    "Not this instance: the user-wide service (daemon or proxy autostart, auto-update \
     schedule) belongs to the default install (the account's HOME without LEAN_CTX_DATA_DIR \
     or another directory override); this instance leaves it alone."
}

/// The PID of the daemon the autostart service runs right now, when the
/// service belongs to this instance. `None` when it runs nothing, is not
/// installed, or belongs to another instance. Read-only.
#[must_use]
pub fn supervised_pid() -> Option<u32> {
    if !is_supported() || !controls_user_services() {
        return None;
    }
    #[cfg(target_os = "macos")]
    {
        if !launchagent_path().exists() {
            return None;
        }
        crate::core::launchd::service_pid(PLIST_LABEL)
    }
    #[cfg(target_os = "linux")]
    {
        if !systemd_path().exists() {
            return None;
        }
        let output = std::process::Command::new("systemctl")
            .args([
                "--user",
                "show",
                "--property=MainPID",
                "--value",
                SYSTEMD_SERVICE,
            ])
            .output()
            .ok()
            .filter(|o| o.status.success())?;
        String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse::<u32>()
            .ok()
            .filter(|pid| *pid != 0)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        None
    }
}

pub fn install(quiet: bool) {
    let binary = crate::proxy_autostart::find_binary();
    install_with_binary(&binary, quiet);
}

pub fn install_with_binary(binary: &str, quiet: bool) {
    if is_supported() && !controls_user_services() {
        if !quiet {
            eprintln!("{}", not_this_instance_message());
        }
        return;
    }
    if binary.is_empty() {
        if !quiet {
            tracing::error!("Cannot find lean-ctx binary for daemon autostart");
        }
        return;
    }

    #[cfg(target_os = "macos")]
    install_launchagent(binary, quiet);

    #[cfg(target_os = "linux")]
    install_systemd(binary, quiet);

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (binary, quiet);
        println!("  Autostart not supported on this platform");
        println!("  Run manually: lean-ctx serve -d");
    }
}

/// Stop the autostart service. A no-op for another instance (see
/// [`controls_user_services`]); that instance's daemon is stopped through
/// its own PID file and endpoint by [`crate::daemon::stop_daemon`].
pub fn stop() {
    if !controls_user_services() {
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

/// Start the autostart service; a no-op for another instance. Returns whether
/// the service manager accepted the start (`false` for another instance, a
/// missing unit or a platform without one); callers may ignore it.
pub fn start() -> bool {
    if !controls_user_services() {
        return false;
    }
    #[cfg(target_os = "macos")]
    {
        let plist_path = launchagent_path();
        if !plist_path.exists() {
            return false;
        }
        repin_launchagent(&plist_path);
        crate::core::launchd::bootstrap(PLIST_LABEL, &plist_path)
    }

    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("systemctl")
            .args(["--user", "start", SYSTEMD_SERVICE])
            .output()
            .is_ok_and(|output| output.status.success())
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        false
    }
}

pub fn uninstall(quiet: bool) {
    if is_supported() && !controls_user_services() {
        if !quiet {
            eprintln!("{}", not_this_instance_message());
        }
        return;
    }
    #[cfg(target_os = "macos")]
    uninstall_launchagent(quiet);

    #[cfg(target_os = "linux")]
    uninstall_systemd(quiet);

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let _ = quiet;
}

/// What [`ensure_supervised`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Supervision {
    /// The service was installed already (and is started if it was not).
    Already,
    /// The service is installed now; a daemon started by hand was handed over.
    Installed,
    /// Installing it did not work: the daemon runs without supervision.
    Failed,
    /// No supervisor on this platform, or a test / isolated instance (a
    /// directory override), which never installs a user-wide service.
    Skipped,
}

/// Make sure the daemon runs under the platform's supervisor (systemd user
/// unit, LaunchAgent): it starts at login and restarts after a crash, so
/// work that lives in the daemon (Cloud's background sync, the outbox) never
/// depends on someone starting it by hand. A daemon started by hand is
/// stopped first so the supervised one can take its socket. `restart`: a
/// supervised daemon already running is restarted (a new connection or a
/// new build takes effect; the daemon reads both only at start).
pub fn ensure_supervised(restart: bool) -> Supervision {
    if !is_supported() || cfg!(test) || !controls_user_services() {
        return Supervision::Skipped;
    }
    if is_installed() {
        // A plist pinned to another layout is re-pinned to this CLI's; launchd
        // reads it only at bootstrap, so a re-pin restarts the daemon.
        #[cfg(target_os = "macos")]
        let restart = repin_launchagent(&launchagent_path()) || restart;
        if restart {
            stop();
        }
        if restart || !crate::daemon::is_daemon_running() {
            start();
        }
        return Supervision::Already;
    }
    if crate::daemon::is_daemon_running() {
        let _ = crate::daemon::stop_daemon();
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    install(true);
    if is_installed() {
        Supervision::Installed
    } else {
        Supervision::Failed
    }
}

/// The line a command shows for `supervision` (`None`: nothing to say).
pub fn supervision_line(supervision: Supervision) -> Option<String> {
    match supervision {
        Supervision::Installed => Some(format!(
            "The LeanCTX daemon now runs supervised ({}): it starts at login and restarts after a crash.{}",
            service_name().unwrap_or("service"),
            linger_hint()
        )),
        Supervision::Failed => Some(
            "The LeanCTX daemon could not be set up to start by itself; run `lean-ctx daemon enable`."
                .to_string(),
        ),
        Supervision::Already | Supervision::Skipped => None,
    }
}

/// Linux without lingering stops user services at logout (a headless
/// server): the hint, else nothing.
fn linger_hint() -> String {
    #[cfg(target_os = "linux")]
    {
        let user = whoami();
        let lingering = std::process::Command::new("loginctl")
            .args(["show-user", &user, "-p", "Linger", "--value"])
            .output()
            .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "yes");
        if !lingering {
            return format!(
                " To keep it running while you are logged out: loginctl enable-linger {user}"
            );
        }
    }
    String::new()
}

/// Whether this instance's autostart service is installed. Another
/// instance's service (see [`controls_user_services`]) never counts.
pub fn is_installed() -> bool {
    if !controls_user_services() {
        return false;
    }
    #[cfg(target_os = "macos")]
    {
        launchagent_path().exists()
    }
    #[cfg(target_os = "linux")]
    {
        if !systemd_path().exists() {
            return false;
        }
        std::process::Command::new("systemctl")
            .args(["--user", "is-enabled", "--quiet", SYSTEMD_SERVICE])
            .status()
            .is_ok_and(|s| s.success())
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        false
    }
}

// ---------------------------------------------------------------------------
// macOS LaunchAgent
// ---------------------------------------------------------------------------

#[cfg(target_os = "macos")]
fn launchagent_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("Library/LaunchAgents")
        .join(format!("{PLIST_LABEL}.plist"))
}

/// Re-pin an installed LaunchAgent (`plist_path`) to the directory layout this
/// CLI resolves now, so a refresh that only re-bootstraps the existing plist
/// never keeps a stale `LEAN_CTX_*_DIR` pin (an install re-renders the whole
/// plist anyway). Only the `EnvironmentVariables` block changes. `true` when
/// the file was rewritten. Never touches a real plist under `cfg(test)`.
#[cfg(target_os = "macos")]
pub(crate) fn repin_launchagent(plist_path: &std::path::Path) -> bool {
    if cfg!(test) {
        return false;
    }
    let env_block = crate::core::tcc_guard_sandbox::pinned_layout_env_xml();
    match crate::core::tcc_guard_sandbox::repin_launchagent_file(plist_path, &env_block) {
        Ok(changed) => changed,
        Err(e) => {
            tracing::warn!("re-pin of {} failed: {e}", plist_path.display());
            false
        }
    }
}

#[cfg(target_os = "macos")]
fn install_launchagent(binary: &str, quiet: bool) {
    let la_dir = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("Library/LaunchAgents");
    let _ = std::fs::create_dir_all(&la_dir);

    let plist_path = la_dir.join(format!("{PLIST_LABEL}.plist"));
    let data_dir = dirs::data_local_dir()
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".local/share"))
        .join("lean-ctx");

    let _ = std::fs::create_dir_all(&data_dir);

    let plist = launchagent_plist(binary, &data_dir);

    let _ = std::fs::write(&plist_path, &plist);

    let ok = crate::core::launchd::bootstrap(PLIST_LABEL, &plist_path);

    if !quiet {
        if ok {
            println!("  Installed LaunchAgent: {PLIST_LABEL}");
            println!("  Service file: {}", plist_path.display());
            println!("  Daemon will start on login and restart if stopped");
        } else {
            println!("  Created plist at {}", plist_path.display());
            println!("  Load reported a problem; check: launchctl print {PLIST_LABEL}");
        }
    }
}

/// Render the daemon LaunchAgent plist (pure, so tests can check it).
#[cfg(target_os = "macos")]
fn launchagent_plist(binary: &str, data_dir: &std::path::Path) -> String {
    use crate::core::tcc_guard_sandbox as tcc;
    // rc2-2: the binary itself is ProgramArguments[0], so Login Items name
    // LeanCTX, not sandbox-exec. The daemon applies the #356 deny-~/Documents
    // seatbelt to itself at start-up (`reexec_under_seatbelt_if_needed`).
    let program_args = tcc::program_args_xml(
        &tcc::launchd_program_args(binary, &["serve", "--_foreground-daemon"]),
        "        ",
    );
    let bundle_ids = tcc::associated_bundle_ids_xml("    ");

    // #449: pin the directory layout so the launchd-spawned daemon resolves the
    // same config/data dirs as the installing CLI (see `pinned_layout_env_xml`).
    let env_vars = tcc::pinned_layout_env_xml();

    format!(
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
{bundle_ids}{env_vars}    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <dict>
        <key>SuccessfulExit</key>
        <false/>
    </dict>
    <key>ThrottleInterval</key>
    <integer>10</integer>
    <key>StandardOutPath</key>
    <string>{stdout}</string>
    <key>StandardErrorPath</key>
    <string>{stderr}</string>
</dict>
</plist>
"#,
        stdout = data_dir.join("daemon-stdout.log").display(),
        stderr = data_dir.join("daemon-stderr.log").display(),
    )
}

#[cfg(target_os = "macos")]
fn uninstall_launchagent(quiet: bool) {
    let plist_path = launchagent_path();
    if !plist_path.exists() {
        if !quiet {
            println!("  Daemon LaunchAgent not installed, nothing to remove");
        }
        return;
    }
    crate::core::launchd::bootout(PLIST_LABEL, &plist_path);
    let _ = std::fs::remove_file(&plist_path);
    if !quiet {
        println!("  Removed daemon LaunchAgent: {PLIST_LABEL}");
        println!("  Service file: {}", plist_path.display());
    }
}

// ---------------------------------------------------------------------------
// Linux systemd
// ---------------------------------------------------------------------------

#[cfg(target_os = "linux")]
fn systemd_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(".config/systemd/user")
        .join(format!("{SYSTEMD_SERVICE}.service"))
}

#[cfg(target_os = "linux")]
fn install_systemd(binary: &str, quiet: bool) {
    let service_dir = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(".config/systemd/user");
    let _ = std::fs::create_dir_all(&service_dir);

    let service_path = service_dir.join(format!("{SYSTEMD_SERVICE}.service"));

    let unit = format!(
        r"[Unit]
Description=lean-ctx IPC Daemon
After=network.target
StartLimitIntervalSec=300
StartLimitBurst=5

[Service]
Type=simple
ExecStart={binary} serve --_foreground-daemon
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

    match result {
        Ok(o) if o.status.success() => {
            if !quiet {
                println!("  Installed systemd user service: {SYSTEMD_SERVICE}");
                println!("  Service file: {}", service_path.display());
                println!("  Daemon will start on login and restart if stopped");
            }
        }
        Ok(o) => {
            let err = String::from_utf8_lossy(&o.stderr);
            eprintln!("  Created service file but `systemctl enable` failed: {err}");
            eprintln!("  Try manually: systemctl --user enable --now {SYSTEMD_SERVICE}");
        }
        Err(e) => {
            eprintln!("  Created service file at {}", service_path.display());
            eprintln!("  Could not run systemctl: {e}");
        }
    }

    // Hint about linger for headless/server use (needed for boot-time start without login)
    if !quiet
        && let Ok(o) = std::process::Command::new("loginctl")
            .args(["show-user", &whoami(), "-p", "Linger", "--value"])
            .output()
    {
        let val = String::from_utf8_lossy(&o.stdout).trim().to_string();
        if val != "yes" {
            println!(
                "  Note: for daemon to start at boot (without login), run:\n    \
                 loginctl enable-linger {}",
                whoami()
            );
        }
    }
}

#[cfg(target_os = "linux")]
fn whoami() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .unwrap_or_else(|_| "$(whoami)".to_string())
}

#[cfg(target_os = "linux")]
fn uninstall_systemd(quiet: bool) {
    let service_path = systemd_path();
    if !service_path.exists() {
        if !quiet {
            println!("  Daemon systemd service not installed, nothing to remove");
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
        println!("  Removed daemon systemd service: {SYSTEMD_SERVICE}");
        println!("  Service file: {}", service_path.display());
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    /// 5 Nov 2026: the user-wide service belongs to the default install
    /// only. A directory override or a sandbox HOME is another instance.
    #[test]
    fn user_services_belong_to_the_default_install_only() {
        let account = Path::new("/home/someone");
        let sandbox = Path::new("/tmp/sandbox-home");
        assert!(super::services_belong_to_instance(
            false,
            Some(account),
            Some(account)
        ));
        assert!(!super::services_belong_to_instance(
            true,
            Some(account),
            Some(account)
        ));
        assert!(!super::services_belong_to_instance(
            false,
            Some(sandbox),
            Some(account)
        ));
        assert!(!super::services_belong_to_instance(
            false,
            None,
            Some(account)
        ));
        assert!(!super::services_belong_to_instance(
            false,
            Some(account),
            None
        ));
    }

    /// Under a directory override nothing reaches the service manager:
    /// `stop`/`start`/`uninstall` return before `systemctl`/`launchctl`.
    #[test]
    fn directory_override_never_controls_user_services() {
        let _lock = crate::core::data_dir::test_env_lock();
        let dir = tempfile::tempdir().expect("tempdir");
        crate::test_env::set_var("LEAN_CTX_DATA_DIR", dir.path());
        let controls = super::controls_user_services();
        let installed = super::is_installed();
        let supervised = super::supervised_pid();
        crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
        assert!(!controls);
        assert!(!installed);
        assert_eq!(supervised, None);
    }

    /// #2176: support and the service path agree, and the unsupported line
    /// never claims success.
    #[test]
    fn autostart_support_matches_service_path() {
        assert_eq!(super::is_supported(), super::service_file_path().is_some());
        let msg = super::unsupported_message();
        assert!(msg.contains("not available"));
        assert!(msg.contains("lean-ctx daemon start"));
        assert!(!msg.contains('✓') && !msg.contains("enabled"));
    }

    /// rc2-2: Login Items name the item after ProgramArguments[0], so it must
    /// be the lean-ctx binary (not sandbox-exec), and the plist attributes the
    /// item to the LeanCTX app.
    #[cfg(target_os = "macos")]
    #[test]
    fn launchagent_plist_starts_binary_and_names_the_app() {
        let plist = super::launchagent_plist("/opt/lc/lean-ctx", Path::new("/tmp/lc-data"));
        let args = &plist[plist.find("<key>ProgramArguments</key>").expect("args")..];
        let first = args.find("<string>").expect("first arg") + "<string>".len();
        let first = &args[first..first + args[first..].find("</string>").expect("end")];
        assert_eq!(first, "/opt/lc/lean-ctx");
        assert!(!plist.contains("sandbox-exec"));
        assert!(plist.contains("<key>AssociatedBundleIdentifiers</key>"));
        assert!(plist.contains("<string>com.leanctx.workspace</string>"));
        assert!(!plist.contains("LEAN_CTX_SEATBELT"));
    }

    // GH #394: the service file path must be discoverable programmatically so
    // enable/disable/status/doctor can print it.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn service_file_path_matches_platform_conventions() {
        let path = super::service_file_path().expect("supported platform");
        let name = super::service_name().expect("supported platform");
        let s = path.to_string_lossy();
        #[cfg(target_os = "macos")]
        {
            assert!(s.contains("Library/LaunchAgents"), "got: {s}");
            assert!(s.ends_with("com.leanctx.daemon.plist"), "got: {s}");
            assert_eq!(name, "com.leanctx.daemon");
        }
        #[cfg(target_os = "linux")]
        {
            assert!(s.contains(".config/systemd/user"), "got: {s}");
            assert!(s.ends_with("lean-ctx-daemon.service"), "got: {s}");
            assert_eq!(name, "lean-ctx-daemon");
        }
    }
}
