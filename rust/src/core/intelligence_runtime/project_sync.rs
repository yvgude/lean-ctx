// SPDX-License-Identifier: Apache-2.0
//! Schedule an existing private sync operation; no public sync implementation.
use super::{InstallError, Result, health, install, read_regular, sha256, trust_key};
use crate::core::{config::Config, pathutil};
use std::{path::Path, process::Stdio, time::Duration};

fn private_reason(stderr: &[u8]) -> &'static str {
    match std::str::from_utf8(stderr).unwrap_or_default().trim() {
        "checkpoint key unavailable" => "key_unavailable",
        "personal sync entitlement unavailable" => "license_unavailable",
        "checkpoint transport failed" => "transport_failed",
        "checkpoint continuation failed" => "continuation_failed",
        "checkpoint transport configuration unavailable"
        | "checkpoint transport requires bounded private regular files" => {
            "private_configuration_invalid"
        }
        _ => "invalid_response",
    }
}

pub(super) fn public_reason(value: &str) -> Option<&'static str> {
    [
        "key_unavailable",
        "license_unavailable",
        "transport_failed",
        "continuation_failed",
        "private_configuration_invalid",
        "deadline_exceeded",
        "invalid_response",
        "host_admission_failed",
    ]
    .into_iter()
    .find(|reason| *reason == value)
}

fn selected_home() -> Result<std::ffi::OsString> {
    #[cfg(windows)]
    let home = std::env::var_os("HOME")
        .or_else(|| dirs::home_dir().map(std::path::PathBuf::into_os_string));
    #[cfg(not(windows))]
    let home = std::env::var_os("HOME");
    home.ok_or(InstallError::Configuration)
}

pub(super) fn enable(project: &Path) -> Result<serde_json::Value> {
    setup(Some(project), "--setup-sync-project", None)
}

pub(super) fn invite(output: &Path) -> Result<serde_json::Value> {
    setup(None, "--pair-device-begin", Some(output.as_os_str()))
}

pub(super) fn approve(project: &Path, code: &Path) -> Result<serde_json::Value> {
    setup(
        Some(project),
        "--approve-sync-project",
        Some(code.as_os_str()),
    )
}

pub(super) fn join(project: &Path, invitation: &str) -> Result<serde_json::Value> {
    if !public_invitation(invitation) {
        return Err(InstallError::Configuration);
    }
    setup(
        Some(project),
        "--join-sync-project",
        Some(invitation.as_ref()),
    )
}

