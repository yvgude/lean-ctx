// SPDX-License-Identifier: Apache-2.0
//! Launcher-bound Workspace authority (#2120).
//!
//! Without it, the roots a session may reach come from process-wide knobs
//! (`extra_roots`, `allow_paths`, `LEAN_CTX_ALLOW_PATH`, the `home` jail scope)
//! and from what the client reports (`roots/list`, Cursor's workspace env,
//! `ctx_multi_repo add_root`). None of that can express "this MCP server may
//! reach exactly these roots, and this one has just been detached".
//!
//! A trusted launcher opts in by starting the MCP server with
//! `LEAN_CTX_WORKSPACE_FILE=<absolute path>`. The file names one Workspace: a
//! primary root, additional roots (read-write or read-only) and a revision.
//! While it is set the file is the **only** root authority of the process:
//!
//! - every tool path is checked against the request's Workspace snapshot
//!   (`pathjail`), never against client roots, config/env widening, the
//!   `home`/`projects` jail scope or auto-admitted cache folders;
//! - each tool call takes one immutable snapshot, so a call sees one revision
//!   from start to end, and the next call sees a root removed in between;
//! - a file that is missing, unreadable, malformed or insecurely stored denies
//!   every path (fail closed) — it never falls back to the legacy roots.
//!
//! The client never chooses the Workspace: the environment of the MCP server
//! process is set by its launcher, and nothing a client sends (MCP roots,
//! `_meta`, thread or workspace ids) is consulted. Shell commands run by
//! `ctx_shell` are processes of their own; the Workspace bounds their working
//! directory, not what a command reads.
//!
//! Mutations (`lean-ctx workspace create|add|remove`) are compare-and-swap on
//! the revision under an exclusive lock, written atomically with mode 0600.
//! Records hold paths and the revision only — no timestamps — so every text
//! built from them is a deterministic function of the stored state (#498).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

/// Environment variable through which a launcher binds the process.
pub const ENV_FILE: &str = "LEAN_CTX_WORKSPACE_FILE";
/// Schema identifier of a Workspace file.
pub const SCHEMA: &str = "lean-ctx.workspace/v1";
/// Largest Workspace file read.
const MAX_FILE_BYTES: u64 = 64 * 1024;
/// Most additional roots one Workspace holds.
pub const MAX_ROOTS: usize = 64;
/// Longest Workspace id.
const MAX_ID_LEN: usize = 64;

/// Access an attached root grants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Access {
    ReadWrite,
    ReadOnly,
}

impl Access {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadWrite => "read_write",
            Self::ReadOnly => "read_only",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RootRecord {
    path: String,
    access: Access,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkspaceRecord {
    schema: String,
    id: String,
    revision: u64,
    primary: String,
    #[serde(default)]
    roots: Vec<RootRecord>,
}

/// One attached additional root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachedRoot {
    pub path: PathBuf,
    pub access: Access,
}

/// An immutable view of one Workspace revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub file: PathBuf,
    pub id: String,
    pub revision: u64,
    /// Canonical primary root (read-write).
    pub primary: PathBuf,
    /// Canonical additional roots that exist; see [`Snapshot::unavailable`].
    pub roots: Vec<AttachedRoot>,
    /// Attached roots that no longer resolve to the recorded directory
    /// (deleted, or replaced by a symlink). They grant nothing.
    pub unavailable: Vec<String>,
}

impl Snapshot {
    /// Every root that grants access, primary first.
    pub fn granting_roots(&self) -> impl Iterator<Item = (&Path, Access)> {
        std::iter::once((self.primary.as_path(), Access::ReadWrite)).chain(
            self.roots
                .iter()
                .map(|root| (root.path.as_path(), root.access)),
        )
    }

    /// The additional roots as strings, for the request's tool context.
    pub fn additional_root_strings(&self) -> Vec<String> {
        self.roots
            .iter()
            .map(|root| root.path.to_string_lossy().into_owned())
            .collect()
    }

    /// The granting root containing the canonical `path`, if any. The most
    /// specific root wins, so a read-only root nested in a read-write one
    /// stays read-only.
    pub fn root_for(&self, canonical: &Path) -> Option<(&Path, Access)> {
        self.granting_roots()
            .filter(|(root, _)| canonical.starts_with(root))
            .max_by_key(|(root, _)| root.components().count())
    }

    /// True when the canonical `path` lies in a granting root.
    pub fn admits(&self, canonical: &Path) -> bool {
        self.root_for(canonical).is_some()
    }

    /// True when the canonical `path` lies in a read-write root.
    pub fn admits_write(&self, canonical: &Path) -> bool {
        self.root_for(canonical)
            .is_some_and(|(_, access)| access == Access::ReadWrite)
    }

