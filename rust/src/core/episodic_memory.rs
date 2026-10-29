//! Episodic Memory — persistent cross-session experiences with outcomes.
//!
//! Automatically records what the agent did in each session, with what result.
//! Enables learning from past experiences: "What happened last time I refactored auth?"

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
};

use crate::core::context_admission::stores::StoreAdmission;
use crate::core::memory_policy::EpisodicPolicy;
use crate::core::policy::runtime::PublicationAuthority;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EpisodicStore {
    pub project_hash: String,
    pub episodes: Vec<Episode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Episode {
    pub id: String,
    pub session_id: String,
    pub timestamp: DateTime<Utc>,
    pub task_description: String,
    pub actions: Vec<Action>,
    pub outcome: Outcome,
    pub affected_files: Vec<String>,
    pub summary: String,
    pub duration_secs: u64,
    pub tokens_used: u64,
    #[serde(default)]
    pub agent_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Action {
    pub tool: String,
    pub description: String,
    pub timestamp: DateTime<Utc>,
    pub duration_ms: u64,
    pub success: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Outcome {
    Success { tests_passed: bool },
    Failure { error: String },
    Partial { details: String },
    Unknown,
}

impl Outcome {
    pub fn label(&self) -> &'static str {
        match self {
            Outcome::Success { .. } => "success",
            Outcome::Failure { .. } => "failure",
            Outcome::Partial { .. } => "partial",
            Outcome::Unknown => "unknown",
        }
    }
}

fn episodic_lock(project_hash: &str) -> Arc<Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<String, Arc<Mutex<()>>>>> = OnceLock::new();
    let mut locks = LOCKS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    locks.entry(project_hash.to_string()).or_default().clone()
}

/// How long an episodic write waits for another holder of the store's file
/// lock (a parallel CLI, daemon or MCP write). A single attempt dropped the
/// episode whenever two writers met (#18, a flaky gate on 24.10.).
const EPISODIC_LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

fn acquire_file_lock(path: &Path) -> Option<std::fs::File> {
    let parent = path.parent()?;
    let name = path.file_name()?.to_string_lossy();
    let lock_path = parent.join(format!(".{name}.lock"));
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&lock_path, std::fs::Permissions::from_mode(0o600));
    }
    crate::core::file_lock::acquire_exclusive_timeout(&file, EPISODIC_LOCK_WAIT).ok()?;
    Some(file)
}

/// Require every nonempty affected-path list to stay inside its owning project.
/// This is retrieval scoping only: no file contents are opened and no stored
/// episode is rewritten by the view.
fn episode_paths_belong_to_project(
    project_root: &Path,
    canonical_root: &Path,
    affected_files: &[String],
) -> bool {
    if affected_files.is_empty() {
        return true;
    }

    affected_files.iter().all(|raw| {
        if raw.is_empty() || raw.as_bytes().contains(&0) {
            return false;
        }
        let path = Path::new(raw);
        if has_ambiguous_foreign_path_syntax(raw, path)
            || path
                .components()
                .any(|component| matches!(component, Component::ParentDir))
        {
            return false;
        }

        if path.is_absolute()
            && !path.starts_with(project_root)
            && !path.starts_with(canonical_root)
        {
            // Avoid probing an arbitrary absolute path from an untrusted store.
            return false;
        }
        let candidate = if path.is_absolute() {
            path.to_path_buf()
        } else {
            canonical_root.join(path)
        };
        canonicalized_existing_ancestor(&candidate)
            .is_some_and(|resolved| resolved.starts_with(canonical_root))
    })
}

/// On Unix, Windows absolute/drive paths otherwise look relative and could be
/// reinterpreted beneath the active project. On Windows, reject rooted paths
/// without a drive/share prefix and drive-relative paths.
#[cfg(unix)]
fn has_ambiguous_foreign_path_syntax(raw: &str, _path: &Path) -> bool {
    let bytes = raw.as_bytes();
    bytes.contains(&0x5c)
        || (bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':')
}

#[cfg(windows)]
fn has_ambiguous_foreign_path_syntax(_raw: &str, path: &Path) -> bool {
    !path.is_absolute()
        && (path.has_root() || matches!(path.components().next(), Some(Component::Prefix(_))))
}

#[cfg(not(any(unix, windows)))]
fn has_ambiguous_foreign_path_syntax(_raw: &str, _path: &Path) -> bool {
    false
}

/// Resolve the longest existing ancestor before appending a missing historical
/// suffix. symlink_metadata detects dangling symlinks; canonicalization then
/// rejects existing and missing descendants that escape through a link.
fn canonicalized_existing_ancestor(path: &Path) -> Option<PathBuf> {
    let mut current = path.to_path_buf();
    let mut missing: Vec<std::ffi::OsString> = Vec::new();
    loop {
        match std::fs::symlink_metadata(&current) {
            Ok(_) => {
                let mut resolved = std::fs::canonicalize(&current).ok()?;
                for name in missing.iter().rev() {
                    resolved.push(name);
                }
                return Some(resolved);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                missing.push(current.file_name()?.to_os_string());
                if !current.pop() {
                    return None;
                }
            }
            Err(_) => return None,
        }
    }
}

