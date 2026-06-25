#[cfg(any(target_os = "macos", target_os = "linux"))]
use std::path::PathBuf;

#[cfg(target_os = "macos")]
const PLIST_LABEL: &str = "com.leanctx.proxy";
#[cfg(target_os = "linux")]
const SYSTEMD_SERVICE: &str = "lean-ctx-proxy";

pub fn install(port: u16, quiet: bool) {
    let binary = find_binary();
    if binary.is_empty() {
        if !quiet {
            tracing::error!("Cannot find lean-ctx binary for autostart");
        }
        return;
    }

    #[cfg(target_os = "macos")]
    install_launchagent(&binary, port, quiet);

    #[cfg(target_os = "linux")]
    install_systemd(&binary, port, quiet);

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (&binary, quiet);
        println!("  Autostart not supported on this platform");
        println!("  Run manually: lean-ctx proxy start --port={port}");
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

pub fn uninstall(_quiet: bool) {
    #[cfg(target_os = "macos")]
    uninstall_launchagent(_quiet);

    #[cfg(target_os = "linux")]
    uninstall_systemd(_quiet);
}

/// Whether this platform has a proxy-autostart backend (`LaunchAgent` on macOS,
/// systemd user service on Linux). Windows and other targets have none, so a
/// missing autostart there must not be treated as a failure by `doctor` (#416).
#[must_use]
pub fn is_supported() -> bool {
    cfg!(any(target_os = "macos", target_os = "linux"))
}

/// Returns true if the proxy autostart is installed (plist/systemd service file exists).
#[must_use]
pub fn is_installed() -> bool {
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

pub fn status() {
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
fn install_launchagent(binary: &str, port: u16, quiet: bool) {
    let plist_dir = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("Library/LaunchAgents");
    let _ = std::fs::create_dir_all(&plist_dir);

    let plist_path = plist_dir.join(format!("{PLIST_LABEL}.plist"));
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
fn install_systemd(binary: &str, port: u16, quiet: bool) {
    let service_dir = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(".config/systemd/user");
    let _ = std::fs::create_dir_all(&service_dir);

    let service_path = service_dir.join(format!("{SYSTEMD_SERVICE}.service"));

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
        match result {
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

#[must_use]
pub fn find_binary() -> String {
    crate::core::portable_binary::resolve_portable_binary()
}
