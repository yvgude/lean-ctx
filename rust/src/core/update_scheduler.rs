//! OS-specific auto-update scheduler management.
//! Supports macOS `LaunchAgent`, Linux systemd/cron, Windows Task Scheduler.

use std::path::PathBuf;

#[cfg(target_os = "macos")]
const LABEL: &str = "com.leanctx.autoupdate";

#[derive(Debug, Clone)]
pub struct ScheduleInfo {
    pub enabled: bool,
    pub mechanism: String,
    pub interval_hours: u64,
    pub scheduler_path: Option<PathBuf>,
    pub last_check: Option<String>,
}

impl std::fmt::Display for ScheduleInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.enabled {
            write!(
                f,
                "Auto-update: enabled ({}, every {}h)",
                self.mechanism, self.interval_hours
            )?;
            if let Some(ref path) = self.scheduler_path {
                write!(f, "\n  Scheduler: {}", path.display())?;
            }
            if let Some(ref last) = self.last_check {
                write!(f, "\n  Last check: {last}")?;
            }
        } else {
            write!(f, "Auto-update: disabled")?;
        }
        Ok(())
    }
}

pub fn install_schedule(interval_hours: u64) -> Result<ScheduleInfo, String> {
    let binary = std::path::PathBuf::from(super::portable_binary::resolve_portable_binary());

    #[cfg(target_os = "macos")]
    return install_macos_launchagent(&binary, interval_hours * 3600, interval_hours);

    #[cfg(target_os = "linux")]
    return install_linux_scheduler(&binary, interval_hours);

    #[cfg(target_os = "windows")]
    return install_windows_task(&binary, interval_hours);

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        let _ = binary;
        Err("Auto-update scheduling not supported on this platform".to_string())
    }
}

pub fn remove_schedule() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    return remove_macos_launchagent();

    #[cfg(target_os = "linux")]
    return remove_linux_scheduler();

    #[cfg(target_os = "windows")]
    return remove_windows_task();

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    Ok(())
}

#[must_use]
pub fn schedule_status() -> ScheduleInfo {
    #[cfg(target_os = "macos")]
    return macos_status();

    #[cfg(target_os = "linux")]
    return linux_status();

    #[cfg(target_os = "windows")]
    return windows_status();

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    ScheduleInfo {
        enabled: false,
        mechanism: "unsupported".into(),
        interval_hours: 0,
        scheduler_path: None,
        last_check: None,
    }
}

// ─── macOS ───────────────────────────────────────────────

#[cfg(target_os = "macos")]
fn plist_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("Library/LaunchAgents")
        .join(format!("{LABEL}.plist"))
}

#[cfg(target_os = "macos")]
fn install_macos_launchagent(
    binary: &std::path::Path,
    interval_secs: u64,
    interval_hours: u64,
) -> Result<ScheduleInfo, String> {
    let path = plist_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }

    // GH #439: auto-update logs are STATE — route through the typed resolver so a
    // post-split install writes to $XDG_STATE_HOME/lean-ctx, not a re-created
    // ~/.lean-ctx. Legacy single-dir installs keep resolving to ~/.lean-ctx.
    let log_dir =
        crate::core::paths::state_dir().unwrap_or_else(|_| std::env::temp_dir().join("lean-ctx"));
    let _ = std::fs::create_dir_all(&log_dir);

    let binary_str = binary.to_string_lossy();
    let stdout_log = log_dir.join("autoupdate-stdout.log");
    let stderr_log = log_dir.join("autoupdate-stderr.log");

    // #356: wrap the launchd invocation in a deny-~/Documents seatbelt sandbox
    // so the scheduled updater (a TCC-standalone process) can never trip the
    // privacy prompt.
    let program_args = crate::core::tcc_guard_sandbox::program_args_xml(
        &crate::core::tcc_guard_sandbox::wrap_launchd_args(
            &binary_str,
            &["update", "--quiet", "--scheduled"],
        ),
        "    ",
    );

    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{LABEL}</string>
  <key>ProgramArguments</key>
  <array>
{program_args}
  </array>
  <key>StartInterval</key>
  <integer>{interval_secs}</integer>
  <key>RunAtLoad</key>
  <false/>
  <key>StandardOutPath</key>
  <string>{}</string>
  <key>StandardErrorPath</key>
  <string>{}</string>
