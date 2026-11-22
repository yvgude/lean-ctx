use lean_ctx_time::UtcDateTime;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

const MAX_DIARY_ENTRIES: usize = 100;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentDiary {
    pub agent_id: String,
    pub agent_type: String,
    pub project_root: String,
    pub entries: Vec<DiaryEntry>,
    pub created_at: UtcDateTime,
    pub updated_at: UtcDateTime,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiaryEntry {
    pub entry_type: DiaryEntryType,
    pub content: String,
    pub context: Option<String>,
    pub timestamp: UtcDateTime,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum DiaryEntryType {
    Discovery,
    Decision,
    Blocker,
    Progress,
    Insight,
}

impl AgentDiary {
    pub(crate) fn new(agent_id: &str, agent_type: &str, project_root: &str) -> Self {
        let now = UtcDateTime::now();
        Self {
            agent_id: agent_id.to_string(),
            agent_type: agent_type.to_string(),
            project_root: project_root.to_string(),
            entries: Vec::new(),
            created_at: now,
            updated_at: now,
        }
    }

    pub fn add_entry(&mut self, entry_type: DiaryEntryType, content: &str, context: Option<&str>) {
        self.entries.push(DiaryEntry {
            entry_type,
            content: content.to_string(),
            context: context.map(std::string::ToString::to_string),
            timestamp: UtcDateTime::now(),
        });
        if self.entries.len() > MAX_DIARY_ENTRIES {
            self.entries
                .drain(0..self.entries.len() - MAX_DIARY_ENTRIES);
        }
        self.updated_at = UtcDateTime::now();
    }

    pub fn format_summary(&self) -> String {
        if self.entries.is_empty() {
            return format!("Diary [{}]: empty", self.agent_id);
        }
        let mut out = format!(
            "Diary [{}] ({} entries):\n",
            self.agent_id,
            self.entries.len()
        );
        let now = UtcDateTime::now();
        for e in self.entries.iter().rev().take(10) {
            let age = (now - e.timestamp).num_minutes();
            let prefix = match e.entry_type {
                DiaryEntryType::Discovery => "FOUND",
                DiaryEntryType::Decision => "DECIDED",
                DiaryEntryType::Blocker => "BLOCKED",
                DiaryEntryType::Progress => "DONE",
                DiaryEntryType::Insight => "INSIGHT",
            };
            let ctx = e
                .context
                .as_deref()
                .map(|c| format!(" [{c}]"))
                .unwrap_or_default();
            out.push_str(&format!("  [{prefix}] {}{ctx} ({age}m ago)\n", e.content));
        }
        out
    }

    /// Persist into this diary's project partition. An agent id used in two
    /// projects keeps one diary per project.
    pub fn save(&self) -> Result<(), String> {
        let dir = partition_dir(&self.project_root)?;
        // Same reasoning as #1619: name the path and the operation, or a
        // sandbox denial reads as an unattributable "Operation not permitted".
        std::fs::create_dir_all(&dir)
            .map_err(|error| format!("create agent diary directory {}: {error}", dir.display()))?;
        let path = dir.join(format!("{}.json", sanitize_filename(&self.agent_id)));
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        crate::config_io::write_atomic(&path, &json)
            .map_err(|error| format!("write agent diary {}: {error}", path.display()))
    }

    /// The diary `agent_id` keeps in `project_root`. The stored root, not
    /// the directory, decides: a diary of releases before 3.11.5 (shared
    /// directory) or one left in an older partition after the project's
    /// identity (remote, package name) changed is served to the project it
    /// records, and the next save moves it into the current partition.
    pub fn load_for_project(agent_id: &str, project_root: &str) -> Option<Self> {
        let file = format!("{}.json", sanitize_filename(agent_id));
        search_dirs(project_root)
            .iter()
            .filter_map(|dir| read_diary(&dir.join(&file)))
            .find(|diary| {
                diary.agent_id == agent_id && same_root(&diary.project_root, project_root)
            })
    }

    pub fn load_or_create(agent_id: &str, agent_type: &str, project_root: &str) -> Self {
        Self::load_for_project(agent_id, project_root)
            .unwrap_or_else(|| Self::new(agent_id, agent_type, project_root))
    }

    /// `(agent_id, entries, updated_at)` of every diary kept in `project_root`.
    pub fn list_for_project(project_root: &str) -> Vec<(String, usize, UtcDateTime)> {
        Self::load_all_for_project(project_root)
            .into_iter()
            .map(|diary| (diary.agent_id, diary.entries.len(), diary.updated_at))
            .collect()
    }

    /// Load every diary whose `project_root` matches `project_root`, most
    /// recently updated first. Used by skillify to mine a project's decisions
    /// and insights across all its agents (#290).
    /// Reads at most [`MAX_PROJECT_DIARY_FILES`] diary files
    /// ([`Self::load_all_for_project_bounded`]).
    pub fn load_all_for_project(project_root: &str) -> Vec<AgentDiary> {
        Self::load_all_for_project_bounded(project_root, MAX_PROJECT_DIARY_FILES)
    }

    /// [`Self::load_all_for_project`] reading at most `max_files` diary files
    /// (the current partition's first, then the others, by name within a
    /// directory). Symlinked partitions and files and files over
    /// [`MAX_DIARY_FILE_BYTES`] are skipped before anything is read.
    pub fn load_all_for_project_bounded(project_root: &str, max_files: usize) -> Vec<AgentDiary> {
        let mut diaries: Vec<AgentDiary> = Vec::new();
        // The current partition first: its diary supersedes an older one.
        for (_, path) in diary_files(&search_dirs(project_root))
            .into_iter()
            .take(max_files)
        {
            if let Some(diary) = read_diary(&path)
                && same_root(&diary.project_root, project_root)
                && !diaries.iter().any(|known| known.agent_id == diary.agent_id)
            {
                diaries.push(diary);
            }
        }
        diaries.sort_by_key(|d| std::cmp::Reverse(d.updated_at));
        diaries
    }

    /// Every diary on this machine (all partitions and the shared directory
    /// of releases before 3.11.5), one per agent and project (the newest
    /// copy wins), newest first (ties: agent id, project). Reads at most
    /// `max_files` diary files (the most recently written) and skips
    /// symlinks and files over [`MAX_DIARY_FILE_BYTES`].
    pub fn load_all(max_files: usize) -> Vec<AgentDiary> {
        let Ok(base) = diary_dir() else {
            return Vec::new();
        };
        let mut dirs = vec![base.clone()];
        dirs.extend(partitions(&base));
        let mut files = diary_files(&dirs);
        files.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        files.truncate(max_files);
        let mut newest: std::collections::BTreeMap<(String, String), AgentDiary> =
            std::collections::BTreeMap::new();
        for (_, path) in files {
            let Some(diary) = read_diary(&path) else {
                continue;
            };
            let key = (diary.agent_id.clone(), diary.project_root.clone());
            match newest.get(&key) {
                Some(known) if known.updated_at >= diary.updated_at => {}
                _ => {
                    newest.insert(key, diary);
                }
            }
        }
        let mut diaries: Vec<AgentDiary> = newest.into_values().collect();
        diaries.sort_by(|a, b| {
            b.updated_at
                .cmp(&a.updated_at)
                .then_with(|| a.agent_id.cmp(&b.agent_id))
                .then_with(|| a.project_root.cmp(&b.project_root))
        });
        diaries
    }
}

/// The largest diary file any load reads (100 entries of ordinary agent
/// notes stay far below it).
pub const MAX_DIARY_FILE_BYTES: u64 = 1024 * 1024;

/// The most diary files [`AgentDiary::load_all_for_project`] reads.
pub const MAX_PROJECT_DIARY_FILES: usize = 2000;

/// The partition directories in `base`, by name. A symlink is no partition.
fn partitions(base: &std::path::Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(base) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|t| t.is_dir()))
        .map(|entry| entry.path())
        .collect();
    dirs.sort();
    dirs
}

