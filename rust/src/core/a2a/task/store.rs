// SPDX-License-Identifier: Apache-2.0

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use super::authority::TaskDescriptorV1;
use super::model::{Task, TaskPart, TaskState};
use super::{
    IDEMPOTENCY_RETENTION_SECONDS, MAX_DESCRIPTION_BYTES, MAX_HISTORY, MAX_ID_BYTES,
    MAX_IDEMPOTENCY_RECORDS, MAX_MESSAGES, MAX_METADATA_ENTRIES, MAX_METADATA_VALUE_BYTES,
    MAX_PARTS, MAX_PARTS_PER_MESSAGE, MAX_TASK_STORE_BYTES, MAX_TASKS, MAX_TEXT_BYTES,
    TASK_DESCRIPTOR_VERSION,
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct TaskIdempotencyRecordV1 {
    schema_version: u32,
    key: String,
    sender: String,
    recipient: String,
    tenant_id: String,
    project_id: String,
    action: String,
    key_id: String,
    descriptor_digest: String,
    task_id: String,
    pub(super) created_at: DateTime<Utc>,
}

impl TaskIdempotencyRecordV1 {
    fn matches_authority(&self, descriptor: &TaskDescriptorV1) -> bool {
        self.sender == descriptor.sender
            && self.recipient == descriptor.recipient
            && self.tenant_id == descriptor.tenant_id
            && self.project_id == descriptor.project_id
            && self.action == descriptor.action
            && self.key_id == descriptor.key_id
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteTaskMaterialization {
    pub task_id: String,
    pub duplicate: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TaskStore {
    pub tasks: Vec<Task>,
    pub updated_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub(super) idempotency: Vec<TaskIdempotencyRecordV1>,
}

impl TaskStore {
    pub fn default_path() -> std::io::Result<PathBuf> {
        task_store_path()
    }

    pub fn load() -> std::io::Result<Self> {
        Self::load_from_path(&task_store_path()?)
    }

    pub fn save(&self) -> std::io::Result<()> {
        let path = task_store_path()?;
        Self::prepare_parent(&path)?;
        let lock_path = path.with_extension("lock");
        let _lock =
            crate::core::agents::FileLock::acquire(&lock_path).map_err(std::io::Error::other)?;
        self.save_to_path(&path)
    }

    pub fn scoped_path(project_root: &str) -> std::io::Result<PathBuf> {
        let root = Path::new(project_root);
        let metadata = std::fs::symlink_metadata(root)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "project root must be a real directory",
            ));
        }
        Ok(root
            .canonicalize()?
            .join(".lean-ctx")
            .join("a2a")
            .join("tasks-v1.json"))
    }

    pub fn read_locked<R>(path: &Path, read: impl FnOnce(&Self) -> R) -> std::io::Result<R> {
        Self::prepare_parent(path)?;
        let _lock = crate::core::agents::FileLock::acquire(&path.with_extension("lock"))
            .map_err(std::io::Error::other)?;
        let store = Self::load_from_path(path)?;
        Ok(read(&store))
    }

    pub fn mutate_locked<R, E>(
        path: &Path,
        mutate: impl FnOnce(&mut Self) -> Result<R, E>,
    ) -> Result<R, E>
    where
        E: From<std::io::Error>,
    {
        Self::prepare_parent(path).map_err(E::from)?;
        let _lock = crate::core::agents::FileLock::acquire(&path.with_extension("lock"))
            .map_err(std::io::Error::other)
            .map_err(E::from)?;
        let mut store = Self::load_from_path(path).map_err(E::from)?;
        let result = mutate(&mut store)?;
        if store.tasks.len() > MAX_TASKS {
            return Err(E::from(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "task store entry limit exceeded",
            )));
        }
        if store.idempotency.len() > MAX_IDEMPOTENCY_RECORDS {
            return Err(E::from(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "task idempotency entry limit exceeded",
            )));
        }
        store.updated_at = Some(Utc::now());
        store.save_to_path(path).map_err(E::from)?;
        Ok(result)
    }

    fn prepare_parent(path: &Path) -> std::io::Result<()> {
        let parent = path.parent().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "task store has no parent")
        })?;
        for directory in [Some(parent), parent.parent()].into_iter().flatten() {
            validate_existing_directory(directory)?;
        }
        std::fs::create_dir_all(parent)?;
        for directory in [Some(parent), parent.parent()].into_iter().flatten() {
            validate_existing_directory(directory)?;
        }
        Ok(())
    }

    fn load_from_path(path: &Path) -> std::io::Result<Self> {
        let metadata = match std::fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => return Err(error),
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "task store must be a regular file",
            ));
        }
        if metadata.len() > MAX_TASK_STORE_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "task store exceeds size limit",
            ));
        }
        let file = open_read_nofollow(path)?;
        let mut content = String::new();
        file.take(MAX_TASK_STORE_BYTES + 1)
            .read_to_string(&mut content)?;
        if content.len() as u64 > MAX_TASK_STORE_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "task store exceeds size limit",
            ));
        }
        let store: Self = serde_json::from_str(&content).map_err(std::io::Error::other)?;
        if store.idempotency.len() > MAX_IDEMPOTENCY_RECORDS {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "task idempotency entry limit exceeded",
            ));
        }
        store.validate_restored()?;
        Ok(store)
    }

    fn validate_restored(&self) -> std::io::Result<()> {
        if self.tasks.len() > MAX_TASKS {
            return invalid_data("task store entry limit exceeded");
        }
        let mut task_ids = std::collections::HashSet::with_capacity(self.tasks.len());
        for task in &self.tasks {
            validate_restored_identifier(&task.id, "task id")?;
            if !task_ids.insert(task.id.as_str()) {
                return invalid_data("task store contains duplicate task ids");
            }
            validate_restored_identifier(&task.from_agent, "from_agent")?;
            validate_restored_identifier(&task.to_agent, "to_agent")?;
            validate_bounded_nonempty(
                &task.description,
                "task description",
                MAX_DESCRIPTION_BYTES,
            )?;
            if task.created_at > task.updated_at {
                return invalid_data("task timestamps are inconsistent");
            }
            if task.messages.len() > MAX_MESSAGES
                || task.artifacts.len() > MAX_PARTS
                || task.history.is_empty()
                || task.history.len() > MAX_HISTORY
                || task.metadata.len() > MAX_METADATA_ENTRIES
            {
                return invalid_data("task collection limit exceeded");
            }
            for message in &task.messages {
                validate_restored_identifier(&message.role, "message role")?;
                if message.timestamp < task.created_at || message.timestamp > task.updated_at {
                    return invalid_data("message timestamp is outside task lifetime");
                }
                if message.parts.len() > MAX_PARTS_PER_MESSAGE {
                    return invalid_data("message part limit exceeded");
                }
                for part in &message.parts {
                    validate_part(part)?;
                }
            }
            for artifact in &task.artifacts {
                validate_part(artifact)?;
            }
            for (key, value) in &task.metadata {
                validate_restored_identifier(key, "metadata key")?;
                validate_bounded(value, "metadata value", MAX_METADATA_VALUE_BYTES)?;
            }
            validate_history(task)?;
        }
        Ok(())
    }

    fn save_to_path(&self, path: &Path) -> std::io::Result<()> {
        // Every writer must preserve the same invariants required on reload.
        self.validate_restored()?;
        if let Ok(metadata) = std::fs::symlink_metadata(path)
            && (metadata.file_type().is_symlink() || !metadata.is_file())
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "task store must be a regular file",
            ));
        }

        let json = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        if json.len() as u64 > MAX_TASK_STORE_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "task store exceeds size limit",
            ));
        }
        let tmp = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&tmp)?;
        file.write_all(json.as_bytes())?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp, path)?;
        sync_parent(path)?;
        Ok(())
    }

    pub fn create_task(&mut self, from: &str, to: &str, description: &str) -> String {
        let task = Task::new(from, to, description);
        let id = task.id.clone();
        self.tasks.push(task);
        self.updated_at = Some(Utc::now());
        id
    }

    /// Durably materialize an already-authorized remote task descriptor.
    ///
    /// The idempotency ledger lives in the same file as the tasks and is
    /// written under the same lock and atomic rename, so a delivery is either
    /// visible with its ledger entry after a restart or not at all — never
    /// acknowledged without being stored.
    ///
    /// Callers MUST verify [`TaskDescriptorV1::verify_authority`] first; this
    /// re-validates shape only, and cannot re-derive authority on its own.
    pub fn materialize_remote_task(
        path: &Path,
        descriptor: &TaskDescriptorV1,
    ) -> std::io::Result<RemoteTaskMaterialization> {
        descriptor
            .validate_shape()
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error))?;
        let digest = descriptor.content_digest();
        Self::mutate_locked(path, |store| {
            let now = Utc::now();
            store.prune_idempotency(now);
            if let Some(existing) = store
                .idempotency
                .iter()
                .find(|record| record.key == descriptor.idempotency_key)
            {
                if existing.matches_authority(descriptor) && existing.descriptor_digest == digest {
                    return Ok(RemoteTaskMaterialization {
                        task_id: existing.task_id.clone(),
                        duplicate: true,
                    });
                }
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    "conflicting idempotency key reuse",
                ));
            }
            if store.idempotency.len() >= MAX_IDEMPOTENCY_RECORDS || store.tasks.len() >= MAX_TASKS
            {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "task idempotency entry limit exceeded",
                ));
            }
            let task_id = format!("task-{}", uuid::Uuid::new_v4());
            let task = Task::from_remote_descriptor(descriptor, task_id.clone());
            store.tasks.push(task);
            store.idempotency.push(TaskIdempotencyRecordV1 {
                schema_version: TASK_DESCRIPTOR_VERSION,
                key: descriptor.idempotency_key.clone(),
                sender: descriptor.sender.clone(),
                recipient: descriptor.recipient.clone(),
                tenant_id: descriptor.tenant_id.clone(),
                project_id: descriptor.project_id.clone(),
                action: descriptor.action.clone(),
                key_id: descriptor.key_id.clone(),
                descriptor_digest: digest,
                task_id: task_id.clone(),
                created_at: now,
            });
            Ok(RemoteTaskMaterialization {
                task_id,
                duplicate: false,
            })
        })
    }

    /// Drop idempotency records that no live descriptor can still reference.
    ///
    /// A descriptor may live at most [`MAX_TASK_LIFETIME_SECONDS`] and is
    /// rejected once expired, so a record older than that plus the accepted
    /// clock skew can never turn a replay into a duplicate. Records with a
    /// creation time in the future (clock moved backwards) are kept.
    fn prune_idempotency(&mut self, now: DateTime<Utc>) {
        let cutoff = now - chrono::Duration::seconds(IDEMPOTENCY_RETENTION_SECONDS);
        self.idempotency.retain(|record| record.created_at > cutoff);
    }

    pub fn get_task(&self, task_id: &str) -> Option<&Task> {
        self.tasks.iter().find(|t| t.id == task_id)
    }

    pub fn get_task_mut(&mut self, task_id: &str) -> Option<&mut Task> {
        self.tasks.iter_mut().find(|t| t.id == task_id)
    }

    pub fn tasks_for_agent(&self, agent_id: &str) -> Vec<&Task> {
        self.tasks
            .iter()
            .filter(|t| t.to_agent == agent_id || t.from_agent == agent_id)
            .collect()
    }

    pub fn pending_tasks_for(&self, agent_id: &str) -> Vec<&Task> {
        self.tasks
            .iter()
            .filter(|t| t.to_agent == agent_id && !t.state.is_terminal())
            .collect()
    }

    pub fn cleanup_old(&mut self, max_age_hours: u64) {
        let now = Utc::now();
        let cutoff = now - chrono::Duration::hours(max_age_hours as i64);
        self.tasks
            .retain(|t| !t.state.is_terminal() || t.updated_at > cutoff);
        self.prune_idempotency(now);
    }
}

