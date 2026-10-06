use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::core::agents::{AgentRegistry, FileLock};
use crate::core::ocla::types::{OclaError, OclaResult};

const STORE_VERSION: u16 = 1;
const MAX_ENTRIES_PER_SCOPE: usize = 1_000;
const MAX_STORE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_MESSAGE_BYTES: usize = 2 * 1024 * 1024;
const MAX_IDENTIFIER_BYTES: usize = 512;
pub(crate) const MAX_ERROR_BYTES: usize = 64 * 1024;
const LOCK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub enum DeadLetterDelivery {
    LocalAgentBus,
    RemoteHttp { endpoint_url: String },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DlqScope {
    pub tenant_id: String,
    pub project_id: String,
}

impl DlqScope {
    pub fn new(tenant_id: impl Into<String>, project_id: impl Into<String>) -> OclaResult<Self> {
        let scope = Self {
            tenant_id: tenant_id.into(),
            project_id: project_id.into(),
        };
        scope.validate()?;
        Ok(scope)
    }

    fn validate(&self) -> OclaResult<()> {
        validate_identifier("tenant_id", &self.tenant_id)?;
        validate_identifier("project_id", &self.project_id)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeadLetter {
    pub id: String,
    #[serde(default)]
    pub peer_id: String,
    #[serde(default)]
    pub delivery_id: String,
    pub tenant_id: String,
    pub project_id: String,
    pub delivery: DeadLetterDelivery,
    pub original_message: String,
    pub target_agent: String,
    pub error: String,
    pub attempts: u8,
    pub first_failed_at: String,
    pub last_failed_at: String,
}

impl DeadLetter {
    fn validate(&self) -> OclaResult<()> {
        validate_identifier("id", &self.id)?;
        validate_identifier("peer_id", &self.peer_id)?;
        validate_identifier("delivery_id", &self.delivery_id)?;
        DlqScope::new(self.tenant_id.clone(), self.project_id.clone())?;
        validate_identifier("target_agent", &self.target_agent)?;
        if self.original_message.is_empty() || self.original_message.len() > MAX_MESSAGE_BYTES {
            return Err(OclaError::InvalidRequest(
                "dead letter message must be non-empty and bounded".into(),
            ));
        }
        if self.error.is_empty() || self.error.len() > MAX_ERROR_BYTES {
            return Err(OclaError::InvalidRequest(
                "dead letter error must be non-empty and bounded".into(),
            ));
        }
        if self.attempts == 0 {
            return Err(OclaError::InvalidRequest(
                "dead letter attempts must be greater than zero".into(),
            ));
        }
        let first_failed_at = DateTime::parse_from_rfc3339(&self.first_failed_at)
            .map_err(|_| OclaError::InvalidRequest("invalid first_failed_at".into()))?;
        let last_failed_at = DateTime::parse_from_rfc3339(&self.last_failed_at)
            .map_err(|_| OclaError::InvalidRequest("invalid last_failed_at".into()))?;
        if last_failed_at < first_failed_at {
            return Err(OclaError::InvalidRequest(
                "last_failed_at must not precede first_failed_at".into(),
            ));
        }
        if let DeadLetterDelivery::RemoteHttp { endpoint_url } = &self.delivery {
            let url = reqwest::Url::parse(endpoint_url).map_err(|_| {
                OclaError::InvalidRequest("invalid dead letter endpoint_url".into())
            })?;
            if !matches!(url.scheme(), "http" | "https")
                || url.host_str().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
            {
                return Err(OclaError::InvalidRequest(
                    "dead letter endpoint_url must be absolute HTTP(S)".into(),
                ));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DlqStats {
    pub total: usize,
    pub max_scope_depth: usize,
    pub oldest_age_seconds: u64,
    pub by_target_agent: BTreeMap<String, usize>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DurableDlqV1 {
    version: u16,
    entries: Vec<DeadLetter>,
}

impl Default for DurableDlqV1 {
    fn default() -> Self {
        Self {
            version: STORE_VERSION,
            entries: Vec::new(),
        }
    }
}

#[derive(Clone)]
enum QueueBackend {
    Memory(Arc<Mutex<Vec<DeadLetter>>>),
    Durable {
        root: PathBuf,
        process_lock: Arc<Mutex<()>>,
    },
    Unavailable(String),
}

#[derive(Clone)]
pub struct DeadLetterQueue {
    backend: QueueBackend,
}

pub struct DlqRetryGuard {
    _lock: Option<FileLock>,
}

impl Default for DeadLetterQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl DeadLetterQueue {
    #[must_use]
    pub fn new() -> Self {
        Self {
            backend: QueueBackend::Memory(Arc::new(Mutex::new(Vec::new()))),
        }
    }

    pub fn persistent_default() -> std::io::Result<Self> {
        let path = crate::core::data_dir::lean_ctx_data_dir()
            .map_err(std::io::Error::other)?
            .join("agents")
            .join("a2a")
            .join("dead-letters-v1.json");
        Self::persistent(path)
    }

    pub fn persistent(path: impl AsRef<Path>) -> std::io::Result<Self> {
        let path = path.as_ref();
        let root = path.with_extension("d");
        if path.exists() {
            prepare_parent(&path)?;
            prepare_store_directory(&root)?;
            migrate_legacy_store(&path, &root)?;
        } else {
            for directory in [path.parent(), path.parent().and_then(Path::parent)]
                .into_iter()
                .flatten()
            {
                validate_directory(directory)?;
            }
            validate_directory(&root)?;
        }
        let queue = Self {
            backend: QueueBackend::Durable {
                root,
                process_lock: Arc::new(Mutex::new(())),
            },
        };
        Ok(queue)
    }

    #[must_use]
    pub fn unavailable(error: impl Into<String>) -> Self {
        Self {
            backend: QueueBackend::Unavailable(error.into()),
        }
    }

    pub fn enqueue(&self, letter: DeadLetter) -> OclaResult<()> {
        letter.validate()?;
        let scope = DlqScope::new(letter.tenant_id.clone(), letter.project_id.clone())?;
        self.with_entries(Some(&scope), true, |entries| {
            if let Some(existing) = entries.iter_mut().find(|entry| {
                entry.peer_id == letter.peer_id
                    && entry.delivery_id == letter.delivery_id
                    && entry.tenant_id == letter.tenant_id
                    && entry.project_id == letter.project_id
            }) {
                existing.error = letter.error;
                existing.attempts = existing.attempts.max(letter.attempts);
                existing.last_failed_at = letter.last_failed_at;
                return Ok(());
            }
            if entries
                .iter()
                .filter(|entry| {
                    entry.tenant_id == letter.tenant_id && entry.project_id == letter.project_id
                })
                .count()
                >= MAX_ENTRIES_PER_SCOPE
                && let Some(position) = entries.iter().position(|entry| {
                    entry.tenant_id == letter.tenant_id && entry.project_id == letter.project_id
                })
            {
                entries.remove(position);
            }
            entries.push(letter);
            while serialized_store_size(entries)? > MAX_STORE_BYTES {
                if entries.len() == 1 {
                    return Err(OclaError::InvalidRequest(
                        "dead letter exceeds per-scope store size limit".into(),
                    ));
                }
                entries.remove(0);
            }
            Ok(())
        })
    }

    pub fn dequeue(&self, scope: &DlqScope, id: &str) -> OclaResult<Option<DeadLetter>> {
        scope.validate()?;
        validate_identifier("id", id)?;
        self.with_entries(Some(scope), true, |entries| {
            let position = entries.iter().position(|letter| {
                letter.id == id
                    && letter.tenant_id == scope.tenant_id
                    && letter.project_id == scope.project_id
            });
            Ok(position.map(|position| entries.remove(position)))
        })
    }

    pub fn peek(&self, scope: &DlqScope) -> OclaResult<Vec<DeadLetter>> {
        scope.validate()?;
        self.with_entries(Some(scope), false, |entries| {
            Ok(entries
                .iter()
                .filter(|letter| {
                    letter.tenant_id == scope.tenant_id && letter.project_id == scope.project_id
                })
                .cloned()
                .collect())
        })
    }

    pub fn get(&self, scope: &DlqScope, id: &str) -> OclaResult<Option<DeadLetter>> {
        scope.validate()?;
        validate_identifier("id", id)?;
        self.with_entries(Some(scope), false, |entries| {
            Ok(entries
                .iter()
                .find(|letter| {
                    letter.id == id
                        && letter.tenant_id == scope.tenant_id
                        && letter.project_id == scope.project_id
                })
                .cloned())
        })
    }

    pub fn lock_retry(&self, scope: &DlqScope, id: &str) -> OclaResult<DlqRetryGuard> {
        scope.validate()?;
        validate_identifier("id", id)?;
        match &self.backend {
            QueueBackend::Durable { root, .. } => {
                // One stable lock file per scope prevents duplicate retries across
                // processes without globally serializing unrelated tenants or
                // accumulating one persistent lock artifact per dead letter.
                let path = scope_store_path(root, scope).with_extension("retry.lock");
                let lock = FileLock::acquire_with_timeout(&path, LOCK_TIMEOUT)
                    .map_err(|error| store_error(&std::io::Error::other(error)))?;
                Ok(DlqRetryGuard { _lock: Some(lock) })
            }
            QueueBackend::Memory(_) => Ok(DlqRetryGuard { _lock: None }),
            QueueBackend::Unavailable(error) => Err(OclaError::InvalidRequest(format!(
                "dead letter store unavailable: {error}"
            ))),
        }
    }

    pub fn retry(&self, scope: &DlqScope, id: &str) -> OclaResult<()> {
        scope.validate()?;
        validate_identifier("id", id)?;
        let retry_error = self.with_entries(Some(scope), true, |entries| {
            let position = entries
                .iter()
                .position(|letter| letter.id == id)
                .ok_or_else(|| OclaError::InvalidRequest(format!("dead letter not found: {id}")))?;
            if !matches!(
                entries[position].delivery,
                DeadLetterDelivery::LocalAgentBus
            ) {
                return Err(OclaError::InvalidRequest(
                    "remote dead letters require the configured authenticated transport".into(),
                ));
            }
            match resend_local(&entries[position]) {
                Ok(()) => {
                    entries.remove(position);
                    Ok(None)
                }
                Err(error) => {
                    entries[position].attempts = entries[position].attempts.saturating_add(1);
                    entries[position].last_failed_at = Utc::now().to_rfc3339();
                    Ok(Some(error))
                }
            }
        })?;
        retry_error.map_or(Ok(()), Err)
    }

    pub fn stats(&self, scope: Option<&DlqScope>) -> OclaResult<DlqStats> {
        if let Some(scope) = scope {
            scope.validate()?;
        }
        self.with_entries(scope, false, |entries| {
            let now = Utc::now();
            let mut oldest_age_seconds = 0;
            let mut by_target_agent = BTreeMap::new();
            let mut by_scope = BTreeMap::new();
            let mut total = 0;
            for letter in entries.iter().filter(|letter| {
                scope.is_none_or(|scope| {
                    letter.tenant_id == scope.tenant_id && letter.project_id == scope.project_id
                })
            }) {
                total += 1;
                *by_scope
                    .entry((&letter.tenant_id, &letter.project_id))
                    .or_insert(0_usize) += 1;
                *by_target_agent
                    .entry(letter.target_agent.clone())
                    .or_insert(0) += 1;
                if let Ok(failed_at) = DateTime::parse_from_rfc3339(&letter.first_failed_at) {
                    let age = u64::try_from((now - failed_at.with_timezone(&Utc)).num_seconds())
                        .unwrap_or(0);
                    oldest_age_seconds = oldest_age_seconds.max(age);
                }
            }
            Ok(DlqStats {
                total,
                max_scope_depth: by_scope.values().copied().max().unwrap_or(0),
                oldest_age_seconds,
                by_target_agent,
            })
        })
    }

    pub fn ensure_writable(&self, scope: &DlqScope) -> OclaResult<()> {
        scope.validate()?;
        match &self.backend {
            QueueBackend::Memory(_) => Ok(()),
            QueueBackend::Durable { root, process_lock } => {
                let _process_guard = process_lock
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                prepare_store_directory(root).map_err(|error| store_error(&error))?;
                let path = scope_store_path(root, scope);
                let _lock =
                    FileLock::acquire_with_timeout(&path.with_extension("lock"), LOCK_TIMEOUT)
                        .map_err(|error| store_error(&std::io::Error::other(error)))?;
                load_store(&path, Some(scope)).map_err(|error| store_error(&error))?;
                probe_store_directory(root).map_err(|error| store_error(&error))
            }
            QueueBackend::Unavailable(error) => Err(OclaError::InvalidRequest(format!(
                "dead letter store unavailable: {error}"
            ))),
        }
    }

    fn with_entries<R>(
        &self,
        scope: Option<&DlqScope>,
        mutate: bool,
        operation: impl FnOnce(&mut Vec<DeadLetter>) -> OclaResult<R>,
    ) -> OclaResult<R> {
        match &self.backend {
            QueueBackend::Memory(entries) => {
                let mut entries = entries
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                operation(&mut entries)
            }
            QueueBackend::Durable { root, process_lock } => {
                let _process_guard = process_lock
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if !root.exists() && !mutate {
                    return operation(&mut Vec::new());
                }
                prepare_store_directory(root).map_err(|error| store_error(&error))?;
                if let Some(scope) = scope {
                    let path = scope_store_path(root, scope);
                    let _lock =
                        FileLock::acquire_with_timeout(&path.with_extension("lock"), LOCK_TIMEOUT)
                            .map_err(|error| store_error(&std::io::Error::other(error)))?;
                    let mut store =
                        load_store(&path, Some(scope)).map_err(|error| store_error(&error))?;
                    let result = operation(&mut store.entries)?;
                    if mutate {
                        save_store(&path, &store).map_err(|error| store_error(&error))?;
                    }
                    Ok(result)
                } else {
                    if mutate {
                        return Err(OclaError::InvalidRequest(
                            "unscoped durable DLQ mutation is forbidden".into(),
                        ));
                    }
                    let mut entries =
                        load_all_scope_entries(root).map_err(|error| store_error(&error))?;
                    operation(&mut entries)
                }
            }
            QueueBackend::Unavailable(error) => Err(OclaError::InvalidRequest(format!(
                "dead letter store unavailable: {error}"
            ))),
        }
    }
}

fn validate_identifier(label: &str, value: &str) -> OclaResult<()> {
    if value.trim().is_empty() || value.len() > MAX_IDENTIFIER_BYTES || value.contains('\0') {
        return Err(OclaError::InvalidRequest(format!("invalid {label}")));
    }
    Ok(())
}

fn store_error(error: &std::io::Error) -> OclaError {
    OclaError::InvalidRequest(format!("dead letter store unavailable: {error}"))
}

fn resend_local(letter: &DeadLetter) -> OclaResult<()> {
    AgentRegistry::mutate_locked(|registry| {
        registry
            .post_message(
                "dead-letter-queue",
                Some(&letter.target_agent),
                "retry",
                &letter.original_message,
            )
            .map(|_| ())
    })
    .and_then(|(_, result)| result)
    .map_err(|error| OclaError::InvalidRequest(format!("dead letter retry failed: {error}")))
}

fn prepare_parent(path: &Path) -> std::io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "DLQ store has no parent")
    })?;
    #[cfg(unix)] // only the unix permission branch below reads it
    let parent_existed = parent.exists();
    for directory in [Some(parent), parent.parent()].into_iter().flatten() {
        validate_directory(directory)?;
    }
    std::fs::create_dir_all(parent)?;
    for directory in [Some(parent), parent.parent()].into_iter().flatten() {
        validate_directory(directory)?;
    }
    #[cfg(unix)]
    if !parent_existed {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn validate_directory(path: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "DLQ path components must be real directories",
            ))
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn prepare_store_directory(root: &Path) -> std::io::Result<()> {
    let parent = root.parent().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "DLQ root has no parent")
    })?;
    validate_directory(parent)?;
    validate_directory(root)?;
    std::fs::create_dir_all(root)?;
    validate_directory(root)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        use std::os::unix::fs::PermissionsExt;
        let metadata = std::fs::metadata(root)?;
        // SAFETY: `geteuid` has no arguments and only reads process credentials.
        if metadata.uid() != unsafe { libc::geteuid() } {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "DLQ store directory must be owned by the current user",
            ));
        }
        std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn scope_store_path(root: &Path, scope: &DlqScope) -> PathBuf {
    let mut material = Vec::with_capacity(scope.tenant_id.len() + scope.project_id.len() + 1);
    material.extend_from_slice(scope.tenant_id.as_bytes());
    material.push(0);
    material.extend_from_slice(scope.project_id.as_bytes());
    root.join(format!("{}.json", blake3::hash(&material).to_hex()))
}

fn serialized_store_size(entries: &[DeadLetter]) -> OclaResult<u64> {
    serde_json::to_vec_pretty(&DurableDlqV1 {
        version: STORE_VERSION,
        entries: entries.to_vec(),
    })
    .map(|json| json.len() as u64)
    .map_err(|error| OclaError::InvalidRequest(format!("failed to size dead letters: {error}")))
}

fn load_all_scope_entries(root: &Path) -> std::io::Result<Vec<DeadLetter>> {
    let mut entries = Vec::new();
    for item in std::fs::read_dir(root)? {
        let item = item?;
        let file_type = item.file_type()?;
        let name = item.file_name();
        let name = name.to_str().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "invalid DLQ store filename",
            )
        })?;
        if item.path().extension() == Some(std::ffi::OsStr::new("lock"))
            || name == "migration-v1.complete"
        {
            continue;
        }
        if file_type.is_file() && is_scope_store_temp_name(name) {
            continue;
        }
        if file_type.is_symlink() || !file_type.is_file() || !is_scope_store_name(name) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "DLQ root contains an unexpected entry",
            ));
        }
        let store = load_store(&item.path(), None)?;
        if let Some(first) = store.entries.first() {
            let scope = DlqScope::new(first.tenant_id.clone(), first.project_id.clone())
                .map_err(std::io::Error::other)?;
            if scope_store_path(root, &scope) != item.path()
                || store.entries.iter().any(|entry| {
                    entry.tenant_id != scope.tenant_id || entry.project_id != scope.project_id
                })
            {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "DLQ scope store identity does not match its contents",
                ));
            }
        }
        entries.extend(store.entries);
    }
    Ok(entries)
}

