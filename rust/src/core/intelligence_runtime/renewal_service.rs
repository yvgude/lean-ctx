// SPDX-License-Identifier: Apache-2.0
//! Explicit user scheduling of the existing verified, saved-binding renewal command.
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod manager;
#[cfg(windows)]
mod manager_windows;
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod native;
#[cfg(windows)]
mod native_windows;
#[cfg(windows)]
mod principal_windows;
#[cfg(any(target_os = "macos", target_os = "linux", windows))]
mod specification;

#[cfg(windows)]
pub(super) fn run_saved(binding: &std::path::Path) -> super::Result<serde_json::Value> {
    native_windows::run_saved(binding)
}

pub(super) fn run_with_mode(
    operation: &str,
    sync_configuration: Option<&std::path::Path>,
    staging: bool,
) -> super::Result<serde_json::Value> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        native::run(operation, sync_configuration, staging)
    }
    #[cfg(windows)]
    {
        native_windows::run(operation, sync_configuration, staging)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    {
        let _ = (operation, sync_configuration, staging);
        Err(super::InstallError::RenewalService)
    }
}
