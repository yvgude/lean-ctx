// SPDX-License-Identifier: Apache-2.0
//! The former cockpit Pro state, owned by the engine per project partition:
//! alert settings and history, protection profiles, applied and dismissed
//! adaptive recommendations, and the alerts watch's audit-trail cursor.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::wire::{AdaptiveState, AlertRecord, AlertSettings, Profile};
use super::{GUARD_KINDS, ProResult, ProjectScope, read_json, write_json};

pub(crate) const ALERT_DEFAULT_KINDS: [&str; 5] = [
    "secret_redacted",
    "shell_blocked",
    "path_blocked",
    "injection_flagged",
    "content_withheld",
];
const HISTORY_KEEP: usize = 50;
const PROFILES_KEEP: usize = 20;
const ADAPTIVE_KEEP: usize = 500;

/// The stored alert file.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub(crate) struct AlertsFile {
    #[serde(default)]
    pub(crate) settings: Map<String, Value>,
    #[serde(default)]
    pub(crate) history: Vec<AlertRecord>,
}

/// The Pro state files of one project partition.
pub(crate) struct ProState {
    dir: PathBuf,
}

impl ProState {
    pub(crate) fn new(scope: &ProjectScope) -> Self {
        Self {
            dir: scope.partition_dir(),
        }
    }

    fn file(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    pub(crate) fn alerts_file(&self) -> AlertsFile {
        let mut file: AlertsFile = read_json(&self.file("alerts.json")).unwrap_or_default();
        let keep = file.history.len().saturating_sub(HISTORY_KEEP);
        file.history.drain(..keep);
        file
    }

    /// Settings as the cockpit always read them: unknown kinds dropped,
    /// defaults for missing keys.
    pub(crate) fn alert_settings(&self) -> AlertSettings {
        let stored = self.alerts_file().settings;
        let kinds = stored.get("kinds").and_then(Value::as_array).map_or_else(
            || {
                ALERT_DEFAULT_KINDS
                    .iter()
                    .map(|k| (*k).to_string())
                    .collect()
            },
            |list| {
                list.iter()
                    .filter_map(Value::as_str)
                    .filter(|k| GUARD_KINDS.contains(k))
                    .map(str::to_string)
                    .collect()
            },
        );
        AlertSettings {
            enabled: stored.get("enabled") == Some(&Value::Bool(true)),
            kinds,
            desktop: stored.get("desktop") != Some(&Value::Bool(false)),
            webhook: stored
                .get("webhook")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        }
    }

    /// Input `alerts_state`.
    pub(crate) fn alerts_state(&self) -> Value {
        let file = self.alerts_file();
        let settings = serde_json::to_value(self.alert_settings()).unwrap_or(Value::Null);
        json!({ "settings": settings, "history": file.history })
    }

    pub(crate) fn put_alert_settings(&self, settings: &AlertSettings) -> ProResult<()> {
        let mut file = self.alerts_file();
        file.settings = match serde_json::to_value(settings) {
            Ok(Value::Object(map)) => map,
            _ => Map::new(),
        };
        write_json(&self.file("alerts.json"), &file)
    }

    pub(crate) fn append_alert(&self, record: AlertRecord) -> ProResult<()> {
        let mut file = self.alerts_file();
        file.history.push(record);
        let keep = file.history.len().saturating_sub(HISTORY_KEEP);
        file.history.drain(..keep);
        write_json(&self.file("alerts.json"), &file)
    }

    pub(crate) fn profiles(&self) -> Vec<Profile> {
        read_json(&self.file("security-profiles.json")).unwrap_or_default()
    }

    pub(crate) fn put_profile(&self, profile: Profile) -> ProResult<()> {
        let mut profiles: Vec<Profile> = self
            .profiles()
            .into_iter()
            .filter(|p| p.name != profile.name)
            .collect();
        profiles.push(profile);
        let keep = profiles.len().saturating_sub(PROFILES_KEEP);
        profiles.drain(..keep);
        write_json(&self.file("security-profiles.json"), &profiles)
    }

    pub(crate) fn delete_profile(&self, name: &str) -> ProResult<()> {
        let profiles: Vec<Profile> = self
            .profiles()
            .into_iter()
            .filter(|p| p.name != name)
            .collect();
        write_json(&self.file("security-profiles.json"), &profiles)
    }

    pub(crate) fn adaptive(&self) -> AdaptiveState {
        read_json(&self.file("adaptive.json")).unwrap_or_default()
    }

    pub(crate) fn put_adaptive(&self, mut state: AdaptiveState) -> ProResult<()> {
        let keep = state.applied.len().saturating_sub(ADAPTIVE_KEEP);
        state.applied.drain(..keep);
        let keep = state.dismissed.len().saturating_sub(ADAPTIVE_KEEP);
        state.dismissed.drain(..keep);
        write_json(&self.file("adaptive.json"), &state)
    }

    /// The alerts watch's byte offset in the audit trail.
    pub(crate) fn guard_cursor(&self) -> Option<u64> {
        read_json::<Value>(&self.file("guard-cursor.json"))?
            .get("offset")?
            .as_u64()
    }

    pub(crate) fn put_guard_cursor(&self, offset: u64) -> ProResult<()> {
        write_json(
            &self.file("guard-cursor.json"),
            &json!({ "offset": offset }),
        )
    }
}