fn is_scope_store_name(name: &str) -> bool {
    let path = Path::new(name);
    path.extension() == Some(std::ffi::OsStr::new("json"))
        && path.file_stem().is_some_and(|stem| {
            let bytes = stem.as_encoded_bytes();
            bytes.len() == 64 && bytes.iter().all(u8::is_ascii_hexdigit)
        })
}

fn is_scope_store_temp_name(name: &str) -> bool {
    let Some((store_name, suffix)) = name.split_once(".tmp-") else {
        return false;
    };
    !suffix.is_empty()
        && suffix
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() || byte == b'-')
        && store_name.len() == 64
        && store_name.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn probe_store_directory(root: &Path) -> std::io::Result<()> {
    let path = root.join(format!("writable-{}.lock", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.custom_flags(0x0020_0000);
        }
        options.open(&path)?.sync_all()?;
        std::fs::remove_file(&path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&path);
    }
    result
}

fn migrate_legacy_store(legacy_path: &Path, root: &Path) -> std::io::Result<()> {
    if !legacy_path.exists() || root.join("migration-v1.complete").exists() {
        return Ok(());
    }
    let _migration_lock =
        FileLock::acquire_with_timeout(&root.join("migration-v1.lock"), LOCK_TIMEOUT)
            .map_err(std::io::Error::other)?;
    let marker = root.join("migration-v1.complete");
    if marker.exists() {
        return Ok(());
    }
    let legacy = load_store(legacy_path, None)?;
    let mut grouped: BTreeMap<(String, String), Vec<DeadLetter>> = BTreeMap::new();
    for letter in legacy.entries {
        grouped
            .entry((letter.tenant_id.clone(), letter.project_id.clone()))
            .or_default()
            .push(letter);
    }
    for ((tenant_id, project_id), letters) in grouped {
        let scope = DlqScope::new(tenant_id, project_id).map_err(std::io::Error::other)?;
        let path = scope_store_path(root, &scope);
        let _scope_lock =
            FileLock::acquire_with_timeout(&path.with_extension("lock"), LOCK_TIMEOUT)
                .map_err(std::io::Error::other)?;
        let mut current = load_store(&path, Some(&scope))?;
        for letter in letters {
            if let Some(existing) = current
                .entries
                .iter_mut()
                .find(|entry| entry.id == letter.id)
            {
                if letter.last_failed_at > existing.last_failed_at {
                    *existing = letter;
                }
            } else {
                current.entries.push(letter);
            }
        }
        if current.entries.len() > MAX_ENTRIES_PER_SCOPE {
            let excess = current.entries.len() - MAX_ENTRIES_PER_SCOPE;
            current.entries.drain(..excess);
        }
        while serialized_store_size(&current.entries).map_err(std::io::Error::other)?
            > MAX_STORE_BYTES
        {
            if current.entries.is_empty() {
                break;
            }
            current.entries.remove(0);
        }
        save_store(&path, &current)?;
    }
    write_marker(&marker)?;
    let backup = legacy_path.with_extension("json.migrated-v1");
    if !backup.exists() {
        std::fs::rename(legacy_path, backup)?;
        sync_parent(legacy_path)?;
    }
    Ok(())
}

