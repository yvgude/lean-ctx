// SPDX-License-Identifier: Apache-2.0
//! Engine side of the cockpit analysis bridge.
//!
//! The cockpit asks for an op's inputs; the engine validates the op and
//! collects bounded inputs from the bound project only. The cockpit computes
//! the answer and hands it back; the engine executes any write it plans with
//! its own code paths, reading every effect back. The computation never
//! touches files, clock or env.
//!
//! Analysis state the cockpit used to keep (alerts, profiles, adaptive context,
//! knowledge snapshots) lives under `<data>/dashboard-pro/<partition>/`,
//! private (0600) and written atomically.

pub(crate) mod bridge;
pub(crate) mod collect;
pub(crate) mod effects;
pub(crate) mod export;
pub(crate) mod guard;
pub(crate) mod host_usage;
pub(crate) mod ledger;
pub(crate) mod request;
pub(crate) mod security_fields;
pub(crate) mod snapshots;
pub(crate) mod state;
pub(crate) mod watch;
pub(crate) mod wire;

#[cfg(test)]
mod tests;

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

/// Guard kinds the audit trail records, in the cockpit's order.
pub(crate) const GUARD_KINDS: [&str; 7] = [
    "secret_redacted",
    "shell_blocked",
    "path_blocked",
    "injection_flagged",
    "pii_redacted",
    "content_withheld",
    "coverage_incomplete",
];
/// Upper bound of any JSON state file this module reads.
const STATE_FILE_LIMIT: u64 = 4 << 20;

/// An answer the cockpit shows: HTTP status and a plain message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProError {
    pub(crate) status: u16,
    pub(crate) message: String,
}

impl ProError {
    pub(crate) fn new(status: u16, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }
    pub(crate) fn bad(message: impl Into<String>) -> Self {
        Self::new(400, message)
    }
    pub(crate) fn not_found(message: impl Into<String>) -> Self {
        Self::new(404, message)
    }
    pub(crate) fn conflict(message: impl Into<String>) -> Self {
        Self::new(409, message)
    }
    pub(crate) fn internal(message: impl Into<String>) -> Self {
        Self::new(500, message)
    }
}

impl std::fmt::Display for ProError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.message, self.status)
    }
}

pub(crate) type ProResult<T> = Result<T, ProError>;

/// The bound project and its private Pro partition.
#[derive(Debug, Clone)]
pub(crate) struct ProjectScope {
    /// The root as the dashboard binds it; the knowledge store is keyed by it.
    pub(crate) root: String,
    /// The canonical root, for jail checks.
    pub(crate) canonical: PathBuf,
    /// The engine's project hash; names the Pro partition.
    pub(crate) partition: String,
    /// The project's display name.
    pub(crate) label: String,
    /// Ledger folder hashes (`savings_ledger::repo_hash`) of the project.
    /// Empty for the device scope, which the runtime reads as not narrowed.
    pub(crate) repo_hashes: Vec<String>,
    /// Device scope only: the roots of the projects on this device, which
    /// name their ledger folders and host sessions.
    pub(crate) projects: Vec<String>,
    /// The agent sessions worked in the project's folders, which place its
    /// audit-trail entries. Empty for the device scope.
    pub(crate) sessions: std::collections::HashSet<String>,
    pub(crate) data_dir: PathBuf,
    pub(crate) state_dir: PathBuf,
    pub(crate) home: Option<PathBuf>,
}

impl ProjectScope {
    /// Every project on this device, for an "All projects" request of an op
    /// that reads device-wide stores ([`request::Op::device_wide`]).
    pub(crate) fn device(projects: &[String]) -> ProResult<Self> {
        let data = crate::core::data_dir::lean_ctx_data_dir().map_err(ProError::internal)?;
        let state = crate::core::paths::state_dir().map_err(ProError::internal)?;
        Ok(Self::device_with_dirs(
            projects,
            data,
            state,
            dirs::home_dir(),
        ))
    }

