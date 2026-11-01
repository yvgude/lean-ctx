use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use fs2::FileExt;
use serde::{Deserialize, Serialize};

use super::{CheckpointId, CheckpointRecord, ProvenanceId, ProvenanceRecord};

const PROVENANCE_FILE: &str = "provenance.jsonl";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "record_type", content = "record", rename_all = "snake_case")]
enum StoredRecord {
    FileTouch(ProvenanceRecord),
    Checkpoint(CheckpointRecord),
}

/// Project-scoped JSONL persistence for edit provenance and checkpoints.
#[derive(Debug, Clone)]
pub struct ProvenanceStore {
    project_hash: String,
    path: PathBuf,
}

impl ProvenanceStore {
    /// Opens the default `<data>/provenance/<project-hash>/provenance.jsonl` store.
    pub fn new(project_hash: impl Into<String>) -> Result<Self, String> {
        let data_dir = crate::core::data_dir::lean_ctx_data_dir()?;
        Self::with_data_dir(data_dir, project_hash)
    }

    /// Opens a store below an explicit data directory, primarily for isolated callers and tests.
    pub fn with_data_dir(
        data_dir: impl AsRef<Path>,
        project_hash: impl Into<String>,
    ) -> Result<Self, String> {
        let project_hash = project_hash.into();
        validate_project_hash(&project_hash)?;
        let path = data_dir
            .as_ref()
            .join("provenance")
            .join(&project_hash)
            .join(PROVENANCE_FILE);
        Ok(Self { project_hash, path })
    }

    pub fn project_hash(&self) -> &str {
        &self.project_hash
    }

    pub fn storage_path(&self) -> &Path {
        &self.path
    }

    /// Persists a file observation, returning an existing record ID for a duplicate operation.
    pub fn record_file_touch(&self, mut record: ProvenanceRecord) -> Result<ProvenanceId, String> {
        self.mutate(|records| {
            if !record.operation_id.is_empty() {
                if let Some(existing) = records.iter().find_map(|entry| match entry {
                    StoredRecord::FileTouch(existing)
                        if existing.operation_id == record.operation_id =>
                    {
                        Some(existing.id.clone())
                    }
                    _ => None,
                }) {
                    return Ok(existing);
                }
            }

            if record.id.is_empty() {
                record.id = generate_id("prov")?;
            }
            if record.project_hash.is_empty() {
                record.project_hash.clone_from(&self.project_hash);
            }
            let id = record.id.clone();
            records.push(StoredRecord::FileTouch(record));
            Ok(id)
        })
    }

    pub fn record_checkpoint(&self, mut record: CheckpointRecord) -> Result<CheckpointId, String> {
        self.mutate(|records| {
            if record.id.is_empty() {
                record.id = generate_id("ckpt")?;
            }
            let id = record.id.clone();
            records.push(StoredRecord::Checkpoint(record));
            Ok(id)
        })
    }

