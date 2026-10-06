// SPDX-License-Identifier: Apache-2.0
//! Bounded Windows Task Scheduler adapter. All scheduler data crosses the
//! process boundary as JSON in an environment variable, never as script text.
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::Serialize;
use std::{
    ffi::OsString,
    io::Write,
    os::windows::ffi::OsStringExt,
    path::PathBuf,
    process::Command,
    time::{Duration, Instant},
};
use windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW;

use crate::core::intelligence_runtime::{InstallError, Result};

const POWERSHELL_TIMEOUT: Duration = Duration::from_secs(10);
const STDOUT_LIMIT: usize = 8 * 1024;
const STDERR_LIMIT: usize = 1024;
const MAX_REQUEST_UTF16_UNITS: usize = 30_000;
const POLL_INTERVAL: Duration = Duration::from_millis(250);
const SCRIPT: &str = include_str!("scheduler_windows.ps1");
const BOOTSTRAP: &str = "$script = [Console]::In.ReadToEnd(); Invoke-Expression -Command $script";

#[derive(Serialize)]
pub(super) struct Task {
    pub(super) label: String,
    pub(super) sid: String,
    pub(super) host: String,
    pub(super) arguments: String,
    pub(super) working_directory: String,
    pub(super) start_boundary: String,
}

#[derive(Serialize)]
struct Request<'a> {
    action: &'a str,
    label: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    task: Option<&'a Task>,
}

pub(super) fn exists(label: &str) -> Result<bool> {
    validate_label(label)?;
    let response = invoke("exists", label, None, POWERSHELL_TIMEOUT)?;
    response_bool(&response, "exists")
}

pub(super) fn status(task: &Task) -> Result<serde_json::Value> {
    validate_task(task)?;
    let response = invoke("status", &task.label, Some(task), POWERSHELL_TIMEOUT)?;
    if response_bool(&response, "registered")? {
        response_bool(&response, "enabled")?;
    }
    Ok(response)
}

fn response_bool(value: &serde_json::Value, key: &str) -> Result<bool> {
    value
        .get(key)
        .and_then(serde_json::Value::as_bool)
        .ok_or(InstallError::RenewalService)
}

pub(super) fn install(task: &Task) -> Result<serde_json::Value> {
    validate_task(task)?;
    let response = invoke("install", &task.label, Some(task), POWERSHELL_TIMEOUT)?;
    if !response_bool(&response, "registered")? || !response_bool(&response, "enabled")? {
        return Err(InstallError::RenewalService);
    }
    Ok(response)
}

pub(super) fn remove(task: &Task, sync: bool) -> Result<()> {
    validate_task(task)?;
    let disabled = invoke(
        "remove-disable",
        &task.label,
        Some(task),
        POWERSHELL_TIMEOUT,
    )?;
    if !response_bool(&disabled, "registered")? {
        return Ok(());
    }
    if !response_bool(&disabled, "disabled")? {
        return Err(InstallError::RenewalService);
    }

    let drain_timeout = Duration::from_secs(if sync { 310 } else { 90 });
    let deadline = Instant::now() + drain_timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(InstallError::RenewalService);
        }
        let call_timeout = POWERSHELL_TIMEOUT.min(remaining);
        let state = invoke("remove-state", &task.label, Some(task), call_timeout)?;
        if !response_bool(&state, "registered")? {
            return Ok(());
        }
        if response_bool(&state, "drained")? {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(InstallError::RenewalService);
            }
            let deleted = invoke(
                "remove-delete",
                &task.label,
                Some(task),
                POWERSHELL_TIMEOUT.min(remaining),
            )?;
            if !response_bool(&deleted, "registered")? || response_bool(&deleted, "deleted")? {
                return Ok(());
            }
            if response_bool(&deleted, "drained")? {
                return Err(InstallError::RenewalService);
            }
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(InstallError::RenewalService);
        }
        std::thread::sleep(POLL_INTERVAL.min(remaining));
    }
}

fn validate_label(label: &str) -> Result<()> {
    let valid = !label.is_empty()
        && label.len() <= 128
        && label != "."
        && label != ".."
        && label
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'));
    if valid {
        Ok(())
    } else {
        Err(InstallError::RenewalService)
    }
}

