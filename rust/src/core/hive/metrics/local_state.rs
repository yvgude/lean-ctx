// SPDX-License-Identifier: Apache-2.0
//! The device's own metrics files under `<data>/hive/metrics/`: the random
//! device id and the upload state (debounce, last success, an organization
//! switch-off Hive answered). All private to the user (0600) and written
//! atomically.

use std::io::Write;
use std::path::Path;

use serde::{Deserialize, Serialize};

const DEVICE_ID_FILE: &str = "device-id";
const STATE_FILE: &str = "state.json";
const SMALL_FILE_LIMIT: u64 = 64 * 1024;

fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(dir)
    }
}

/// A 0600 temporary file next to `path`, holding `bytes`.
fn private_temp(path: &Path, bytes: &[u8]) -> std::io::Result<tempfile::NamedTempFile> {
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("no parent directory"))?;
    create_private_dir(parent)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(file.path(), std::fs::Permissions::from_mode(0o600))?;
    }
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    Ok(file)
}

fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    private_temp(path, bytes)?
        .persist(path)
        .map(|_| ())
        .map_err(|e| e.error)
}

fn read_small(path: &Path) -> Option<String> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    if !meta.file_type().is_file() || meta.len() > SMALL_FILE_LIMIT {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

/// This data directory's device id: a random UUID v4 created once, stored at
/// `<dir>/device-id`. Two processes racing to create it agree on one id:
/// the file appears complete or not at all, and only the first one wins.
pub(crate) fn device_id_at(dir: &Path) -> std::io::Result<String> {
    let path = dir.join(DEVICE_ID_FILE);
    if let Some(existing) = read_device_id(&path) {
        return Ok(existing);
    }
    let fresh = uuid::Uuid::new_v4().hyphenated().to_string();
    match private_temp(&path, fresh.as_bytes())?.persist_noclobber(&path) {
        Ok(_) => Ok(fresh),
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
            read_device_id(&path).ok_or_else(|| std::io::Error::other("unreadable device id"))
        }
        Err(error) => Err(error.error),
    }
}

fn read_device_id(path: &Path) -> Option<String> {
    let text = read_small(path)?;
    let id = text.trim();
    super::payload::valid_device_id(id).then(|| id.to_string())
}

/// What this device remembers about its metrics uploads.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct UploadState {
    /// Unix seconds of the last run that completed (sent, or had nothing to
    /// send); the hourly debounce counts from here. A failed run is not one.
    pub(crate) last_run: Option<i64>,
    /// Unix seconds of the last upload Hive accepted completely.
    pub(crate) last_success: Option<i64>,
    /// Day entries that upload carried.
    pub(crate) last_days: u64,
    /// The 90-day backfill completed once.
    pub(crate) backfill_done: bool,
    /// Hive answered `upload_disabled` under this lease: nothing is sent
    /// again until a newer lease allows the upload.
    pub(crate) disabled_under_lease: Option<String>,
}

pub(crate) fn load_state_at(dir: &Path) -> UploadState {
    read_small(&dir.join(STATE_FILE))
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

pub(crate) fn store_state_at(dir: &Path, state: &UploadState) -> std::io::Result<()> {
    write_private(
        &dir.join(STATE_FILE),
        &serde_json::to_vec(state).map_err(std::io::Error::other)?,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_id_is_created_once_private_and_reused() {
        let dir = tempfile::tempdir().unwrap();
        let metrics = dir.path().join("metrics");
        let first = device_id_at(&metrics).unwrap();
        assert!(super::super::payload::valid_device_id(&first));
        assert_eq!(device_id_at(&metrics).unwrap(), first);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(metrics.join(DEVICE_ID_FILE))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        // A damaged file is not an id; deleting it makes a new device.
        std::fs::remove_file(metrics.join(DEVICE_ID_FILE)).unwrap();
        assert_ne!(device_id_at(&metrics).unwrap(), first);
    }

    #[test]
    fn upload_state_round_trips_privately() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load_state_at(dir.path()), UploadState::default());
        let state = UploadState {
            last_run: Some(1),
            last_success: Some(1),
            last_days: 3,
            backfill_done: true,
            disabled_under_lease: Some("lse_1".into()),
        };
        store_state_at(dir.path(), &state).unwrap();
        assert_eq!(load_state_at(dir.path()), state);
    }
}
