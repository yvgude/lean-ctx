// SPDX-License-Identifier: Apache-2.0

use chrono::Utc;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use super::heuristics::{normalize_loaded_session, session_matches_project_root};
use super::paths::sessions_dir;
use super::save_outcome::SaveOutcome;
use super::state::extract_session_facts;
#[allow(clippy::wildcard_imports)]
use super::types::*;

/// Keep the startup warm set deliberately small: cache warming is an optional
/// optimisation and must never turn process startup into a session-store scan.
const PROJECT_HISTORY_LIMIT: usize = 8;

/// How long a save waits for the per-session lock before reporting failure.
/// Generous enough that an ordinary concurrent save wins it, short enough that
/// a holder which has stopped making progress cannot hold a caller forever.
#[cfg(not(test))]
const SAVE_LOCK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
/// The wedged-holder test waits this out under the global test env lock.
#[cfg(test)]
const SAVE_LOCK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionScopeError {
    ProjectRootMismatch,
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct ProjectSessionIndex {
    version: u8,
    project_root: String,
    /// Oldest to newest; duplicates are removed before appending on every save.
    session_ids: Vec<String>,
    /// Set only by a full-store scan that found no session for this root, written
    /// under the index lock. It makes "no session" a cached answer: without it
    /// every process (each hook call loads config, which resolves the project
    /// root) re-parsed the whole session store for a project without sessions.
    /// Any save for the root appends an id, which ends the empty state.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    verified_empty: bool,
}

fn normalized_safe_project_root(project_root: &str) -> Option<String> {
    let path = std::path::Path::new(project_root);
    if project_root.trim().is_empty() || crate::core::pathutil::is_broad_or_unsafe_root(path) {
        return None;
    }
    Some(
        crate::core::pathutil::safe_canonicalize_or_self(path)
            .to_string_lossy()
            .to_string(),
    )
}

/// Normalize a receiving project root without accepting an implicit CWD.
///
/// The legacy loader intentionally accepts relative roots for compatibility;
/// session replacement must not use that heuristic as an authority because a
/// relative value can resolve to a different process working directory.
fn normalized_strict_project_root(project_root: &str) -> Option<String> {
    let path = std::path::Path::new(project_root);
    if project_root.trim().is_empty()
        || !path.is_absolute()
        || crate::core::pathutil::is_broad_or_unsafe_root(path)
    {
        return None;
    }

    let normalized = crate::core::pathutil::safe_canonicalize_or_self(path);
    if !normalized.is_absolute() || crate::core::pathutil::is_broad_or_unsafe_root(&normalized) {
        return None;
    }
    Some(normalized.to_string_lossy().to_string())
}

fn project_index_path(dir: &std::path::Path, project_root: &str) -> std::path::PathBuf {
    let key = blake3::hash(project_root.as_bytes()).to_hex();
    dir.join("project-index").join(format!("{key}.json"))
}

fn read_project_index(dir: &std::path::Path, project_root: &str) -> Option<ProjectSessionIndex> {
    std::fs::read_to_string(project_index_path(dir, project_root))
        .ok()
        .and_then(|json| serde_json::from_str::<ProjectSessionIndex>(&json).ok())
        .filter(|index| index.version == 1 && index.project_root == project_root)
}

fn project_head_from_bytes(
    id: &str,
    project_root: &str,
    bytes: &[u8],
) -> Result<(String, String), String> {
    let json =
        std::str::from_utf8(bytes).map_err(|_| "corrupt project session head".to_string())?;
    let session = SessionState::from_storage_json(json)
        .map(normalize_loaded_session)
        .map_err(|_| "corrupt project session head".to_string())?;
    if session.id != id {
        return Err("corrupt project session head id".to_string());
    }
    if !session_matches_project_root(&session, std::path::Path::new(project_root)) {
        return Err("project session head root mismatch".to_string());
    }
    let digest = Sha256::digest(bytes);
    Ok((session.id, format!("sha256:{}", hex::encode(digest))))
}

/// Resolve the canonical project head without changing the index. Callers hold
/// the project lock so a save cannot move the index between selection and read.
fn read_project_head_locked(
    dir: &std::path::Path,
    project_root: &str,
) -> Result<Option<(String, String)>, String> {
    if project_index_path(dir, project_root).exists()
        && read_project_index(dir, project_root).is_none()
    {
        return Err("corrupt project session index".to_string());
    }
    if let Some(index) = read_project_index(dir, project_root)
        && let Some(id) = index.session_ids.last()
    {
        validate_session_id(id).map_err(|_| "invalid project session index head".to_string())?;
        let path = dir.join(format!("{id}.json"));
        match std::fs::read(path) {
            Ok(bytes) => {
                let json = std::str::from_utf8(&bytes)
                    .map_err(|_| "corrupt project session head".to_string())?;
                let session = SessionState::from_storage_json(json)
                    .map(normalize_loaded_session)
                    .map_err(|_| "corrupt project session head".to_string())?;
                if session.id.as_str() != id.as_str() {
                    return Err("corrupt project session head id".to_string());
                }
                let Some(session_root) = session.project_root.as_deref() else {
                    return Err("unsafe project session head root".to_string());
                };
                if normalized_safe_project_root(session_root).is_none() {
                    return Err("unsafe project session head root".to_string());
                }
                if session_matches_project_root(&session, std::path::Path::new(project_root)) {
                    let digest = Sha256::digest(&bytes);
                    return Ok(Some((
                        session.id,
                        format!("sha256:{}", hex::encode(digest)),
                    )));
                }
                // A safe but stale index entry follows the existing loader's
                // repair semantics; the scan below remains read-only for CAS.
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("read project session head: {error}")),
        }
    }

    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("scan project sessions: {error}")),
    };
    let mut matches = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| format!("scan project sessions: {error}"))?;
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
            continue;
        }
        let Some(id) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        if id == "latest" || id.starts_with('.') {
            continue;
        }
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(format!("read project session: {error}")),
        };
        let json = std::str::from_utf8(&bytes)
            .map_err(|_| "unreadable session during project recovery".to_string())?;
        let session = SessionState::from_storage_json(json)
            .map(normalize_loaded_session)
            .map_err(|_| "unreadable session during project recovery".to_string())?;
        if session.id != id {
            continue;
        }
        if session_matches_project_root(&session, std::path::Path::new(project_root)) {
            matches.push((session.updated_at, session.id, bytes));
        }
    }
    matches.sort_by_key(|(updated_at, _, _)| *updated_at);
    matches
        .pop()
        .map(|(_, id, bytes)| project_head_from_bytes(&id, project_root, &bytes))
        .transpose()
}