fn invalid_data<T>(message: &str) -> std::io::Result<T> {
    Err(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        message,
    ))
}

fn validate_bounded(value: &str, field: &str, max_bytes: usize) -> std::io::Result<()> {
    if value.len() > max_bytes {
        return invalid_data(&format!("invalid {field}"));
    }
    Ok(())
}

fn validate_restored_identifier(value: &str, field: &str) -> std::io::Result<()> {
    validate_bounded_nonempty(value, field, MAX_ID_BYTES)?;
    if value.chars().any(char::is_control) {
        return invalid_data(&format!("invalid {field}"));
    }
    Ok(())
}

fn validate_bounded_nonempty(value: &str, field: &str, max_bytes: usize) -> std::io::Result<()> {
    if value.is_empty() {
        return invalid_data(&format!("invalid {field}"));
    }
    validate_bounded(value, field, max_bytes)
}

fn validate_part(part: &TaskPart) -> std::io::Result<()> {
    match part {
        TaskPart::Text { text } => validate_bounded(text, "text part", MAX_TEXT_BYTES),
        TaskPart::Data { mime_type, data } => {
            validate_restored_identifier(mime_type, "data mime_type")?;
            validate_bounded(data, "data part", MAX_TEXT_BYTES)
        }
        TaskPart::File {
            name,
            mime_type,
            data,
            uri,
        } => {
            validate_restored_identifier(name, "file name")?;
            if let Some(value) = mime_type {
                validate_restored_identifier(value, "file mime_type")?;
            }
            if let Some(value) = data {
                validate_bounded(value, "file data", MAX_TEXT_BYTES)?;
            }
            if let Some(value) = uri {
                validate_bounded_nonempty(value, "file uri", MAX_TEXT_BYTES)?;
            }
            if data.is_none() && uri.is_none() {
                return invalid_data("file part requires data or uri");
            }
            Ok(())
        }
    }
}

