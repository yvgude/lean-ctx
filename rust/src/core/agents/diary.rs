use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

const MAX_DIARY_ENTRIES: usize = 100;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct AgentDiary {
    pub agent_id: String,
    pub agent_type: String,
    pub project_root: String,
    pub entries: Vec<DiaryEntry>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct DiaryEntry {
    pub entry_type: DiaryEntryType,
    pub content: String,
    pub context: Option<String>,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) enum DiaryEntryType {
    Discovery,
    Decision,
    Blocker,
    Progress,
    Insight,
}

impl AgentDiary {
    pub(crate) fn new(agent_id: &str, agent_type: &str, project_root: &str) -> Self {
        let now = Utc::now();
        Self {
            agent_id: agent_id.to_string(),
            agent_type: agent_type.to_string(),
            project_root: project_root.to_string(),
            entries: Vec::new(),
            created_at: now,
            updated_at: now,
        }
    }

    pub(crate) fn add_entry(
        &mut self,
        entry_type: DiaryEntryType,
        content: &str,
        context: Option<&str>,
    ) {
        self.entries.push(DiaryEntry {
            entry_type,
            content: content.to_string(),
            context: context.map(std::string::ToString::to_string),
            timestamp: Utc::now(),
        });
        if self.entries.len() > MAX_DIARY_ENTRIES {
            self.entries
                .drain(0..self.entries.len() - MAX_DIARY_ENTRIES);
        }
        self.updated_at = Utc::now();
    }

    pub(crate) fn format_summary(&self) -> String {
        if self.entries.is_empty() {
            return format!("Diary [{}]: empty", self.agent_id);
        }
        let mut out = format!(
            "Diary [{}] ({} entries):\n",
            self.agent_id,
            self.entries.len()
        );
        let now = Utc::now();
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
    pub(crate) fn save(&self) -> Result<(), String> {
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
    pub(crate) fn load_for_project(agent_id: &str, project_root: &str) -> Option<Self> {
        let file = format!("{}.json", sanitize_filename(agent_id));
        search_dirs(project_root)
            .iter()
            .filter_map(|dir| read_diary(&dir.join(&file)))
            .find(|diary| {
                diary.agent_id == agent_id && same_root(&diary.project_root, project_root)
            })
    }

    pub(crate) fn load_or_create(agent_id: &str, agent_type: &str, project_root: &str) -> Self {
        Self::load_for_project(agent_id, project_root)
            .unwrap_or_else(|| Self::new(agent_id, agent_type, project_root))
    }

    /// `(agent_id, entries, updated_at)` of every diary kept in `project_root`.
    pub(crate) fn list_for_project(project_root: &str) -> Vec<(String, usize, DateTime<Utc>)> {
        Self::load_all_for_project(project_root)
            .into_iter()
            .map(|diary| (diary.agent_id, diary.entries.len(), diary.updated_at))
            .collect()
    }

    /// Load every diary whose `project_root` matches `project_root`, most
    /// recently updated first. Used by skillify to mine a project's decisions
    /// and insights across all its agents (#290).
    pub(crate) fn load_all_for_project(project_root: &str) -> Vec<AgentDiary> {
        let mut diaries: Vec<AgentDiary> = Vec::new();
        // The current partition first: its diary supersedes an older one.
        for dir in search_dirs(project_root) {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                if let Some(diary) = read_diary(&entry.path())
                    && same_root(&diary.project_root, project_root)
                    && !diaries.iter().any(|known| known.agent_id == diary.agent_id)
                {
                    diaries.push(diary);
                }
            }
        }
        diaries.sort_by_key(|d| std::cmp::Reverse(d.updated_at));
        diaries
    }
}

fn read_diary(path: &std::path::Path) -> Option<AgentDiary> {
    if path.extension().and_then(|e| e.to_str()) != Some("json") {
        return None;
    }
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
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
    let mut dirs = vec![current.clone()];
    if let Ok(entries) = std::fs::read_dir(&base) {
        let mut others: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.is_dir() && *path != current)
            .collect();
        others.sort();
        dirs.extend(others);
    }
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
