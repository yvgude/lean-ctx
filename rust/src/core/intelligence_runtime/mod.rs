// SPDX-License-Identifier: Apache-2.0
//! Public installation authority for the optional, separately licensed runtime.
//! No private implementation, account, entitlement, or implicit network access.

mod bootstrap;
mod catalog;
mod channel;
mod cli;
pub(crate) mod cockpit;
pub(crate) mod code_security;
pub(crate) mod compliance_frameworks;
mod configuration;
pub(crate) mod dashboard_pro;
mod exchange;
pub(crate) mod org_policy_verifier;
pub(crate) mod personal_protection;
pub(crate) mod semantic_coverage;
pub(crate) mod semantic_detectors;
mod semantic_worker;
pub(crate) use configuration::{DISCLOSURE, inspect_setup};
mod health;
mod install;
mod license;
// The Hive connector (`core::hive::runtime_lease`) registers the source.
pub(crate) mod lease;
mod manifest;
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
use manifest::{VerifiedPackage, verify, verify_kind};

// The signed archive member stays platform-neutral; only materialized files vary.
const EXECUTABLE_NAME: &str = if cfg!(windows) {
    "leanctx-intelligence.exe"
} else {
    "leanctx-intelligence"
};

/// The signed package kinds the runtime channel delivers. Both share the
/// archive shape, the signature domains and the trust chain; the signed
/// `runtime-description.json` names the kind (`engine_id`), and every use
/// re-authenticates the package as the kind it expects, so one kind never
/// runs as the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuntimeKind {
    /// The Pro Intelligence Runtime: licence-bound outside staging.
    Intelligence,
    /// The Cockpit web UI: free for everyone, never licence-bound.
    Cockpit,
}

impl RuntimeKind {
    pub(crate) const fn engine_id(self) -> &'static str {
        match self {
            Self::Intelligence => "leanctx-intelligence",
            Self::Cockpit => "leanctx-cockpit",
        }
    }

    /// The store directory beside the context data: `<name>` for staging,
    /// `<name>-production` for production. Kinds never share a store.
    pub(crate) const fn store_name(self) -> &'static str {
        match self {
            Self::Intelligence => "intelligence-runtime",
            Self::Cockpit => "cockpit-runtime",
        }
    }
}

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
    #[error("runtime channel is older than the active one; a downgrade must be requested")]
    Downgrade,
    #[error("atomic runtime selection write failed")]
    Commit,
    #[error("runtime health check failed or exceeded its bound")]
    Health,
    #[error("runtime invocation is not supported by the authenticated package and this host")]
    InvocationAdmission,
    #[error("runtime invocation failed or exceeded its bound")]
    Exchange,
    #[error("runtime request or policy input was rejected")]
    Policy,
    #[error("runtime global configuration could not be updated")]
    Configuration,
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