impl EpisodicStore {
    pub fn new(project_hash: &str) -> Self {
        Self {
            project_hash: project_hash.to_string(),
            episodes: Vec::new(),
        }
    }

    pub fn record_episode(&mut self, mut episode: Episode, policy: &EpisodicPolicy) {
        episode.actions.truncate(policy.max_actions_per_episode);

        if episode.summary.is_empty() {
            episode.summary = auto_summarize(&episode, policy.summary_max_chars);
        }

        self.episodes.push(episode);

        if self.episodes.len() > policy.max_episodes {
            self.episodes
                .drain(0..self.episodes.len() - policy.max_episodes);
        }
    }

    pub fn search(&self, query: &str) -> Vec<&Episode> {
        let q = query.to_lowercase();
        let terms: Vec<&str> = q.split_whitespace().collect();

        let mut scored: Vec<(&Episode, f32)> = self
            .episodes
            .iter()
            .filter_map(|ep| {
                let searchable = format!(
                    "{} {} {}",
                    ep.task_description.to_lowercase(),
                    ep.summary.to_lowercase(),
                    ep.affected_files.join(" ").to_lowercase()
                );
                let hits = terms.iter().filter(|t| searchable.contains(**t)).count();
                if hits > 0 {
                    let relevance = hits as f32 / terms.len() as f32;
                    Some((ep, relevance))
                } else {
                    None
                }
            })
            .collect();

        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.into_iter().map(|(ep, _)| ep).collect()
    }

    pub fn recent(&self, n: usize) -> Vec<&Episode> {
        self.episodes.iter().rev().take(n).collect()
    }

    pub fn by_outcome(&self, outcome_label: &str) -> Vec<&Episode> {
        self.episodes
            .iter()
            .filter(|ep| ep.outcome.label() == outcome_label)
            .collect()
    }

    pub fn by_file(&self, file_path: &str) -> Vec<&Episode> {
        self.episodes
            .iter()
            .filter(|ep| ep.affected_files.iter().any(|f| f.contains(file_path)))
            .collect()
    }

    pub fn stats(&self) -> EpisodicStats {
        let total = self.episodes.len();
        let successes = self
            .episodes
            .iter()
            .filter(|ep| matches!(ep.outcome, Outcome::Success { .. }))
            .count();
        let failures = self
            .episodes
            .iter()
            .filter(|ep| matches!(ep.outcome, Outcome::Failure { .. }))
            .count();
        let total_tokens: u64 = self.episodes.iter().map(|ep| ep.tokens_used).sum();

        EpisodicStats {
            total_episodes: total,
            successes,
            failures,
            success_rate: if total > 0 {
                successes as f32 / total as f32
            } else {
                0.0
            },
            total_tokens,
        }
    }

    fn store_path(project_hash: &str) -> Option<PathBuf> {
        let dir = crate::core::data_dir::lean_ctx_data_dir()
            .ok()?
            .join("memory")
            .join("episodes");
        Some(dir.join(format!("{project_hash}.json")))
    }

    pub fn load(project_hash: &str) -> Option<Self> {
        let mut store = Self::load_raw(project_hash).ok()??;
        store.migrate_legacy_episodes();
        Some(store)
    }