fn write_marker(path: &Path) -> std::io::Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x0020_0000);
    }
    let mut file = options.open(path)?;
    file.write_all(b"v1\n")?;
    file.sync_all()?;
    sync_parent(path)
}

fn load_store(path: &Path, expected_scope: Option<&DlqScope>) -> std::io::Result<DurableDlqV1> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(DurableDlqV1::default());
        }
        Err(error) => return Err(error),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "DLQ store must be a regular file",
        ));
    }
    if metadata.len() > MAX_STORE_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "DLQ store exceeds size limit",
        ));
    }
    let mut content = Vec::new();
    open_read_nofollow(path)?
        .take(MAX_STORE_BYTES + 1)
        .read_to_end(&mut content)?;
    if content.len() as u64 > MAX_STORE_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "DLQ store exceeds size limit",
        ));
    }
    let mut store: DurableDlqV1 =
        serde_json::from_slice(&content).map_err(std::io::Error::other)?;
    if store.version != STORE_VERSION || store.entries.len() > MAX_ENTRIES_PER_SCOPE {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "invalid DLQ store bounds or version",
        ));
    }
    for entry in &mut store.entries {
        // Older v1 entries did not carry peer-scoped delivery identity. Keep
        // them readable while making the compatibility scope explicit.
        if entry.peer_id.is_empty() {
            entry.peer_id = "legacy".to_string();
        }
        if entry.delivery_id.is_empty() {
            entry.delivery_id = entry.id.clone();
        }
        entry
            .validate()
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        if expected_scope.is_some_and(|scope| {
            entry.tenant_id != scope.tenant_id || entry.project_id != scope.project_id
        }) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "DLQ scope store contains an entry from another scope",
            ));
        }
    }
    Ok(store)
}

