// SPDX-License-Identifier: Apache-2.0
//! Explicit handoff to the installed private verifier; no public license logic.

use std::path::Path;
#[cfg(any(unix, windows))]
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use crate::core::config::Config;

use super::{InstallError, Result, health, install, read_regular, trust_key};

/// One due-time check using the saved binding, suitable for a user scheduler.
/// The private verifier decides whether a signed lease actually needs renewal.
pub(super) fn renew() -> Result<serde_json::Value> {
    renew_cancellable(|| false)
}

pub(super) fn renew_cancellable(cancelled: impl FnMut() -> bool) -> Result<serde_json::Value> {
    renew_with_mode(None, cancelled)
}

#[cfg(any(target_os = "macos", target_os = "linux", windows))]
pub(super) fn renew_cancellable_for_mode(
    staging: bool,
    cancelled: impl FnMut() -> bool,
) -> Result<serde_json::Value> {
    renew_with_mode(Some(staging), cancelled)
}

fn renew_with_mode(
    expected_staging: Option<bool>,
    cancelled: impl FnMut() -> bool,
) -> Result<serde_json::Value> {
    let observed = Config::try_load_global()
        .map_err(|_| InstallError::Configuration)?
        .intelligence_runtime;
    let configuration = Path::new(&observed.license_configuration);
    if !(observed.enabled
        && observed.accept_proprietary
        && super::bootstrap::permits_config(&observed)
        && expected_staging.is_none_or(|staging| observed.staging == staging)
        && configuration.is_absolute())
    {
        return Err(InstallError::Configuration);
    }
    // No inherited environment fallback: this command uses only the binding
    // saved after provisioning, not authority chosen by a caller or project.
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
    command
        .arg("--renew-license")
        .env("LEANCTX_INTELLIGENCE_LICENSE_CONFIG", configuration)
        .stdin(Stdio::null());
    let captured = crate::core::process_capture::run_with_output_limits_cancellable(
        &mut command,
        Duration::from_mins(1),
        1_024,
        4_096,
        cancelled,
    )
    .map_err(|_| InstallError::Renewal)?;
    if captured.timed_out
        || captured.cancelled
        || !captured.output.status.success()
        || !captured.output.stderr.is_empty()
        || !captured.output.stdout.is_empty()
    {
        return Err(InstallError::Renewal);
    }
    temporary.close().map_err(|_| InstallError::Renewal)?;
    if read_regular(configuration, 16 * 1024).map_err(|_| InstallError::Renewal)? != original
        || Config::try_load_global()
            .map_err(|_| InstallError::Renewal)?
            .intelligence_runtime
            != observed
    {
        return Err(InstallError::Renewal);
    }
    Ok(
        serde_json::json!({"status": "renewal_checked", "service_running": false,
        "release_approved": false}),
    )
}

pub(super) fn provision(configuration: &Path, enrollment: &Path) -> Result<serde_json::Value> {
    if !configuration.is_absolute() || !enrollment.is_absolute() {
        return Err(InstallError::Usage);
    }
    let configuration_text = configuration.to_str().ok_or(InstallError::Configuration)?;
    let observed = Config::try_load_global()
        .map_err(|_| InstallError::Configuration)?
        .intelligence_runtime;
    if !(observed.enabled
        && observed.accept_proprietary
        && super::bootstrap::permits_config(&observed))
        || (!observed.license_configuration.is_empty()
            && observed.license_configuration != configuration_text)
    {
        return Err(InstallError::Configuration);
    }
    // The two files have different authority: the explicit operator config
    // selects issuer/account/device. Enrollment never provides that selection.
    let original_configuration = read_regular(configuration, 16 * 1024)?;
    let package = install::verified_active(
        Path::new(&observed.root),
        &observed.manifest_sha256,
        &trust_key(&observed.trust_key_hex)?,
    )?;
    if package.description["entitlement_required"] != true {
        return Err(InstallError::Policy);
    }
    let (temporary, mut command) = health::prepare(&package, Path::new(&observed.root))?;
    command
        .arg("--provision-device")
        .arg(enrollment)
        .env("LEANCTX_INTELLIGENCE_LICENSE_CONFIG", configuration)
        .stdin(Stdio::null());
    // The private core owns bounded input, permissions, TLS, signed grants,
    // revision floor and no-overwrite publication. Never echo child diagnostics.
    let captured = crate::core::process_capture::run_with_output_limits(
        &mut command,
        Some(Duration::from_mins(1)),
        1_024,
        4_096,
    )
    .map_err(|_| InstallError::Provisioning)?;
    if captured.timed_out
        || captured.cancelled
        || !captured.output.status.success()
        || !captured.output.stderr.is_empty()
        || captured.output.stdout
            != b"Commercial device provisioned; configured maintenance may now start.\n"
    {
        return Err(InstallError::Provisioning);
    }
    temporary
        .close()
        .map_err(|_| InstallError::ProvisionedConfiguration)?;
    if read_regular(configuration, 16 * 1024).map_err(|_| InstallError::ProvisionedConfiguration)?
        != original_configuration
    {
        return Err(InstallError::ProvisionedConfiguration);
    }
    Config::try_update_global(|config| {
        if config.intelligence_runtime != observed {
            return Err(crate::core::error::LeanCtxError::Config(
                "runtime provisioning configuration changed".into(),
            ));
        }
        configuration_text.clone_into(&mut config.intelligence_runtime.license_configuration);
        Ok(())
    })
    .map_err(|_| InstallError::ProvisionedConfiguration)?;
    Ok(
        serde_json::json!({"status": "device_provisioned", "configuration_saved": true,
        "service_running": false, "release_approved": false}),
    )
}

/// A saved binding applies only to the same explicit global package selection.
/// Direct operator invocations retain their independently provided environment.
#[cfg(any(unix, windows))]
pub(super) fn configuration_path(root: &Path, selected: &str, key: &[u8; 32]) -> Result<PathBuf> {
    // Explicit pinned invocations historically work without a global file.
    // A broken global file cannot provide saved authority; only an independent
    // explicit environment binding may then be used by this operator path.
    let config = Config::try_load_global()
        .map(|config| config.intelligence_runtime)
        .unwrap_or_default();
    let saved = config.enabled
        && config.accept_proprietary
        && super::bootstrap::permits_config(&config)
        && Path::new(&config.root) == root
        && config.manifest_sha256 == selected
        && trust_key(&config.trust_key_hex).is_ok_and(|configured| configured == *key)
        && !config.license_configuration.is_empty();
    let path = if saved {
        PathBuf::from(config.license_configuration)
    } else {
        std::env::var_os("LEANCTX_INTELLIGENCE_LICENSE_CONFIG")
            .map(PathBuf::from)
            .ok_or(InstallError::Configuration)?
    };
    if !path.is_absolute() {
        return Err(InstallError::Configuration);
    }
    read_regular(&path, 16 * 1024)?;
    Ok(path)
}