fn validate_task(task: &Task) -> Result<()> {
    validate_label(&task.label)?;
    let sid_parts: Vec<_> = task.sid.split('-').collect();
    if task.sid.len() > 184
        || sid_parts.len() < 4
        || sid_parts[0] != "S"
        || sid_parts[1] != "1"
        || sid_parts[2..]
            .iter()
            .any(|part| part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()))
        || !bounded_text(&task.host, 32_767, false)
        || !bounded_text(&task.arguments, 32_767, true)
        || !bounded_text(&task.working_directory, 32_767, false)
        || !bounded_text(&task.start_boundary, 128, false)
        || task.host.contains('%')
        || task.arguments.contains('%')
        || task.working_directory.contains('%')
    {
        return Err(InstallError::RenewalService);
    }
    let payload = serde_json::to_string(&Request {
        action: "validate",
        label: &task.label,
        task: Some(task),
    })
    .map_err(|_| InstallError::RenewalService)?;
    if payload.encode_utf16().count() > MAX_REQUEST_UTF16_UNITS {
        return Err(InstallError::RenewalService);
    }
    Ok(())
}

fn bounded_text(value: &str, max_utf16_units: usize, allow_empty: bool) -> bool {
    (allow_empty || !value.is_empty())
        && !value.contains('\0')
        && value.encode_utf16().count() <= max_utf16_units
}

fn invoke(
    action: &str,
    label: &str,
    task: Option<&Task>,
    timeout: Duration,
) -> Result<serde_json::Value> {
    let request = serde_json::to_string(&Request {
        action,
        label,
        task,
    })
    .map_err(|_| InstallError::RenewalService)?;
    if request.encode_utf16().count() > MAX_REQUEST_UTF16_UNITS {
        return Err(InstallError::RenewalService);
    }

    let (powershell, system_root) = powershell_paths()?;
    let mut command = Command::new(powershell);
    let (reader, mut writer) = std::io::pipe().map_err(|_| InstallError::RenewalService)?;
    let writer_thread = std::thread::Builder::new()
        .name("leanctx-task-script".to_owned())
        .spawn(move || writer.write_all(SCRIPT.as_bytes()))
        .map_err(|_| InstallError::RenewalService)?;
    command
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-EncodedCommand",
        ])
        .arg(encode_utf16_base64(BOOTSTRAP))
        .env_clear()
        .env("SystemRoot", &system_root)
        .env("WINDIR", &system_root)
        .env("LEANCTX_TASK_REQUEST", request)
        .stdin(reader);

    let captured = crate::core::process_capture::run_with_output_limits(
        &mut command,
        Some(timeout),
        STDOUT_LIMIT,
        STDERR_LIMIT,
    );
    drop(command);
    writer_thread
        .join()
        .map_err(|_| InstallError::RenewalService)?
        .map_err(|_| InstallError::RenewalService)?;
    let captured = captured.map_err(|_| InstallError::RenewalService)?;
    if captured.timed_out
        || captured.cancelled
        || !captured.output.status.success()
        || !captured.output.stderr.is_empty()
    {
        return Err(InstallError::RenewalService);
    }
    serde_json::from_slice(&captured.output.stdout).map_err(|_| InstallError::RenewalService)
}

fn powershell_paths() -> Result<(PathBuf, OsString)> {
    let mut buffer = vec![0_u16; 32_768];
    // SAFETY: GetSystemDirectoryW writes at most buffer.len() UTF-16 units.
    let length = unsafe { GetSystemDirectoryW(buffer.as_mut_ptr(), buffer.len() as u32) } as usize;
    if length == 0 || length >= buffer.len() {
        return Err(InstallError::RenewalService);
    }
    let system_directory = PathBuf::from(OsString::from_wide(&buffer[..length]));
    let system_root = system_directory
        .parent()
        .ok_or(InstallError::RenewalService)?
        .as_os_str()
        .to_owned();
    let powershell = system_directory.join(r"WindowsPowerShell\v1.0\powershell.exe");
    Ok((powershell, system_root))
}

fn encode_utf16_base64(value: &str) -> String {
    let bytes: Vec<u8> = value.encode_utf16().flat_map(u16::to_le_bytes).collect();
    STANDARD.encode(bytes)
}