    pub(crate) fn device_with_dirs(
        projects: &[String],
        data_dir: PathBuf,
        state_dir: PathBuf,
        home: Option<PathBuf>,
    ) -> Self {
        let mut roots: Vec<String> = Vec::new();
        for root in projects.iter().map(|r| r.trim()).filter(|r| !r.is_empty()) {
            let canonical = Path::new(root)
                .canonicalize()
                .map(|p| p.to_string_lossy().into_owned());
            for candidate in std::iter::once(root.to_string()).chain(canonical) {
                if !roots.contains(&candidate) {
                    roots.push(candidate);
                }
            }
        }
        Self {
            root: String::new(),
            canonical: PathBuf::new(),
            partition: repo_hash("lean-ctx:device"),
            label: "All projects".to_string(),
            repo_hashes: Vec::new(),
            projects: roots,
            sessions: Default::default(),
            data_dir,
            state_dir,
            home,
        }
    }

    /// Whether this is the device scope (no bound project).
    pub(crate) fn is_device(&self) -> bool {
        self.root.is_empty()
    }

    /// The dashboard's bound project with the engine's own directories.
    pub(crate) fn bind(root: &str) -> ProResult<Self> {
        let data = crate::core::data_dir::lean_ctx_data_dir().map_err(ProError::internal)?;
        let state = crate::core::paths::state_dir().map_err(ProError::internal)?;
        let mut scope = Self::with_dirs(root, data, state, dirs::home_dir())?;
        scope.adopt_folders(&crate::core::savings_ledger::ProjectScope::for_project(
            &scope.canonical,
        ));
        Ok(scope)
    }

    /// Counts the project's subfolders and agent worktrees towards it, as the
    /// project switch groups them: their ledger folders and their sessions.
    pub(crate) fn adopt_folders(&mut self, folders: &crate::core::savings_ledger::ProjectScope) {
        for hash in folders.repo_hashes() {
            if !self.repo_hashes.contains(&hash) {
                self.repo_hashes.push(hash);
            }
        }
        self.sessions.extend(folders.sessions().iter().cloned());
    }