fn write_project_index(
    index_path: &std::path::Path,
    index: &ProjectSessionIndex,
) -> Result<(), String> {
    let json = serde_json::to_string(index).map_err(|e| format!("serialize project index: {e}"))?;
    let tmp = index_path.with_extension(format!("{}.tmp", std::process::id()));
    std::fs::write(&tmp, json).map_err(|e| format!("write project index: {e}"))?;
    restrict_file_permissions(&tmp);
    std::fs::rename(tmp, index_path).map_err(|e| format!("commit project index: {e}"))
}

fn with_project_index_lock<T>(
    dir: &std::path::Path,
    project_root: &str,
    operation: impl FnOnce(&std::path::Path) -> Result<T, String>,
) -> Result<T, String> {
    use fs2::FileExt;
    use std::time::Duration;

    const LOCK_TIMEOUT: Duration = Duration::from_millis(200);

    let index_path = project_index_path(dir, project_root);
    let index_dir = index_path.parent().ok_or("project index has no parent")?;
    std::fs::create_dir_all(index_dir).map_err(|e| format!("create project index: {e}"))?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(index_path.with_extension("lock"))
        .map_err(|e| format!("project index lock: {e}"))?;
    crate::core::file_lock::acquire_exclusive_timeout(&lock, LOCK_TIMEOUT)
        .map_err(|e| format!("project index lock: {e}"))?;
    let result = operation(&index_path);
    let _ = FileExt::unlock(&lock);
    result
}

/// Update one project's bounded warm-history index while its project lock is
/// already held. The index remains an acceleration structure.
fn update_project_index_locked(
    dir: &std::path::Path,
    project_root: &str,
    index_path: &std::path::Path,
    id: &str,
) -> Result<(), String> {
    let mut index = read_project_index(dir, project_root).unwrap_or_else(|| ProjectSessionIndex {
        version: 1,
        project_root: project_root.to_string(),
        session_ids: Vec::new(),
        verified_empty: false,
    });
    index.verified_empty = false;
    index.session_ids.retain(|existing| existing != id);
    index.session_ids.push(id.to_string());
    let excess = index
        .session_ids
        .len()
        .saturating_sub(PROJECT_HISTORY_LIMIT);
    if excess > 0 {
        index.session_ids.drain(..excess);
    }
    write_project_index(index_path, &index)
}

fn repair_project_index(dir: &std::path::Path, project_root: &str) -> Option<SessionState> {
    // Scan outside the short index lock. A save can proceed during a slow scan;
    // its index entry is merged below before the repaired index is written.
    let entries = std::fs::read_dir(dir).ok()?;
    let mut matches = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
            continue;
        }
        let Some(id) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        if id == "latest" || id.starts_with('.') {
            continue;
        }
        let Some(session) = SessionState::load_by_id(id) else {
            continue;
        };
        if session_matches_project_root(&session, std::path::Path::new(project_root)) {
            matches.push(session);
        }
    }
    matches.sort_by_key(|session| session.updated_at);
    let latest = matches.last().cloned();
    let scanned_ids: Vec<String> = matches.iter().map(|session| session.id.clone()).collect();
    let result = with_project_index_lock(dir, project_root, |index_path| {
        // The scan ran outside the lock; a save may have indexed a session in the
        // meantime. Merge instead of overwriting so that id is never lost, but drop
        // ids whose file is gone (the reason this repair ran).
        let mut session_ids = scanned_ids;
        if let Some(current) = read_project_index(dir, project_root) {
            for id in current.session_ids {
                if validate_session_id(&id).is_err() || !dir.join(format!("{id}.json")).is_file() {
                    continue;
                }
                session_ids.retain(|existing| existing != &id);
                session_ids.push(id);
            }
        }
        let first_retained = session_ids.len().saturating_sub(PROJECT_HISTORY_LIMIT);
        session_ids.drain(..first_retained);
        write_project_index(
            index_path,
            &ProjectSessionIndex {
                version: 1,
                project_root: project_root.to_string(),
                verified_empty: session_ids.is_empty(),
                session_ids,
            },
        )?;
        Ok(latest)
    });
    if let Err(error) = &result {
        tracing::debug!("lean-ctx: session project index repair skipped: {error}");
    }
    result.ok().flatten()
}

#[cfg(unix)]
fn restrict_file_permissions(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let perms = std::fs::Permissions::from_mode(0o600);
    let _ = std::fs::set_permissions(path, perms);
}

#[cfg(not(unix))]
fn restrict_file_permissions(_path: &std::path::Path) {}

fn validate_session_id(id: &str) -> Result<(), String> {
    if id.is_empty()
        || id == "latest"
        || id.starts_with('.')
        || id.contains('/')
        || id.contains('\\')
        || id.contains(std::path::MAIN_SEPARATOR)
    {
        return Err("invalid session id".to_string());
    }
    Ok(())
}

fn persist_session_facts(session: &SessionState) -> Result<(), String> {
    let Some(project_root) = session
        .project_root
        .as_deref()
        .filter(|project_root| !project_root.trim().is_empty())
    else {
        return Ok(());
    };

    let facts = extract_session_facts(session);
    if facts.is_empty() {
        return Ok(());
    }

    let mut knowledge = crate::core::knowledge::ProjectKnowledge::load_or_create(project_root);
    for fact in facts {
        knowledge.add_fact(fact);
    }
    knowledge.save()
}