    /// The request's project root: `wanted` when a granting root contains
    /// it, else the primary root.
    pub fn project_root_for(&self, wanted: &str) -> String {
        // Unresolvable (or timed out) means the primary, never a lexical path.
        if let Ok(canonical) = crate::core::pathutil::canonicalize_secure(Path::new(wanted))
            && self.admits(&canonical)
        {
            return canonical.to_string_lossy().into_owned();
        }
        self.primary.to_string_lossy().into_owned()
    }

    /// One line naming the binding, for MCP status output.
    pub fn status_line(&self) -> String {
        let mut line = format!(
            "Workspace: {} rev {} (launcher-bound) — primary {}",
            self.id,
            self.revision,
            self.primary.display()
        );
        for root in &self.roots {
            line.push_str(&format!(
                "; {} ({})",
                root.path.display(),
                root.access.as_str()
            ));
        }
        if !self.unavailable.is_empty() {
            line.push_str(&format!("; unavailable: {}", self.unavailable.join(", ")));
        }
        line
    }

    /// Structured form for `lean-ctx workspace show --json` and `/api/scope`.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "schema": SCHEMA,
            "file": self.file,
            "id": self.id,
            "revision": self.revision,
            "primary": self.primary,
            "roots": self.roots.iter().map(|root| serde_json::json!({
                "path": root.path,
                "access": root.access.as_str(),
            })).collect::<Vec<_>>(),
            "unavailable": self.unavailable,
        })
    }
}

/// The root authority in force.
#[derive(Debug, Clone)]
pub enum Authority {
    /// No Workspace configured: the legacy root rules apply unchanged.
    Legacy,
    /// A valid Workspace bounds every path.
    Workspace(Arc<Snapshot>),
    /// A Workspace is configured but cannot be used: every path is denied.
    Unavailable(String),
}

impl Authority {
    pub fn is_active(&self) -> bool {
        !matches!(self, Self::Legacy)
    }

    pub fn snapshot(&self) -> Option<&Arc<Snapshot>> {
        match self {
            Self::Workspace(snapshot) => Some(snapshot),
            _ => None,
        }
    }
}

tokio::task_local! {
    /// The Workspace snapshot one tool call was admitted under.
    static REQUEST: Authority;
}

/// The configured Workspace file, if the launcher set one. An empty value is
/// treated as set-but-invalid, never as "off": a launcher that meant to bind
/// the process must not silently get the legacy roots.
pub fn configured_file() -> Option<Result<PathBuf, String>> {
    let raw = std::env::var_os(ENV_FILE)?;
    let path = PathBuf::from(&raw);
    if raw.is_empty() {
        return Some(Err(format!("{ENV_FILE} is set but empty")));
    }
    if !path.is_absolute() {
        return Some(Err(format!(
            "{ENV_FILE} must be an absolute path (got {})",
            path.display()
        )));
    }
    Some(Ok(path))
}

/// True when a Workspace (valid or not) bounds this call: the process is
/// bound by its launcher, or the running call was admitted under one.
pub fn is_configured() -> bool {
    REQUEST.try_with(Authority::is_active).unwrap_or(false) || std::env::var_os(ENV_FILE).is_some()
}

/// The authority for the running tool call: its admitted snapshot, else the
/// current file state.
pub fn current() -> Authority {
    REQUEST
        .try_with(Clone::clone)
        .unwrap_or_else(|_| load_current())
}

/// Load the current Workspace state from the launcher-set file.
pub fn load_current() -> Authority {
    match configured_file() {
        None => Authority::Legacy,
        Some(Err(reason)) => Authority::Unavailable(reason),
        Some(Ok(path)) => match load_cached(&path) {
            Ok(snapshot) => Authority::Workspace(snapshot),
            Err(reason) => Authority::Unavailable(reason),
        },
    }
}

/// Run one async tool call under `authority`.
pub async fn scope<F: std::future::Future>(authority: Authority, request: F) -> F::Output {
    REQUEST.scope(authority, request).await
}

/// Run one synchronous handler under `authority` (task locals do not reach
/// the blocking pool).
pub fn sync_scope<R>(authority: Authority, operation: impl FnOnce() -> R) -> R {
    REQUEST.sync_scope(authority, operation)
}

/// Whether a root-scoped store (an index, a repo root) may serve `root` now:
/// always without a Workspace; under one only when the request's snapshot
/// contains it; never while the Workspace is unusable.
pub fn root_authorized(root: &Path) -> bool {
    match current() {
        Authority::Legacy => true,
        Authority::Workspace(snapshot) => crate::core::pathutil::canonicalize_secure(root)
            .is_ok_and(|canonical| snapshot.admits(&canonical)),
        Authority::Unavailable(_) => false,
    }
}

