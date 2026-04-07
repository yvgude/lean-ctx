use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

const MAX_SCRATCHPAD_ENTRIES: usize = 200;
const MAX_DIARY_ENTRIES: usize = 100;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRegistry {
    pub agents: Vec<AgentEntry>,
    pub scratchpad: Vec<ScratchpadEntry>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentDiary {
    pub agent_id: String,
    pub agent_type: String,
    pub project_root: String,
    pub entries: Vec<DiaryEntry>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiaryEntry {
    pub entry_type: DiaryEntryType,
    pub content: String,
    pub context: Option<String>,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum DiaryEntryType {
    Discovery,
    Decision,
    Blocker,
    Progress,
    Insight,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentEntry {
    pub agent_id: String,
    pub agent_type: String,
    pub role: Option<String>,
    pub project_root: String,
    pub started_at: DateTime<Utc>,
    pub last_active: DateTime<Utc>,
    pub pid: u32,
    pub status: AgentStatus,
    pub status_message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum AgentStatus {
    Active,
    Idle,
    Finished,
}

impl std::fmt::Display for AgentStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentStatus::Active => write!(f, "active"),
            AgentStatus::Idle => write!(f, "idle"),
            AgentStatus::Finished => write!(f, "finished"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScratchpadEntry {
    pub id: String,
    pub from_agent: String,
    pub to_agent: Option<String>,
    pub category: String,
    pub message: String,
    pub timestamp: DateTime<Utc>,
    pub read_by: Vec<String>,
}

impl AgentRegistry {
    pub fn new() -> Self {
        Self {
            agents: Vec::new(),
            scratchpad: Vec::new(),
            updated_at: Utc::now(),
        }
    }

    pub fn register(&mut self, agent_type: &str, role: Option<&str>, project_root: &str) -> String {
        let pid = std::process::id();
        let agent_id = format!("{}-{}-{}", agent_type, pid, &generate_short_id());

        if let Some(existing) = self.agents.iter_mut().find(|a| a.pid == pid) {
            existing.last_active = Utc::now();
            existing.status = AgentStatus::Active;
            if let Some(r) = role {
                existing.role = Some(r.to_string());
            }
            return existing.agent_id.clone();
        }

        self.agents.push(AgentEntry {
            agent_id: agent_id.clone(),
            agent_type: agent_type.to_string(),
            role: role.map(|r| r.to_string()),
            project_root: project_root.to_string(),
            started_at: Utc::now(),
            last_active: Utc::now(),
            pid,
            status: AgentStatus::Active,
            status_message: None,
        });

        self.updated_at = Utc::now();
        agent_id
    }

    pub fn update_heartbeat(&mut self, agent_id: &str) {
        if let Some(agent) = self.agents.iter_mut().find(|a| a.agent_id == agent_id) {
            agent.last_active = Utc::now();
        }
    }

    pub fn set_status(&mut self, agent_id: &str, status: AgentStatus, message: Option<&str>) {
        if let Some(agent) = self.agents.iter_mut().find(|a| a.agent_id == agent_id) {
            agent.status = status;
            agent.status_message = message.map(|s| s.to_string());
            agent.last_active = Utc::now();
        }
        self.updated_at = Utc::now();
    }

    pub fn list_active(&self, project_root: Option<&str>) -> Vec<&AgentEntry> {
        self.agents
            .iter()
            .filter(|a| {
                if let Some(root) = project_root {
                    a.project_root == root && a.status != AgentStatus::Finished
                } else {
                    a.status != AgentStatus::Finished
                }
            })
            .collect()
    }

    pub fn list_all(&self) -> &[AgentEntry] {
        &self.agents
    }

    pub fn post_message(
        &mut self,
        from_agent: &str,
        to_agent: Option<&str>,
        category: &str,
        message: &str,
    ) -> String {
        let id = generate_short_id();
        self.scratchpad.push(ScratchpadEntry {
            id: id.clone(),
            from_agent: from_agent.to_string(),
            to_agent: to_agent.map(|s| s.to_string()),
            category: category.to_string(),
            message: message.to_string(),
            timestamp: Utc::now(),
            read_by: vec![from_agent.to_string()],
        });

        if self.scratchpad.len() > MAX_SCRATCHPAD_ENTRIES {
            self.scratchpad
                .drain(0..self.scratchpad.len() - MAX_SCRATCHPAD_ENTRIES);
        }

        self.updated_at = Utc::now();
        id
    }

    pub fn read_messages(&mut self, agent_id: &str) -> Vec<&ScratchpadEntry> {
        let unread: Vec<usize> = self
            .scratchpad
            .iter()
            .enumerate()
            .filter(|(_, e)| {
                !e.read_by.contains(&agent_id.to_string())
                    && (e.to_agent.is_none() || e.to_agent.as_deref() == Some(agent_id))
            })
            .map(|(i, _)| i)
            .collect();

        for i in &unread {
            self.scratchpad[*i].read_by.push(agent_id.to_string());
        }

        self.scratchpad
            .iter()
            .filter(|e| e.to_agent.is_none() || e.to_agent.as_deref() == Some(agent_id))
            .filter(|e| e.from_agent != agent_id)
            .collect()
    }

    pub fn read_unread(&mut self, agent_id: &str) -> Vec<&ScratchpadEntry> {
        let unread_indices: Vec<usize> = self
            .scratchpad
            .iter()
            .enumerate()
            .filter(|(_, e)| {
                !e.read_by.contains(&agent_id.to_string())
                    && e.from_agent != agent_id
                    && (e.to_agent.is_none() || e.to_agent.as_deref() == Some(agent_id))
            })
            .map(|(i, _)| i)
            .collect();

        for i in &unread_indices {
            self.scratchpad[*i].read_by.push(agent_id.to_string());
        }

        self.updated_at = Utc::now();

        self.scratchpad
            .iter()
            .filter(|e| {
                e.from_agent != agent_id
                    && (e.to_agent.is_none() || e.to_agent.as_deref() == Some(agent_id))
                    && e.read_by.contains(&agent_id.to_string())
                    && e.read_by.iter().filter(|r| *r == agent_id).count() == 1
            })
            .collect()
    }

    pub fn cleanup_stale(&mut self, max_age_hours: u64) {
        let cutoff = Utc::now() - chrono::Duration::hours(max_age_hours as i64);

        for agent in &mut self.agents {
            if agent.last_active < cutoff
                && agent.status != AgentStatus::Finished
                && !is_process_alive(agent.pid)
            {
                agent.status = AgentStatus::Finished;
            }
        }

        self.agents
            .retain(|a| !(a.status == AgentStatus::Finished && a.last_active < cutoff));

        self.updated_at = Utc::now();
    }

    pub fn save(&self) -> Result<(), String> {
        let dir = agents_dir()?;
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;

        let path = dir.join("registry.json");
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;

        let lock_path = dir.join("registry.lock");
        let _lock = FileLock::acquire(&lock_path)?;

        std::fs::write(&path, json).map_err(|e| e.to_string())
    }

    pub fn load() -> Option<Self> {
        let dir = agents_dir().ok()?;
        let path = dir.join("registry.json");
        let content = std::fs::read_to_string(&path).ok()?;
        serde_json::from_str(&content).ok()
    }

    pub fn load_or_create() -> Self {
        Self::load().unwrap_or_default()
    }
}

impl Default for AgentRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentDiary {
    pub fn new(agent_id: &str, agent_type: &str, project_root: &str) -> Self {
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

    pub fn add_entry(&mut self, entry_type: DiaryEntryType, content: &str, context: Option<&str>) {
        self.entries.push(DiaryEntry {
            entry_type,
            content: content.to_string(),
            context: context.map(|s| s.to_string()),
            timestamp: Utc::now(),
        });
        if self.entries.len() > MAX_DIARY_ENTRIES {
            self.entries
                .drain(0..self.entries.len() - MAX_DIARY_ENTRIES);
        }
        self.updated_at = Utc::now();
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
        for e in self.entries.iter().rev().take(10) {
            let age = (Utc::now() - e.timestamp).num_minutes();
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

    pub fn format_compact(&self) -> String {
        if self.entries.is_empty() {
            return String::new();
        }
        let items: Vec<String> = self
            .entries
            .iter()
            .rev()
            .take(5)
            .map(|e| {
                let prefix = match e.entry_type {
                    DiaryEntryType::Discovery => "F",
                    DiaryEntryType::Decision => "D",
                    DiaryEntryType::Blocker => "B",
                    DiaryEntryType::Progress => "P",
                    DiaryEntryType::Insight => "I",
                };
                format!("{prefix}:{}", truncate(&e.content, 50))
            })
            .collect();
        format!("diary:{}|{}", self.agent_id, items.join("|"))
    }

    pub fn save(&self) -> Result<(), String> {
        let dir = diary_dir()?;
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let path = dir.join(format!("{}.json", sanitize_filename(&self.agent_id)));
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(&path, json).map_err(|e| e.to_string())
    }

    pub fn load(agent_id: &str) -> Option<Self> {
        let dir = diary_dir().ok()?;
        let path = dir.join(format!("{}.json", sanitize_filename(agent_id)));
        let content = std::fs::read_to_string(&path).ok()?;
        serde_json::from_str(&content).ok()
    }

    pub fn load_or_create(agent_id: &str, agent_type: &str, project_root: &str) -> Self {
        Self::load(agent_id).unwrap_or_else(|| Self::new(agent_id, agent_type, project_root))
    }

    pub fn list_all() -> Vec<(String, usize, DateTime<Utc>)> {
        let dir = match diary_dir() {
            Ok(d) => d,
            Err(_) => return Vec::new(),
        };
        if !dir.exists() {
            return Vec::new();
        }
        let mut results = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                if entry.path().extension().and_then(|e| e.to_str()) == Some("json") {
                    if let Ok(content) = std::fs::read_to_string(entry.path()) {
                        if let Ok(diary) = serde_json::from_str::<AgentDiary>(&content) {
                            results.push((diary.agent_id, diary.entries.len(), diary.updated_at));
                        }
                    }
                }
            }
        }
        results.sort_by(|a, b| b.2.cmp(&a.2));
        results
    }
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
    let home = dirs::home_dir().ok_or("Cannot determine home directory")?;
    Ok(home.join(".lean-ctx").join("agents").join("diaries"))
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

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max.saturating_sub(3)])
    }
}

fn agents_dir() -> Result<PathBuf, String> {
    let home = dirs::home_dir().ok_or("Cannot determine home directory")?;
    Ok(home.join(".lean-ctx").join("agents"))
}

fn generate_short_id() -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    use std::time::SystemTime;

    let mut hasher = DefaultHasher::new();
    SystemTime::now().hash(&mut hasher);
    std::process::id().hash(&mut hasher);
    format!("{:08x}", hasher.finish() as u32)
}

fn is_process_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        true
    }
}

struct FileLock {
    path: PathBuf,
}

impl FileLock {
    fn acquire(path: &std::path::Path) -> Result<Self, String> {
        for _ in 0..50 {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
            {
                Ok(_) => {
                    return Ok(Self {
                        path: path.to_path_buf(),
                    })
                }
                Err(_) => {
                    if let Ok(metadata) = std::fs::metadata(path) {
                        if let Ok(modified) = metadata.modified() {
                            if modified.elapsed().unwrap_or_default().as_secs() > 5 {
                                let _ = std::fs::remove_file(path);
                                continue;
                            }
                        }
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            }
        }
        Err("Could not acquire lock after 5 seconds".to_string())
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_and_list() {
        let mut reg = AgentRegistry::new();
        let id = reg.register("cursor", Some("dev"), "/tmp/project");
        assert!(!id.is_empty());
        assert_eq!(reg.list_active(None).len(), 1);
        assert_eq!(reg.list_active(None)[0].agent_type, "cursor");
    }

    #[test]
    fn reregister_same_pid() {
        let mut reg = AgentRegistry::new();
        let id1 = reg.register("cursor", Some("dev"), "/tmp/project");
        let id2 = reg.register("cursor", Some("review"), "/tmp/project");
        assert_eq!(id1, id2);
        assert_eq!(reg.agents.len(), 1);
        assert_eq!(reg.agents[0].role, Some("review".to_string()));
    }

    #[test]
    fn post_and_read_messages() {
        let mut reg = AgentRegistry::new();
        reg.post_message("agent-a", None, "finding", "Found a bug in auth.rs");
        reg.post_message("agent-b", Some("agent-a"), "request", "Please review");

        let msgs = reg.read_unread("agent-a");
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].category, "request");
    }

    #[test]
    fn set_status() {
        let mut reg = AgentRegistry::new();
        let id = reg.register("claude", None, "/tmp/project");
        reg.set_status(&id, AgentStatus::Idle, Some("waiting for review"));
        assert_eq!(reg.agents[0].status, AgentStatus::Idle);
        assert_eq!(
            reg.agents[0].status_message,
            Some("waiting for review".to_string())
        );
    }

    #[test]
    fn broadcast_message() {
        let mut reg = AgentRegistry::new();
        reg.post_message("agent-a", None, "status", "Starting refactor");

        let msgs_b = reg.read_unread("agent-b");
        assert_eq!(msgs_b.len(), 1);
        assert_eq!(msgs_b[0].message, "Starting refactor");

        let msgs_a = reg.read_unread("agent-a");
        assert!(msgs_a.is_empty());
    }

    #[test]
    fn diary_add_and_format() {
        let mut diary = AgentDiary::new("test-agent-001", "cursor", "/tmp/project");
        diary.add_entry(
            DiaryEntryType::Discovery,
            "Found auth module at src/auth.rs",
            Some("auth"),
        );
        diary.add_entry(
            DiaryEntryType::Decision,
            "Use JWT RS256 for token signing",
            None,
        );
        diary.add_entry(
            DiaryEntryType::Progress,
            "Implemented login endpoint",
            Some("auth"),
        );

        assert_eq!(diary.entries.len(), 3);

        let summary = diary.format_summary();
        assert!(summary.contains("test-agent-001"));
        assert!(summary.contains("FOUND"));
        assert!(summary.contains("DECIDED"));
        assert!(summary.contains("DONE"));
    }

    #[test]
    fn diary_compact_format() {
        let mut diary = AgentDiary::new("test-agent-002", "claude", "/tmp/project");
        diary.add_entry(DiaryEntryType::Insight, "DB queries are N+1", None);
        diary.add_entry(
            DiaryEntryType::Blocker,
            "Missing API credentials",
            Some("deploy"),
        );

        let compact = diary.format_compact();
        assert!(compact.contains("diary:test-agent-002"));
        assert!(compact.contains("B:Missing API credentials"));
        assert!(compact.contains("I:DB queries are N+1"));
    }

    #[test]
    fn diary_entry_types() {
        let types = vec![
            DiaryEntryType::Discovery,
            DiaryEntryType::Decision,
            DiaryEntryType::Blocker,
            DiaryEntryType::Progress,
            DiaryEntryType::Insight,
        ];
        for t in types {
            assert!(!format!("{}", t).is_empty());
        }
    }

    #[test]
    fn diary_truncation() {
        let mut diary = AgentDiary::new("test-agent", "cursor", "/tmp");
        for i in 0..150 {
            diary.add_entry(DiaryEntryType::Progress, &format!("Step {i}"), None);
        }
        assert!(diary.entries.len() <= 100);
    }
}
