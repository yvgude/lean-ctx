// SPDX-License-Identifier: Apache-2.0
//! What the retired daily metrics upload left under `<data>/hive/metrics/`:
//! `state.json` with the organization switch-off Hive answered, which
//! `hive status` still names. Nothing writes it any more.

use std::path::Path;

use serde::{Deserialize, Serialize};

const STATE_FILE: &str = "state.json";
const SMALL_FILE_LIMIT: u64 = 64 * 1024;

fn read_small(path: &Path) -> Option<String> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    if !meta.file_type().is_file() || meta.len() > SMALL_FILE_LIMIT {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

/// What this device remembers about its metrics uploads.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct UploadState {
    /// Hive answered `upload_disabled` under this lease.
    pub(crate) disabled_under_lease: Option<String>,
}

pub(crate) fn load_state_at(dir: &Path) -> UploadState {
    read_small(&dir.join(STATE_FILE))
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_recorded_switch_off_is_read_back() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load_state_at(dir.path()), UploadState::default());
        std::fs::write(
            dir.path().join(STATE_FILE),
            r#"{"last_run":1,"backfill_done":true,"disabled_under_lease":"lse_1"}"#,
        )
        .unwrap();
        assert_eq!(
            load_state_at(dir.path()).disabled_under_lease.as_deref(),
            Some("lse_1")
        );
    }
}
