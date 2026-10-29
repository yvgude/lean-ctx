// SPDX-License-Identifier: Apache-2.0
//! Where the commercial licence configuration of the active package is: the
//! saved global binding, or the operator's explicit environment. Only the
//! private runtime interprets the file.

#[cfg(any(unix, windows))]
use std::path::{Path, PathBuf};

#[cfg(any(unix, windows))]
use crate::core::config::Config;

#[cfg(any(unix, windows))]
use super::{InstallError, Result, read_regular, trust_key};

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