/// Confine a model-supplied path that a handler takes straight from its
/// arguments. Without a Workspace it is returned unchanged (the handler's
/// existing rules apply); under one it must resolve inside the snapshot —
/// relative paths against `project_root` — and, for `write`, inside a
/// read-write root.
pub fn confine(raw: &str, project_root: &str, write: bool) -> Result<String, String> {
    if !current().is_active() {
        return Ok(raw.to_string());
    }
    let root = if project_root.is_empty() {
        "."
    } else {
        project_root
    };
    let jailed = crate::core::pathjail::jail_path(Path::new(raw), Path::new(root))
        .map_err(|error| error.to_string())?;
    if write {
        crate::core::pathjail::enforce_writable(&jailed)?;
    }
    Ok(jailed.to_string_lossy().into_owned())
}

/// What `""`/`"."` (the project itself) means for a tool path: unchanged
/// without a Workspace; under one the call's root in the snapshot, never the
/// server's own working directory.
pub fn project_dot(normalized: &str, project_root: Option<&str>) -> Result<String, String> {
    match current() {
        Authority::Legacy => Ok(normalized.to_string()),
        Authority::Workspace(snapshot) => Ok(snapshot.project_root_for(project_root.unwrap_or(""))),
        Authority::Unavailable(reason) => Err(unavailable_reason(&reason)),
    }
}

/// The denial text for a configured but unusable Workspace.
pub fn unavailable_reason(reason: &str) -> String {
    format!(
        "workspace authority unavailable ({ENV_FILE}): {reason} — every path is denied until the \
         Workspace file is valid; inspect it with `lean-ctx workspace diagnose`"
    )
}

// ── Loading ─────────────────────────────────────────────────────────────────

/// Load the Workspace for a call. The file is at most 64 KiB and re-read on
/// every call: no cache can serve a revision the file no longer holds, and
/// every root is re-resolved each time.
fn load_cached(path: &Path) -> Result<Arc<Snapshot>, String> {
    load(path).map(Arc::new)
}

/// Load and validate the Workspace at `path`.
pub fn load(path: &Path) -> Result<Snapshot, String> {
    let (file, meta) = open_checked(path)?;
    parse_open(path, file, &meta)
}

fn parse_open(
    path: &Path,
    file: std::fs::File,
    meta: &std::fs::Metadata,
) -> Result<Snapshot, String> {
    use std::io::Read as _;
    if meta.len() > MAX_FILE_BYTES {
        return Err(format!(
            "{} is larger than {MAX_FILE_BYTES} bytes",
            path.display()
        ));
    }
    let mut text = String::new();
    file.take(MAX_FILE_BYTES + 1)
        .read_to_string(&mut text)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    if text.len() as u64 > MAX_FILE_BYTES {
        return Err(format!(
            "{} is larger than {MAX_FILE_BYTES} bytes",
            path.display()
        ));
    }
    let record: WorkspaceRecord = serde_json::from_str(&text)
        .map_err(|error| format!("{} is not a valid Workspace file: {error}", path.display()))?;
    snapshot_from(path, &record)
}

fn snapshot_from(path: &Path, record: &WorkspaceRecord) -> Result<Snapshot, String> {
    if record.schema != SCHEMA {
        return Err(format!(
            "unsupported Workspace schema '{}' (expected {SCHEMA})",
            record.schema
        ));
    }
    validate_id(&record.id)?;
    if record.revision == 0 {
        return Err("Workspace revision must be at least 1".to_string());
    }
    if record.roots.len() > MAX_ROOTS {
        return Err(format!(
            "a Workspace holds at most {MAX_ROOTS} additional roots"
        ));
    }
    let primary = resolve_recorded_root(&record.primary)
        .map_err(|reason| format!("primary root {}: {reason}", record.primary))?;
    let mut roots: Vec<AttachedRoot> = Vec::new();
    let mut unavailable = Vec::new();
    let mut seen = vec![record.primary.clone()];
    for entry in &record.roots {
        if seen.contains(&entry.path) {
            return Err(format!("root {} is listed twice", entry.path));
        }
        seen.push(entry.path.clone());
        match resolve_recorded_root(&entry.path) {
            Ok(path) => roots.push(AttachedRoot {
                path,
                access: entry.access,
            }),
            // A recorded root that is gone (or no longer the directory it
            // was) grants nothing; it does not take the whole Workspace down.
            Err(RootError::Unavailable(_)) => unavailable.push(entry.path.clone()),
            Err(RootError::Invalid(reason)) => {
                return Err(format!("root {}: {reason}", entry.path));
            }
        }
    }
    let snapshot = Snapshot {
        file: path.to_path_buf(),
        id: record.id.clone(),
        revision: record.revision,
        primary,
        roots,
        unavailable,
    };
    // The file is the agent's authority: inside one of its own roots, the
    // agent could edit it with the very tools it bounds.
    let location = canonical_location(path)?;
    if snapshot.admits(&location) {
        return Err(format!(
            "the Workspace file {} lies inside one of its own roots; keep it outside every root",
            location.display()
        ));
    }
    // lean-ctx's own directories are written by the engine for every
    // project; the file must not be one more thing in them.
    if own_dirs().iter().any(|dir| location.starts_with(dir)) {
        return Err(format!(
            "the Workspace file {} lies inside lean-ctx's own data, state, cache or config \
             directory; keep it in a directory of the launcher",
            location.display()
        ));
    }
    Ok(snapshot)
}

