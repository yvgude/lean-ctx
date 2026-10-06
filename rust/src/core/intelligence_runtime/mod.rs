// SPDX-License-Identifier: Apache-2.0
//! Public installation authority for the optional, separately licensed runtime.
//! No private implementation, account, entitlement, or implicit network access.

mod bootstrap;
mod catalog;
mod channel;
mod cli;
pub(crate) mod code_security;
mod configuration;
pub(crate) mod context_policy;
pub(crate) mod context_selection;
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod context_sharing;
pub(crate) mod memory_curation;
pub(crate) mod personal_protection;
pub(crate) mod semantic_detectors;
pub(crate) use configuration::{DISCLOSURE, inspect_setup};
mod activation;
mod health;
mod install;
mod manifest;
#[cfg(any(target_os = "macos", target_os = "linux", windows))]
mod project_sync;
mod provisioning;
mod renewal_service;
mod rotation;
#[cfg(any(unix, windows))]
mod session;
#[cfg(windows)]
mod session_windows;

#[cfg(test)]
mod tests;

use std::io::Read;
use std::path::Path;

pub(crate) use cli::run;
use manifest::{VerifiedPackage, verify};

// The signed archive member stays platform-neutral; only materialized files vary.
const EXECUTABLE_NAME: &str = if cfg!(windows) {
    "leanctx-intelligence.exe"
} else {
    "leanctx-intelligence"
};

#[derive(Debug, thiserror::Error)]
pub(crate) enum InstallError {
    #[error("invalid runtime installer arguments")]
    Usage,
    #[error("runtime download URL, transport or response rejected")]
    Download,
    #[error("runtime package I/O failed")]
    Io(#[from] std::io::Error),
    #[error("runtime metadata or package exceeds its size bound")]
    Size,
    #[error("runtime input is not a regular, non-symlink file")]
    Input,
    #[error("runtime manifest is not independently selected or compatible")]
    Manifest,
    #[error("runtime signature does not match the independent trust key")]
    Signature,
    #[error("runtime archive is invalid or incompatible with this host")]
    Archive,
    #[error("runtime installation directory is not private and canonical")]
    Directory,
    #[error("another runtime installation is active")]
    Busy,
    #[error("runtime selection changed or retained package is inconsistent")]
    State,
    #[error("atomic runtime selection write failed")]
    Commit,
    #[error("runtime health check failed or exceeded its bound")]
    Health,
    #[error("runtime invocation failed or exceeded its bound")]
    Exchange,
    #[cfg(any(target_os = "macos", target_os = "linux", windows))]
    #[error("personal sync failed: {0}")]
    SyncCycle(&'static str),
    #[error("runtime request or policy input was rejected")]
    Policy,
    #[error("runtime global configuration could not be updated")]
    Configuration,
    #[error("runtime device provisioning failed; inspect configured device state before retrying")]
    Provisioning,
    #[error("runtime license renewal check failed; no renewed access is confirmed")]
    Renewal,
    #[error(
        "runtime user service operation failed; inspect its saved service state before retrying"
    )]
    RenewalService,
    #[cfg(any(unix, windows))]
    #[error(
        "personal activation did not finish; check account access and retained personal-license state before retrying"
    )]
    Activation,
    #[error("device provisioned but runtime configuration changed or could not be saved")]
    ProvisionedConfiguration,
}

pub(crate) type Result<T> = std::result::Result<T, InstallError>;

fn trust_key(encoded: &str) -> Result<[u8; 32]> {
    if !is_digest(encoded, 64) {
        return Err(InstallError::Usage);
    }
    let mut key = [0; 32];
    for (index, byte) in key.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&encoded[index * 2..index * 2 + 2], 16)
            .map_err(|_| InstallError::Usage)?;
    }
    Ok(key)
}

/// Open once and bound the actual read, including files that grow after stat.
pub(crate) fn read_regular(path: &Path, limit: usize) -> Result<Vec<u8>> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !regular_metadata(&metadata) {
        return Err(InstallError::Input);
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
        // Bind the leaf itself even if it becomes a reparse point after stat.
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !regular_metadata(&metadata) {
        return Err(InstallError::Input);
    }
    if metadata.len() > limit as u64 {
        return Err(InstallError::Size);
    }
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(InstallError::Size);
    }
    Ok(bytes)
}

fn regular_metadata(metadata: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
        // Symlinks are only one reparse tag; reject every reparse-backed input.
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return false;
        }
    }
    metadata.is_file() && !metadata.file_type().is_symlink()
}

use super::updater::sha256_hex as sha256;

pub(crate) fn is_digest(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