    fn load_raw(project_hash: &str) -> Result<Option<Self>, String> {
        let Some(path) = Self::store_path(project_hash) else {
            return Err("Cannot determine data directory".into());
        };
        let data = match std::fs::read_to_string(path) {
            Ok(data) => data,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.to_string()),
        };
        let store: Self = serde_json::from_str(&data).map_err(|error| error.to_string())?;
        if store.project_hash != project_hash {
            return Err("episodic store project scope mismatch".into());
        }
        Ok(Some(store))
    }

    /// Load content after admitting every textual field. Migration and project
    /// scoping are in-memory only; reads never rewrite persisted history.
    pub(crate) fn load_admitted(
        project_hash: &str,
        admission: &StoreAdmission,
    ) -> Result<Self, String> {
        Self::load_admitted_inner(project_hash, None, admission)
    }

    /// Retrieval-only view that withholds any episode whose affected paths do
    /// not all resolve inside this project. Empty path lists keep the existing
    /// per-project-store behavior because there is no path evidence to inspect.
    pub(crate) fn load_admitted_for_project(
        project_hash: &str,
        project_root: &Path,
        admission: &StoreAdmission,
    ) -> Result<Self, String> {
        let canonical_root = std::fs::canonicalize(project_root)
            .map_err(|error| format!("cannot verify episodic project root: {error}"))?;
        Self::load_admitted_inner(
            project_hash,
            Some((project_root, canonical_root.as_path())),
            admission,
        )
    }

    fn load_admitted_inner(
        project_hash: &str,
        project_scope: Option<(&Path, &Path)>,
        admission: &StoreAdmission,
    ) -> Result<Self, String> {
        let mut store = Self::load_raw(project_hash)?.unwrap_or_else(|| Self::new(project_hash));
        if let Some((root, canonical_root)) = project_scope {
            store.retain_episodes_for_project(root, canonical_root);
        }
        let mut admit =
            |text: &str| crate::core::session::admission::admit_store_text(admission, text);
        store
            .episodes
            .retain_mut(|episode| sanitize_episode(episode, &mut admit));
        store.migrate_legacy_episodes();
        if !admission.is_current() {
            return Err("session admission policy changed; retry".into());
        }
        Ok(store)
    }

    fn retain_episodes_for_project(&mut self, project_root: &Path, canonical_root: &Path) {
        self.episodes.retain(|episode| {
            episode_paths_belong_to_project(project_root, canonical_root, &episode.affected_files)
        });
    }

    /// Mutate an admitted store under its project file lock. Old records are
    /// scrubbed before migration or derivation, and the policy epoch is checked
    /// under the lock immediately before persistence.
    pub(crate) fn mutate_locked_admitted<T>(
        project_hash: &str,
        admission: &StoreAdmission,
        mut admit: impl FnMut(&str) -> Option<String>,
        mutate: impl FnOnce(&mut Self) -> Result<T, String>,
    ) -> Result<(Self, T), String> {
        let authority = PublicationAuthority::capture()
            .map_err(|_| "source authority unavailable".to_string())?;
        let lock = episodic_lock(project_hash);
        let _guard = lock
            .try_lock()
            .map_err(|_| "episodic store is busy; retry".to_string())?;
        let path = Self::store_path(project_hash)
            .ok_or_else(|| "Cannot determine data directory".to_string())?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|error| error.to_string())?;
        }
        let _file_lock = acquire_file_lock(&path)
            .ok_or_else(|| "episodic store lock unavailable".to_string())?;
        let mut store = Self::load_raw(project_hash)?.unwrap_or_else(|| Self::new(project_hash));
        store
            .episodes
            .retain_mut(|episode| sanitize_episode(episode, &mut admit));
        store.migrate_legacy_episodes();
        let result = mutate(&mut store)?;
        store
            .episodes
            .retain_mut(|episode| sanitize_episode(episode, &mut admit));
        let json = serde_json::to_string_pretty(&store).map_err(|error| error.to_string())?;
        admission.authorize_publication(&authority)?;
        crate::core::atomic_fs::write_bytes_with_fallback(&path, json.as_bytes(), None)?;
        Ok((store, result))
    }

    /// One-time repair for episodes recorded before per-task metrics existed:
    /// those episodes share one id per session (derived from the session
    /// *start* date) and carry cumulative session token counters instead of
    /// per-task deltas. Detected via duplicate ids within one session;
    /// idempotent because rewritten ids are unique afterwards.
    fn migrate_legacy_episodes(&mut self) -> bool {
        use std::collections::HashSet;

        let mut seen: HashSet<(String, String)> = HashSet::new();
        let mut needs_migration = false;
        for ep in &self.episodes {
            if !seen.insert((ep.session_id.clone(), ep.id.clone())) {
                needs_migration = true;
                break;
            }
        }
        if !needs_migration {
            return false;
        }

        let mut sessions: HashSet<String> = HashSet::new();
        for ep in &self.episodes {
            sessions.insert(ep.session_id.clone());
        }

        for session_id in sessions {
            let mut idx: Vec<usize> = (0..self.episodes.len())
                .filter(|&i| self.episodes[i].session_id == session_id)
                .collect();
            idx.sort_by_key(|&i| self.episodes[i].timestamp);

            // Cumulative counters are monotonically non-decreasing; only
            // then is converting to deltas safe.
            let monotonic = idx
                .windows(2)
                .all(|w| self.episodes[w[0]].tokens_used <= self.episodes[w[1]].tokens_used);

            let mut prev_tokens: u64 = 0;
            let mut prev_ts: Option<DateTime<Utc>> = None;
            let mut used_ids: HashSet<String> = HashSet::new();
            for &i in &idx {
                let ts = self.episodes[i].timestamp;
                let ep = &mut self.episodes[i];
                let mut id = format!("ep-{}", ts.format("%Y%m%d-%H%M%S"));
                let mut n = 1;
                while !used_ids.insert(id.clone()) {
                    n += 1;
                    id = format!("ep-{}-{n}", ts.format("%Y%m%d-%H%M%S"));
                }
                ep.id = id;
                if monotonic {
                    let cumulative = ep.tokens_used;
                    ep.tokens_used = cumulative.saturating_sub(prev_tokens);
                    prev_tokens = cumulative;
                }
                if ep.duration_secs == 0
                    && let Some(p) = prev_ts
                {
                    ep.duration_secs = (ts - p).num_seconds().max(0) as u64;
                }
                prev_ts = Some(ts);
            }
        }
        true
    }

    pub fn load_or_create(project_hash: &str) -> Self {
        Self::load(project_hash).unwrap_or_else(|| Self::new(project_hash))
    }

    pub fn mutate_locked<T>(
        project_hash: &str,
        mutate: impl FnOnce(&mut Self) -> T,
    ) -> Result<(Self, T), String> {
        let lock = episodic_lock(project_hash);
        let _guard = lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let path = Self::store_path(project_hash)
            .ok_or_else(|| "Cannot determine data directory".to_string())?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{e}"))?;
        }
        let _file_lock = acquire_file_lock(&path)
            .ok_or_else(|| "episodic store lock unavailable".to_string())?;

        let mut store = Self::load_or_create(project_hash);
        let result = mutate(&mut store);
        store.save()?;
        Ok((store, result))
    }

    pub fn save(&self) -> Result<(), String> {
        let path = Self::store_path(&self.project_hash)
            .ok_or_else(|| "Cannot determine data directory".to_string())?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{e}"))?;
        }
        let json = serde_json::to_string_pretty(self).map_err(|e| format!("{e}"))?;
        crate::core::atomic_fs::write_bytes_with_fallback(&path, json.as_bytes(), None)
    }
}

