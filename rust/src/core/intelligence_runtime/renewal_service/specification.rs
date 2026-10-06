// SPDX-License-Identifier: Apache-2.0
//! Saved service identity shared by native schedulers; contains no credentials.
use crate::core::intelligence_runtime::{InstallError, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

fn legacy_staging() -> bool {
    true
}

#[derive(Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct Specification {
    pub(super) schema_version: u32,
    pub(super) host: PathBuf,
    pub(super) home: PathBuf,
    pub(super) configuration_directory: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) sync_configuration: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) sync_data_directory: Option<PathBuf>,
    #[serde(default = "legacy_staging")]
    pub(super) staging: bool,
}

impl Specification {
    pub(super) fn validate(&self, home: &Path, config: &Path, sync: Option<&Path>) -> Result<()> {
        if self.schema_version != 1
            || self.home != home
            || self.configuration_directory != config
            || self.sync_configuration.as_deref() != sync
            || (self.sync_configuration.is_some() || !self.staging)
                != self.sync_data_directory.is_some()
        {
            return Err(InstallError::RenewalService);
        }
        safe_text(&self.host)?;
        Ok(())
    }
}

pub(super) fn safe_text(path: &Path) -> Result<&str> {
    path.to_str()
        .filter(|text| path.is_absolute() && !text.chars().any(char::is_control))
        .ok_or(InstallError::RenewalService)
}

#[cfg(any(windows, test))]
pub(super) fn windows_task_arguments(binding: &Path) -> Result<String> {
    let value = binding.to_str().ok_or(InstallError::RenewalService)?;
    // The path is separately admitted by the private Directory boundary. Refuse
    // quotes and a trailing slash instead of allowing command-line ambiguity.
    if value.is_empty()
        || value.ends_with('\\')
        || value.contains('"')
        || value.contains('%')
        || value.chars().any(char::is_control)
    {
        return Err(InstallError::RenewalService);
    }
    Ok(format!("engine runtime service-run --binding \"{value}\""))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scheduler_binding_stays_one_argument_and_cannot_inject_flags() {
        assert_eq!(
            windows_task_arguments(Path::new(r"C:\Users\A B\a&b\record.json")).unwrap(),
            r#"engine runtime service-run --binding "C:\Users\A B\a&b\record.json""#
        );
        for value in [
            "",
            "C:\\record\" --staging",
            "C:\\record\n.json",
            "C:\\%TEMP%\\record.json",
            "C:\\directory\\",
        ] {
            assert!(windows_task_arguments(Path::new(value)).is_err());
        }
    }
}