enum RootError {
    /// Malformed or forbidden — the record itself is wrong.
    Invalid(String),
    /// Well-formed but not currently a directory at that canonical path.
    Unavailable(String),
}

impl std::fmt::Display for RootError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(reason) | Self::Unavailable(reason) => f.write_str(reason),
        }
    }
}

/// A recorded root must be the canonical path of an existing directory: a
/// directory later replaced by a symlink (pointing anywhere) stops granting.
fn resolve_recorded_root(raw: &str) -> Result<PathBuf, RootError> {
    let path = Path::new(raw);
    check_root_shape(path).map_err(RootError::Invalid)?;
    match crate::core::pathutil::canonicalize_secure(path) {
        Ok(canonical) if canonical == path && canonical.is_dir() => Ok(canonical),
        Ok(canonical) if canonical != path => Err(RootError::Unavailable(format!(
            "now resolves to {} (not recorded)",
            canonical.display()
        ))),
        Ok(_) => Err(RootError::Unavailable("is not a directory".to_string())),
        Err(error) => Err(RootError::Unavailable(error.to_string())),
    }
}

/// Shape rules for any root: absolute, no `..`, not a broad directory (`/`,
/// home or one of its ancestors, agent config dirs), not lean-ctx's own data.
fn check_root_shape(path: &Path) -> Result<(), String> {
    if !path.is_absolute() {
        return Err("must be an absolute path".to_string());
    }
    if path.components().any(|c| {
        matches!(
            c,
            std::path::Component::ParentDir | std::path::Component::CurDir
        )
    }) {
        return Err("must not contain '.' or '..' components".to_string());
    }
    if path.to_string_lossy().as_bytes().contains(&0) {
        return Err("contains a null byte".to_string());
    }
    if crate::core::pathutil::is_broad_or_unsafe_root(path) {
        return Err("is a broad or unsafe directory (/, home, agent config)".to_string());
    }
    if let Some(home) = dirs::home_dir() {
        let canonical_home =
            crate::core::pathutil::canonicalize_secure(&home).unwrap_or(home.clone());
        if home.starts_with(path) || canonical_home.starts_with(path) {
            return Err("contains the home directory".to_string());
        }
    }
    for own in own_dirs() {
        if path.starts_with(&own) || own.starts_with(path) {
            return Err("overlaps lean-ctx's own data directory".to_string());
        }
    }
    Ok(())
}

/// lean-ctx's own directories (canonical where they exist).
fn own_dirs() -> Vec<PathBuf> {
    [
        crate::core::data_dir::lean_ctx_data_dir().ok(),
        crate::core::paths::state_dir_read_only().ok(),
        crate::core::paths::cache_dir_read_only().ok(),
        crate::core::paths::config_dir_read_only().ok(),
    ]
    .into_iter()
    .flatten()
    .map(|dir| crate::core::pathutil::canonicalize_secure(&dir).unwrap_or(dir))
    .collect()
}

fn validate_id(id: &str) -> Result<(), String> {
    let valid = !id.is_empty()
        && id.len() <= MAX_ID_LEN
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        && !id.starts_with('.');
    if valid {
        Ok(())
    } else {
        Err(format!(
            "Workspace id must be 1-{MAX_ID_LEN} characters of A-Z a-z 0-9 . _ - (not starting with '.')"
        ))
    }
}

// ── Storage security ────────────────────────────────────────────────────────

/// Ownership and permission rule for the Workspace file: owned by the
/// process's effective user and not accessible to group or others.
#[cfg_attr(not(unix), allow(dead_code))]
fn check_file_owner_mode(uid: u32, mode: u32, euid: u32) -> Result<(), String> {
    if uid != euid {
        return Err(format!(
            "is owned by uid {uid}, not by this process's user (uid {euid})"
        ));
    }
    if mode & 0o077 != 0 {
        return Err(format!(
            "has mode {:o}; it must not be accessible to group or others (chmod 600)",
            mode & 0o7777
        ));
    }
    Ok(())
}