/// The diary files in `dirs` (in `dirs` order, by name within a directory)
/// with their modification time: regular `*.json` files of at most
/// [`MAX_DIARY_FILE_BYTES`], never a symlink. Nothing is read.
fn diary_files(dirs: &[PathBuf]) -> Vec<(std::time::SystemTime, PathBuf)> {
    let mut files = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        let mut found: Vec<(std::time::SystemTime, PathBuf)> = entries
            .flatten()
            .filter_map(|entry| {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("json") {
                    return None;
                }
                let meta = std::fs::symlink_metadata(&path).ok()?;
                (meta.is_file() && meta.len() <= MAX_DIARY_FILE_BYTES)
                    .then(|| (meta.modified().unwrap_or(std::time::UNIX_EPOCH), path))
            })
            .collect();
        found.sort_by(|a, b| a.1.cmp(&b.1));
        files.extend(found);
    }
    files
}

/// The diary in `path`: opened without following a symlink (the opened
/// file is checked, so a swap after listing is refused too) and read only
/// when it is a regular file of at most [`MAX_DIARY_FILE_BYTES`].
fn read_diary(path: &std::path::Path) -> Option<AgentDiary> {
    use std::io::Read;
    if path.extension().and_then(|e| e.to_str()) != Some("json") {
        return None;
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(not(unix))]
    {
        if !std::fs::symlink_metadata(path).ok()?.is_file() {
            return None;
        }
    }
    let file = options.open(path).ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() || meta.len() > MAX_DIARY_FILE_BYTES {
        return None;
    }
    let mut text = String::new();
    file.take(MAX_DIARY_FILE_BYTES + 1)
        .read_to_string(&mut text)
        .ok()?;
    if text.len() as u64 > MAX_DIARY_FILE_BYTES {
        return None;
    }
    serde_json::from_str(&text).ok()
}