</dict>
</plist>"#,
        stdout_log.display(),
        stderr_log.display()
    );

    crate::core::launchd::bootout(LABEL, &path);

    std::fs::write(&path, plist).map_err(|e| format!("Failed to write plist: {e}"))?;

    if !crate::core::launchd::bootstrap(LABEL, &path) {
        return Err("launchctl bootstrap failed; check: launchctl print gui/$(id -u)".into());
    }

    Ok(ScheduleInfo {
        enabled: true,
        mechanism: "LaunchAgent".into(),
        interval_hours,
        scheduler_path: Some(path),
        last_check: None,
    })
}

#[cfg(target_os = "macos")]
fn remove_macos_launchagent() -> Result<(), String> {
    let path = plist_path();
    if path.exists() {
        crate::core::launchd::bootout(LABEL, &path);
        std::fs::remove_file(&path).map_err(|e| format!("Failed to remove plist: {e}"))?;
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn macos_status() -> ScheduleInfo {
    let path = plist_path();
    let enabled = path.exists();
    let interval_hours = if enabled {
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|content| {
                let idx = content.find("<key>StartInterval</key>")?;
                let after = &content[idx..];
                let int_start = after.find("<integer>")? + 9;
                let int_end = after.find("</integer>")?;
                after[int_start..int_end].parse::<u64>().ok()
            })
            .map_or(6, |s| s / 3600)
    } else {
        0
    };
    ScheduleInfo {
        enabled,
        mechanism: "LaunchAgent".into(),
        interval_hours,
        scheduler_path: Some(path),
        last_check: read_last_check_time(),
    }
}

// ─── Linux ───────────────────────────────────────────────

#[cfg(target_os = "linux")]
fn has_systemd() -> bool {
    std::path::Path::new("/run/systemd/system").exists()
}

#[cfg(target_os = "linux")]
fn systemd_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(".config/systemd/user")
}

#[cfg(target_os = "linux")]
fn install_linux_scheduler(
    binary: &std::path::Path,
    interval_hours: u64,
) -> Result<ScheduleInfo, String> {
    if has_systemd() {
        install_linux_systemd(binary, interval_hours)
    } else {
        install_linux_cron(binary, interval_hours)
    }
}