/// Rule for each directory above the Workspace file: owned by this user or
/// root, and not writable by group or others unless sticky (so no other user
/// can rename the file or a directory on its path).
#[cfg_attr(not(unix), allow(dead_code))]
fn check_dir_owner_mode(uid: u32, mode: u32, euid: u32) -> Result<(), String> {
    if uid != euid && uid != 0 {
        return Err(format!(
            "is owned by uid {uid}, neither this user (uid {euid}) nor root"
        ));
    }
    if mode & 0o022 != 0 && mode & 0o1000 == 0 {
        return Err(format!(
            "has mode {:o}: writable by group or others without the sticky bit",
            mode & 0o7777
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn euid() -> u32 {
    // SAFETY: geteuid has no arguments or failure state.
    unsafe { libc::geteuid() }
}

/// Canonical location of the Workspace file: its directory resolved, the file
/// name kept (the file itself must not be a symlink).
fn canonical_location(path: &Path) -> Result<PathBuf, String> {
    let name = path
        .file_name()
        .ok_or_else(|| format!("{} has no file name", path.display()))?;
    let parent = path
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", path.display()))?;
    let parent = crate::core::pathutil::canonicalize_secure(parent)
        .map_err(|error| format!("directory of {}: {error}", path.display()))?;
    Ok(parent.join(name))
}

#[cfg(unix)]
fn check_ancestors(file: &Path) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt as _;
    let euid = euid();
    let mut dir = file.parent();
    while let Some(current) = dir {
        let meta = std::fs::symlink_metadata(current)
            .map_err(|error| format!("directory {}: {error}", current.display()))?;
        check_dir_owner_mode(meta.uid(), meta.mode(), euid)
            .map_err(|reason| format!("directory {} {reason}", current.display()))?;
        dir = current.parent();
    }
    Ok(())
}

/// Ownership and ACLs of the file and its directories are not verified on
/// this platform yet, so a Workspace cannot be trusted here: fail closed.
#[cfg(not(unix))]
fn check_ancestors(_file: &Path) -> Result<(), String> {
    Err(
        "Workspace authority needs a Unix host: file ownership cannot be verified on this \
         platform yet"
            .to_string(),
    )
}

/// Open the Workspace file without following a symlink and check what was
/// opened (not a path re-resolved later).
fn open_checked(path: &Path) -> Result<(std::fs::File, std::fs::Metadata), String> {
    let location = canonical_location(path)?;
    check_ancestors(&location)?;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let link = std::fs::symlink_metadata(&location)
        .map_err(|error| format!("cannot open {}: {error}", location.display()))?;
    if link.file_type().is_symlink() {
        return Err(format!(
            "{} is a symlink; a Workspace file must not be",
            location.display()
        ));
    }
    let file = options
        .open(&location)
        .map_err(|error| format!("cannot open {}: {error}", location.display()))?;
    let meta = file
        .metadata()
        .map_err(|error| format!("cannot inspect {}: {error}", location.display()))?;
    if !meta.is_file() {
        return Err(format!("{} is not a regular file", location.display()));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        check_file_owner_mode(meta.uid(), meta.mode(), euid())
            .map_err(|reason| format!("{} {reason}", location.display()))?;
    }
    Ok((file, meta))
}

// ── Mutation (CLI) ──────────────────────────────────────────────────────────

/// Exclusive lock beside the Workspace file, held for one mutation.
struct MutationLock {
    _file: std::fs::File,
}

fn lock_path(location: &Path) -> PathBuf {
    let mut name = location.file_name().unwrap_or_default().to_os_string();
    name.push(".lock");
    location.with_file_name(name)
}

fn acquire_lock(location: &Path) -> Result<MutationLock, String> {
    use fs2::FileExt as _;
    let path = lock_path(location);
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let file = options
        .open(&path)
        .map_err(|error| format!("cannot open lock {}: {error}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let meta = file
            .metadata()
            .map_err(|error| format!("cannot inspect lock {}: {error}", path.display()))?;
        check_file_owner_mode(meta.uid(), meta.mode(), euid())
            .map_err(|reason| format!("lock {} {reason}", path.display()))?;
    }
    file.lock_exclusive()
        .map_err(|error| format!("cannot lock {}: {error}", path.display()))?;
    Ok(MutationLock { _file: file })
}

fn write_record(location: &Path, record: &WorkspaceRecord) -> Result<(), String> {
    let mut bytes = serde_json::to_vec_pretty(record).map_err(|error| error.to_string())?;
    bytes.push(b'\n');
    #[cfg(unix)]
    let permissions = {
        use std::os::unix::fs::PermissionsExt as _;
        Some(std::fs::Permissions::from_mode(0o600))
    };
    #[cfg(not(unix))]
    let permissions: Option<std::fs::Permissions> = None;
    crate::core::atomic_fs::try_atomic_write(location, &bytes, permissions.as_ref())
        .map_err(|error| format!("cannot write {}: {error}", location.display()))?;
    if let Some(parent) = location.parent() {
        crate::core::atomic_fs::sync_directory(parent)
            .map_err(|error| format!("cannot sync {}: {error}", parent.display()))?;
    }
    Ok(())
}

fn read_record(location: &Path) -> Result<WorkspaceRecord, String> {
    use std::io::Read as _;
    let (file, meta) = open_checked(location)?;
    if meta.len() > MAX_FILE_BYTES {
        return Err(format!(
            "{} is larger than {MAX_FILE_BYTES} bytes",
            location.display()
        ));
    }
    let mut text = String::new();
    file.take(MAX_FILE_BYTES + 1)
        .read_to_string(&mut text)
        .map_err(|error| format!("cannot read {}: {error}", location.display()))?;
    let record: WorkspaceRecord = serde_json::from_str(&text).map_err(|error| {
        format!(
            "{} is not a valid Workspace file: {error}",
            location.display()
        )
    })?;
    // A mutation starts only from a valid Workspace.
    snapshot_from(location, &record)?;
    Ok(record)
}

/// Canonical form of a root given on the command line: it must exist now.
fn canonical_new_root(raw: &Path) -> Result<String, String> {
    let absolute = if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| error.to_string())?
            .join(raw)
    };
    let canonical = crate::core::pathutil::canonicalize_secure(&absolute)
        .map_err(|error| format!("{}: {error}", absolute.display()))?;
    if !canonical.is_dir() {
        return Err(format!("{} is not a directory", canonical.display()));
    }
    check_root_shape(&canonical).map_err(|reason| format!("{}: {reason}", canonical.display()))?;
    Ok(canonical.to_string_lossy().into_owned())
}

fn ensure_absolute_file(path: &Path) -> Result<(), String> {
    if path.is_absolute() {
        Ok(())
    } else {
        Err(format!(
            "the Workspace file must be an absolute path (got {})",
            path.display()
        ))
    }
}

/// Create a new Workspace file (fails if one exists). Returns revision 1.
pub fn create(file: &Path, id: &str, primary: &Path) -> Result<Snapshot, String> {
    ensure_absolute_file(file)?;
    validate_id(id)?;
    let primary = canonical_new_root(primary)?;
    let location = canonical_location(file)?;
    check_ancestors(&location)?;
    let _lock = acquire_lock(&location)?;
    if std::fs::symlink_metadata(&location).is_ok() {
        return Err(format!("{} already exists", location.display()));
    }
    let record = WorkspaceRecord {
        schema: SCHEMA.to_string(),
        id: id.to_string(),
        revision: 1,
        primary,
        roots: Vec::new(),
    };
    snapshot_from(&location, &record)?;
    write_record(&location, &record)?;
    load(&location)
}

/// Attach `root` if the stored revision is still `expect_revision`.
pub fn add_root(
    file: &Path,
    root: &Path,
    access: Access,
    expect_revision: u64,
) -> Result<Snapshot, String> {
    mutate(file, expect_revision, |record| {
        let root = canonical_new_root(root)?;
        if root == record.primary || record.roots.iter().any(|entry| entry.path == root) {
            return Err(format!("{root} is already attached"));
        }
        if record.roots.len() >= MAX_ROOTS {
            return Err(format!(
                "a Workspace holds at most {MAX_ROOTS} additional roots"
            ));
        }
        record.roots.push(RootRecord { path: root, access });
        Ok(())
    })
}

/// Detach `root` if the stored revision is still `expect_revision`. The
/// primary root cannot be detached. `root` is matched as recorded, so a root
/// that no longer exists can still be removed.
pub fn remove_root(file: &Path, root: &Path, expect_revision: u64) -> Result<Snapshot, String> {
    mutate(file, expect_revision, |record| {
        let wanted = crate::core::pathutil::canonicalize_secure(root)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| root.to_string_lossy().into_owned());
        let raw = root.to_string_lossy().into_owned();
        if wanted == record.primary || raw == record.primary {
            return Err("the primary root cannot be detached".to_string());
        }
        let before = record.roots.len();
        record
            .roots
            .retain(|entry| entry.path != wanted && entry.path != raw);
        if record.roots.len() == before {
            return Err(format!("{raw} is not attached"));
        }
        Ok(())
    })
}

fn mutate(
    file: &Path,
    expect_revision: u64,
    change: impl FnOnce(&mut WorkspaceRecord) -> Result<(), String>,
) -> Result<Snapshot, String> {
    ensure_absolute_file(file)?;
    let location = canonical_location(file)?;
    let _lock = acquire_lock(&location)?;
    let mut record = read_record(&location)?;
    if record.revision != expect_revision {
        return Err(format!(
            "revision conflict: the Workspace is at revision {}, not {expect_revision} — \
             re-read it with `lean-ctx workspace show` and retry",
            record.revision
        ));
    }
    change(&mut record)?;
    record.revision = record
        .revision
        .checked_add(1)
        .ok_or_else(|| "revision overflow".to_string())?;
    snapshot_from(&location, &record)?;
    write_record(&location, &record)?;
    load(&location)
}

// ── Diagnostics ─────────────────────────────────────────────────────────────

/// What `lean-ctx workspace diagnose` and `doctor` report.
pub fn diagnose_lines() -> Vec<String> {
    match configured_file() {
        None => vec![format!(
            "no Workspace bound ({ENV_FILE} unset): legacy root rules apply"
        )],
        Some(Err(reason)) => vec![unavailable_reason(&reason)],
        Some(Ok(path)) => match load(&path) {
            Ok(snapshot) => {
                let mut lines = vec![snapshot.status_line()];
                lines.push(
                    "client roots, extra_roots/allow_paths, the home jail scope and \
                     ctx_multi_repo roots grant nothing while bound"
                        .to_string(),
                );
                lines
            }
            Err(reason) => vec![unavailable_reason(&reason)],
        },
    }
}

// A Workspace is refused on non-Unix hosts (ownership is not verifiable
// there), so the store and enforcement tests run on Unix.
#[cfg(all(test, unix))]
#[path = "workspace_authority_tests.rs"]
mod enforcement_tests;

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn private_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        dir
    }

    fn project(base: &Path, name: &str) -> PathBuf {
        let path = base.join(name);
        std::fs::create_dir_all(&path).unwrap();
        crate::core::pathutil::canonicalize_secure(&path).unwrap()
    }

    #[test]
    fn create_add_remove_with_cas() {
        let store = private_dir();
        let code = tempfile::tempdir().unwrap();
        let primary = project(code.path(), "app");
        let lib = project(code.path(), "lib");
        let file = store.path().join("ws.json");

        let created = create(&file, "team-a", &primary).unwrap();
        assert_eq!(created.revision, 1);
        assert_eq!(created.primary, primary);
        assert!(created.roots.is_empty());

        let added = add_root(&file, &lib, Access::ReadOnly, 1).unwrap();
        assert_eq!(added.revision, 2);
        assert_eq!(added.roots[0].path, lib);
        assert!(added.admits(&lib.join("src/x.rs")));
        assert!(!added.admits_write(&lib.join("src/x.rs")));
        assert!(added.admits_write(&primary.join("main.rs")));

        // A stale writer loses.
        let stale = add_root(&file, &lib, Access::ReadWrite, 1).unwrap_err();
        assert!(stale.contains("revision conflict"), "{stale}");

        let removed = remove_root(&file, &lib, 2).unwrap();
        assert_eq!(removed.revision, 3);
        assert!(!removed.admits(&lib.join("src/x.rs")));
        assert!(
            remove_root(&file, &primary, 3)
                .unwrap_err()
                .contains("primary")
        );
        assert!(
            create(&file, "team-a", &primary)
                .unwrap_err()
                .contains("exists")
        );
    }

    #[test]
    fn nested_read_only_root_stays_read_only() {
        let store = private_dir();
        let code = tempfile::tempdir().unwrap();
        let primary = project(code.path(), "app");
        let vendored = project(&primary, "vendor");
        let file = store.path().join("ws.json");
        create(&file, "w", &primary).unwrap();
        let snapshot = add_root(&file, &vendored, Access::ReadOnly, 1).unwrap();
        assert!(snapshot.admits_write(&primary.join("a.rs")));
        assert!(!snapshot.admits_write(&vendored.join("b.rs")));
    }

    #[test]
    fn the_file_must_lie_outside_its_roots() {
        let code = private_dir();
        let primary = project(code.path(), "app");
        let inside = primary.join("ws.json");
        assert!(
            create(&inside, "w", &primary)
                .unwrap_err()
                .contains("own roots")
        );
        assert!(!inside.exists());

        let store = private_dir();
        let file = store.path().join("ws.json");
        create(&file, "w", &primary).unwrap();
        let store_root = crate::core::pathutil::canonicalize_secure(store.path()).unwrap();
        assert!(
            add_root(&file, &store_root, Access::ReadOnly, 1)
                .unwrap_err()
                .contains("own roots")
        );
        assert_eq!(load(&file).unwrap().revision, 1);
    }

    #[test]
    fn broad_and_own_roots_are_refused() {
        let store = private_dir();
        let file = store.path().join("ws.json");
        assert!(create(&file, "w", Path::new("/")).is_err());
        if let Some(home) = dirs::home_dir() {
            assert!(create(&file, "w", &home).is_err());
        }
        assert!(create(&file, "../x", store.path()).is_err(), "bad id");
        assert!(!file.exists());
    }

    #[test]
    fn malformed_and_unknown_fields_fail_closed() {
        let store = private_dir();
        let code = tempfile::tempdir().unwrap();
        let primary = project(code.path(), "app");
        let file = store.path().join("ws.json");
        let write = |text: String| {
            std::fs::write(&file, text).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
            }
        };
        write("{".to_string());
        assert!(load(&file).is_err());
        write(format!(
            r#"{{"schema":"{SCHEMA}","id":"w","revision":1,"primary":{primary:?},"roots":[],"grant":"all"}}"#
        ));
        assert!(load(&file).unwrap_err().contains("unknown field"));
        write(format!(
            r#"{{"schema":"{SCHEMA}","id":"w","revision":0,"primary":{primary:?}}}"#
        ));
        assert!(load(&file).unwrap_err().contains("revision"));
        // A relative or non-canonical primary is refused.
        write(format!(
            r#"{{"schema":"{SCHEMA}","id":"w","revision":1,"primary":"{}/../app"}}"#,
            primary.display()
        ));
        assert!(load(&file).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn insecure_storage_is_refused() {
        use std::os::unix::fs::PermissionsExt as _;
        let store = private_dir();
        let code = tempfile::tempdir().unwrap();
        let primary = project(code.path(), "app");
        let file = store.path().join("ws.json");
        create(&file, "w", &primary).unwrap();
        assert!(load(&file).is_ok());

        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(load(&file).unwrap_err().contains("group or others"));
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();

        // A symlink to a valid file is not a Workspace file.
        let link = store.path().join("link.json");
        std::os::unix::fs::symlink(&file, &link).unwrap();
        assert!(load(&link).unwrap_err().contains("symlink"));

        // A world-writable directory on the path (without sticky bit).
        std::fs::set_permissions(store.path(), std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(load(&file).unwrap_err().contains("sticky"));
        std::fs::set_permissions(store.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(load(&file).is_ok());
    }

    /// Multi-UID rules, checked as pure functions (changing owners needs
    /// root): a file owned by another user is refused even when private, a
    /// directory owned by root or this user is fine, a foreign directory is not.
    #[test]
    fn foreign_owners_are_refused() {
        assert!(check_file_owner_mode(1000, 0o100600, 1000).is_ok());
        assert!(
            check_file_owner_mode(1001, 0o100600, 1000)
                .unwrap_err()
                .contains("uid 1001")
        );
        assert!(
            check_file_owner_mode(0, 0o100600, 1000).is_err(),
            "root-owned file is foreign"
        );
        assert!(check_file_owner_mode(1000, 0o100640, 1000).is_err());
        assert!(
            check_dir_owner_mode(0, 0o41777, 1000).is_ok(),
            "sticky /tmp"
        );
        assert!(check_dir_owner_mode(0, 0o40755, 1000).is_ok());
        assert!(check_dir_owner_mode(1000, 0o40700, 1000).is_ok());
        assert!(check_dir_owner_mode(1001, 0o40700, 1000).is_err());
        assert!(check_dir_owner_mode(1000, 0o40775, 1000).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_root_replaced_by_a_symlink_stops_granting() {
        let store = private_dir();
        let code = tempfile::tempdir().unwrap();
        let primary = project(code.path(), "app");
        let lib = project(code.path(), "lib");
        let elsewhere = project(code.path(), "secret");
        let file = store.path().join("ws.json");
        create(&file, "w", &primary).unwrap();
        add_root(&file, &lib, Access::ReadWrite, 1).unwrap();

        std::fs::remove_dir(&lib).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &lib).unwrap();
        let snapshot = load(&file).unwrap();
        assert!(snapshot.roots.is_empty());
        assert_eq!(
            snapshot.unavailable,
            vec![lib.to_string_lossy().into_owned()]
        );
        assert!(!snapshot.admits(&elsewhere.join("key")));
        // The stale entry can still be detached.
        assert_eq!(remove_root(&file, &lib, 2).unwrap().revision, 3);
    }

    #[test]
    fn concurrent_writers_serialize_and_one_wins() {
        let store = private_dir();
        let code = tempfile::tempdir().unwrap();
        let primary = project(code.path(), "app");
        let roots: Vec<PathBuf> = (0..8)
            .map(|i| project(code.path(), &format!("r{i}")))
            .collect();
        let file = store.path().join("ws.json");
        create(&file, "w", &primary).unwrap();
        let wins: Vec<bool> = std::thread::scope(|scope| {
            let handles: Vec<_> = roots
                .iter()
                .map(|root| {
                    let file = file.clone();
                    scope.spawn(move || add_root(&file, root, Access::ReadWrite, 1).is_ok())
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        assert_eq!(
            wins.iter().filter(|w| **w).count(),
            1,
            "exactly one CAS succeeds"
        );
        let snapshot = load(&file).unwrap();
        assert_eq!(snapshot.revision, 2);
        assert_eq!(snapshot.roots.len(), 1);
    }

    #[test]
    fn request_scope_pins_one_revision() {
        let snapshot = Arc::new(Snapshot {
            file: PathBuf::from("/w.json"),
            id: "w".into(),
            revision: 7,
            primary: PathBuf::from("/work/app"),
            roots: Vec::new(),
            unavailable: Vec::new(),
        });
        let seen = sync_scope(Authority::Workspace(Arc::clone(&snapshot)), || {
            current().snapshot().map(|s| s.revision)
        });
        assert_eq!(seen, Some(7));
        assert_eq!(snapshot.project_root_for("/elsewhere"), "/work/app");
    }
}
