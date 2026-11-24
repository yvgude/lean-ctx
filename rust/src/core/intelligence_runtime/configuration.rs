// SPDX-License-Identifier: Apache-2.0
//! Persist explicit user consent only after the selected runtime passes health.

use std::path::Path;

use crate::core::config::{Config, IntelligenceRuntimeConfig};

use super::{InstallError, Result, health};

pub const DISCLOSURE: &str = "Intelligence Runtime: proprietary, runs locally. Its commercial features are licensed through LeanCTX Cloud or a commercial licence; the public reference runtime works without either. Staging packages are not production releases.";

/// Verified setup snapshot: discovery never starts proprietary code or grants consent.
pub struct SetupRuntime {
    config: IntelligenceRuntimeConfig,
    observed_config: IntelligenceRuntimeConfig,
    version: Option<String>,
}

impl SetupRuntime {
    pub fn available(&self) -> bool {
        self.version.is_some()
    }

    pub fn channel_available(&self) -> bool {
        !self.config.channel_url.is_empty()
    }

    pub fn enabled(&self) -> bool {
        self.available() && self.config.enabled && self.config.accept_proprietary
    }

    /// Accepted once, then switched off with `setup runtime deactivate`:
    /// setup leaves it off. From the configuration only, so a package that
    /// no longer verifies never turns a switched-off runtime back on.
    pub fn deactivated(&self) -> bool {
        self.config.deactivated()
    }

    /// What setup finds for `config` when nothing verifies (no version):
    /// for planning only; it installs nothing.
    #[doc(hidden)]
    #[must_use]
    pub fn not_installed(config: IntelligenceRuntimeConfig) -> Self {
        Self {
            observed_config: config.clone(),
            config,
            version: None,
        }
    }

    /// Enabled and verified, but the signed model set of its package is not
    /// installed (left for later, or a background load that failed). Never
    /// downloads; any error reads as "nothing pending".
    pub fn companions_pending(&self) -> bool {
        if !self.enabled() {
            return false;
        }
        let root = Path::new(&self.config.root);
        super::trust_key(&self.config.trust_key_hex)
            .and_then(|key| {
                super::install::verified_active(root, &self.config.manifest_sha256, &key)
            })
            .and_then(|package| super::install::companions_missing(root, &package))
            .unwrap_or(false)
    }

