// SPDX-License-Identifier: Apache-2.0
//! Persist explicit user consent only after the selected runtime passes health.

use std::path::Path;

use crate::core::config::{Config, IntelligenceRuntimeConfig};

use super::{InstallError, Result, health};

pub(crate) const DISCLOSURE: &str = "Optional Intelligence Runtime: proprietary, runs locally. New commercial builds require a Pro or Enterprise entitlement. The public reference runtime works without it. Staging packages are not production releases.";

/// Verified setup snapshot: discovery never starts proprietary code or grants consent.
pub(crate) struct SetupRuntime {
    config: IntelligenceRuntimeConfig,
    observed_config: IntelligenceRuntimeConfig,
    version: Option<String>,
}

impl SetupRuntime {
    pub(crate) fn available(&self) -> bool {
        self.version.is_some()
    }

    pub(crate) fn channel_available(&self) -> bool {
        !self.config.channel_url.is_empty()
    }

    pub(crate) fn enabled(&self) -> bool {
        self.available() && self.config.enabled && self.config.accept_proprietary
    }

    pub(crate) fn status(&self) -> &'static str {
        if self.enabled() {
            "configured"
        } else if self.available() {
            "available"
        } else if self.channel_available() {
            if self.config.manifest_sha256.is_empty() {
                "download_available"
            } else {
                "repair_available"
            }
        } else {
            "not_configured"
        }
    }

    pub(crate) fn report(&self) -> serde_json::Value {
        serde_json::json!({"schema_version": 1, "status": self.status(),
            "version": self.version, "disclosure": DISCLOSURE,
            "runtime_started": false, "release_approved": false})
    }

    /// The caller must obtain explicit consent for this captured, trusted selection.
    pub(crate) fn enable(&self) -> Result<serde_json::Value> {
        if !self.available() {
            return Err(InstallError::State);
        }
        let _guard = super::install::configuration_guard(Path::new(&self.config.root))?;
        activate_checked(
            Path::new(&self.config.root),
            &self.config.manifest_sha256,
            &self.config.trust_key_hex,
            Some(self),
        )
    }

    /// Called only after explicit consent; discovery itself never uses the network.
    pub(crate) fn install_and_enable(&self) -> Result<serde_json::Value> {
        if !self.channel_available() {
            return Err(InstallError::Policy);
        }
        if global_runtime()? != self.observed_config {
            return Err(InstallError::Configuration);
        }
        let root = Path::new(&self.config.root);
        if self.config != self.observed_config {
            super::install::prepare_parent_chain(root.parent().ok_or(InstallError::Directory)?)?;
        }
        let _guard = super::install::configuration_guard(root)?;
        if global_runtime()? != self.observed_config {
            return Err(InstallError::Configuration);
        }
        let expected = super::install::active_manifest(root)?;
        let result = super::catalog::sync(
            [&self.config.channel_url, &self.config.channel_signature_url],
            &super::trust_key(&self.config.channel_root_key_hex)?,
            root,
            &expected,
        )?;
        let selected = result["receipt"]["manifest_sha256"]
            .as_str()
            .ok_or(InstallError::State)?;
        let key = result["artifact_key_hex"]
            .as_str()
            .ok_or(InstallError::State)?;
        let activation = activate_checked(root, selected, key, Some(self))?;
        Ok(
            serde_json::json!({"status": "installed_and_activated", "installation": result,
            "activation": activation, "release_approved": false}),
        )
    }

    /// Restore only the retained, authenticated predecessor of this snapshot.
    pub(crate) fn rollback_and_enable(&self) -> Result<serde_json::Value> {
        if self.config.manifest_sha256.is_empty() || self.config.staging {
            return Err(InstallError::Policy);
        }
        let root = Path::new(&self.config.root);
        let _guard = super::install::configuration_guard(root)?;
        if global_runtime()? != self.observed_config {
            return Err(InstallError::Configuration);
        }
        let (result, key) =
            super::install::rollback_configured(root, &self.config.manifest_sha256)?;
        let selected = result["receipt"]["manifest_sha256"]
            .as_str()
            .ok_or(InstallError::State)?;
        // A failed health/configuration check leaves the old binding mismatched
        // and therefore unusable. Retrying reauthenticates the signed rollback
        // relationship and completes activation without another selection change.
        let activation = activate_checked(root, selected, &key, Some(self))?;
        Ok(
            serde_json::json!({"status": "rolled_back_and_activated", "rollback": result,
            "activation": activation, "release_approved": false}),
        )
    }
}

