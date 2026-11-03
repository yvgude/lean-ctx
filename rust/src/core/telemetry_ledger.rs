// SPDX-License-Identifier: Apache-2.0
//! Local append-only ledger of sent telemetry heartbeats.
//!
//! Every successful heartbeat is recorded as a JSON line in
//! `<state_dir>/telemetry_heartbeats.jsonl`. This gives the user full
//! transparency over what was sent and when — visible in the dashboard
//! and via `lean-ctx telemetry history`.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use super::telemetry_aggregate::lock_telemetry_file;

const MAX_LEDGER_BYTES: u64 = 1_048_576;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeartbeatRecord {
    pub timestamp: String,
    pub installation_id: String,
    pub version: String,
    pub os: String,
    pub arch: String,
    #[serde(default)]
    pub schema_version: u16,
    #[serde(default)]
    pub event_names: Vec<String>,
    #[serde(default)]
    pub payload_hash: String,
    #[serde(default)]
    pub endpoint: String,
    #[serde(default)]
    pub status: String,
}

fn ledger_path() -> Result<PathBuf, String> {
    crate::core::paths::state_dir().map(|d| d.join("telemetry_heartbeats.jsonl"))
}

pub(crate) fn append(record: &HeartbeatRecord) -> Result<(), String> {
    let path = ledger_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("Cannot create state dir: {e}"))?;
    }
    let lock = open_lock(&path)?;
    lock_telemetry_file(&lock, "ledger")?;
    if read_paths(&path)?.iter().any(|existing| {
        !record.payload_hash.is_empty() && existing.payload_hash == record.payload_hash
    }) {
        return Ok(());
    }
    let mut line =
        serde_json::to_string(record).map_err(|e| format!("JSON serialization error: {e}"))?;
    line.push('\n');

    let current_len = std::fs::metadata(&path).map_or(0, |metadata| metadata.len());
    if current_len.saturating_add(line.len() as u64) > MAX_LEDGER_BYTES {
        rotate(&path)?;
    }

    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options
        .open(&path)
        .map_err(|e| format!("Cannot open telemetry ledger: {e}"))?;
    let mut writer = std::io::BufWriter::new(file);
    writer
        .write_all(line.as_bytes())
        .map_err(|e| format!("Cannot write to telemetry ledger: {e}"))?;
    writer
        .flush()
        .map_err(|e| format!("Cannot flush telemetry ledger: {e}"))?;
    writer
        .get_ref()
        .sync_all()
        .map_err(|e| format!("Cannot sync telemetry ledger: {e}"))?;
    Ok(())
}

pub fn read_all() -> Vec<HeartbeatRecord> {
    let Ok(path) = ledger_path() else {
        return Vec::new();
    };
    read_paths(&path).unwrap_or_default()
}

pub(crate) fn latest_valid_version() -> Result<Option<String>, String> {
    let path = ledger_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("Cannot create state dir: {e}"))?;
    }
    let lock = open_lock(&path)?;
    lock_telemetry_file(&lock, "ledger")?;
    Ok(read_paths(&path)?
        .into_iter()
        .rev()
        .find_map(|record| (!record.version.is_empty()).then_some(record.version)))
}

pub fn purge_local() -> Result<(), String> {
    let path = ledger_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("Cannot create state dir: {e}"))?;
    }
    let lock = open_lock(&path)?;
    lock_telemetry_file(&lock, "ledger")?;
    for candidate in [&path, &rotated_path(&path)] {
        match std::fs::remove_file(candidate) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("Cannot purge telemetry ledger: {error}")),
        }
    }
    Ok(())
}

fn open_lock(path: &std::path::Path) -> Result<std::fs::File, String> {
    std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path.with_extension("lock"))
        .map_err(|e| format!("Cannot open telemetry ledger lock: {e}"))
}

fn rotated_path(path: &std::path::Path) -> PathBuf {
    path.with_extension("jsonl.1")
}