#[derive(Debug)]
pub struct EpisodicStats {
    pub total_episodes: usize,
    pub successes: usize,
    pub failures: usize,
    pub success_rate: f32,
    pub total_tokens: u64,
}

pub fn create_episode_from_session(
    session: &super::session::SessionState,
    tool_calls: &[(String, u64)],
) -> Episode {
    let actions: Vec<Action> = tool_calls
        .iter()
        .map(|(tool, duration_ms)| Action {
            tool: tool.clone(),
            description: String::new(),
            timestamp: Utc::now(),
            duration_ms: *duration_ms,
            success: true,
        })
        .collect();

    let affected_files: Vec<String> = session
        .files_touched
        .iter()
        .map(|f| f.path.clone())
        .collect();

    let task_description = session
        .task
        .as_ref()
        .map(|t| t.description.clone())
        .unwrap_or_default();

    let outcome = if session.findings.iter().any(|f| {
        f.summary.to_lowercase().contains("error") || f.summary.to_lowercase().contains("failed")
    }) {
        Outcome::Failure {
            error: session
                .findings
                .iter()
                .find(|f| {
                    f.summary.to_lowercase().contains("error")
                        || f.summary.to_lowercase().contains("failed")
                })
                .map(|f| f.summary.clone())
                .unwrap_or_default(),
        }
    } else if !session.findings.is_empty() || !session.decisions.is_empty() {
        Outcome::Success { tests_passed: true }
    } else {
        Outcome::Unknown
    };

    Episode {
        // Record-time based id: session ids start with the session *start*
        // date, so deriving the episode id from them produced colliding ids
        // for every task completed in one long-running session.
        id: format!("ep-{}", Utc::now().format("%Y%m%d-%H%M%S")),
        session_id: session.id.clone(),
        timestamp: Utc::now(),
        task_description,
        actions,
        outcome,
        affected_files,
        summary: String::new(),
        duration_secs: 0,
        // Cumulative session counter at record time; the caller converts
        // this into a per-task delta (see `finalize_episode_metrics`).
        tokens_used: session.stats.total_tokens_saved,
        agent_id: None,
    }
}

pub fn record_session_episode(
    project_hash: &str,
    session: &super::session::SessionState,
    tool_calls: &[(String, u64)],
    agent_id: Option<&str>,
    policy: &EpisodicPolicy,
    deduplicate: bool,
) -> Result<Option<String>, String> {
    let root = session
        .strict_project_root()
        .ok_or_else(|| "session project scope unavailable".to_string())?;
    if crate::core::project_hash::hash_project_root(&root) != project_hash {
        return Err("episodic store project scope mismatch".into());
    }

    crate::core::session::admission::with_store_admission(session, |admission| {
        let admitted_session = crate::core::session::admission::for_storage(session)?;
        let normalized_agent_id = agent_id
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(str::to_string);
        let mut episode = create_episode_from_session(&admitted_session, tool_calls);
        episode.agent_id.clone_from(&normalized_agent_id);
        let mut admit =
            |text: &str| crate::core::session::admission::admit_store_text(admission, text);
        if !sanitize_episode(&mut episode, &mut admit) {
            return Err("episode content withheld by current policy".into());
        }

        let (_, episode_id) = EpisodicStore::mutate_locked_admitted(
            project_hash,
            admission,
            |text| crate::core::session::admission::admit_store_text(admission, text),
            |store| {
                if deduplicate
                    && store.episodes.iter().any(|existing| {
                        existing.session_id == episode.session_id
                            && existing.agent_id == episode.agent_id
                            && existing.task_description == episode.task_description
                    })
                {
                    return Ok(None);
                }
                let mut episode = episode;
                finalize_episode_metrics(&mut episode, store, admitted_session.started_at);
                let id = episode.id.clone();
                store.record_episode(episode, policy);
                Ok(Some(id))
            },
        )?;
        Ok(episode_id)
    })
}