    pub(crate) fn with_dirs(
        root: &str,
        data_dir: PathBuf,
        state_dir: PathBuf,
        home: Option<PathBuf>,
    ) -> ProResult<Self> {
        let root = root.trim();
        if root.is_empty() {
            return Err(ProError::bad(
                "Choose a project: this view reads one project's data",
            ));
        }
        let path = Path::new(root);
        if !path.is_absolute() || !path.is_dir() {
            return Err(ProError::bad("The bound project is not a directory"));
        }
        let canonical = path
            .canonicalize()
            .map_err(|_| ProError::bad("The bound project is not readable"))?;
        let partition = crate::core::project_hash::hash_project_root(root);
        if !is_partition(&partition) {
            return Err(ProError::internal("Unexpected project hash"));
        }
        let mut repo_hashes = vec![repo_hash(root)];
        let canonical_text = canonical.to_string_lossy().into_owned();
        if canonical_text != root {
            repo_hashes.push(repo_hash(&canonical_text));
        }
        let label = canonical.file_name().map_or_else(
            || "Project".to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        Ok(Self {
            root: root.to_string(),
            canonical,
            partition,
            label,
            repo_hashes,
            projects: Vec::new(),
            sessions: Default::default(),
            data_dir,
            state_dir,
            home,
        })
    }

    /// `<data>/dashboard-pro/<partition>`.
    pub(crate) fn partition_dir(&self) -> PathBuf {
        self.data_dir.join("dashboard-pro").join(&self.partition)
    }

    /// The wire `project` of a request.
    pub(crate) fn project(&self) -> wire::Project {
        wire::Project {
            root_hash: self.repo_hashes.first().cloned().unwrap_or_default(),
            labels: self.labels(),
            repo_hashes: self.repo_hashes.clone(),
            name: Some(self.label.clone()),
        }
    }

    pub(crate) fn labels(&self) -> Vec<wire::ProjectLabel> {
        if self.is_device() {
            return self
                .projects
                .iter()
                .map(|root| wire::ProjectLabel {
                    hash: repo_hash(root),
                    name: Path::new(root).file_name().map_or_else(
                        || "Project".to_string(),
                        |n| n.to_string_lossy().into_owned(),
                    ),
                })
                .collect();
        }
        self.repo_hashes
            .iter()
            .map(|hash| wire::ProjectLabel {
                hash: hash.clone(),
                name: self.label.clone(),
            })
            .collect()
    }

    /// A project-relative path inside the jail: no absolute part, no `..`,
    /// and resolving under the canonical root.
    pub(crate) fn jailed(&self, relative: &str) -> ProResult<PathBuf> {
        let relative = relative.trim();
        let candidate = Path::new(relative);
        if self.is_device()
            || relative.is_empty()
            || relative.len() > 4096
            || relative.chars().any(char::is_control)
            || candidate.is_absolute()
            || crate::dashboard::routes::helpers::is_windows_absolute_path(relative)
            || candidate
                .components()
                .any(|c| !matches!(c, std::path::Component::Normal(_)))
        {
            return Err(ProError::conflict("The path is outside this project"));
        }
        let joined = self.canonical.join(candidate);
        if let Ok(resolved) = joined.canonicalize()
            && !resolved.starts_with(&self.canonical)
        {
            return Err(ProError::conflict("The path is outside this project"));
        }
        Ok(joined)
    }
}

/// `savings_ledger::repo_hash` of a folder: the first 16 hex digits of
/// SHA-256 over its path.
pub(crate) fn repo_hash(path: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(path.as_bytes());
    hex::encode(digest)[..16].to_string()
}

fn is_partition(value: &str) -> bool {
    (8..=64).contains(&value.len()) && value.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Writes `bytes` to `path` atomically, private to the user.
pub(crate) fn write_private(path: &Path, bytes: &[u8]) -> ProResult<()> {
    let parent = path
        .parent()
        .ok_or_else(|| ProError::internal("state path has no parent"))?;
    create_private_dir(parent)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)
        .map_err(|_| ProError::internal("Cockpit state could not be written"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(temporary.path(), std::fs::Permissions::from_mode(0o600))
            .map_err(|_| ProError::internal("Cockpit state could not be written"))?;
    }
    temporary
        .write_all(bytes)
        .and_then(|()| temporary.as_file().sync_all())
        .map_err(|_| ProError::internal("Cockpit state could not be written"))?;
    temporary
        .persist(path)
        .map_err(|_| ProError::internal("Cockpit state could not be written"))?;
    Ok(())
}

pub(crate) fn write_json(path: &Path, value: &impl serde::Serialize) -> ProResult<()> {
    let bytes =
        serde_json::to_vec(value).map_err(|_| ProError::internal("Cockpit state is not JSON"))?;
    write_private(path, &bytes)
}

pub(crate) fn create_private_dir(dir: &Path) -> ProResult<()> {
    if std::fs::symlink_metadata(dir).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(ProError::internal("Cockpit state directory is a symlink"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .map_err(|_| ProError::internal("Cockpit state directory could not be created"))
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(dir)
            .map_err(|_| ProError::internal("Cockpit state directory could not be created"))
    }
}

/// A regular, non-symlink file read up to `limit` bytes; `None` when it is
/// missing, not regular or larger than `limit`.
pub(crate) fn read_bounded(path: &Path, limit: u64) -> Option<Vec<u8>> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    if !meta.file_type().is_file() || meta.len() > limit {
        return None;
    }
    let mut bytes = Vec::with_capacity(usize::try_from(meta.len()).unwrap_or(0));
    std::fs::File::open(path)
        .ok()?
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() as u64 <= limit).then_some(bytes)
}

pub(crate) fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Option<T> {
    serde_json::from_slice(&read_bounded(path, STATE_FILE_LIMIT)?).ok()
}

/// Text without control characters, at most `limit` characters.
pub(crate) fn plain_text(value: &str, limit: usize) -> bool {
    value.chars().count() <= limit && !value.chars().any(char::is_control)
}

pub(crate) fn error_body(error: &ProError) -> Value {
    json!({ "error": error.message })
}
