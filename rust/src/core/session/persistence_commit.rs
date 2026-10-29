// SPDX-License-Identifier: Apache-2.0
//! The commit side of a session save: an admitted save, its authority check
//! inside the commit's locks, and the writes that follow it.

use sha2::{Digest as _, Sha256};

use super::{
    SAVE_LOCK_TIMEOUT, in_request_scope, read_project_head_locked, restrict_file_permissions,
    update_project_index_locked, with_project_index_lock,
};
use crate::core::session::save_outcome::SaveOutcome;
use crate::core::session::{SaveError, SessionState};

/// A prepared save that has passed storage admission: the bytes to publish and
/// the authority they were admitted under. The authority is verified again
/// inside the commit's locks, immediately before the first byte is written.
pub(crate) struct AdmittedSave {
    pub(super) canonical: bool,
    pub(super) expected_storage_digest: Option<lean_ctx_protocol::Sha256Digest>,
    pub(super) dir: std::path::PathBuf,
    pub(super) id: String,
    pub(super) version: u32,
    pub(super) json: String,
    pub(super) pointer_json: String,
    pub(super) compaction_snapshot: Option<String>,
    /// Canonical, safe project root used to update the bounded warm-history
    /// index after the primary session file has been committed.
    pub(super) project_index_root: Option<String>,
    pub(super) request_project: Option<std::path::PathBuf>,
    pub(super) authority: crate::core::session::admission::StorageAuthority,
}

impl AdmittedSave {
    /// Commit an admitted save on its own. Production code reaches the commit
    /// through `PreparedSave`; tests use this to change the authority between
    /// admission and commit.
    #[cfg(test)]
    pub(crate) fn write_to_disk(self) -> Result<(), SaveError> {
        let mut outcome =
            SaveOutcome::new(&self.id, self.version, self.expected_storage_digest.clone());
        self.commit(false, &mut outcome)
    }

    pub(super) fn commit(
        self,
        require_new: bool,
        outcome: &mut SaveOutcome,
    ) -> Result<(), SaveError> {
        let request_project = self.request_project.clone();
        in_request_scope(request_project, || {
            self.write_primary_and_pointers(require_new, outcome)
        })
    }

    pub(super) fn write_new_if_project_head(
        self,
        expected: Option<&(String, String)>,
    ) -> Result<Option<SaveOutcome>, String> {
        let Some(project_root) = self.project_index_root.clone() else {
            return Err("project head publication requires a safe project root".to_string());
        };
        let dir = self.dir.clone();
        let request_project = self.request_project.clone();
        let mut outcome =
            SaveOutcome::new(&self.id, self.version, self.expected_storage_digest.clone());
        let published = in_request_scope(request_project, || {
            with_project_index_lock(&dir, &project_root, |index_path| {
                let current = read_project_head_locked(&dir, &project_root)?;
                if current.as_ref() != expected {
                    return Ok(false);
                }
                let result = self.write_primary_and_pointers_locked(
                    true,
                    &mut outcome,
                    Some((&project_root, index_path)),
                );
                outcome.result = result;
                Ok(true)
            })
        })?;
        if published {
            Ok(Some(outcome))
        } else {
            Ok(None)
        }
    }

    fn write_primary_and_pointers(
        self,
        require_new: bool,
        outcome: &mut SaveOutcome,
    ) -> Result<(), SaveError> {
        if !self.dir.exists() {
            std::fs::create_dir_all(&self.dir)
                .map_err(|error| SaveError::Storage(error.to_string()))?;
        }
        if let Some(project_root) = self.project_index_root.clone() {
            let dir = self.dir.clone();
            return with_project_index_lock(&dir, &project_root, |index_path| {
                Ok(self.write_primary_and_pointers_locked(
                    require_new,
                    outcome,
                    Some((&project_root, index_path)),
                ))
            })
            .map_err(SaveError::Storage)?;
        }
        self.write_primary_and_pointers_locked(require_new, outcome, None)
    }