fn sanitize_episode(episode: &mut Episode, admit: &mut impl FnMut(&str) -> Option<String>) -> bool {
    fn field(value: &mut String, admit: &mut impl FnMut(&str) -> Option<String>) -> bool {
        if value.is_empty() {
            return true;
        }
        let Some(projected) = admit(value) else {
            return false;
        };
        *value = projected;
        true
    }

    if !field(&mut episode.task_description, admit)
        || !field(&mut episode.summary, admit)
        || !episode
            .affected_files
            .iter_mut()
            .all(|path| field(path, admit))
        || !episode
            .actions
            .iter_mut()
            .all(|action| field(&mut action.tool, admit) && field(&mut action.description, admit))
    {
        return false;
    }
    let outcome_is_admitted = match &mut episode.outcome {
        Outcome::Failure { error } => field(error, admit),
        Outcome::Partial { details } => field(details, admit),
        Outcome::Success { .. } | Outcome::Unknown => true,
    };
    outcome_is_admitted
        && episode
            .agent_id
            .as_mut()
            .is_none_or(|agent| field(agent, admit))
}

/// Converts the cumulative session counters captured by
/// [`create_episode_from_session`] into per-task values.
///
/// `tokens_used` becomes the delta since the previous episode of the same
/// session (so the per-session sum of episode tokens matches the session
/// total), and `duration_secs` becomes the wall-clock span this task was the
/// active one (since the previous episode, or since session start for the
/// first).
pub fn finalize_episode_metrics(
    episode: &mut Episode,
    store: &EpisodicStore,
    session_started_at: DateTime<Utc>,
) {
    let prior_tokens: u64 = store
        .episodes
        .iter()
        .filter(|e| e.session_id == episode.session_id)
        .map(|e| e.tokens_used)
        .sum();
    episode.tokens_used = episode.tokens_used.saturating_sub(prior_tokens);

    let since = store
        .episodes
        .iter()
        .filter(|e| e.session_id == episode.session_id)
        .map(|e| e.timestamp)
        .max()
        .unwrap_or(session_started_at);
    episode.duration_secs = (episode.timestamp - since).num_seconds().max(0) as u64;
}

fn auto_summarize(episode: &Episode, max_chars: usize) -> String {
    let tool_counts = count_tools(&episode.actions);
    let top_tools: Vec<String> = tool_counts
        .into_iter()
        .take(3)
        .map(|(tool, count)| format!("{tool}x{count}"))
        .collect();

    let files_hint = if episode.affected_files.len() <= 3 {
        episode.affected_files.join(", ")
    } else {
        format!(
            "{}, ... +{} more",
            episode.affected_files[..3].join(", "),
            episode.affected_files.len() - 3
        )
    };

    let task = if episode.task_description.chars().count() > max_chars {
        episode.task_description.chars().take(max_chars).collect()
    } else {
        episode.task_description.clone()
    };
    let mut summary = format!(
        "{task} [{}] tools:[{}]",
        episode.outcome.label(),
        top_tools.join(",")
    );

    if !files_hint.is_empty() {
        summary.push_str(&format!(" files:[{files_hint}]"));
    }

    summary
}

fn count_tools(actions: &[Action]) -> Vec<(String, usize)> {
    let mut counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for action in actions {
        *counts.entry(&action.tool).or_insert(0) += 1;
    }
    let mut sorted: Vec<(String, usize)> = counts
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
    sorted.sort_by_key(|item| std::cmp::Reverse(item.1));
    sorted
}

pub fn format_episode_compact(episode: &Episode) -> String {
    format!(
        "[{}] {} — {} ({} actions, {} files)",
        episode.outcome.label(),
        episode.task_description,
        episode.summary,
        episode.actions.len(),
        episode.affected_files.len()
    )
}

#[cfg(test)]
pub mod tests {
    use super::*;

    fn make_episode(task: &str, outcome: Outcome) -> Episode {
        Episode {
            id: "ep-test".to_string(),
            session_id: "sess-1".to_string(),
            timestamp: Utc::now(),
            task_description: task.to_string(),
            actions: vec![
                Action {
                    tool: "ctx_read".to_string(),
                    description: String::new(),
                    timestamp: Utc::now(),
                    duration_ms: 50,
                    success: true,
                },
                Action {
                    tool: "ctx_shell".to_string(),
                    description: String::new(),
                    timestamp: Utc::now(),
                    duration_ms: 200,
                    success: true,
                },
            ],
            outcome,
            affected_files: vec!["src/main.rs".to_string(), "src/lib.rs".to_string()],
            summary: String::new(),
            duration_secs: 60,
            tokens_used: 5000,
            agent_id: None,
        }
    }