    pub fn status(&self) -> &'static str {
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
        // Reported here, once and without content, instead of as an
        // unavailable semantic check on every admission: a configured
        // runtime whose package does not offer semantic detectors is a
        // property of the installation.
        // The install-side fields are the ones `runtime health` prints, from
        // the same function: what the authenticated package and its
        // installed model set offer. `semantic_categories` are the categories
        // that set examines; a category that is not listed is not covered.
        let mut report = serde_json::json!({"schema_version": 1, "status": self.status(),
            "version": self.version, "disclosure": DISCLOSURE,
            "semantic_detectors": "not_configured",
            "semantic_descriptor_sha256_prefix": null,
            "semantic_versions": [], "semantic_categories": [],
            "runtime_started": false, "release_approved": false});
        if self.enabled() {
            let authenticated = super::trust_key(&self.config.trust_key_hex)
                .and_then(|key| {
                    super::install::verified_active(
                        Path::new(&self.config.root),
                        &self.config.manifest_sha256,
                        &key,
                    )
                })
                .ok();
            if let Some(package) = authenticated {
                let state = super::install::semantic_state(&package);
                let versions = super::install::semantic_set(&package)
                    .map_or_else(Vec::new, |set| set.versions);
                for (key, value) in health::semantic_report(&package, &state, versions) {
                    report[key] = value;
                }
            } else {
                report["semantic_detectors"] = "unverified".into();
            }
        }
        for (key, value) in health::semantic_worker_report(&self.config) {
            report[key] = value;
        }
        report
    }

    /// The caller must obtain explicit consent for this captured, trusted selection.
    pub fn enable(&self) -> Result<serde_json::Value> {
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
    pub fn install_and_enable(&self) -> Result<serde_json::Value> {
        self.install_and_enable_with(super::catalog::Companions::Inline)
    }

    /// As [`Self::install_and_enable`] for setup (owner decision
    /// 23.11.2026): the runtime only, so setup stays fast with or without
    /// a terminal; the companions of a v4 package (hundreds of megabytes)
    /// are left for [`load_companions`] in the background. The result's
    /// `installation.semantic_models_reason` is then
    /// [`COMPANIONS_LATER`].
    pub fn install_runtime_and_enable(&self) -> Result<serde_json::Value> {
        self.install_and_enable_with(super::catalog::Companions::Later)
    }

    fn install_and_enable_with(
        &self,
        companions: super::catalog::Companions,
    ) -> Result<serde_json::Value> {
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
            companions,
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

/// `semantic_models_reason` of an install that left the companions for
/// [`load_companions`].
pub const COMPANIONS_LATER: &str = super::catalog::COMPANIONS_LATER;

/// Load the companions setup left for later (`setup runtime
/// load-companions`, started in the background by setup): only for the
/// enabled, verified runtime, from the location its signed channel names.
pub fn load_companions() -> Result<serde_json::Value> {
    let candidate = inspect_setup()?;
    if !candidate.enabled() {
        return Err(InstallError::State);
    }
    let config = &candidate.config;
    let root = Path::new(&config.root);
    // One background load at a time: a second one ends here (`Busy`).
    let _loading = super::install::companions_guard(root)?;
    let package = {
        let _guard = super::install::configuration_guard(root)?;
        if global_runtime()? != candidate.observed_config {
            return Err(InstallError::Configuration);
        }
        super::install::verified_active(
            root,
            &config.manifest_sha256,
            &super::trust_key(&config.trust_key_hex)?,
        )?
    };
    // No lock is held across the download: install_channel_companions takes
    // install.lock only to check and rename the verified set, and publishes
    // nothing when the runtime was switched off (or changed) meanwhile.
    let unchanged = || global_runtime().is_ok_and(|now| now == candidate.observed_config);
    let loaded = super::catalog::load_companions(
        [&config.channel_url, &config.channel_signature_url],
        &super::trust_key(&config.channel_root_key_hex)?,
        root,
        &package,
        &unchanged,
    )?;
    Ok(serde_json::json!({
        "status": if loaded { "companions_installed" } else { "companions_not_present" },
        "release_approved": false
    }))
}

pub fn inspect_setup() -> Result<SetupRuntime> {
    // Never merge project configuration, search PATH, download or start a child.
    let observed_config = global_runtime()?;
    // A runtime switched off before its first install is still the
    // bundled channel, kept off.
    let config = if observed_config.is_unset() {
        IntelligenceRuntimeConfig {
            declined: observed_config.declined,
            ..super::bootstrap::config()?
        }
    } else {
        observed_config.clone()
    };
    let version = if config.is_unset() {
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

/// The user-global runtime configuration, with the decline recorded next
/// to `config.toml` merged in.
fn global_runtime() -> Result<IntelligenceRuntimeConfig> {
    Config::try_load_global()
        .map(|config| config.intelligence_runtime.with_recorded_decline())
        .map_err(|_| InstallError::Configuration)
}

/// `config` as `global_runtime` reads it, without touching the marker: the
/// decline of `observed` stands for it.
fn same_as_observed(
    config: &IntelligenceRuntimeConfig,
    observed: &IntelligenceRuntimeConfig,
) -> bool {
    IntelligenceRuntimeConfig {
        declined: observed.declined,
        ..config.clone()
    } == *observed
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
        if !same_as_observed(&config.intelligence_runtime, &observed) {
            return Err(crate::core::error::LeanCtxError::Config(
                "runtime setup configuration changed".into(),
            ));
        }
        config.intelligence_runtime =
            IntelligenceRuntimeConfig::activated(previous, staging, root, selected, encoded);
        Ok(())
    })
    .map_err(|_| InstallError::Configuration)?;
    // An explicit install switches it on again: the decline goes too.
    crate::core::config::record_decline(false).map_err(|_| InstallError::Configuration)?;
    Ok(serde_json::json!({"status": "activated", "health": checked,
        "staging_only": staging, "release_approved": false}))
}

pub(super) fn deactivate() -> Result<serde_json::Value> {
    // The decline first (next to config.toml, where 3.12.7 never looks), so
    // an interrupted deactivate never leaves the runtime to be switched on.
    crate::core::config::record_decline(true).map_err(|_| InstallError::Configuration)?;
    Config::update_global(|config| config.intelligence_runtime.switch_off())
        .map_err(|_| InstallError::Configuration)?;
    Ok(serde_json::json!({"status": "disabled", "release_approved": false}))
}