fn rotate(path: &std::path::Path) -> Result<(), String> {
    let rotated = rotated_path(path);
    match std::fs::remove_file(&rotated) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("Cannot replace rotated telemetry ledger: {error}")),
    }
    let length = std::fs::metadata(path).map_or(0, |metadata| metadata.len());
    if length > MAX_LEDGER_BYTES {
        use std::io::{Read, Seek};
        let mut file = std::fs::File::open(path)
            .map_err(|error| format!("Cannot open oversized telemetry ledger: {error}"))?;
        file.seek(std::io::SeekFrom::End(-(MAX_LEDGER_BYTES as i64)))
            .map_err(|error| format!("Cannot seek telemetry ledger: {error}"))?;
        let mut tail = Vec::with_capacity(MAX_LEDGER_BYTES as usize);
        file.read_to_end(&mut tail)
            .map_err(|error| format!("Cannot read telemetry ledger tail: {error}"))?;
        let start = tail
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(tail.len(), |index| index + 1);
        #[cfg(unix)]
        let permissions = {
            use std::os::unix::fs::PermissionsExt;
            Some(std::fs::Permissions::from_mode(0o600))
        };
        #[cfg(not(unix))]
        let permissions: Option<std::fs::Permissions> = None;
        crate::core::atomic_fs::try_atomic_write(&rotated, &tail[start..], permissions.as_ref())
            .map_err(|error| format!("Cannot write bounded telemetry ledger: {error}"))?;
        std::fs::remove_file(path)
            .map_err(|error| format!("Cannot replace oversized telemetry ledger: {error}"))?;
        return Ok(());
    }
    match std::fs::rename(path, rotated) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("Cannot rotate telemetry ledger: {error}")),
    }
}

fn read_paths(path: &std::path::Path) -> Result<Vec<HeartbeatRecord>, String> {
    let mut records = Vec::new();
    for candidate in [rotated_path(path), path.to_path_buf()] {
        let content = match std::fs::read_to_string(candidate) {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(format!("Cannot read telemetry ledger: {error}")),
        };
        records.extend(
            content
                .lines()
                .filter_map(|line| serde_json::from_str(line).ok()),
        );
    }
    Ok(records)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fs2::FileExt;

    #[test]
    fn contended_ledger_operations_return_without_mutating_records() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        let path = ledger_path().unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let record = HeartbeatRecord {
            timestamp: String::new(),
            installation_id: String::new(),
            version: "4".into(),
            os: String::new(),
            arch: String::new(),
            schema_version: 2,
            event_names: vec![],
            payload_hash: "b".repeat(64),
            endpoint: String::new(),
            status: "success".into(),
        };
        append(&record).unwrap();
        assert_eq!(latest_valid_version().unwrap().as_deref(), Some("4"));
        let original = std::fs::read(&path).unwrap();
        let blocker = open_lock(&path).unwrap();
        blocker.lock_exclusive().unwrap();
        let started = std::time::Instant::now();
        assert!(append(&record).unwrap_err().contains("timed out"));
        assert!(latest_valid_version().unwrap_err().contains("timed out"));
        assert!(purge_local().unwrap_err().contains("timed out"));
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
        assert_eq!(std::fs::read(&path).unwrap(), original);
        drop(blocker);
        append(&record).unwrap();
        assert_eq!(latest_valid_version().unwrap().as_deref(), Some("4"));
        purge_local().unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn unreadable_ledger_is_not_an_empty_history() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        let path = ledger_path().unwrap();
        assert_eq!(latest_valid_version().unwrap(), None);
        // A directory is unreadable as a ledger even under a privileged test user.
        std::fs::create_dir_all(&path).unwrap();
        assert!(
            latest_valid_version()
                .unwrap_err()
                .contains("Cannot read telemetry ledger")
        );
    }

    #[test]
    fn append_and_read_roundtrip() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        let record = HeartbeatRecord {
            timestamp: "2026-07-30T23:00:00Z".to_string(),
            installation_id: "test-uuid-1234".to_string(),
            version: "3.9.13".to_string(),
            os: "macos".to_string(),
            arch: "aarch64".to_string(),
            schema_version: 2,
            event_names: vec!["heartbeat".to_string()],
            payload_hash: "a".repeat(64),
            endpoint: "https://api.leanctx.com/api/telemetry/v2/batch".to_string(),
            status: "success".to_string(),
        };
        append(&record).unwrap();
        append(&record).unwrap();
        let all = read_all();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].installation_id, "test-uuid-1234");
        assert_eq!(all[0].version, "3.9.13");
        purge_local().expect("purge ledger");
        assert!(read_all().is_empty());
    }
}
