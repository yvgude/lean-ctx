// SPDX-License-Identifier: Apache-2.0
use crate::core::intelligence_runtime::{InstallError, Result};
use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

pub(super) fn uid() -> u32 {
    // SAFETY: geteuid has no arguments and only reads this process identity.
    unsafe { libc::geteuid() }
}

fn command(args: &[&str]) -> Result<std::process::Output> {
    command_with_timeout(args, Duration::from_secs(10))
}

fn command_with_timeout(args: &[&str], timeout: Duration) -> Result<std::process::Output> {
    #[cfg(target_os = "macos")]
    let mut cmd = Command::new("/bin/launchctl");
    #[cfg(target_os = "linux")]
    let mut cmd = Command::new("/usr/bin/systemctl");
    cmd.env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LC_ALL", "C")
        .args(args)
        .stdin(Stdio::null());
    #[cfg(target_os = "linux")]
    cmd.env("XDG_RUNTIME_DIR", format!("/run/user/{}", uid()));
    let result = crate::core::process_capture::run_with_output_limits(
        &mut cmd,
        Some(timeout),
        32 * 1024,
        4 * 1024,
    )
    .map_err(|_| InstallError::RenewalService)?;
    if result.timed_out || result.cancelled {
        return Err(InstallError::RenewalService);
    }
    Ok(result.output)
}

fn checked(args: &[&str]) -> Result<()> {
    if !command(args)?.status.success() {
        return Err(InstallError::RenewalService);
    }
    Ok(())
}

#[cfg(target_os = "macos")]
pub(super) fn status(label: &str) -> Result<serde_json::Value> {
    let output = command(&["print", &format!("gui/{}/{label}", uid())])?;
    if !output.status.success() {
        let diagnostic = std::str::from_utf8(&output.stderr).unwrap_or_default();
        if output.status.code() == Some(113)
            && diagnostic.contains(&format!("Could not find service \"{label}\""))
            && diagnostic.contains(&format!("domain for user gui: {}", uid()))
        {
            return Ok(serde_json::json!({"registered":false}));
        }
        return Err(InstallError::RenewalService);
    }
    let text = std::str::from_utf8(&output.stdout).map_err(|_| InstallError::RenewalService)?;
    // Only first top-level fields; nested coalition state is not job state.
    let field = |name: &str| {
        text.lines().find_map(|line| {
            line.strip_prefix('\t')
                .filter(|line| !line.starts_with('\t'))
                .and_then(|line| line.strip_prefix(name))
                .and_then(|line| line.strip_prefix(" = "))
                .map(str::trim)
        })
    };
    let numeric = |name| field(name).and_then(|value| value.parse::<i64>().ok());
    let state = field("state")
        .filter(|value| matches!(*value, "running" | "not running" | "waiting" | "exited"));
    Ok(
        serde_json::json!({"registered":true,"state":state,"runs":numeric("runs"),"pid":numeric("pid"),"last_exit_code":numeric("last exit code")}),
    )
}

#[cfg(target_os = "linux")]
pub(super) fn status(label: &str) -> Result<serde_json::Value> {
    let output = command(&["--user", "is-active", &format!("{label}.timer")])?;
    if !output.status.success() && !matches!(output.status.code(), Some(3 | 4)) {
        return Err(InstallError::RenewalService);
    }
    Ok(serde_json::json!({"registered":output.status.success()}))
}

pub(super) fn install(label: &str, units: &[(PathBuf, Vec<u8>)]) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        checked(&["enable", &format!("gui/{}/{label}", uid())])?;
        if status(label)?["registered"] != true {
            checked(&[
                "bootstrap",
                &format!("gui/{}", uid()),
                units[0].0.to_str().ok_or(InstallError::RenewalService)?,
            ])?;
        }
    }
    #[cfg(target_os = "linux")]
    {
        let _ = units;
        reload()?;
        checked(&["--user", "enable", "--now", &format!("{label}.timer")])?;
    }
    if status(label)?["registered"] != true {
        return Err(InstallError::RenewalService);
    }
    Ok(())
}

