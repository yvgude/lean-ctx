// SPDX-License-Identifier: Apache-2.0
//! User-global opt-in and independent trust pins for the optional local runtime.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The file next to `config.toml` that records `setup runtime deactivate`
/// (see [`IntelligenceRuntimeConfig::declined`]).
pub const DECLINED_MARKER: &str = "runtime-declined";

/// Where the decline of this configuration is recorded.
#[must_use]
pub fn declined_marker() -> Option<PathBuf> {
    super::Config::path().and_then(|config| config.parent().map(|dir| dir.join(DECLINED_MARKER)))
}

/// Whether `marker` records a decline. A symlink counts as recorded (fail
/// closed: the runtime stays off) and is never followed.
#[must_use]
pub fn decline_recorded_at(marker: &Path) -> bool {
    marker.symlink_metadata().is_ok()
}

/// Records (`true`) or clears (`false`) the decline at `marker`. Clearing
/// a missing marker succeeds.
pub fn record_decline_at(marker: &Path, declined: bool) -> std::io::Result<()> {
    if !declined {
        return match std::fs::remove_file(marker) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            other => other,
        };
    }
    if decline_recorded_at(marker) {
        return Ok(());
    }
    if let Some(dir) = marker.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    match options.open(marker) {
        Ok(mut file) => std::io::Write::write_all(
            &mut file,
            b"lean-ctx setup runtime deactivate; cleared by lean-ctx setup runtime install\n",
        ),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error),
    }
}

/// [`record_decline_at`] for this configuration.
pub fn record_decline(declined: bool) -> std::io::Result<()> {
    let marker = declined_marker().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "no configuration directory")
    })?;
    record_decline_at(&marker, declined)
}

/// Never merged from a project file, including trusted workspaces.
/// A public signing key is not a credential; the package cannot provide its pin.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct IntelligenceRuntimeConfig {
    pub enabled: bool,
    /// Switched off with `setup runtime deactivate`, also before anything
    /// was installed: setup, onboard, repair, bootstrap, harden and connect
    /// never install or enable the runtime on their own. Only an explicit
    /// `setup runtime install` (an activation) clears it.
    ///
    /// Never written to `config.toml`: this section is `deny_unknown_fields`,
    /// and 3.12.7 (a running daemon, a rollback) would refuse the whole file
    /// and fall back to the defaults. The decline is the file
    /// [`DECLINED_MARKER`] next to `config.toml`, which older versions
    /// neither read nor rewrite; [`Self::with_recorded_decline`] merges it.
    /// Still read from `config.toml`, so a configuration written by an early
    /// 3.12.8 build keeps loading.
    #[serde(skip_serializing)]
    pub declined: bool,
    pub accept_proprietary: bool,
    pub staging: bool,
    pub root: String,
    pub manifest_sha256: String,
    pub trust_key_hex: String,
    pub channel_url: String,
    pub channel_signature_url: String,
    pub channel_root_key_hex: String,
    /// Explicit user-global handoff to the private verifier, never project input.
    pub license_configuration: String,
    /// No effect: read-strategy policies are no longer learned by a runtime.
    /// Kept so an existing config (`deny_unknown_fields`) still loads.
    pub context_policy_apply: bool,
    /// The projects of this user that read shared team artifacts, each with
    /// its team identity. Empty by default: a project without an entry uses
    /// no artifact. Global-only like the rest of this section, so a
    /// repository cannot name itself. It carries no clearance: none is
    /// configurable here.
    pub team_artifact_projects: Vec<TeamArtifactProject>,
}

impl IntelligenceRuntimeConfig {
    /// Switched off with `setup runtime deactivate`, before or after an
    /// install (an install accepted once and no longer enabled counts too).
    /// Read from the configuration alone: whether the installed package
    /// still verifies does not change the person's choice.
    #[must_use]
    pub fn deactivated(&self) -> bool {
        self.declined || (self.accept_proprietary && !self.enabled)
    }

    /// This configuration with the decline recorded next to `config.toml`.
    #[must_use]
    pub fn with_recorded_decline(mut self) -> Self {
        self.declined |= declined_marker().is_some_and(|marker| decline_recorded_at(&marker));
        self
    }

    /// Nothing configured, apart from a switch-off.
    #[must_use]
    pub fn is_unset(&self) -> bool {
        Self {
            declined: false,
            ..self.clone()
        } == Self::default()
    }

    /// `setup runtime deactivate`: off, and kept off by setup.
    pub fn switch_off(&mut self) {
        self.enabled = false;
        self.declined = true;
    }

    /// The configuration after an activation of `selected` (an explicit
    /// install or activate): enabled, accepted and no longer switched off.
    #[must_use]
    pub fn activated(
        previous: Self,
        staging: bool,
        root: String,
        selected: &str,
        trust_key_hex: &str,
    ) -> Self {
        Self {
            enabled: true,
            declined: false,
            accept_proprietary: true,
            staging,
            root,
            manifest_sha256: selected.to_owned(),
            trust_key_hex: trust_key_hex.to_owned(),
            ..previous
        }
    }
}

/// One `[[intelligence_runtime.team_artifact_projects]]` entry.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TeamArtifactProject {
    /// Absolute path of the project root.
    pub root: String,
    pub organization_id: String,
    pub project_id: String,
    /// The project's opaque repository identity — not a URL or a path.
    pub repository_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `[intelligence_runtime]` as 3.12.7 parses it (MR !569 review): the
    /// same fields, `deny_unknown_fields`. A field it does not know makes
    /// 3.12.7 refuse the whole `config.toml` and run on the defaults.
    #[allow(dead_code)]
    #[derive(Deserialize)]
    #[serde(default, deny_unknown_fields)]
    #[derive(Default)]
    struct Shape3127 {
        enabled: bool,
        accept_proprietary: bool,
        staging: bool,
        root: String,
        manifest_sha256: String,
        trust_key_hex: String,
        channel_url: String,
        channel_signature_url: String,
        channel_root_key_hex: String,
        license_configuration: String,
        context_policy_apply: bool,
        team_artifact_projects: Vec<toml::Value>,
    }

    #[test]
    fn a_switched_off_runtime_stays_readable_by_3_12_7() {
        let installed = IntelligenceRuntimeConfig::activated(
            IntelligenceRuntimeConfig::default(),
            false,
            "/r".into(),
            &"b".repeat(64),
            "k",
        );
        for mut config in [IntelligenceRuntimeConfig::default(), installed] {
            config.switch_off();
            assert!(config.deactivated());
            let written = toml::to_string(&config).unwrap();
            assert!(!written.contains("declined"), "{written}");
            let old: Shape3127 = toml::from_str(&written).unwrap();
            assert!(!old.enabled, "3.12.7 sees the runtime off");
        }
        // A configuration from an early 3.12.8 build still loads here.
        let early: IntelligenceRuntimeConfig = toml::from_str("declined = true\n").unwrap();
        assert!(early.deactivated());
    }

    #[test]
    fn the_decline_marker_records_and_clears() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("config").join(DECLINED_MARKER);
        assert!(!decline_recorded_at(&marker));
        record_decline_at(&marker, false).unwrap();
        record_decline_at(&marker, true).unwrap();
        record_decline_at(&marker, true).unwrap();
        assert!(decline_recorded_at(&marker));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&marker).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        record_decline_at(&marker, false).unwrap();
        assert!(!decline_recorded_at(&marker));
        // A dangling symlink counts as a decline (fail closed), unfollowed.
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(dir.path().join("nowhere"), &marker).unwrap();
            assert!(decline_recorded_at(&marker));
        }
    }
}