    /// The commit. With the project-index lock (when the session has a project)
    /// and the per-session save lock held, the admitting authority is verified
    /// against current configuration and policy bytes; only then are the
    /// primary file, latest pointer, compaction snapshot and project index
    /// written. A changed authority, a deleted project root or an unavailable
    /// policy refresh publishes none of them.
    fn write_primary_and_pointers_locked(
        self,
        require_new: bool,
        outcome: &mut SaveOutcome,
        project_index: Option<(&str, &std::path::Path)>,
    ) -> Result<(), SaveError> {
        use fs2::FileExt;
        let lock_path = self.dir.join(format!(".{}.save.lock", self.id));
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(lock_path)
            .map_err(|e| SaveError::Storage(format!("open session save lock: {e}")))?;
        crate::core::file_lock::acquire_exclusive_timeout(&lock, SAVE_LOCK_TIMEOUT)
            .map_err(|e| SaveError::Storage(format!("lock session save: {e}")))?;

        if let Err(detail) = self.authority.verify() {
            let _ = FileExt::unlock(&lock);
            return Err(SaveError::authority_changed(&detail));
        }

        let result: Result<(), String> = (|| {
            let path = self.dir.join(format!("{}.json", self.id));
            if require_new && path.try_exists().map_err(|error| error.to_string())? {
                return Err("import destination already exists".to_string());
            }
            if !self.canonical
                && persisted_session_version(&path).is_some_and(|version| version > self.version)
            {
                return Ok(());
            }
            let previous = match std::fs::read(&path) {
                Ok(bytes) => Some(bytes),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => return Err(error.to_string()),
            };
            let json = crate::core::context_checkpoint::prepare_session_checkpoint_commit(
                &self.json,
                previous.as_deref(),
            )?;
            let committed = SessionState::from_storage_json(&json)?.canonical_checkpoint;
            outcome.primary_sha256 = Some(hex::encode(Sha256::digest(json.as_bytes())));
            let tmp = self.dir.join(format!(".{}.json.tmp", self.id));
            if require_new {
                use std::io::Write as _;
                let mut options = std::fs::OpenOptions::new();
                options.write(true).create_new(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt as _;
                    options.mode(0o600);
                }
                let mut file = options.open(&tmp).map_err(|error| error.to_string())?;
                file.write_all(json.as_bytes())
                    .map_err(|error| error.to_string())?;
                file.sync_all().map_err(|error| error.to_string())?;
            } else {
                std::fs::write(&tmp, &json).map_err(|e| e.to_string())?;
            }
            restrict_file_permissions(&tmp);
            if require_new {
                // Atomic no-replace publication, including non-cooperating
                // writers that do not acquire this session's file lock.
                std::fs::hard_link(&tmp, &path).map_err(|error| error.to_string())?;
                outcome.committed = committed;
                std::fs::remove_file(&tmp).map_err(|error| error.to_string())?;
            } else {
                std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
                outcome.committed = committed;
            }

            let latest_path = self.dir.join("latest.json");
            let latest_tmp = self.dir.join(format!(".{}.latest.tmp", self.id));
            std::fs::write(&latest_tmp, &self.pointer_json).map_err(|e| e.to_string())?;
            restrict_file_permissions(&latest_tmp);
            std::fs::rename(&latest_tmp, &latest_path).map_err(|e| e.to_string())?;

            if let Some(snapshot) = self.compaction_snapshot {
                let snap_path = self.dir.join(format!("{}_snapshot.txt", self.id));
                if let Err(error) = crate::core::atomic_fs::write_bytes_with_fallback(
                    &snap_path,
                    snapshot.as_bytes(),
                    None,
                ) {
                    tracing::debug!("lean-ctx: compaction snapshot update skipped: {error}");
                } else {
                    restrict_file_permissions(&snap_path);
                }
            }
            if let Some((project_root, index_path)) = project_index
                && let Err(error) =
                    update_project_index_locked(&self.dir, project_root, index_path, &self.id)
            {
                tracing::debug!("lean-ctx: session warm-history index update skipped: {error}");
            }
            Ok(())
        })();
        let _ = FileExt::unlock(&lock);
        result.map_err(SaveError::Storage)
    }
}

fn persisted_session_version(path: &std::path::Path) -> Option<u32> {
    let value: serde_json::Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    value["version"].as_u64()?.try_into().ok()
}