    pub fn query_by_path(&self, path: &str) -> Vec<ProvenanceRecord> {
        self.read_records()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|entry| match entry {
                StoredRecord::FileTouch(record) if record.path == path => Some(record),
                _ => None,
            })
            .collect()
    }

    pub fn query_by_session(&self, session_id: &str) -> Vec<ProvenanceRecord> {
        self.read_records()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|entry| match entry {
                StoredRecord::FileTouch(record) if record.session_id == session_id => Some(record),
                _ => None,
            })
            .collect()
    }

    /// Returns the most recently observed file touches, newest first.
    pub fn query_recent_file_touches(&self, limit: usize) -> Vec<ProvenanceRecord> {
        let mut records = self
            .read_records()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|entry| match entry {
                StoredRecord::FileTouch(record) => Some(record),
                StoredRecord::Checkpoint(_) => None,
            })
            .collect::<Vec<_>>();
        records.sort_by_key(|r| std::cmp::Reverse(r.observed_at));
        records.truncate(limit);
        records
    }

    pub fn query_checkpoints(&self, session_id: Option<&str>) -> Vec<CheckpointRecord> {
        self.read_records()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|entry| match entry {
                StoredRecord::Checkpoint(record)
                    if session_id.is_none_or(|session_id| record.session_id == session_id) =>
                {
                    Some(record)
                }
                _ => None,
            })
            .collect()
    }

    pub(crate) fn update_file_touch(
        &self,
        provenance_id: &str,
        update: impl FnOnce(&mut ProvenanceRecord),
    ) -> Result<(), String> {
        self.mutate(|records| {
            let record = records.iter_mut().find_map(|entry| match entry {
                StoredRecord::FileTouch(record) if record.id == provenance_id => Some(record),
                _ => None,
            });
            let Some(record) = record else {
                return Err(format!("provenance record not found: {provenance_id}"));
            };
            update(record);
            Ok(())
        })
    }

    pub(crate) fn update_checkpoint(
        &self,
        checkpoint_id: &str,
        update: impl FnOnce(&mut CheckpointRecord),
    ) -> Result<(), String> {
        self.mutate(|records| {
            let record = records.iter_mut().find_map(|entry| match entry {
                StoredRecord::Checkpoint(record) if record.id == checkpoint_id => Some(record),
                _ => None,
            });
            let Some(record) = record else {
                return Err(format!("checkpoint record not found: {checkpoint_id}"));
            };
            update(record);
            Ok(())
        })
    }

    fn mutate<T>(
        &self,
        update: impl FnOnce(&mut Vec<StoredRecord>) -> Result<T, String>,
    ) -> Result<T, String> {
        let parent = self.parent_dir()?;
        fs::create_dir_all(&parent)
            .map_err(|error| format!("create provenance directory: {error}"))?;
        let lock = acquire_file_lock(&parent)?;
        let mut records = self.read_records()?;
        let result = update(&mut records)?;
        self.write_records_atomic(&records)?;
        drop(lock);
        Ok(result)
    }

    fn read_records(&self) -> Result<Vec<StoredRecord>, String> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let mut content = String::new();
        File::open(&self.path)
            .and_then(|mut file| file.read_to_string(&mut content))
            .map_err(|error| format!("read provenance records: {error}"))?;
        content
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                serde_json::from_str(line)
                    .map_err(|error| format!("parse provenance record: {error}"))
            })
            .collect()
    }

    fn write_records_atomic(&self, records: &[StoredRecord]) -> Result<(), String> {
        let parent = self.parent_dir()?;
        let temporary = parent.join(format!(".{PROVENANCE_FILE}.{}.tmp", random_hex()?));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|error| format!("create temporary provenance file: {error}"))?;

        let write_result = (|| {
            for record in records {
                let line = serde_json::to_string(record)
                    .map_err(|error| format!("serialize provenance record: {error}"))?;
                writeln!(file, "{line}")
                    .map_err(|error| format!("write provenance record: {error}"))?;
            }
            file.sync_all()
                .map_err(|error| format!("sync provenance records: {error}"))
        })();
        if let Err(error) = write_result {
            let _ = fs::remove_file(&temporary);
            return Err(error);
        }

        fs::rename(&temporary, &self.path)
            .map_err(|error| format!("replace provenance records: {error}"))
    }

    pub(super) fn parent_dir(&self) -> Result<PathBuf, String> {
        self.path
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| "provenance storage path has no parent".to_owned())
    }
}

fn validate_project_hash(project_hash: &str) -> Result<(), String> {
    if project_hash.is_empty()
        || !project_hash
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return Err("project hash must contain only ASCII letters, digits, '-' or '_'".to_owned());
    }
    Ok(())
}

fn acquire_file_lock(dir: &Path) -> Result<File, String> {
    let lock_path = dir.join(".provenance.lock");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path)
        .map_err(|error| format!("open provenance lock: {error}"))?;
    file.lock_exclusive()
        .map_err(|error| format!("lock provenance storage: {error}"))?;
    Ok(file)
}

fn generate_id(prefix: &str) -> Result<String, String> {
    Ok(format!("{prefix}-{}", random_hex()?))
}

fn random_hex() -> Result<String, String> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes)
        .map_err(|error| format!("provenance CSPRNG unavailable: {error}"))?;
    Ok(bytes.iter().fold(String::with_capacity(32), |mut s, byte| {
        use std::fmt::Write;
        let _ = write!(s, "{byte:02x}");
        s
    }))
}
