use chrono::Utc;

use super::heuristics::{normalize_loaded_session, session_matches_project_root};
use super::paths::sessions_dir;
use super::state::BATCH_SAVE_INTERVAL;
#[allow(clippy::wildcard_imports)]
use super::types::*;

#[cfg(unix)]
fn restrict_file_permissions(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let perms = std::fs::Permissions::from_mode(0o600);
    let _ = std::fs::set_permissions(path, perms);
}

#[cfg(not(unix))]
fn restrict_file_permissions(_path: &std::path::Path) {}

impl PreparedSave {
    /// Writes the pre-serialized session data, latest pointer, and compaction
    /// snapshot to disk atomically.
    pub fn write_to_disk(self) -> Result<(), String> {
        if !self.dir.exists() {
            std::fs::create_dir_all(&self.dir).map_err(|e| e.to_string())?;
        }
        let path = self.dir.join(format!("{}.json", self.id));
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
            let _ = std::fs::write(&snap_path, &snapshot);
            restrict_file_permissions(&snap_path);
        }
        Ok(())
    }
}

impl SessionState {
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
        Ok(PreparedSave {
            dir,
            id: self.id.clone(),
            json,
            pointer_json,
            compaction_snapshot,
        })
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
    #[must_use]
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
    #[must_use]
    pub fn load_global_latest_pointer() -> Option<Self> {
        let dir = sessions_dir()?;
        let latest_path = dir.join("latest.json");
        let pointer_json = std::fs::read_to_string(&latest_path).ok()?;
        let pointer: LatestPointer = serde_json::from_str(&pointer_json).ok()?;
        Self::load_by_id(&pointer.id)
    }

    /// Loads the most recent session matching a specific project root.
    #[must_use]
    pub fn load_latest_for_project_root(project_root: &str) -> Option<Self> {
        // Broad roots ("/", HOME, agent sandboxes) never own a session. Bail out
        // BEFORE scanning: the daemon boots with cwd "/" and previously walked
        // every stored session here, stat-ing each session's project_root /
        // shell_cwd. For roots under ~/Documents that probe popped the macOS
        // TCC prompt in lean-ctx's name on every launchd (re)start (#356) —
        // and `shell_cwd.starts_with("/")` could even leak an arbitrary
        // project's session into the broad-root context.
        if crate::core::pathutil::is_broad_or_unsafe_root(std::path::Path::new(project_root)) {
            return None;
        }
        let dir = sessions_dir()?;
        let target_root =
            crate::core::pathutil::safe_canonicalize_or_self(std::path::Path::new(project_root));
        let mut latest_match: Option<Self> = None;

        for entry in std::fs::read_dir(&dir).ok()?.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            if path.file_name().and_then(|n| n.to_str()) == Some("latest.json") {
                continue;
            }

            let Some(id) = path.file_stem().and_then(|n| n.to_str()) else {
                continue;
            };
            let Some(session) = Self::load_by_id(id) else {
                continue;
            };

            if !session_matches_project_root(&session, &target_root) {
                continue;
            }

            if latest_match
                .as_ref()
                .is_none_or(|existing| session.updated_at > existing.updated_at)
            {
                latest_match = Some(session);
            }
        }

        latest_match
    }

    /// Loads a specific session from disk by its unique ID.
    #[must_use]
    pub fn load_by_id(id: &str) -> Option<Self> {
        let dir = sessions_dir()?;
        let path = dir.join(format!("{id}.json"));
        let json = std::fs::read_to_string(&path).ok()?;
        let session: Self = serde_json::from_str(&json).ok()?;
        Some(normalize_loaded_session(session))
    }

    /// Lists all saved sessions as summaries, sorted by most recently updated.
    #[must_use]
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
    #[must_use]
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

    /// Deletes sessions older than `max_age_days`, preserving the latest. Returns count removed.
    #[must_use]
    pub fn cleanup_old_sessions(max_age_days: i64) -> u32 {
        let Some(dir) = sessions_dir() else { return 0 };

        let cutoff = Utc::now() - chrono::Duration::days(max_age_days);
        let latest = Self::load_latest().map(|s| s.id);
        let mut removed = 0u32;

        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("json") {
                    continue;
                }
                let filename = path.file_stem().and_then(|n| n.to_str()).unwrap_or("");
                if filename == "latest" || filename.starts_with('.') {
                    continue;
                }
                if latest.as_deref() == Some(filename) {
                    continue;
                }
                if let Ok(json) = std::fs::read_to_string(&path)
                    && let Ok(session) = serde_json::from_str::<SessionState>(&json)
                    && session.updated_at < cutoff
                    && std::fs::remove_file(&path).is_ok()
                {
                    removed += 1;
                }
            }
        }

        removed
    }
}