impl PreparedSave {
    /// Writes the pre-serialized session data, latest pointer, and compaction
    /// snapshot to disk atomically. A per-session file lock and version check
    /// make deferred saves monotonic even when background tasks finish out of
    /// order.
    ///
    /// The lock has a deadline. A save that cannot get it reports that instead
    /// of waiting, because `SessionState::save` restores `unsaved_changes` on
    /// `Err` and the next batch tries again — whereas a caller stuck in an
    /// unbounded wait keeps whatever it holds until the other side lets go
    /// (#1783).
    pub fn write_to_disk(self) -> Result<(), String> {
        self.write_to_disk_with_policy(false).result
    }

    pub(super) fn write_to_disk_with_policy(self, require_new: bool) -> SaveOutcome {
        let mut outcome = SaveOutcome::new(&self);
        outcome.result = self.write_primary_and_pointers(require_new, &mut outcome);
        outcome
    }

    fn write_new_if_project_head(
        self,
        expected: Option<&(String, String)>,
    ) -> Result<Option<SaveOutcome>, String> {
        let Some(project_root) = self.project_index_root.clone() else {
            return Err("project head publication requires a safe project root".to_string());
        };
        let dir = self.dir.clone();
        let mut outcome = SaveOutcome::new(&self);
        let published = with_project_index_lock(&dir, &project_root, |index_path| {
            let current = read_project_head_locked(&dir, &project_root)?;
            if current.as_ref() != expected {
                return Ok(false);
            }
            outcome.result = self.write_primary_and_pointers_locked(
                true,
                &mut outcome,
                Some((&project_root, index_path)),
            );
            Ok(true)
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
    ) -> Result<(), String> {
        if !self.dir.exists() {
            std::fs::create_dir_all(&self.dir).map_err(|e| e.to_string())?;
        }
        if let Some(project_root) = self.project_index_root.clone() {
            let dir = self.dir.clone();
            return with_project_index_lock(&dir, &project_root, |index_path| {
                self.write_primary_and_pointers_locked(
                    require_new,
                    outcome,
                    Some((&project_root, index_path)),
                )
            });
        }
        self.write_primary_and_pointers_locked(require_new, outcome, None)
    }

    fn write_primary_and_pointers_locked(
        self,
        require_new: bool,
        outcome: &mut SaveOutcome,
        project_index: Option<(&str, &std::path::Path)>,
    ) -> Result<(), String> {
        use fs2::FileExt;
        let lock_path = self.dir.join(format!(".{}.save.lock", self.id));
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(lock_path)
            .map_err(|e| format!("open session save lock: {e}"))?;
        crate::core::file_lock::acquire_exclusive_timeout(&lock, SAVE_LOCK_TIMEOUT)
            .map_err(|e| format!("lock session save: {e}"))?;

        let result = (|| {
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
        result
    }
}

fn persisted_session_version(path: &std::path::Path) -> Option<u32> {
    let value: serde_json::Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    value["version"].as_u64()?.try_into().ok()
}

impl SessionState {
    /// Return the exact persisted project head and its primary-file SHA-256.
    ///
    /// The project index is consulted first, with the existing repair scan
    /// semantics used read-only when the index is absent or stale.
    pub(crate) fn project_head(project_root: &str) -> Result<Option<(String, String)>, String> {
        let Some(project_root) = normalized_safe_project_root(project_root) else {
            return Err("unsafe project root".to_string());
        };
        let dir = sessions_dir().ok_or("cannot determine home directory")?;
        with_project_index_lock(&dir, &project_root, |_| {
            read_project_head_locked(&dir, &project_root)
        })
    }

    /// Load the selected session and hash the exact primary file bytes together.
    /// If the primary changes after head selection, fail closed for caller retry.
    pub(crate) fn load_project_head_snapshot(
        project_root: &str,
    ) -> Result<Option<(Self, String)>, String> {
        let Some(project_root) = normalized_safe_project_root(project_root) else {
            return Err("unsafe project root".to_string());
        };
        let dir = sessions_dir().ok_or("cannot determine home directory")?;
        with_project_index_lock(&dir, &project_root, |_| {
            let Some((id, expected_digest)) = read_project_head_locked(&dir, &project_root)? else {
                return Ok(None);
            };
            let path = dir.join(format!("{id}.json"));
            let bytes = std::fs::read(path)
                .map_err(|_| "project session head changed; retry".to_string())?;
            let actual_hex = hex::encode(Sha256::digest(&bytes));
            if expected_digest != format!("sha256:{actual_hex}") {
                return Err("project session head changed; retry".to_string());
            }
            let json = std::str::from_utf8(&bytes)
                .map_err(|_| "corrupt project session head".to_string())?;
            let session = SessionState::from_storage_json(json)
                .map(normalize_loaded_session)
                .map_err(|_| "corrupt project session head".to_string())?;
            if session.id != id
                || !session_matches_project_root(&session, std::path::Path::new(&project_root))
            {
                return Err("project session head changed; retry".to_string());
            }
            Ok(Some((session, actual_hex)))
        })
    }

    /// Return the current project root only when it is an absolute, non-broad
    /// root normalized with the same path rules as scoped session loading.
    pub(crate) fn strict_project_root(&self) -> Option<String> {
        self.project_root
            .as_deref()
            .and_then(normalized_strict_project_root)
    }

    /// Compare a loaded session against an already trusted normalized root.
    ///
    /// Re-normalizing the expected value keeps this helper safe for callers
    /// that do not hold the receiving-root invariant themselves.
    pub(crate) fn has_exact_project_root(&self, expected_root: &str) -> bool {
        let Some(expected_root) = normalized_strict_project_root(expected_root) else {
            return false;
        };
        self.strict_project_root()
            .is_some_and(|actual_root| actual_root == expected_root)
    }

    /// Counts locally recorded decisions from the trailing seven days.
    #[must_use]
    pub fn decision_count_this_week() -> u64 {
        let cutoff = Utc::now() - chrono::Duration::days(7);
        Self::list_sessions()
            .into_iter()
            .filter_map(|summary| Self::load_by_id(&summary.id))
            .flat_map(|session| session.decisions)
            .filter(|decision| decision.timestamp >= cutoff)
            .count()
            .try_into()
            .unwrap_or(u64::MAX)
    }

    /// Serializes and writes the session state to disk synchronously.
    pub fn save(&mut self) -> Result<(), String> {
        let prepared = self
            .prepare_save()
            .inspect_err(|_| self.note_save_failure())?;
        self.acknowledge_save(prepared.write_to_disk_with_policy(false))
    }

    /// Import publication never replaces an existing session archive. A failure
    /// after primary publication may retain a new recovery candidate, not erase
    /// the previous live session or source archive.
    pub(crate) fn save_new(&mut self) -> Result<(), String> {
        let prepared = self
            .prepare_save()
            .inspect_err(|_| self.note_save_failure())?;
        self.acknowledge_save(prepared.write_to_disk_with_policy(true))
    }

    /// Publish a new project session only when the persisted project head still
    /// equals `expected`; a conflict leaves all session files and pointers alone.
    pub(crate) fn save_new_if_project_head(
        &mut self,
        expected: Option<&(String, String)>,
    ) -> Result<bool, String> {
        self.save_new_if_project_head_with_digest(expected)
            .map(|committed| committed.is_some())
    }

    /// Publish with the existing project-head CAS and return the exact primary
    /// session file digest produced by this commit.
    pub(crate) fn save_new_if_project_head_with_digest(
        &mut self,
        expected: Option<&(String, String)>,
    ) -> Result<Option<String>, String> {
        let prepared = self
            .prepare_save()
            .inspect_err(|_| self.note_save_failure())?;
        match prepared.write_new_if_project_head(expected) {
            Ok(Some(outcome)) => {
                let digest = outcome
                    .primary_sha256
                    .clone()
                    .ok_or_else(|| "committed session digest unavailable".to_string())?;
                self.acknowledge_save(outcome)?;
                Ok(Some(digest))
            }
            Ok(None) => Ok(None),
            Err(error) => {
                self.note_save_failure();
                Err(error)
            }
        }
    }

    /// Serialize session state while holding the lock (CPU-only), retaining the
    /// unsaved counter until acknowledged, and return I/O that can be deferred
    /// to a background thread via `write_to_disk()`.
    pub fn prepare_save(&mut self) -> Result<PreparedSave, String> {
        validate_session_id(&self.id)?;
        if self
            .project_root
            .as_deref()
            .is_some_and(|root| normalized_safe_project_root(root).is_none())
        {
            return Err(
                "refusing to persist a session for a broad or unsafe project root".to_string(),
            );
        }
        let dir = sessions_dir().ok_or("cannot determine home directory")?;
        let compaction_snapshot = if self.stats.total_tool_calls > 0 {
            Some(self.build_compaction_snapshot())
        } else {
            None
        };
        let (json, _) = self.storage_json()?;
        let pointer_json = serde_json::to_string(&LatestPointer {
            id: self.id.clone(),
        })
        .map_err(|e| e.to_string())?;
        Ok(PreparedSave {
            canonical: self.canonical_checkpoint.is_some(),
            expected_storage_digest: self
                .canonical_checkpoint
                .as_ref()
                .and_then(|binding| binding.storage_digest.clone()),
            dir,
            id: self.id.clone(),
            version: self.version,
            json,
            pointer_json,
            compaction_snapshot,
            project_index_root: self
                .project_root
                .as_deref()
                .and_then(normalized_safe_project_root),
        })
    }

    /// Load the bounded warm-history set for one safe project root.
    ///
    /// There is intentionally no legacy full-store fallback: cache warming is
    /// optional, while scanning every persisted session on each MCP launch is
    /// not acceptable under concurrent agent load. New saves populate the
    /// index; legacy sessions remain available through explicit session tools.
    pub(crate) fn load_recent_for_project_root(project_root: &str, limit: usize) -> Vec<Self> {
        let Some(project_root) = normalized_strict_project_root(project_root) else {
            return Vec::new();
        };
        let Some(dir) = sessions_dir() else {
            return Vec::new();
        };
        let Some(index) = std::fs::read_to_string(project_index_path(&dir, &project_root))
            .ok()
            .and_then(|json| serde_json::from_str::<ProjectSessionIndex>(&json).ok())
            .filter(|index| index.version == 1 && index.project_root == project_root)
        else {
            return Vec::new();
        };

        index
            .session_ids
            .iter()
            .rev()
            .take(limit.min(PROJECT_HISTORY_LIMIT))
            .filter_map(|id| {
                Self::load_by_id_for_project_root(id, &project_root)
                    .ok()
                    .flatten()
            })
            .collect()
    }

    /// Loads the most recent session matching the current working directory's
    /// project root.
    ///
    /// Returns `None` (a fresh session) rather than falling back to the global
    /// `latest.json` pointer: that unconditional fallback bypassed project-root
    /// matching and was the root cause of cross-project session leakage — one
    /// project's findings/decisions/knowledge bleeding into another project's
    /// first session. The correct project session is loaded later from the MCP
    /// `roots` handshake (`load_latest_for_project_root`).
    ///
    /// Also refuses to scope to a broad/unsafe cwd (e.g. the MCP daemon's HOME),
    /// which would otherwise resurrect the contaminated "HOME mega-session".
    pub fn load_latest() -> Option<Self> {
        let cwd = std::env::current_dir().ok()?;
        if crate::core::pathutil::is_broad_or_unsafe_root(&cwd) {
            return None;
        }
        Self::load_latest_for_project_root(&cwd.to_string_lossy())
    }

    /// Loads the session referenced by the global `latest.json` pointer,
    /// regardless of project. Intended only for explicit, cross-project UX
    /// (e.g. `lean-ctx session` status from an arbitrary directory) — never for
    /// injecting knowledge into a new project's context. Prefer `load_latest`.
    pub fn load_global_latest_pointer() -> Option<Self> {
        let dir = sessions_dir()?;
        let latest_path = dir.join("latest.json");
        let pointer_json = std::fs::read_to_string(&latest_path).ok()?;
        let pointer: LatestPointer = serde_json::from_str(&pointer_json).ok()?;
        Self::load_by_id(&pointer.id)
    }

    /// Loads the most recent session matching a specific project root.
    ///
    /// A valid per-project index is the only nominal path: one index read and
    /// one session read, independent of the global session-store cardinality.
    /// A missing, corrupt, or stale index is repaired by one exceptional scan.
    pub fn load_latest_for_project_root(project_root: &str) -> Option<Self> {
        let target_root = normalized_safe_project_root(project_root)?;
        let dir = sessions_dir()?;

        if let Some(index) = read_project_index(&dir, &target_root) {
            if let Some(id) = index.session_ids.last()
                && let Some(session) = Self::load_by_id(id)
                && session_matches_project_root(&session, std::path::Path::new(&target_root))
            {
                return Some(session);
            }
            if index.session_ids.is_empty() && index.verified_empty {
                return None;
            }
        }

        repair_project_index(&dir, &target_root)
    }

    fn load_by_id_raw(id: &str) -> Option<Self> {
        validate_session_id(id).ok()?;
        let dir = sessions_dir()?;
        let path = dir.join(format!("{id}.json"));
        let json = std::fs::read_to_string(&path).ok()?;
        Self::from_storage_json(&json).ok()
    }

    /// Loads a specific session only when its persisted root already matches
    /// the receiving root; legacy normalization runs after that check.
    pub(crate) fn load_by_id_for_project_root(
        id: &str,
        expected_root: &str,
    ) -> Result<Option<Self>, SessionScopeError> {
        let Some(session) = Self::load_by_id_raw(id) else {
            return Ok(None);
        };
        if !session.has_exact_project_root(expected_root) {
            return Err(SessionScopeError::ProjectRootMismatch);
        }
        let persisted_project_root = session.project_root.clone();
        let mut normalized = normalize_loaded_session(session);
        // Keep the persisted spelling that was checked above; legacy
        // normalization may otherwise replace an admitted root from shell_cwd.
        normalized.project_root = persisted_project_root;
        Ok(Some(normalized))
    }

    /// Loads a specific session from disk by its unique ID.
    pub fn load_by_id(id: &str) -> Option<Self> {
        Self::load_by_id_raw(id).map(normalize_loaded_session)
    }

    /// Deletes a saved session and its compaction snapshot.
    ///
    /// If the deleted session is the global latest pointer, the pointer is
    /// moved to the newest remaining session or removed when none remain.
    pub fn delete_session(id: &str) -> Result<bool, String> {
        validate_session_id(id)?;
        let Some(dir) = sessions_dir() else {
            return Ok(false);
        };
        let path = dir.join(format!("{id}.json"));
        if !path.exists() {
            return Ok(false);
        }

        if let Some(session) = Self::load_by_id(id) {
            persist_session_facts(&session)?;
        }
        std::fs::remove_file(&path).map_err(|e| e.to_string())?;

        let snapshot = dir.join(format!("{id}_snapshot.txt"));
        match std::fs::remove_file(&snapshot) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.to_string()),
        }

        let latest_path = dir.join("latest.json");
        let points_to_deleted = std::fs::read_to_string(&latest_path)
            .ok()
            .and_then(|json| serde_json::from_str::<LatestPointer>(&json).ok())
            .is_some_and(|pointer| pointer.id == id);
        if points_to_deleted {
            if let Some(next) = Self::list_sessions().into_iter().next() {
                let latest_tmp = dir.join(".latest.json.tmp");
                let pointer_json = serde_json::to_string(&LatestPointer { id: next.id })
                    .map_err(|e| e.to_string())?;
                std::fs::write(&latest_tmp, pointer_json).map_err(|e| e.to_string())?;
                restrict_file_permissions(&latest_tmp);
                std::fs::rename(&latest_tmp, &latest_path).map_err(|e| e.to_string())?;
            } else {
                match std::fs::remove_file(&latest_path) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e.to_string()),
                }
            }
        }

        Ok(true)
    }

