use chrono::Utc;
use serde::{Deserialize, Serialize};

use super::heuristics::{normalize_loaded_session, session_matches_project_root};
use super::paths::sessions_dir;
use super::state::{BATCH_SAVE_INTERVAL, extract_session_facts};
#[allow(clippy::wildcard_imports)]
use super::types::*;

/// Keep the startup warm set deliberately small: cache warming is an optional
/// optimisation and must never turn process startup into a session-store scan.
const PROJECT_HISTORY_LIMIT: usize = 8;

#[derive(Debug, Default, Deserialize, Serialize)]
struct ProjectSessionIndex {
    version: u8,
    project_root: String,
    /// Oldest to newest; duplicates are removed before appending on every save.
    session_ids: Vec<String>,
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
    use std::time::{Duration, Instant};

    const LOCK_TIMEOUT: Duration = Duration::from_millis(200);
    const RETRY_INTERVAL: Duration = Duration::from_millis(10);

    let index_path = project_index_path(dir, project_root);
    let index_dir = index_path.parent().ok_or("project index has no parent")?;
    std::fs::create_dir_all(index_dir).map_err(|e| format!("create project index: {e}"))?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(index_path.with_extension("lock"))
        .map_err(|e| format!("project index lock: {e}"))?;
    let deadline = Instant::now() + LOCK_TIMEOUT;
    loop {
        match lock.try_lock_exclusive() {
            Ok(()) => break,
            Err(error)
                if crate::core::file_lock::is_contended(&error) && Instant::now() < deadline =>
            {
                std::thread::sleep(RETRY_INTERVAL);
            }
            Err(error) if crate::core::file_lock::is_contended(&error) => {
                return Err("project index lock timed out".to_string());
            }
            Err(error) => return Err(format!("project index lock: {error}")),
        }
    }
    let result = operation(&index_path);
    let _ = FileExt::unlock(&lock);
    result
}

/// Update one project's bounded warm-history index under a short, local lock.
/// The index is strictly an acceleration structure: a failure never invalidates
/// the already-committed session save.
fn update_project_index(dir: &std::path::Path, project_root: &str, id: &str) -> Result<(), String> {
    with_project_index_lock(dir, project_root, |index_path| {
        let mut index =
            read_project_index(dir, project_root).unwrap_or_else(|| ProjectSessionIndex {
                version: 1,
                project_root: project_root.to_string(),
                session_ids: Vec::new(),
            });
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
    })
}

fn repair_project_index(dir: &std::path::Path, project_root: &str) -> Option<SessionState> {
    let mut matches = Vec::new();
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
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
    let first_retained = matches.len().saturating_sub(PROJECT_HISTORY_LIMIT);
    let session_ids = matches[first_retained..]
        .iter()
        .map(|session| session.id.clone())
        .collect();
    if let Err(error) = with_project_index_lock(dir, project_root, |index_path| {
        write_project_index(
            index_path,
            &ProjectSessionIndex {
                version: 1,
                project_root: project_root.to_string(),
                session_ids,
            },
        )
    }) {
        tracing::debug!("lean-ctx: session project index repair skipped: {error}");
    }
    latest
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
    pub fn write_to_disk(self) -> Result<(), String> {
        use fs2::FileExt;

        if !self.dir.exists() {
            std::fs::create_dir_all(&self.dir).map_err(|e| e.to_string())?;
        }
        let lock_path = self.dir.join(format!(".{}.save.lock", self.id));
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(lock_path)
            .map_err(|e| format!("open session save lock: {e}"))?;
        lock.lock_exclusive()
            .map_err(|e| format!("lock session save: {e}"))?;

        let result = (|| {
            let path = self.dir.join(format!("{}.json", self.id));
            if persisted_session_version(&path).is_some_and(|version| version > self.version) {
                return Ok(());
            }
            let tmp = self.dir.join(format!(".{}.json.tmp", self.id));
            std::fs::write(&tmp, &self.json).map_err(|e| e.to_string())?;
            restrict_file_permissions(&tmp);
            std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;

            let latest_path = self.dir.join("latest.json");
            let latest_tmp = self.dir.join(".latest.json.tmp");
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
            if let Some(project_root) = self.project_index_root.as_deref()
                && let Err(error) = update_project_index(&self.dir, project_root, &self.id)
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
        let prepared = self.prepare_save()?;
        match prepared.write_to_disk() {
            Ok(()) => Ok(()),
            Err(e) => {
                self.stats.unsaved_changes = BATCH_SAVE_INTERVAL;
                Err(e)
            }
        }
    }

    /// Serialize session state while holding the lock (CPU-only), reset the
    /// unsaved counter, and return a `PreparedSave` whose I/O can be deferred
    /// to a background thread via `write_to_disk()`.
    pub fn prepare_save(&mut self) -> Result<PreparedSave, String> {
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
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        let pointer_json = serde_json::to_string(&LatestPointer {
            id: self.id.clone(),
        })
        .map_err(|e| e.to_string())?;
        self.stats.unsaved_changes = 0;
        // #717: arm the time-based flush window.
        self.last_flush = Some(std::time::Instant::now());
        Ok(PreparedSave {
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
        let Some(project_root) = normalized_safe_project_root(project_root) else {
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
            .filter_map(|id| Self::load_by_id(id))
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

        if let Some(index) = read_project_index(&dir, &target_root)
            && let Some(id) = index.session_ids.last()
            && let Some(session) = Self::load_by_id(id)
            && session_matches_project_root(&session, std::path::Path::new(&target_root))
        {
            return Some(session);
        }

        repair_project_index(&dir, &target_root)
    }

    /// Loads a specific session from disk by its unique ID.
    pub fn load_by_id(id: &str) -> Option<Self> {
        validate_session_id(id).ok()?;
        let dir = sessions_dir()?;
        let path = dir.join(format!("{id}.json"));
        let json = std::fs::read_to_string(&path).ok()?;
        let session: Self = serde_json::from_str(&json).ok()?;
        Some(normalize_loaded_session(session))
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
                    && let Ok(session) = serde_json::from_str::<SessionState>(&json)
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

    #[test]
    fn recent_project_sessions_use_bounded_index_without_scanning_legacy_store() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let project = tempfile::tempdir().expect("project tempdir");
        let root = project.path().to_string_lossy().to_string();

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
}
