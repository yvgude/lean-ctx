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

pub fn uninstall(_quiet: bool) {
    #[cfg(target_os = "macos")]
    uninstall_launchagent(_quiet);

    #[cfg(target_os = "linux")]
    uninstall_systemd(_quiet);
}

pub fn status() {
    #[cfg(target_os = "macos")]
    {
        let plist_path = launchagent_path();
        if plist_path.exists() {
            println!("  LaunchAgent: installed at {}", plist_path.display());
            let output = std::process::Command::new("launchctl")
                .args(["list", PLIST_LABEL])
                .output();
            match output {
                Ok(o) if o.status.success() => println!("  Status: loaded"),
                _ => println!(
                    "  Status: not loaded (run: launchctl load {})",
                    plist_path.display()
                ),
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
    let log_dir = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(".lean-ctx/logs");
    let _ = std::fs::create_dir_all(&log_dir);

    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{PLIST_LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{binary}</string>
        <string>proxy</string>
        <string>start</string>
        <string>--port={port}</string>
    </array>
    <key>RunAtLoad</key>
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

    let _ = std::process::Command::new("launchctl")
        .args(["unload", &plist_path.to_string_lossy()])
        .output();

    let result = std::process::Command::new("launchctl")
        .args(["load", &plist_path.to_string_lossy()])
        .output();

    if !quiet {
        match result {
            Ok(o) if o.status.success() => {
                println!("  Installed LaunchAgent: {}", plist_path.display());
                println!("  Proxy will start on login and restart if stopped");
            }
            Ok(o) => {
                let err = String::from_utf8_lossy(&o.stderr);
                println!("  Created LaunchAgent but load failed: {err}");
                println!("  Try: launchctl load {}", plist_path.display());
            }
            Err(e) => {
                println!("  Created LaunchAgent at {}", plist_path.display());
                println!("  Could not load: {e}");
            }
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

    let _ = std::process::Command::new("launchctl")
        .args(["unload", &plist_path.to_string_lossy()])
        .output();

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

[Service]
Type=simple
ExecStart={binary} proxy start --port={port}
Restart=on-failure
RestartSec=5
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

fn find_binary() -> String {
    std::env::current_exe().map_or_else(
        |_| which_lean_ctx().unwrap_or_default(),
        |p| p.to_string_lossy().to_string(),
    )
}

fn which_lean_ctx() -> Option<String> {
    std::process::Command::new("which")
        .arg("lean-ctx")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
}