    #[test]
    fn admission_scrubs_all_episode_text_and_drops_on_any_denial() {
        let mut episode = make_episode(
            "TASK PRIVATE",
            Outcome::Failure {
                error: "ERROR PRIVATE".into(),
            },
        );
        episode.summary = "SUMMARY PRIVATE".into();
        episode.affected_files = vec!["PATH PRIVATE".into()];
        episode.actions[0].description = "ACTION PRIVATE".into();
        episode.agent_id = Some("AGENT PRIVATE".into());
        let mut admit = |text: &str| Some(text.replace("PRIVATE", "redacted"));

        assert!(sanitize_episode(&mut episode, &mut admit));
        assert_eq!(episode.task_description, "TASK redacted");
        assert_eq!(episode.summary, "SUMMARY redacted");
        assert_eq!(episode.affected_files[0], "PATH redacted");
        assert_eq!(episode.actions[0].description, "ACTION redacted");
        assert_eq!(episode.agent_id.as_deref(), Some("AGENT redacted"));
        assert!(
            matches!(&episode.outcome, Outcome::Failure { error } if error == "ERROR redacted")
        );

        let mut denied = make_episode("safe task", Outcome::Unknown);
        denied.agent_id = Some("BLOCK".into());
        assert!(!sanitize_episode(&mut denied, &mut |text| {
            (text != "BLOCK").then(|| text.to_string())
        }));
    }

    #[test]
    fn mutate_locked_preserves_successive_agent_episodes() {
        let policy = EpisodicPolicy::default();
        let mut store = EpisodicStore::new("locked-writes-unit");

        let mut ep_a = make_episode("Task from agent A", Outcome::Unknown);
        ep_a.agent_id = Some("agent-a".to_string());
        store.record_episode(ep_a, &policy);

        let mut ep_b = make_episode("Task from agent B", Outcome::Unknown);
        ep_b.agent_id = Some("agent-b".to_string());
        store.record_episode(ep_b, &policy);

        assert_eq!(store.episodes.len(), 2);
        assert!(
            store
                .episodes
                .iter()
                .any(|episode| episode.agent_id.as_deref() == Some("agent-a"))
        );
        assert!(
            store
                .episodes
                .iter()
                .any(|episode| episode.agent_id.as_deref() == Some("agent-b"))
        );
    }