fn public_invitation(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn setup(
    project: Option<&Path>,
    operation: &str,
    extra: Option<&std::ffi::OsStr>,
) -> Result<serde_json::Value> {
    if operation != "--join-sync-project"
        && extra.is_some_and(|path| !Path::new(path).is_absolute())
    {
        return Err(InstallError::Configuration);
    }
    let project = project
        .map(|project| {
            if !project.is_absolute() {
                return Err(InstallError::SyncCycle("host_admission_failed"));
            }
            let project = pathutil::canonicalize_secure(project)
                .map_err(|_| InstallError::SyncCycle("host_admission_failed"))?;
            let project_metadata = std::fs::symlink_metadata(&project)
                .map_err(|_| InstallError::SyncCycle("host_admission_failed"))?;
            if pathutil::is_symlink_or_reparse(&project_metadata)
                || !project_metadata.is_dir()
                || pathutil::is_broad_or_unsafe_root(&project)
            {
                return Err(InstallError::SyncCycle("host_admission_failed"));
            }
            Ok::<_, InstallError>(project)
        })
        .transpose()?;

    let observed = Config::try_load_global()
        .map_err(|_| InstallError::Configuration)?
        .intelligence_runtime;
    let configuration = Path::new(&observed.license_configuration);
    if !(observed.enabled
        && observed.accept_proprietary
        && super::bootstrap::permits_config(&observed)
        && configuration.is_absolute())
    {
        return Err(InstallError::Configuration);
    }
    let original = read_regular(configuration, 16 * 1024)?;
    let package = install::verified_active(
        Path::new(&observed.root),
        &observed.manifest_sha256,
        &trust_key(&observed.trust_key_hex)?,
    )?;
    if package.description["entitlement_required"] != true {
        return Err(InstallError::Policy);
    }
    let (temporary, mut command) = health::prepare(&package, Path::new(&observed.root))?;

    let home = selected_home()?;
    let data = crate::core::paths::data_dir_read_only().map_err(|_| InstallError::Configuration)?;
    let config =
        crate::core::paths::config_dir_read_only().map_err(|_| InstallError::Configuration)?;
    let engine = std::env::current_exe().map_err(|_| InstallError::Exchange)?;
    let engine = pathutil::canonicalize_secure(&engine).map_err(|_| InstallError::Exchange)?;
    let engine_digest = sha256(&read_regular(&engine, 512 * 1024 * 1024)?);

    let license_parent = configuration
        .parent()
        .filter(|parent| parent.is_absolute())
        .ok_or(InstallError::Configuration)?;
    let directory = project
        .as_ref()
        .map(|project| {
            let text = project.to_str().ok_or(InstallError::Configuration)?;
            Ok::<_, InstallError>(
                license_parent.join(format!("sync-project-{}", sha256(text.as_bytes()))),
            )
        })
        .transpose()?;
    let sync_configuration = directory
        .as_ref()
        .map(|directory| directory.join("sync.json"));

    command
        .env("HOME", home)
        .env("LEAN_CTX_DATA_DIR", &data)
        .env("LEAN_CTX_CONFIG_DIR", config)
        .env("DO_NOT_TRACK", "1")
        .env("__LEAN_CTX_NO_DAEMON", "1")
        .env("LEANCTX_INTELLIGENCE_LICENSE_CONFIG", configuration)
        .arg(operation)
        .stdin(Stdio::null());
    if let (Some(directory), Some(project)) = (&directory, &project) {
        command
            .arg(directory)
            .arg(project)
            .arg(&engine)
            .arg(engine_digest);
    }
    if let Some(extra) = extra {
        command.arg(extra);
    }

    let captured = crate::core::process_capture::run_with_output_limits(
        &mut command,
        Some(Duration::from_mins(1)),
        4096,
        4096,
    )
    .map_err(|_| InstallError::Exchange)?;
    if captured.timed_out || captured.cancelled {
        return Err(InstallError::SyncCycle("deadline_exceeded"));
    }
    if !captured.output.status.success() || !captured.output.stderr.is_empty() {
        return Err(InstallError::SyncCycle(private_reason(
            &captured.output.stderr,
        )));
    }
    let reply: serde_json::Value =
        serde_json::from_slice(&captured.output.stdout).map_err(|_| InstallError::Exchange)?;
    let reply = reply.as_object().ok_or(InstallError::Exchange)?;
    let valid = reply.len() == 2
        && match operation {
            "--pair-device-begin" => {
                reply
                    .get("invitation_id")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(public_invitation)
                    && reply
                        .get("expires_at")
                        .and_then(serde_json::Value::as_i64)
                        .is_some_and(|n| n > 0)
            }
            "--approve-sync-project" => {
                reply
                    .get("schema_version")
                    .and_then(serde_json::Value::as_str)
                    == Some("leanctx.personal-sync-approval/v1")
                    && reply.get("approved").and_then(serde_json::Value::as_bool) == Some(true)
            }
            _ => {
                reply
                    .get("schema_version")
                    .and_then(serde_json::Value::as_str)
                    == Some("leanctx.personal-sync-setup/v1")
                    && reply
                        .get("sync_configuration")
                        .and_then(serde_json::Value::as_str)
                        == sync_configuration.as_ref().and_then(|path| path.to_str())
            }
        };
    if !valid {
        return Err(InstallError::Exchange);
    }
    temporary.close()?;
    if read_regular(configuration, 16 * 1024)? != original
        || Config::try_load_global()
            .map_err(|_| InstallError::Configuration)?
            .intelligence_runtime
            != observed
    {
        return Err(InstallError::Configuration);
    }
    if operation == "--pair-device-begin" {
        let mut result = serde_json::Value::Object(reply.clone());
        result["next_step"] = serde_json::json!(
            "Transfer the private code file through an authenticated confidential channel to your established device, approve its selected project, then join here using this invitation ID. Remove unneeded code copies."
        );
        return Ok(result);
    }
    if operation == "--approve-sync-project" {
        return Ok(serde_json::Value::Object(reply.clone()));
    }
    let sync_configuration = sync_configuration.ok_or(InstallError::Configuration)?;
    read_regular(&sync_configuration, 16 * 1024)?;
    // A receiving device can join before its first local Engine task. Use the
    // existing consented, owner-checked creation path, never permission repair
    // or a fabricated session merely to satisfy the service's directory check.
    install::prepare_parent_chain(&data)?;
    let mut result = super::renewal_service::run_with_mode(
        "sync-service-install",
        Some(&sync_configuration),
        observed.staging,
    )?;
    result["sync_configuration"] = serde_json::json!(sync_configuration);
    Ok(result)
}

pub(super) fn cycle_for_mode(
    sync_configuration: &Path,
    staging: bool,
) -> Result<serde_json::Value> {
    cycle_with_mode(sync_configuration, Some(staging))
}

fn cycle_with_mode(
    sync_configuration: &Path,
    expected_staging: Option<bool>,
) -> Result<serde_json::Value> {
    let observed = Config::try_load_global()
        .map_err(|_| InstallError::Configuration)?
        .intelligence_runtime;
    let configuration = Path::new(&observed.license_configuration);
    if !(observed.enabled
        && observed.accept_proprietary
        && super::bootstrap::permits_config(&observed)
        && expected_staging.is_none_or(|staging| observed.staging == staging)
        && configuration.is_absolute()
        && sync_configuration.is_absolute())
    {
        return Err(InstallError::Configuration);
    }
    let original = read_regular(configuration, 16 * 1024)?;
    let sync_original = read_regular(sync_configuration, 16 * 1024)?;
    let package = install::verified_active(
        Path::new(&observed.root),
        &observed.manifest_sha256,
        &trust_key(&observed.trust_key_hex)?,
    )?;
    if package.description["entitlement_required"] != true {
        return Err(InstallError::Policy);
    }
    let (temporary, mut command) = health::prepare(&package, Path::new(&observed.root))?;
    // Keep the selected user's Engine storage; health's disposable HOME would
    // otherwise make each scheduled tick read a different context store.
    let home = selected_home()?;
    let data = crate::core::paths::data_dir_read_only().map_err(|_| InstallError::Configuration)?;
    let config =
        crate::core::paths::config_dir_read_only().map_err(|_| InstallError::Configuration)?;
    command
        .env("HOME", home)
        .env("LEAN_CTX_DATA_DIR", data)
        .env("LEAN_CTX_CONFIG_DIR", config)
        .env("DO_NOT_TRACK", "1")
        .env("__LEAN_CTX_NO_DAEMON", "1");
    command
        .arg("--sync-once")
        .arg(sync_configuration)
        .env("LEANCTX_INTELLIGENCE_LICENSE_CONFIG", configuration)
        .stdin(Stdio::null());
    // The private implementation owns current entitlement, keys, TLS, policies,
    // Engine continuation and durable CAS. Drain this bounded cycle on SIGTERM.
    let captured = crate::core::process_capture::run_with_output_limits(
        &mut command,
        Some(Duration::from_mins(4)),
        4096,
        4096,
    )
    .map_err(|_| InstallError::Exchange)?;
    if captured.timed_out || captured.cancelled {
        return Err(InstallError::SyncCycle("deadline_exceeded"));
    }
    if !captured.output.status.success() || !captured.output.stderr.is_empty() {
        // Exact fixed private categories only; arbitrary stderr never crosses
        // this boundary, including values appended to a recognized message.
        let reason = private_reason(&captured.output.stderr);
        return Err(InstallError::SyncCycle(reason));
    }
    let reply: serde_json::Value =
        serde_json::from_slice(&captured.output.stdout).map_err(|_| InstallError::Exchange)?;
    let status = reply["status"]
        .as_str()
        .filter(|value| {
            matches!(
                *value,
                "empty" | "unchanged" | "conflict" | "acknowledged" | "uploaded" | "downloaded"
            )
        })
        .ok_or(InstallError::Exchange)?;
    if reply["schema_version"] != "leanctx.personal-sync-status/v1"
        || reply["conflict"] != (status == "conflict")
    {
        return Err(InstallError::Exchange);
    }
    temporary.close()?;
    if read_regular(configuration, 16 * 1024)? != original
        || read_regular(sync_configuration, 16 * 1024)? != sync_original
        || Config::try_load_global()
            .map_err(|_| InstallError::Configuration)?
            .intelligence_runtime
            != observed
    {
        return Err(InstallError::Configuration);
    }
    // Return only the fixed metadata contract, never arbitrary child diagnostics.
    Ok(
        serde_json::json!({"schema_version":"leanctx.personal-sync-status/v1","status":status,"conflict":status == "conflict"}),
    )
}