    /// Lists all saved sessions as summaries, sorted by most recently updated.
    pub fn list_sessions() -> Vec<SessionSummary> {
        let Some(dir) = sessions_dir() else {
            return Vec::new();
        };

        let mut summaries = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("json") {
                    continue;
                }
                if path.file_name().and_then(|n| n.to_str()) == Some("latest.json") {
                    continue;
                }
                if let Ok(json) = std::fs::read_to_string(&path)
                    && let Ok(session) = SessionState::from_storage_json(&json)
                {
                    summaries.push(SessionSummary {
                        id: session.id,
                        started_at: session.started_at,
                        updated_at: session.updated_at,
                        version: session.version,
                        task: session.task.as_ref().map(|t| t.description.clone()),
                        tool_calls: session.stats.total_tool_calls,
                        tokens_saved: session.stats.total_tokens_saved,
                        project_root: session.project_root,
                    });
                }
            }
        }

        summaries.sort_by_key(|x| std::cmp::Reverse(x.updated_at));
        summaries
    }

    /// Scans all saved sessions for contaminated ones — those rooted at a
    /// broad/unsafe path (HOME, filesystem root, agent sandbox dir) without a
    /// real project marker, i.e. the historic "HOME mega-session" artifact.
    ///
    /// Returns `(found, quarantined)` where `found` is `(id, root)` pairs. When
    /// `apply` is true, each offending session file is moved to a
    /// `sessions/quarantine/` subdirectory (non-destructive) instead of being
    /// loaded into any project's context.
    pub fn doctor_quarantine_unsafe_roots(apply: bool) -> (Vec<(String, String)>, usize) {
        let mut found: Vec<(String, String)> = Vec::new();
        let mut quarantined = 0usize;
        let Some(dir) = sessions_dir() else {
            return (found, quarantined);
        };
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return (found, quarantined);
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Some(id) = path.file_stem().and_then(|n| n.to_str()) else {
                continue;
            };
            if id == "latest" || id.starts_with('.') {
                continue;
            }
            let Some(session) = Self::load_by_id(id) else {
                continue;
            };
            let Some(root) = session.project_root.as_deref() else {
                continue;
            };
            let root_path = std::path::Path::new(root);
            if crate::core::pathutil::is_broad_or_unsafe_root(root_path) {
                found.push((id.to_string(), root.to_string()));
                if apply {
                    let q_dir = dir.join("quarantine");
                    if std::fs::create_dir_all(&q_dir).is_ok()
                        && std::fs::rename(&path, q_dir.join(format!("{id}.json"))).is_ok()
                    {
                        quarantined += 1;
                    }
                }
            }
        }
        (found, quarantined)
    }

    /// Deletes sessions older than `max_age_days`, preserving the most recent
    /// session for every project root. Returns the count removed.
    ///
    /// This is an explicit retention operation, so its single store scan stays
    /// off the command hot path.
    pub fn cleanup_old_sessions(max_age_days: i64) -> u32 {
        let Some(dir) = sessions_dir() else { return 0 };
        let cutoff = Utc::now() - chrono::Duration::days(max_age_days);
        let global_latest = Self::load_global_latest_pointer().map(|session| session.id);
        let mut sessions = Vec::new();

        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
                    continue;
                }
                let Some(id) = path.file_stem().and_then(|stem| stem.to_str()) else {
                    continue;
                };
                if id == "latest" || id.starts_with('.') {
                    continue;
                }
                if let Some(session) = Self::load_by_id(id) {
                    sessions.push((path, session));
                }
            }
        }

        let mut newest_by_project = std::collections::HashMap::new();
        for (_, session) in &sessions {
            let Some(project_root) = session
                .project_root
                .as_deref()
                .filter(|root| !root.trim().is_empty())
            else {
                continue;
            };
            newest_by_project
                .entry(project_root)
                .and_modify(|current: &mut &SessionState| {
                    if session.updated_at > current.updated_at {
                        *current = session;
                    }
                })
                .or_insert(session);
        }

        let mut retained_ids: std::collections::HashSet<String> = newest_by_project
            .values()
            .map(|session| session.id.clone())
            .collect();
        if let Some(id) = global_latest {
            retained_ids.insert(id);
        }

        sessions
            .into_iter()
            .filter(|(_, session)| {
                session.updated_at < cutoff && !retained_ids.contains(&session.id)
            })
            .filter(|(_, session)| persist_session_facts(session).is_ok())
            .filter(|(path, _)| std::fs::remove_file(path).is_ok())
            .map(|(path, session)| {
                let snapshot = path.with_file_name(format!("{}_snapshot.txt", session.id));
                let _ = std::fs::remove_file(snapshot);
            })
            .count() as u32
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ProjectSessionIndex, SessionState, normalized_safe_project_root, project_index_path,
        write_project_index,
    };
    use chrono::{Duration, Utc};
    use sha2::{Digest as _, Sha256};

    #[test]
    fn recent_project_sessions_use_bounded_index_without_scanning_legacy_store() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let project = tempfile::tempdir().expect("project tempdir");
        let root = project.path().to_string_lossy().to_string();
        assert_eq!(SessionState::project_head(&root), Ok(None));

        for id in ["first", "second", "third"] {
            let mut session = SessionState::new();
            session.id = id.to_string();
            session.project_root = Some(root.clone());
            session.save().expect("save indexed session");
        }

        // A malformed legacy artifact proves the hot path consults only the
        // per-project index, never `list_sessions()` as a hidden fallback.
        let sessions = crate::core::session::paths::sessions_dir().expect("sessions dir");
        std::fs::write(sessions.join("legacy-unreadable.json"), "not json")
            .expect("write legacy artifact");

        let ids: Vec<_> = SessionState::load_recent_for_project_root(&root, 8)
            .into_iter()
            .map(|session| session.id)
            .collect();
        assert_eq!(ids, ["third", "second", "first"]);
    }

    #[test]
    fn recent_project_sessions_refuse_broad_roots() {
        let _data = crate::core::data_dir::isolated_data_dir();
        assert!(SessionState::load_recent_for_project_root("/", 8).is_empty());
    }

    /// #1783: a save must never wait on the per-session lock indefinitely. The
    /// callers that matter hold a `tokio` guard the rest of the server needs,
    /// so an unbounded wait here is a permanent server-wide wedge; a reported
    /// failure is recoverable, because `save` re-arms `unsaved_changes` and the
    /// next batch writes.
    #[test]
    fn save_gives_up_when_another_holder_keeps_the_session_lock() {
        use fs2::FileExt;

        let _data = crate::core::data_dir::isolated_data_dir();
        let project = tempfile::tempdir().expect("project tempdir");
        let mut session = SessionState::new();
        session.id = "wedged".to_string();
        session.project_root = Some(project.path().to_string_lossy().to_string());

        let sessions = crate::core::session::paths::sessions_dir().expect("sessions dir");
        std::fs::create_dir_all(&sessions).expect("create sessions dir");
        let holder = std::fs::File::create(sessions.join(".wedged.save.lock")).expect("lock file");
        holder.lock_exclusive().expect("hold the save lock");

        let started = std::time::Instant::now();
        let error = session
            .save()
            .expect_err("a held lock must not be waited out");
        let waited = started.elapsed();

        assert!(
            error.contains("lock session save"),
            "unexpected error: {error}"
        );
        assert!(
            waited < super::SAVE_LOCK_TIMEOUT * 4,
            "save must return on its own deadline, waited {waited:?}"
        );
        assert_ne!(
            session.stats.unsaved_changes, 0,
            "a failed save must stay pending so the next batch retries"
        );
        FileExt::unlock(&holder).expect("release");
    }

    #[test]
    fn broad_root_sessions_are_never_persisted() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let mut session = SessionState::new();
        session.project_root = Some("/".to_string());

        let error = session
            .save()
            .expect_err("broad root save must be rejected");

        assert!(error.contains("broad or unsafe"));
        assert!(
            crate::core::session::paths::sessions_dir()
                .expect("sessions dir")
                .read_dir()
                .map_or(true, |mut entries| entries.next().is_none())
        );
    }

    #[test]
    fn latest_project_session_uses_valid_index_without_scanning_unindexed_sessions() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let project = tempfile::tempdir().expect("project tempdir");
        let root = project.path().to_string_lossy().to_string();

        let mut indexed = SessionState::new();
        indexed.id = "indexed".to_string();
        indexed.project_root = Some(root.clone());
        indexed.updated_at = Utc::now() - Duration::hours(1);
        indexed.save().expect("save indexed session");

        // This valid but unindexed legacy file is newer. The nominal path must
        // trust the index and never deserialize unrelated root-level sessions.
        let mut unindexed = SessionState::new();
        unindexed.id = "unindexed".to_string();
        unindexed.project_root = Some(root.clone());
        unindexed.updated_at = Utc::now();
        let sessions = crate::core::session::paths::sessions_dir().expect("sessions dir");
        std::fs::write(
            sessions.join("unindexed.json"),
            serde_json::to_string(&unindexed).expect("serialize legacy session"),
        )
        .expect("write unindexed session");

        assert_eq!(
            SessionState::load_latest_for_project_root(&root)
                .expect("load indexed session")
                .id,
            "indexed"
        );
    }

    #[test]
    fn latest_project_session_repairs_a_missing_index() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let project = tempfile::tempdir().expect("project tempdir");
        let root = project.path().to_string_lossy().to_string();
        let mut session = SessionState::new();
        session.id = "repair-missing".to_string();
        session.project_root = Some(root.clone());
        session.save().expect("save indexed session");

        let sessions = crate::core::session::paths::sessions_dir().expect("sessions dir");
        let canonical_root = normalized_safe_project_root(&root).expect("safe root");
        let index_path = project_index_path(&sessions, &canonical_root);
        std::fs::remove_file(&index_path).expect("remove project index");

        assert_eq!(
            SessionState::load_latest_for_project_root(&root)
                .expect("repair and load session")
                .id,
            "repair-missing"
        );
        let repaired: ProjectSessionIndex =
            serde_json::from_str(&std::fs::read_to_string(index_path).expect("repaired index"))
                .expect("valid repaired index");
        assert_eq!(repaired.session_ids, ["repair-missing"]);
    }

    #[test]
    fn latest_project_session_repairs_an_empty_index() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let project = tempfile::tempdir().expect("project tempdir");
        let root = project.path().to_string_lossy().to_string();
        let mut session = SessionState::new();
        session.id = "repair-empty".to_string();
        session.project_root = Some(root.clone());
        session.save().expect("save indexed session");

        let sessions = crate::core::session::paths::sessions_dir().expect("sessions dir");
        let canonical_root = normalized_safe_project_root(&root).expect("safe root");
        let index_path = project_index_path(&sessions, &canonical_root);
        write_project_index(
            &index_path,
            &ProjectSessionIndex {
                version: 1,
                project_root: canonical_root,
                session_ids: Vec::new(),
                verified_empty: false,
            },
        )
        .expect("empty project index");

        assert_eq!(
            SessionState::load_latest_for_project_root(&root)
                .expect("repair and load session")
                .id,
            "repair-empty"
        );
        let repaired: ProjectSessionIndex =
            serde_json::from_str(&std::fs::read_to_string(index_path).expect("repaired index"))
                .expect("valid repaired index");
        assert_eq!(repaired.session_ids, ["repair-empty"]);
    }

    #[test]
    fn a_scan_that_finds_no_session_is_cached_until_the_next_save() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let project = tempfile::tempdir().expect("project tempdir");
        let root = project.path().to_string_lossy().to_string();
        let sessions = crate::core::session::paths::sessions_dir().expect("sessions dir");
        std::fs::create_dir_all(&sessions).expect("session store exists");

        // First lookup scans the store, finds nothing and records that.
        assert!(SessionState::load_latest_for_project_root(&root).is_none());

        // A later unindexed file is not picked up: the verified-empty answer is
        // reused instead of re-parsing the whole store on every process start.
        let mut unindexed = SessionState::new();
        unindexed.id = "unindexed-after-scan".to_string();
        unindexed.project_root = Some(root.clone());
        std::fs::write(
            sessions.join("unindexed-after-scan.json"),
            serde_json::to_string(&unindexed).expect("serialize session"),
        )
        .expect("write unindexed session");
        assert!(SessionState::load_latest_for_project_root(&root).is_none());

        // A real save indexes the session and ends the empty state.
        let mut saved = SessionState::new();
        saved.id = "saved-after-scan".to_string();
        saved.project_root = Some(root.clone());
        saved.save().expect("save session");
        assert_eq!(
            SessionState::load_latest_for_project_root(&root)
                .expect("saved session")
                .id,
            "saved-after-scan"
        );
    }

    #[test]
    fn latest_project_session_repairs_a_corrupt_index() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let project = tempfile::tempdir().expect("project tempdir");
        let root = project.path().to_string_lossy().to_string();
        let mut session = SessionState::new();
        session.id = "repair-corrupt".to_string();
        session.project_root = Some(root.clone());
        session.save().expect("save indexed session");

        let sessions = crate::core::session::paths::sessions_dir().expect("sessions dir");
        let canonical_root = normalized_safe_project_root(&root).expect("safe root");
        let index_path = project_index_path(&sessions, &canonical_root);
        std::fs::write(&index_path, "not json").expect("corrupt project index");

        assert_eq!(
            SessionState::load_latest_for_project_root(&root)
                .expect("repair and load session")
                .id,
            "repair-corrupt"
        );
        let repaired: ProjectSessionIndex =
            serde_json::from_str(&std::fs::read_to_string(index_path).expect("repaired index"))
                .expect("valid repaired index");
        assert_eq!(repaired.session_ids, ["repair-corrupt"]);
    }

    #[test]
    fn latest_project_session_repairs_an_index_with_a_missing_session() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let project = tempfile::tempdir().expect("project tempdir");
        let root = project.path().to_string_lossy().to_string();

        let mut older = SessionState::new();
        older.id = "repair-older".to_string();
        older.project_root = Some(root.clone());
        older.updated_at = Utc::now() - Duration::hours(1);
        older.save().expect("save older session");

        let mut missing = SessionState::new();
        missing.id = "repair-missing-file".to_string();
        missing.project_root = Some(root.clone());
        missing.save().expect("save missing session");
        let sessions = crate::core::session::paths::sessions_dir().expect("sessions dir");
        std::fs::remove_file(sessions.join("repair-missing-file.json"))
            .expect("remove indexed session");

        assert_eq!(
            SessionState::load_latest_for_project_root(&root)
                .expect("repair and load fallback")
                .id,
            "repair-older"
        );
        let repaired: ProjectSessionIndex = serde_json::from_str(
            &std::fs::read_to_string(project_index_path(
                &sessions,
                &normalized_safe_project_root(&root).expect("safe root"),
            ))
            .expect("repaired index"),
        )
        .expect("valid repaired index");
        assert_eq!(repaired.session_ids, ["repair-older"]);
    }

    #[test]
    fn cleanup_old_sessions_preserves_the_latest_session_for_each_project() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let project_a = tempfile::tempdir().expect("project A");
        let project_b = tempfile::tempdir().expect("project B");
        let root_a = project_a.path().to_string_lossy().to_string();
        let root_b = project_b.path().to_string_lossy().to_string();

        for (id, root, age_days) in [
            ("a-old", root_a.as_str(), 10),
            ("a-latest", root_a.as_str(), 8),
            ("b-old", root_b.as_str(), 10),
            ("b-latest", root_b.as_str(), 8),
        ] {
            let mut session = SessionState::new();
            session.id = id.to_string();
            session.project_root = Some(root.to_string());
            session.updated_at = Utc::now() - Duration::days(age_days);
            session.save().expect("save session");
        }

        assert_eq!(SessionState::cleanup_old_sessions(7), 2);
        assert!(SessionState::load_by_id("a-latest").is_some());
        assert!(SessionState::load_by_id("b-latest").is_some());
        assert!(SessionState::load_by_id("a-old").is_none());
        assert!(SessionState::load_by_id("b-old").is_none());
    }

    #[test]
    fn deferred_save_cannot_replace_a_newer_session_version() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let mut session = SessionState::new();
        let id = session.id.clone();
        let older = session.prepare_save().expect("prepare older save");
        session.increment();
        let expected_version = session.version;
        let newer = session.prepare_save().expect("prepare newer save");

        newer.write_to_disk().expect("write newer save");
        older.write_to_disk().expect("skip older save");

        assert_eq!(
            SessionState::load_by_id(&id)
                .expect("load persisted session")
                .version,
            expected_version
        );
    }

    #[test]
    fn project_head_cas_conflict_preserves_live_files_and_success_returns_exact_digest() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let project = tempfile::tempdir().expect("project tempdir");
        let root = project.path().to_string_lossy().to_string();

        let mut initial = SessionState::new();
        initial.id = "cas-initial".to_string();
        initial.project_root = Some(root.clone());
        initial.save().expect("save initial project session");
        let stale_head = SessionState::project_head(&root)
            .expect("read initial project head")
            .expect("initial head exists");

        let mut local = SessionState::new();
        local.id = "cas-local".to_string();
        local.project_root = Some(root.clone());
        local.save().expect("save competing local session");

        let sessions = crate::core::session::paths::sessions_dir().expect("sessions dir");
        let latest_path = sessions.join("latest.json");
        let index_path = project_index_path(
            &sessions,
            &normalized_safe_project_root(&root).expect("safe root"),
        );
        let latest_before_conflict = std::fs::read(&latest_path).expect("latest pointer");
        let index_before_conflict = std::fs::read(&index_path).expect("project index");

        let mut candidate = SessionState::new();
        candidate.id = "cas-candidate".to_string();
        candidate.project_root = Some(root.clone());
        assert!(
            !candidate
                .save_new_if_project_head(Some(&stale_head))
                .expect("stale head is a conflict")
        );
        assert!(!sessions.join("cas-candidate.json").exists());
        assert_eq!(
            std::fs::read(&latest_path).expect("latest after conflict"),
            latest_before_conflict
        );
        assert_eq!(
            std::fs::read(&index_path).expect("index after conflict"),
            index_before_conflict
        );

        let current_head = SessionState::project_head(&root)
            .expect("read current project head")
            .expect("current head exists");
        assert!(
            candidate
                .save_new_if_project_head(Some(&current_head))
                .expect("publish against current head")
        );
        let persisted =
            std::fs::read(sessions.join("cas-candidate.json")).expect("published primary");
        assert_eq!(
            SessionState::project_head(&root).expect("read published project head"),
            Some((
                "cas-candidate".to_string(),
                format!("sha256:{}", hex::encode(Sha256::digest(&persisted))),
            ))
        );
    }
}