fn validate_history(task: &Task) -> std::io::Result<()> {
    let first = &task.history[0];
    if first.from != TaskState::Created || first.to != TaskState::Created {
        return invalid_data("task history has invalid origin");
    }
    if let Some(reason) = &first.reason {
        validate_bounded(reason, "transition reason", MAX_DESCRIPTION_BYTES)?;
    }
    let mut state = TaskState::Created;
    let mut timestamp = first.timestamp;
    for transition in task.history.iter().skip(1) {
        if transition.from != state
            || !transition.from.can_transition_to(&transition.to)
            || transition.timestamp < timestamp
        {
            return invalid_data("task history is inconsistent");
        }
        if let Some(reason) = &transition.reason {
            validate_bounded(reason, "transition reason", MAX_DESCRIPTION_BYTES)?;
        }
        state = transition.to.clone();
        timestamp = transition.timestamp;
    }
    if state != task.state || first.timestamp < task.created_at || timestamp > task.updated_at {
        return invalid_data("task state does not match history");
    }
    Ok(())
}

fn task_store_path() -> std::io::Result<PathBuf> {
    // GH #439: the A2A task store is agent DATA — resolve through the typed dir
    // so a post-split install writes to $XDG_DATA_HOME/lean-ctx/agents instead
    // of a re-created ~/.lean-ctx. Legacy single-dir installs resolve in place.
    crate::core::data_dir::lean_ctx_data_dir()
        .map(|d| d.join("agents").join("tasks.json"))
        .map_err(std::io::Error::other)
}

fn open_read_nofollow(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "task store must be a regular file",
        ));
    }
    Ok(file)
}

fn validate_existing_directory(path: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "task store path components must be real directories",
            ))
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg_attr(not(unix), allow(unused_variables))] // only the unix branch reads it
fn sync_parent(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        File::open(parent)?.sync_all()?;
    }
    Ok(())
}