pub(super) fn remove(label: &str, sync: bool) -> Result<()> {
    #[cfg(target_os = "macos")]
    let previous_pid = status(label)?["pid"]
        .as_i64()
        .filter(|pid| *pid > 1 && *pid <= i64::from(i32::MAX));
    #[cfg(target_os = "macos")]
    if status(label)?["registered"] == true {
        // Registration may remain briefly while the host drains its child.
        // Only subsequent confirmed absence authorizes deletion of ownership.
        let _ = command(&["bootout", &format!("gui/{}/{label}", uid())]);
    }
    #[cfg(target_os = "linux")]
    {
        let disabled = command(&["--user", "disable", "--now", &format!("{label}.timer")])?;
        if !disabled.status.success() && !unloaded_and_idle(label, false)? {
            return Err(InstallError::RenewalService);
        }
        // The synchronous stop must outlive the unit's bounded child drain.
        let stopped = command_with_timeout(
            &["--user", "stop", &format!("{label}.service")],
            Duration::from_secs(if sync { 310 } else { 80 }),
        )?;
        if !stopped.status.success() && !unloaded_and_idle(label, true)? {
            return Err(InstallError::RenewalService);
        }
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(if sync { 310 } else { 10 });
    loop {
        #[cfg(target_os = "macos")]
        let drained = previous_pid.is_none_or(|pid| {
            // SAFETY: signal0 only queries existence; never signals another job.
            (unsafe { libc::kill(pid as i32, 0) }) != 0
                && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
        });
        #[cfg(target_os = "linux")]
        let drained = true; // successful synchronous systemctl stop above
        if drained && status(label).is_ok_and(|value| value["registered"] == false) {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            return Err(InstallError::RenewalService);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// systemd refuses stop for an invalid legacy unit or an absent interrupted
/// publication. Only explicit unloaded, job-free, process-free state is safe.
#[cfg(target_os = "linux")]
fn unloaded_and_idle(label: &str, service: bool) -> Result<bool> {
    let suffix = if service { "service" } else { "timer" };
    let output = command(&[
        "--user",
        "show",
        &format!("{label}.{suffix}"),
        "-p",
        "LoadState",
        "-p",
        "ActiveState",
        "-p",
        "SubState",
        "-p",
        "MainPID",
        "-p",
        "ControlPID",
        "-p",
        "Job",
    ])?;
    if !output.status.success() {
        return Ok(false);
    }
    let text = std::str::from_utf8(&output.stdout).map_err(|_| InstallError::RenewalService)?;
    let field = |name: &str| {
        text.lines().find_map(|line| {
            let (key, value) = line.split_once('=')?;
            (key == name).then_some(value)
        })
    };
    Ok((field("LoadState") == Some("not-found")
        || service && field("LoadState") == Some("bad-setting"))
        && field("ActiveState") == Some("inactive")
        && field("SubState") == Some("dead")
        && field("Job") == Some("")
        && (!service || field("MainPID") == Some("0") && field("ControlPID") == Some("0")))
}

pub(super) fn reload() -> Result<()> {
    #[cfg(target_os = "linux")]
    checked(&["--user", "daemon-reload"])?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn escaped(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(target_os = "macos")]
pub(super) fn units_with_mode(
    label: &str,
    values: &[&str; 3],
    home: &Path,
    sync: Option<&str>,
    data: Option<&str>,
    staging: bool,
) -> Vec<(PathBuf, Vec<u8>)> {
    let [host, home_text, config] = values.map(escaped);
    let data_env = data
        .map(|path| {
            format!(
                "<key>LEAN_CTX_DATA_DIR</key><string>{}</string>",
                escaped(path)
            )
        })
        .unwrap_or_default();
    let operation = if sync.is_some() {
        "sync-service-tick"
    } else {
        "renewal-service-tick"
    };
    let extra = sync
        .map(|path| {
            format!(
                "<string>--sync-configuration</string><string>{}</string>",
                escaped(path)
            )
        })
        .unwrap_or_default();
    let staging_arg = if staging {
        "<string>--staging</string>"
    } else {
        ""
    };
    let exit_timeout = if sync.is_some() { 300 } else { 70 };
    let body = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>{label}</string>
<key>ProgramArguments</key><array><string>{host}</string><string>engine</string><string>runtime</string><string>{operation}</string>{staging_arg}<string>--accept-proprietary</string>{extra}</array>
<key>EnvironmentVariables</key><dict><key>HOME</key><string>{home_text}</string><key>LEAN_CTX_CONFIG_DIR</key><string>{config}</string><key>DO_NOT_TRACK</key><string>1</string>{data_env}</dict>
<key>WorkingDirectory</key><string>{config}</string>
<key>RunAtLoad</key><true/><key>StartInterval</key><integer>60</integer>
<key>ThrottleInterval</key><integer>10</integer><key>ExitTimeOut</key><integer>{exit_timeout}</integer>
<key>ProcessType</key><string>Background</string><key>Umask</key><integer>63</integer>
</dict></plist>
"#
    );
    vec![(
        home.join("Library/LaunchAgents")
            .join(format!("{label}.plist")),
        body.into_bytes(),
    )]
}

#[cfg(target_os = "linux")]
fn quoted(value: &str) -> String {
    format!(
        "\"{}\"",
        value
            .replace('%', "%%")
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
    )
}

/// The sole pre-17f78 rendering difference: WorkingDirectory was quoted as an
/// ExecStart argument. Match whole bytes, never parse or normalize foreign units.
pub(super) fn legacy_unit(expected: &[u8], config: &Path) -> Option<Vec<u8>> {
    #[cfg(target_os = "linux")]
    {
        let config = config.to_str()?;
        let expected = std::str::from_utf8(expected).ok()?;
        let current = format!("\nWorkingDirectory={}/.\n", config.replace('%', "%%"));
        if !expected.contains(&current) {
            return None; // timers have no legacy variant
        }
        Some(
            expected
                .replacen(
                    &current,
                    &format!("\nWorkingDirectory={}\n", quoted(config)),
                    1,
                )
                .into_bytes(),
        )
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (expected, config);
        None
    }
}

#[cfg(target_os = "linux")]
pub(super) fn units_with_mode(
    label: &str,
    values: &[&str; 3],
    home: &Path,
    sync: Option<&str>,
    data: Option<&str>,
    staging: bool,
) -> Vec<(PathBuf, Vec<u8>)> {
    let [host, home_text, config] = *values;
    let data_env = data
        .map(|path| format!(" {}", quoted(&format!("LEAN_CTX_DATA_DIR={path}"))))
        .unwrap_or_default();
    let operation = if sync.is_some() {
        "sync-service-tick"
    } else {
        "renewal-service-tick"
    };
    let extra = sync
        .map(|path| format!(" --sync-configuration {}", quoted(path)))
        .unwrap_or_default();
    let staging_arg = if staging { " --staging" } else { "" };
    let (start_timeout, stop_timeout) = if sync.is_some() { (300, 300) } else { (75, 70) };
    let directory = home.join(".config/systemd/user");
    // WorkingDirectory is one literal path, not an ExecStart word list.
    // A trailing /. preserves final spaces/backslashes through INI parsing.
    let working_directory = format!("{}/.", config.replace('%', "%%"));
    let service = format!(
        "[Unit]\nDescription=LeanCTX verified runtime renewal\n[Service]\nType=oneshot\nExecStart=:{} engine runtime {operation}{staging_arg} --accept-proprietary{extra}\nEnvironment={} {} DO_NOT_TRACK=1{data_env}\nWorkingDirectory={}\nTimeoutStartSec={start_timeout}\nTimeoutStopSec={stop_timeout}\nKillMode=mixed\nUMask=0077\nNoNewPrivileges=yes\n",
        quoted(host),
        quoted(&format!("HOME={home_text}")),
        quoted(&format!("LEAN_CTX_CONFIG_DIR={config}")),
        working_directory
    );
    let timer = format!(
        "[Unit]\nDescription=LeanCTX runtime renewal schedule\n[Timer]\nOnStartupSec=1\nOnUnitInactiveSec=60\nAccuracySec=1\nUnit={label}.service\n[Install]\nWantedBy=timers.target\n"
    );
    vec![
        (
            directory.join(format!("{label}.service")),
            service.into_bytes(),
        ),
        (directory.join(format!("{label}.timer")), timer.into_bytes()),
    ]
}