pub(crate) fn inspect_setup() -> Result<SetupRuntime> {
    // Never merge project configuration, search PATH, download or start a child.
    let observed_config = global_runtime()?;
    let config = if observed_config == IntelligenceRuntimeConfig::default() {
        super::bootstrap::config()?
    } else {
        observed_config.clone()
    };
    let version = if config == IntelligenceRuntimeConfig::default() {
        None
    } else {
        if !super::bootstrap::permits_config(&config) {
            return Err(InstallError::Policy);
        }
        let has_channel = !config.channel_url.is_empty()
            || !config.channel_signature_url.is_empty()
            || !config.channel_root_key_hex.is_empty();
        if has_channel {
            super::channel::validate_urls(&[&config.channel_url, &config.channel_signature_url])?;
            super::trust_key(&config.channel_root_key_hex)?;
            if !Path::new(&config.root).is_absolute() {
                return Err(InstallError::Directory);
            }
        }
        if config.manifest_sha256.is_empty() && config.trust_key_hex.is_empty() {
            if !has_channel || config.enabled || config.accept_proprietary {
                return Err(InstallError::Policy);
            }
            None
        } else {
            let key = super::trust_key(&config.trust_key_hex)?;
            match super::install::verified_active(
                Path::new(&config.root),
                &config.manifest_sha256,
                &key,
            ) {
                Ok(package) => Some(package.receipt.version),
                // Offer only a new consented, authenticated download; never enable
                // this unverified selection. This also recovers an interrupted update.
                Err(_) if has_channel => None,
                Err(error) => return Err(error),
            }
        }
    };
    Ok(SetupRuntime {
        config,
        observed_config,
        version,
    })
}

fn global_runtime() -> Result<IntelligenceRuntimeConfig> {
    Config::try_load_global()
        .map(|config| config.intelligence_runtime)
        .map_err(|_| InstallError::Configuration)
}

pub(super) fn activate(root: &Path, selected: &str, encoded: &str) -> Result<serde_json::Value> {
    activate_checked(root, selected, encoded, None)
}

fn activate_checked(
    root: &Path,
    selected: &str,
    encoded: &str,
    expected: Option<&SetupRuntime>,
) -> Result<serde_json::Value> {
    let observed = global_runtime()?;
    if expected.is_some_and(|expected| observed != expected.observed_config) {
        return Err(InstallError::Configuration);
    }
    let key = super::trust_key(encoded)?;
    let checked = health::check(root, selected, &key)?;
    let staging = checked["receipt"]["staging_only"]
        .as_bool()
        .ok_or(InstallError::State)?;
    let production = super::bootstrap::production_key(root)?.is_some();
    if staging == production {
        return Err(InstallError::Policy);
    }
    let previous = if production {
        let mut compiled = super::bootstrap::config()?;
        if !super::bootstrap::permits_config(&compiled) {
            return Err(InstallError::Policy);
        }
        compiled.license_configuration = expected
            .map(|expected| expected.config.license_configuration.clone())
            .or_else(|| {
                (!observed.staging && super::bootstrap::permits_config(&observed))
                    .then(|| observed.license_configuration.clone())
            })
            .unwrap_or_default();
        // A channel refresh must not erase the device's existing license binding.
        if observed.root == compiled.root && super::bootstrap::permits_config(&observed) {
            observed.clone()
        } else {
            compiled
        }
    } else if let Some(expected) = expected {
        expected.config.clone()
    } else if Path::new(&observed.root) == root {
        observed.clone()
    } else {
        IntelligenceRuntimeConfig::default()
    };
    let root = root.to_str().ok_or(InstallError::Directory)?.to_owned();
    Config::try_update_global(|config| {
        if config.intelligence_runtime != observed {
            return Err(crate::core::error::LeanCtxError::Config(
                "runtime setup configuration changed".into(),
            ));
        }
        config.intelligence_runtime = IntelligenceRuntimeConfig {
            enabled: true,
            accept_proprietary: true,
            staging,
            root,
            manifest_sha256: selected.to_owned(),
            trust_key_hex: encoded.to_owned(),
            ..previous
        };
        Ok(())
    })
    .map_err(|_| InstallError::Configuration)?;
    Ok(serde_json::json!({"status": "activated", "health": checked,
        "staging_only": staging, "release_approved": false}))
}

pub(super) fn deactivate() -> Result<serde_json::Value> {
    Config::update_global(|config| config.intelligence_runtime.enabled = false)
        .map_err(|_| InstallError::Configuration)?;
    Ok(serde_json::json!({"status": "disabled", "release_approved": false}))
}