    /// #18: a write that meets another holder of the store's file lock waits
    /// for it instead of failing with "episodic store lock unavailable".
    #[test]
    fn a_write_waits_for_another_holder_of_the_file_lock() {
        use fs2::FileExt;
        let _isolated = crate::core::data_dir::isolated_data_dir();
        let path = EpisodicStore::store_path("waits").unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let other = acquire_file_lock(&path).expect("first holder");
        let holder = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(300));
            FileExt::unlock(&other).unwrap();
        });
        let written = EpisodicStore::mutate_locked("waits", |_| ());
        holder.join().unwrap();
        assert!(written.is_ok(), "{written:?}");
    }

    #[test]
    fn record_session_episode_deduplicates_per_agent() {
        let _isolated = crate::core::data_dir::isolated_data_dir();
        let policy = EpisodicPolicy::default();
        let project = tempfile::tempdir().expect("project root");
        let project_root = project.path().to_string_lossy().into_owned();
        let project_hash = crate::core::project_hash::hash_project_root(&project_root);
        let mut session = super::super::session::SessionState::new();
        session.project_root = Some(project_root.clone());
        session.shell_cwd = Some(project_root);
        session.set_task("same task", None);
        let tool_calls = vec![("ctx_read".to_string(), 10)];

        assert!(
            record_session_episode(
                &project_hash,
                &session,
                &tool_calls,
                Some("agent-a"),
                &policy,
                true,
            )
            .unwrap()
            .is_some()
        );
        assert!(
            record_session_episode(
                &project_hash,
                &session,
                &tool_calls,
                Some("agent-a"),
                &policy,
                true,
            )
            .unwrap()
            .is_none()
        );
        assert!(
            record_session_episode(
                &project_hash,
                &session,
                &tool_calls,
                Some("agent-b"),
                &policy,
                true,
            )
            .unwrap()
            .is_some()
        );

        let store = EpisodicStore::load_or_create(&project_hash);
        assert_eq!(store.episodes.len(), 2);
    }

    #[test]
    fn record_and_search() {
        let policy = EpisodicPolicy::default();
        let mut store = EpisodicStore::new("test");
        store.record_episode(
            make_episode(
                "Refactor auth module",
                Outcome::Success { tests_passed: true },
            ),
            &policy,
        );
        store.record_episode(
            make_episode(
                "Fix database connection",
                Outcome::Failure {
                    error: "timeout".to_string(),
                },
            ),
            &policy,
        );

        let results = store.search("auth refactor");
        assert_eq!(results.len(), 1);
        assert!(results[0].task_description.contains("auth"));
    }

    #[test]
    fn filter_by_outcome() {
        let policy = EpisodicPolicy::default();
        let mut store = EpisodicStore::new("test");
        store.record_episode(
            make_episode("Task 1", Outcome::Success { tests_passed: true }),
            &policy,
        );
        store.record_episode(
            make_episode(
                "Task 2",
                Outcome::Failure {
                    error: "err".to_string(),
                },
            ),
            &policy,
        );
        store.record_episode(
            make_episode(
                "Task 3",
                Outcome::Success {
                    tests_passed: false,
                },
            ),
            &policy,
        );

        assert_eq!(store.by_outcome("success").len(), 2);
        assert_eq!(store.by_outcome("failure").len(), 1);
    }

    #[test]
    fn filter_by_file() {
        let policy = EpisodicPolicy::default();
        let mut store = EpisodicStore::new("test");
        store.record_episode(make_episode("Task", Outcome::Unknown), &policy);

        let results = store.by_file("main.rs");
        assert_eq!(results.len(), 1);

        let results = store.by_file("nonexistent.rs");
        assert!(results.is_empty());
    }

    #[test]
    fn recent_episodes() {
        let policy = EpisodicPolicy::default();
        let mut store = EpisodicStore::new("test");
        for i in 0..5 {
            store.record_episode(
                make_episode(&format!("Task {i}"), Outcome::Unknown),
                &policy,
            );
        }

        let recent = store.recent(3);
        assert_eq!(recent.len(), 3);
        assert!(recent[0].task_description.contains('4'));
    }

    #[test]
    fn stats_calculation() {
        let policy = EpisodicPolicy::default();
        let mut store = EpisodicStore::new("test");
        store.record_episode(
            make_episode("T1", Outcome::Success { tests_passed: true }),
            &policy,
        );
        store.record_episode(
            make_episode(
                "T2",
                Outcome::Failure {
                    error: "e".to_string(),
                },
            ),
            &policy,
        );
        store.record_episode(
            make_episode(
                "T3",
                Outcome::Success {
                    tests_passed: false,
                },
            ),
            &policy,
        );

        let stats = store.stats();
        assert_eq!(stats.total_episodes, 3);
        assert_eq!(stats.successes, 2);
        assert_eq!(stats.failures, 1);
        assert!((stats.success_rate - 0.6667).abs() < 0.01);
    }

    #[test]
    fn auto_summary_generation() {
        let mut ep = make_episode("Fix the login bug", Outcome::Success { tests_passed: true });
        ep.summary = String::new();
        let summary = auto_summarize(&ep, EpisodicPolicy::default().summary_max_chars);
        assert!(summary.contains("Fix the login bug"));
        assert!(summary.contains("[success]"));
        assert!(summary.contains("ctx_read"));
    }

    #[test]
    fn max_episodes_enforced() {
        let policy = EpisodicPolicy::default();
        let mut store = EpisodicStore::new("test");
        for i in 0..510 {
            store.record_episode(
                make_episode(&format!("Task {i}"), Outcome::Unknown),
                &policy,
            );
        }
        assert!(store.episodes.len() <= policy.max_episodes);
    }

    #[test]
    fn format_compact() {
        let ep = make_episode("Deploy v2", Outcome::Success { tests_passed: true });
        let output = format_episode_compact(&ep);
        assert!(output.contains("[success]"));
        assert!(output.contains("Deploy v2"));
    }

    #[test]
    fn finalize_metrics_converts_cumulative_to_delta() {
        let mut store = EpisodicStore::new("test");
        let started = Utc::now() - chrono::Duration::seconds(600);

        let mut first = make_episode("T1", Outcome::Unknown);
        first.session_id = "sess-x".to_string();
        first.tokens_used = 1000; // cumulative at record time
        first.timestamp = started + chrono::Duration::seconds(100);
        finalize_episode_metrics(&mut first, &store, started);
        assert_eq!(first.tokens_used, 1000);
        assert_eq!(first.duration_secs, 100);
        store.episodes.push(first);

        let mut second = make_episode("T2", Outcome::Unknown);
        second.session_id = "sess-x".to_string();
        second.tokens_used = 1800; // cumulative at record time
        second.timestamp = started + chrono::Duration::seconds(400);
        finalize_episode_metrics(&mut second, &store, started);
        assert_eq!(second.tokens_used, 800); // delta since first
        assert_eq!(second.duration_secs, 300);
    }

    #[test]
    fn migrate_legacy_dedupes_ids_and_converts_tokens() {
        let mut store = EpisodicStore::new("test");
        let base = Utc::now() - chrono::Duration::seconds(1000);
        for (i, cumulative) in [1000u64, 3000, 6000].iter().enumerate() {
            let mut ep = make_episode(&format!("T{i}"), Outcome::Unknown);
            ep.id = "ep-20260516".to_string(); // legacy colliding id
            ep.session_id = "sess-legacy".to_string();
            ep.tokens_used = *cumulative;
            ep.duration_secs = 0;
            ep.timestamp = base + chrono::Duration::seconds(i as i64 * 120);
            store.episodes.push(ep);
        }

        assert!(store.migrate_legacy_episodes());
        let ids: std::collections::HashSet<String> =
            store.episodes.iter().map(|e| e.id.clone()).collect();
        assert_eq!(ids.len(), 3, "ids must be unique after migration");
        let tokens: Vec<u64> = store.episodes.iter().map(|e| e.tokens_used).collect();
        assert_eq!(tokens, vec![1000, 2000, 3000]);
        // Idempotent: unique ids → no second migration.
        assert!(!store.migrate_legacy_episodes());
    }

    #[test]
    fn project_scope_withholds_whole_mixed_episode_and_keeps_empty_history() {
        let root = tempfile::tempdir().unwrap();
        let foreign = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("src")).unwrap();
        std::fs::write(root.path().join("src/local.rs"), "local").unwrap();
        std::fs::write(foreign.path().join("secret.rs"), "foreign").unwrap();
        let canonical_root = std::fs::canonicalize(root.path()).unwrap();
        let local = root
            .path()
            .join("src/local.rs")
            .to_string_lossy()
            .into_owned();
        let historical_missing = root
            .path()
            .join("src/removed.rs")
            .to_string_lossy()
            .into_owned();
        let outside = foreign
            .path()
            .join("secret.rs")
            .to_string_lossy()
            .into_owned();

        assert!(episode_paths_belong_to_project(
            root.path(),
            &canonical_root,
            std::slice::from_ref(&local),
        ));
        assert!(episode_paths_belong_to_project(
            root.path(),
            &canonical_root,
            &[historical_missing],
        ));
        assert!(episode_paths_belong_to_project(
            root.path(),
            &canonical_root,
            &[],
        ));
        assert!(!episode_paths_belong_to_project(
            root.path(),
            &canonical_root,
            std::slice::from_ref(&outside),
        ));
        assert!(!episode_paths_belong_to_project(
            root.path(),
            &canonical_root,
            &[local.clone(), outside.clone()],
        ));
        assert!(!episode_paths_belong_to_project(
            root.path(),
            &canonical_root,
            &["../foreign/secret.rs".to_string()],
        ));
        assert!(!episode_paths_belong_to_project(
            root.path(),
            &canonical_root,
            &["C:/foreign/secret.rs".to_string()],
        ));

        let root_name = root.path().file_name().unwrap().to_string_lossy();
        let prefix_neighbor = root
            .path()
            .with_file_name(format!("{root_name}-neighbor/file.rs"));
        assert!(!episode_paths_belong_to_project(
            root.path(),
            &canonical_root,
            &[prefix_neighbor.to_string_lossy().into_owned()],
        ));

        let mut safe = make_episode("safe", Outcome::Unknown);
        safe.id = "safe".into();
        safe.affected_files = vec![local.clone()];
        let mut mixed = make_episode("mixed", Outcome::Unknown);
        mixed.id = "mixed".into();
        mixed.affected_files = vec![local, outside];
        let mut no_paths = make_episode("legacy without paths", Outcome::Unknown);
        no_paths.id = "legacy-empty".into();
        no_paths.affected_files.clear();
        let mut view = EpisodicStore::new("scope-test");
        view.episodes = vec![safe, mixed, no_paths];
        view.retain_episodes_for_project(root.path(), &canonical_root);
        let visible_ids: Vec<_> = view
            .episodes
            .iter()
            .map(|episode| episode.id.as_str())
            .collect();
        assert_eq!(visible_ids, vec!["safe", "legacy-empty"]);
    }

    #[cfg(unix)]
    #[test]
    fn project_scope_rejects_existing_and_missing_symlink_escapes() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let foreign = tempfile::tempdir().unwrap();
        std::fs::write(foreign.path().join("existing.rs"), "foreign").unwrap();
        symlink(foreign.path(), root.path().join("escape")).unwrap();
        let canonical_root = std::fs::canonicalize(root.path()).unwrap();
        let existing = root
            .path()
            .join("escape/existing.rs")
            .to_string_lossy()
            .into_owned();
        let missing = root
            .path()
            .join("escape/not-yet-created.rs")
            .to_string_lossy()
            .into_owned();

        assert!(!episode_paths_belong_to_project(
            root.path(),
            &canonical_root,
            &[existing],
        ));
        assert!(!episode_paths_belong_to_project(
            root.path(),
            &canonical_root,
            &[missing],
        ));
    }
}