#[cfg(target_os = "linux")]
fn install_linux_systemd(
    binary: &std::path::Path,
    interval_hours: u64,
) -> Result<ScheduleInfo, String> {
    let dir = systemd_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;

    let binary_str = binary.to_string_lossy();

    let service = format!(
        "[Unit]\nDescription=lean-ctx auto-updater\n\n[Service]\nType=oneshot\nExecStart={binary_str} update --quiet --scheduled\n"
    );
    let timer = format!(
        "[Unit]\nDescription=lean-ctx auto-update timer\n\n[Timer]\nOnBootSec=1h\nOnUnitActiveSec={interval_hours}h\nPersistent=true\n\n[Install]\nWantedBy=timers.target\n"
    );

    std::fs::write(dir.join("lean-ctx-autoupdate.service"), service).map_err(|e| e.to_string())?;
    let timer_path = dir.join("lean-ctx-autoupdate.timer");
    std::fs::write(&timer_path, timer).map_err(|e| e.to_string())?;

    let _ = std::process::Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .output();
    let out = std::process::Command::new("systemctl")
        .args(["--user", "enable", "--now", "lean-ctx-autoupdate.timer"])
        .output()
        .map_err(|e| e.to_string())?;

    if !out.status.success() {
        return Err(format!(
            "systemctl enable failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }

    Ok(ScheduleInfo {
        enabled: true,
        mechanism: "systemd timer".into(),
        interval_hours,
        scheduler_path: Some(timer_path),
        last_check: None,
    })
}

#[cfg(target_os = "linux")]
fn install_linux_cron(
    binary: &std::path::Path,
    interval_hours: u64,
) -> Result<ScheduleInfo, String> {
    let cron_expr = if interval_hours <= 1 {
        "0 * * * *".to_string()
    } else if interval_hours >= 24 {
        "0 4 * * *".to_string()
    } else {
        format!("0 */{interval_hours} * * *")
    };

    let entry = format!(
        "{cron_expr} {} update --quiet --scheduled",
        binary.to_string_lossy()
    );

    let existing = std::process::Command::new("crontab")
        .arg("-l")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default();

    let filtered: String = existing
        .lines()
        .filter(|l| !l.contains("lean-ctx") || !l.contains("update"))
        .chain(std::iter::once(entry.as_str()))
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";

    let mut child = std::process::Command::new("crontab")
        .arg("-")
        .stdin(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;

    use std::io::Write;
    child
        .stdin
        .take()
        .ok_or_else(|| "failed to open crontab stdin".to_string())?
        .write_all(filtered.as_bytes())
        .map_err(|e| e.to_string())?;
    child.wait().map_err(|e| e.to_string())?;

    Ok(ScheduleInfo {
        enabled: true,
        mechanism: "cron".into(),
        interval_hours,
        scheduler_path: None,
        last_check: None,
    })
}

#[cfg(target_os = "linux")]
#[allow(clippy::unnecessary_wraps)]
fn remove_linux_scheduler() -> Result<(), String> {
    let dir = systemd_dir();
    let timer = dir.join("lean-ctx-autoupdate.timer");
    let service = dir.join("lean-ctx-autoupdate.service");
    if timer.exists() {
        let _ = std::process::Command::new("systemctl")
            .args(["--user", "disable", "--now", "lean-ctx-autoupdate.timer"])
            .output();
        let _ = std::fs::remove_file(&timer);
        let _ = std::fs::remove_file(&service);
        let _ = std::process::Command::new("systemctl")
            .args(["--user", "daemon-reload"])
            .output();
    }

    if let Ok(out) = std::process::Command::new("crontab").arg("-l").output() {
        let existing = String::from_utf8_lossy(&out.stdout).to_string();
        if existing.contains("lean-ctx") && existing.contains("update") {
            let filtered: String = existing
                .lines()
                .filter(|l| !(l.contains("lean-ctx") && l.contains("update")))
                .collect::<Vec<_>>()
                .join("\n")
                + "\n";
            if let Ok(mut child) = std::process::Command::new("crontab")
                .arg("-")
                .stdin(std::process::Stdio::piped())
                .spawn()
            {
                use std::io::Write;
                if let Some(mut stdin) = child.stdin.take() {
                    let _ = stdin.write_all(filtered.as_bytes());
                }
                let _ = child.wait();
            }
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn linux_status() -> ScheduleInfo {
    let timer = systemd_dir().join("lean-ctx-autoupdate.timer");
    if timer.exists() {
        return ScheduleInfo {
            enabled: true,
            mechanism: "systemd timer".into(),
            interval_hours: 6,
            scheduler_path: Some(timer),
            last_check: read_last_check_time(),
        };
    }
    if let Ok(out) = std::process::Command::new("crontab").arg("-l").output() {
        let crontab = String::from_utf8_lossy(&out.stdout);
        if crontab.contains("lean-ctx") && crontab.contains("update") {
            return ScheduleInfo {
                enabled: true,
                mechanism: "cron".into(),
                interval_hours: 6,
                scheduler_path: None,
                last_check: read_last_check_time(),
            };
        }
    }
    ScheduleInfo {
        enabled: false,
        mechanism: "none".into(),
        interval_hours: 0,
        scheduler_path: None,
        last_check: None,
    }
}

// ─── Windows ─────────────────────────────────────────────

#[cfg(target_os = "windows")]
fn install_windows_task(
    binary: &std::path::Path,
    interval_hours: u64,
) -> Result<ScheduleInfo, String> {
    let binary_str = binary.to_string_lossy();
    let out = std::process::Command::new("schtasks")
        .args([
            "/Create",
            "/F",
            "/TN",
            "lean-ctx autoupdate",
            "/TR",
            &format!("\"{binary_str}\" update --quiet --scheduled"),
            "/SC",
            "HOURLY",
            "/MO",
            &interval_hours.to_string(),
            "/RL",
            "HIGHEST",
        ])
        .output()
        .map_err(|e| e.to_string())?;

    if !out.status.success() {
        return Err(format!(
            "schtasks failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }

    Ok(ScheduleInfo {
        enabled: true,
        mechanism: "Task Scheduler".into(),
        interval_hours,
        scheduler_path: None,
        last_check: None,
    })
}

#[cfg(target_os = "windows")]
fn remove_windows_task() -> Result<(), String> {
    let _ = std::process::Command::new("schtasks")
        .args(["/Delete", "/F", "/TN", "lean-ctx autoupdate"])
        .output();
    Ok(())
}

#[cfg(target_os = "windows")]
fn windows_status() -> ScheduleInfo {
    let out = std::process::Command::new("schtasks")
        .args(["/Query", "/TN", "lean-ctx autoupdate", "/FO", "LIST"])
        .output();

    let enabled = out.as_ref().is_ok_and(|o| o.status.success());
    ScheduleInfo {
        enabled,
        mechanism: "Task Scheduler".into(),
        interval_hours: if enabled { 6 } else { 0 },
        scheduler_path: None,
        last_check: read_last_check_time(),
    }
}

// ─── Shared ──────────────────────────────────────────────

fn read_last_check_time() -> Option<String> {
    let path = crate::core::paths::cache_dir()
        .ok()?
        .join("latest-version.json");
    let content = std::fs::read_to_string(path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&content).ok()?;
    let ts = v["checked_at"].as_u64()?;
    let dt = chrono::DateTime::from_timestamp(ts as i64, 0)?;
    Some(dt.format("%Y-%m-%d %H:%M UTC").to_string())
}

/// Check if the user has ever configured `auto_update` (the key exists in config.toml).
#[must_use]
pub fn has_user_decided() -> bool {
    // Read the canonical config location (GH #408) — the hardcoded `~/.lean-ctx`
    // path missed configs stored under `~/.config/lean-ctx`, the default for
    // most installs, so the prompt could re-fire after the user had decided.
    let Some(config_path) = crate::core::config::Config::path() else {
        return false;
    };
    let content = std::fs::read_to_string(config_path).unwrap_or_default();
    content.contains("auto_update")
}

/// Writes the `[updates]` settings to config.toml, preserving all comments,
/// formatting, and unrelated keys.
pub fn set_auto_update(enabled: bool, notify_only: bool, interval_hours: u64) {
    let Some(config_path) = crate::core::config::Config::path() else {
        return;
    };
    if let Some(dir) = config_path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }

    let mut doc = crate::config_io::load_toml_document(&config_path);
    apply_auto_update(&mut doc, enabled, notify_only, interval_hours);
    let _ = crate::config_io::write_toml_document(&config_path, &doc);
}

/// Applies the `[updates]` settings onto a TOML document in place. Pure helper
/// so the merge behavior is unit-testable without touching the real home dir.
fn apply_auto_update(
    doc: &mut toml_edit::DocumentMut,
    enabled: bool,
    notify_only: bool,
    interval_hours: u64,
) {
    let updates = doc["updates"].or_insert(toml_edit::table());
    updates["auto_update"] = toml_edit::value(enabled);
    updates["check_interval_hours"] = toml_edit::value(interval_hours as i64);
    updates["notify_only"] = toml_edit::value(notify_only);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schedule_info_display_disabled() {
        let info = ScheduleInfo {
            enabled: false,
            mechanism: "none".into(),
            interval_hours: 0,
            scheduler_path: None,
            last_check: None,
        };
        assert!(info.to_string().contains("disabled"));
    }

    #[test]
    fn schedule_info_display_enabled() {
        let info = ScheduleInfo {
            enabled: true,
            mechanism: "LaunchAgent".into(),
            interval_hours: 6,
            scheduler_path: Some(PathBuf::from("/tmp/test.plist")),
            last_check: Some("2026-05-17 10:00 UTC".into()),
        };
        let s = info.to_string();
        assert!(s.contains("enabled"));
        assert!(s.contains("LaunchAgent"));
        assert!(s.contains("6h"));
    }

    #[test]
    fn apply_auto_update_preserves_existing_keys_and_comments() {
        let mut doc = "\
# important user comment
buddy_enabled = true
"
        .parse::<toml_edit::DocumentMut>()
        .unwrap();

        apply_auto_update(&mut doc, true, false, 12);

        let result = doc.to_string();
        assert!(result.contains("auto_update = true"));
        assert!(result.contains("check_interval_hours = 12"));
        assert!(result.contains("notify_only = false"));
        // Existing key + comment survive.
        assert!(result.contains("buddy_enabled = true"));
        assert!(result.contains("# important user comment"));
    }

    #[test]
    fn apply_auto_update_overwrites_only_updates_section() {
        let mut doc = "\
[updates]
auto_update = false
check_interval_hours = 99
"
        .parse::<toml_edit::DocumentMut>()
        .unwrap();

        apply_auto_update(&mut doc, true, true, 6);

        let result = doc.to_string();
        assert!(result.contains("auto_update = true"));
        assert!(result.contains("check_interval_hours = 6"));
        assert!(result.contains("notify_only = true"));
        assert!(!result.contains("check_interval_hours = 99"));
    }

    #[test]
    fn has_user_decided_false_by_default() {
        // In test env, the config likely doesn't contain auto_update
        // This tests the function doesn't panic
        let _ = has_user_decided();
    }
}