fn save_store(path: &Path, store: &DurableDlqV1) -> std::io::Result<()> {
    if let Ok(metadata) = std::fs::symlink_metadata(path)
        && (metadata.file_type().is_symlink() || !metadata.is_file())
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "DLQ store must be a regular file",
        ));
    }
    let json = serde_json::to_vec_pretty(store).map_err(std::io::Error::other)?;
    if json.len() as u64 > MAX_STORE_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "DLQ store exceeds size limit",
        ));
    }
    let tmp = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.custom_flags(0x0020_0000);
        }
        let mut file = options.open(&tmp)?;
        file.write_all(&json)?;
        file.sync_all()?;
        drop(file);
        atomic_replace(&tmp, path)?;
        sync_parent(path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

fn open_read_nofollow(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x0020_0000);
    }
    options.open(path)
}

#[cfg(not(windows))]
fn atomic_replace(source: &Path, destination: &Path) -> std::io::Result<()> {
    std::fs::rename(source, destination)
}

#[cfg(windows)]
fn atomic_replace(source: &Path, destination: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };

    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    // SAFETY: both vectors own the UTF-16 buffers, include a terminating NUL,
    // and remain alive for the duration of the call.
    let result = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if result == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn sync_parent(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        File::open(parent)?.sync_all()?;
    }
    // `MoveFileExW(MOVEFILE_WRITE_THROUGH)` above waits for the replacement to
    // reach disk. Windows does not support `FlushFileBuffers` on directory
    // handles, so attempting the Unix parent-fsync pattern returns ACCESS_DENIED
    // after an otherwise successful atomic replacement.
    #[cfg(windows)]
    let _ = path;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope(tenant: &str, project: &str) -> DlqScope {
        DlqScope::new(tenant, project).unwrap()
    }
    fn letter(id: &str, tenant: &str, project: &str) -> DeadLetter {
        DeadLetter {
            id: id.into(),
            peer_id: "legacy".into(),
            delivery_id: id.into(),
            tenant_id: tenant.into(),
            project_id: project.into(),
            delivery: DeadLetterDelivery::LocalAgentBus,
            original_message: format!("message-{id}"),
            target_agent: "agent-a".into(),
            error: "delivery failed".into(),
            attempts: 1,
            first_failed_at: "2026-01-01T00:00:00Z".into(),
            last_failed_at: "2026-01-01T00:00:00Z".into(),
        }
    }

    #[test]
    fn scoped_operations_do_not_cross_tenant_or_project() {
        let queue = DeadLetterQueue::new();
        queue
            .enqueue(letter("one", "tenant-a", "project-a"))
            .unwrap();
        queue
            .enqueue(letter("one", "tenant-b", "project-a"))
            .unwrap();
        assert_eq!(
            queue.peek(&scope("tenant-a", "project-a")).unwrap().len(),
            1
        );
        assert!(
            queue
                .dequeue(&scope("tenant-a", "project-b"), "one")
                .unwrap()
                .is_none()
        );
        assert_eq!(queue.stats(None).unwrap().total, 2);
    }

    #[test]
    fn persistent_queue_survives_reopen_and_deduplicates() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a2a").join("dead-letters-v1.json");
        let queue = DeadLetterQueue::persistent(path.clone()).unwrap();
        queue
            .enqueue(letter("one", "tenant-a", "project-a"))
            .unwrap();
        queue
            .enqueue(letter("one", "tenant-a", "project-a"))
            .unwrap();
        drop(queue);
        assert_eq!(
            DeadLetterQueue::persistent(path)
                .unwrap()
                .peek(&scope("tenant-a", "project-a"))
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn concurrent_persistent_enqueues_do_not_lose_entries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a2a").join("dead-letters-v1.json");
        let queue = DeadLetterQueue::persistent(path.clone()).unwrap();
        let workers: Vec<_> = (0..8)
            .map(|worker| {
                let queue = queue.clone();
                std::thread::spawn(move || {
                    for item in 0..25 {
                        queue
                            .enqueue(letter(
                                &format!("worker-{worker}-item-{item}"),
                                "tenant-a",
                                "project-a",
                            ))
                            .unwrap();
                    }
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
        drop(queue);
        assert_eq!(
            DeadLetterQueue::persistent(path)
                .unwrap()
                .peek(&scope("tenant-a", "project-a"))
                .unwrap()
                .len(),
            200
        );
    }

    #[test]
    fn corrupt_oversized_and_symlink_stores_fail_closed() {
        let dir = tempfile::tempdir().unwrap();
        let corrupt = dir.path().join("corrupt.json");
        std::fs::write(&corrupt, b"not json").unwrap();
        assert!(DeadLetterQueue::persistent(corrupt).is_err());
        let oversized = dir.path().join("oversized.json");
        std::fs::write(&oversized, vec![b'x'; (MAX_STORE_BYTES + 1) as usize]).unwrap();
        assert!(DeadLetterQueue::persistent(oversized).is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let target = dir.path().join("target.json");
            std::fs::write(&target, b"{}").unwrap();
            let link = dir.path().join("link.json");
            symlink(&target, &link).unwrap();
            assert!(DeadLetterQueue::persistent(link).is_err());
        }
    }

    #[test]
    fn remote_entries_cannot_be_retried_as_local_messages() {
        let queue = DeadLetterQueue::new();
        let mut item = letter("one", "tenant-a", "project-a");
        item.delivery = DeadLetterDelivery::RemoteHttp {
            endpoint_url: "https://agent.example".into(),
        };
        queue.enqueue(item).unwrap();
        assert!(queue.retry(&scope("tenant-a", "project-a"), "one").is_err());
        assert_eq!(queue.stats(None).unwrap().total, 1);
    }

    #[test]
    fn invalid_attempts_time_order_and_endpoint_credentials_are_rejected() {
        let queue = DeadLetterQueue::new();
        let mut zero_attempts = letter("zero", "tenant-a", "project-a");
        zero_attempts.attempts = 0;
        assert!(queue.enqueue(zero_attempts).is_err());

        let mut reversed = letter("reversed", "tenant-a", "project-a");
        reversed.last_failed_at = "2025-01-01T00:00:00Z".into();
        assert!(queue.enqueue(reversed).is_err());

        let mut credential_url = letter("credentials", "tenant-a", "project-a");
        credential_url.delivery = DeadLetterDelivery::RemoteHttp {
            endpoint_url: "https://user:secret@agent.example".into(),
        };
        assert!(queue.enqueue(credential_url).is_err());
        assert_eq!(queue.stats(None).unwrap().total, 0);
    }

    #[test]
    fn per_scope_capacity_never_evicts_another_scope() {
        let queue = DeadLetterQueue::new();
        queue
            .enqueue(letter("protected", "tenant-b", "project-b"))
            .unwrap();
        for index in 0..=MAX_ENTRIES_PER_SCOPE {
            queue
                .enqueue(letter(&format!("a-{index}"), "tenant-a", "project-a"))
                .unwrap();
        }
        let protected = queue.peek(&scope("tenant-b", "project-b")).unwrap();
        assert_eq!(protected.len(), 1);
        assert_eq!(protected[0].id, "protected");
        let tenant_a = queue.peek(&scope("tenant-a", "project-a")).unwrap();
        assert_eq!(tenant_a.len(), MAX_ENTRIES_PER_SCOPE);
        assert_eq!(tenant_a.first().unwrap().id, "a-1");
    }

    #[test]
    fn durable_scopes_use_distinct_files_and_byte_eviction_is_local() {
        let dir = tempfile::tempdir().unwrap();
        let legacy = dir.path().join("dead-letters-v1.json");
        let queue = DeadLetterQueue::persistent(legacy.clone()).unwrap();
        let mut protected = letter("protected", "tenant-b", "project-b");
        protected.original_message = "b".repeat(MAX_MESSAGE_BYTES);
        queue.enqueue(protected).unwrap();
        for index in 0..5 {
            let mut item = letter(&format!("large-{index}"), "tenant-a", "project-a");
            item.original_message = "a".repeat(MAX_MESSAGE_BYTES);
            queue.enqueue(item).unwrap();
        }

        let root = legacy.with_extension("d");
        let scope_a = scope("tenant-a", "project-a");
        let scope_b = scope("tenant-b", "project-b");
        assert_ne!(
            scope_store_path(&root, &scope_a),
            scope_store_path(&root, &scope_b)
        );
        assert_eq!(queue.peek(&scope_b).unwrap().len(), 1);
        let tenant_a = queue.peek(&scope_a).unwrap();
        assert!(!tenant_a.is_empty());
        assert!(tenant_a.len() < 5);
        assert!(
            std::fs::metadata(scope_store_path(&root, &scope_a))
                .unwrap()
                .len()
                <= MAX_STORE_BYTES
        );
    }

    #[test]
    fn compact_legacy_boundary_is_readable_and_next_enqueue_uses_writer_size() {
        let dir = tempfile::tempdir().unwrap();
        let legacy = dir.path().join("dead-letters-v1.json");
        let root = legacy.with_extension("d");
        let scope = scope("tenant-a", "project-a");
        let queue = DeadLetterQueue::persistent(&legacy).unwrap();
        prepare_store_directory(&root).unwrap();

        let mut entries = (0..MAX_ENTRIES_PER_SCOPE)
            .map(|index| {
                let mut item = letter(&format!("boundary-{index}"), "tenant-a", "project-a");
                item.original_message = "x".repeat(8_200);
                item
            })
            .collect::<Vec<_>>();
        while serde_json::to_vec(&DurableDlqV1 {
            version: STORE_VERSION,
            entries: entries.clone(),
        })
        .unwrap()
        .len() as u64
            > MAX_STORE_BYTES
        {
            entries.remove(0);
        }
        let compact = serde_json::to_vec(&DurableDlqV1 {
            version: STORE_VERSION,
            entries: entries.clone(),
        })
        .unwrap();
        assert!(compact.len() as u64 <= MAX_STORE_BYTES);
        assert!(
            serde_json::to_vec_pretty(&DurableDlqV1 {
                version: STORE_VERSION,
                entries,
            })
            .unwrap()
            .len() as u64
                > MAX_STORE_BYTES
        );
        std::fs::write(scope_store_path(&root, &scope), compact).unwrap();

        queue.ensure_writable(&scope).unwrap();
        queue
            .enqueue(letter("after-boundary", "tenant-a", "project-a"))
            .unwrap();
        let stored = std::fs::metadata(scope_store_path(&root, &scope)).unwrap();
        assert!(stored.len() <= MAX_STORE_BYTES);
        assert!(queue.get(&scope, "after-boundary").unwrap().is_some());
    }

    #[test]
    fn aggregate_stats_ignore_regular_interrupted_write_artifacts() {
        let dir = tempfile::tempdir().unwrap();
        let legacy = dir.path().join("dead-letters-v1.json");
        let scope = scope("tenant-a", "project-a");
        let queue = DeadLetterQueue::persistent(&legacy).unwrap();
        queue
            .enqueue(letter("good", "tenant-a", "project-a"))
            .unwrap();
        let store = scope_store_path(&legacy.with_extension("d"), &scope);
        let tmp = store.with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));
        std::fs::write(&tmp, b"partial").unwrap();

        assert_eq!(queue.stats(None).unwrap().total, 1);
        std::fs::remove_file(tmp).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn persistent_store_repairs_preexisting_directory_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let legacy = dir.path().join("dead-letters-v1.json");
        let root = legacy.with_extension("d");
        std::fs::create_dir(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o777)).unwrap();

        let queue = DeadLetterQueue::persistent(&legacy).unwrap();
        queue
            .enqueue(letter("private", "tenant-a", "project-a"))
            .unwrap();
        assert_eq!(
            std::fs::metadata(root).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    #[test]
    fn legacy_global_store_migrates_without_cross_scope_cohabitation() {
        let dir = tempfile::tempdir().unwrap();
        let legacy = dir.path().join("dead-letters-v1.json");
        save_store(
            &legacy,
            &DurableDlqV1 {
                version: STORE_VERSION,
                entries: vec![
                    letter("a", "tenant-a", "project-a"),
                    letter("b", "tenant-b", "project-b"),
                ],
            },
        )
        .unwrap();

        let queue = DeadLetterQueue::persistent(legacy.clone()).unwrap();
        assert!(!legacy.exists());
        assert_eq!(
            queue.peek(&scope("tenant-a", "project-a")).unwrap().len(),
            1
        );
        assert_eq!(
            queue.peek(&scope("tenant-b", "project-b")).unwrap().len(),
            1
        );
        let stores = std::fs::read_dir(legacy.with_extension("d"))
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
            .count();
        assert_eq!(stores, 2);
    }

    #[test]
    fn corrupt_scope_does_not_block_another_scope_or_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let legacy = dir.path().join("dead-letters-v1.json");
        let scope_a = scope("tenant-a", "project-a");
        let scope_b = scope("tenant-b", "project-b");
        let queue = DeadLetterQueue::persistent(legacy.clone()).unwrap();
        queue
            .enqueue(letter("good", "tenant-a", "project-a"))
            .unwrap();
        queue
            .enqueue(letter("bad", "tenant-b", "project-b"))
            .unwrap();
        std::fs::write(
            scope_store_path(&legacy.with_extension("d"), &scope_b),
            b"not json",
        )
        .unwrap();

        let reopened = DeadLetterQueue::persistent(legacy).unwrap();
        assert_eq!(reopened.peek(&scope_a).unwrap()[0].id, "good");
        assert!(reopened.peek(&scope_b).is_err());
    }

    #[test]
    fn read_only_stats_does_not_create_store_directory() {
        let dir = tempfile::tempdir().unwrap();
        let legacy = dir.path().join("missing").join("dead-letters-v1.json");
        let root = legacy.with_extension("d");
        let queue = DeadLetterQueue::persistent(legacy).unwrap();

        assert_eq!(queue.stats(None).unwrap().total, 0);
        assert!(!root.exists());
    }

    #[test]
    fn aggregate_stats_report_worst_scope_depth() {
        let queue = DeadLetterQueue::new();
        queue
            .enqueue(letter("a1", "tenant-a", "project-a"))
            .unwrap();
        queue
            .enqueue(letter("a2", "tenant-a", "project-a"))
            .unwrap();
        queue
            .enqueue(letter("b1", "tenant-b", "project-b"))
            .unwrap();

        let stats = queue.stats(None).unwrap();
        assert_eq!(stats.total, 3);
        assert_eq!(stats.max_scope_depth, 2);
        assert_eq!(
            queue
                .stats(Some(&scope("tenant-b", "project-b")))
                .unwrap()
                .max_scope_depth,
            1
        );
    }

    #[test]
    fn successful_local_retry_atomically_persists_removal() {
        let dir = tempfile::tempdir().unwrap();
        let legacy = dir.path().join("dead-letters-v1.json");
        let scope = scope("tenant-a", "project-a");
        let queue = DeadLetterQueue::persistent(legacy.clone()).unwrap();
        let mut item = letter("retry", "tenant-a", "project-a");
        item.target_agent = "retry-target".into();
        queue.enqueue(item).unwrap();

        queue.retry(&scope, "retry").unwrap();
        drop(queue);
        let stored = DeadLetterQueue::persistent(legacy)
            .unwrap()
            .peek(&scope)
            .unwrap();
        assert!(stored.is_empty());
    }

    #[test]
    fn retry_lock_artifacts_are_bounded_per_scope() {
        let dir = tempfile::tempdir().unwrap();
        let legacy = dir.path().join("dead-letters-v1.json");
        let queue = DeadLetterQueue::persistent(&legacy).unwrap();
        let scope_a = scope("tenant-a", "project-a");
        let scope_b = scope("tenant-b", "project-b");
        queue
            .enqueue(letter("first", "tenant-a", "project-a"))
            .unwrap();
        queue
            .enqueue(letter("third", "tenant-b", "project-b"))
            .unwrap();

        drop(queue.lock_retry(&scope_a, "first").unwrap());
        drop(queue.lock_retry(&scope_a, "second").unwrap());
        drop(queue.lock_retry(&scope_b, "third").unwrap());

        let retry_locks = std::fs::read_dir(legacy.with_extension("d"))
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".retry.lock"))
            .count();
        assert_eq!(retry_locks, 2);
    }
}