fn same_root(stored: &str, requested: &str) -> bool {
    use crate::core::pathutil::{normalize_tool_path, safe_canonicalize_bounded};
    let canonical = |root: &str| safe_canonicalize_bounded(std::path::Path::new(root), 2000);
    normalize_tool_path(stored).trim_end_matches('/')
        == normalize_tool_path(requested).trim_end_matches('/')
        || canonical(stored) == canonical(requested)
}

/// Where a diary of `project_root` may be: its current partition first, then
/// every other partition (an identity change moved the hash), then the shared
/// directory of releases before 3.11.5. The stored root decides membership.
fn search_dirs(project_root: &str) -> Vec<PathBuf> {
    let (Ok(current), Ok(base)) = (partition_dir(project_root), diary_dir()) else {
        return Vec::new();
    };
    // A symlinked partition is never searched, the current one included.
    let mut dirs: Vec<PathBuf> = std::fs::symlink_metadata(&current)
        .is_ok_and(|meta| meta.is_dir())
        .then(|| current.clone())
        .into_iter()
        .collect();
    dirs.extend(partitions(&base).into_iter().filter(|dir| *dir != current));
    dirs.push(base);
    dirs
}

fn partition_dir(project_root: &str) -> Result<PathBuf, String> {
    Ok(diary_dir()?.join(crate::core::project_hash::hash_project_root(project_root)))
}

impl std::fmt::Display for DiaryEntryType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DiaryEntryType::Discovery => write!(f, "discovery"),
            DiaryEntryType::Decision => write!(f, "decision"),
            DiaryEntryType::Blocker => write!(f, "blocker"),
            DiaryEntryType::Progress => write!(f, "progress"),
            DiaryEntryType::Insight => write!(f, "insight"),
        }
    }
}

fn diary_dir() -> Result<PathBuf, String> {
    let dir = crate::core::data_dir::lean_ctx_data_dir()?;
    Ok(dir.join("agents").join("diaries"))
}

fn sanitize_filename(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}
