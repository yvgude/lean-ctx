#[cfg(any(target_os = "macos", target_os = "linux"))]
use std::path::PathBuf;

#[cfg(target_os = "macos")]
const PLIST_LABEL: &str = "com.leanctx.daemon";
#[cfg(target_os = "linux")]
const SYSTEMD_SERVICE: &str = "lean-ctx-daemon";

/// Platform service unit name (`systemctl --user <name>` / `launchctl print gui/$UID/<name>`).
/// `None` on platforms without autostart support.
#[must_use]
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
/// (`LaunchAgent` plist on macOS, systemd user unit on Linux), regardless of
/// whether it currently exists. `None` on unsupported platforms.
#[must_use]
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

pub fn install(quiet: bool) {
    let binary = crate::proxy_autostart::find_binary();
    if binary.is_empty() {
        if !quiet {
            tracing::error!("Cannot find lean-ctx binary for daemon autostart");
        }
        return;
    }

    #[cfg(target_os = "macos")]
    install_launchagent(&binary, quiet);

    #[cfg(target_os = "linux")]
    install_systemd(&binary, quiet);

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (&binary, quiet);
        println!("  Autostart not supported on this platform");
        println!("  Run manually: lean-ctx serve -d");
    }
}

pub fn stop() {
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

pub fn start() {
    #[cfg(target_os = "macos")]
    {
        let plist_path = launchagent_path();
        if plist_path.exists() {
            crate::core::launchd::bootstrap(PLIST_LABEL, &plist_path);
        }
    }

    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("systemctl")
            .args(["--user", "start", SYSTEMD_SERVICE])
            .output();
    }
}

pub fn uninstall(quiet: bool) {
    #[cfg(target_os = "macos")]
    uninstall_launchagent(quiet);

    #[cfg(target_os = "linux")]
    uninstall_systemd(quiet);

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let _ = quiet;
}

#[must_use]
pub fn is_installed() -> bool {
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

    // #356: wrap the launchd invocation in a deny-~/Documents seatbelt sandbox
    // so the daemon (a TCC-standalone process) can never trip the privacy prompt.
    let program_args = crate::core::tcc_guard_sandbox::program_args_xml(
        &crate::core::tcc_guard_sandbox::wrap_launchd_args(
            binary,
            &["serve", "--_foreground-daemon"],
        ),
        "        ",
    );

    // #449: pin the directory layout so the launchd-spawned daemon resolves the
    // same config/data dirs as the installing CLI (see `pinned_layout_env_xml`).
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
    );

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
