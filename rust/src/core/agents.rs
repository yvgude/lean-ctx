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
        crate::core::events::emit_agent_action(&agent_id, "register", None);
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

    pub fn share_knowledge(
        &mut self,
        from_agent: &str,
        category: &str,
        facts: &[(String, String)],
    ) {
        for (key, value) in facts {
            let msg = format!("K:{category}:{key}={value}");
            self.post_message(from_agent, None, "knowledge", &msg);
        }
    }

    pub fn receive_shared_knowledge(&mut self, agent_id: &str) -> Vec<SharedFact> {
        let messages = self.read_unread(agent_id);
        messages
            .iter()
            .filter(|m| m.category == "knowledge")
            .filter_map(|m| {
                let body = m.message.strip_prefix("K:")?;
                let (cat_key, value) = body.split_once('=')?;
                let (category, key) = cat_key.split_once(':')?;
                Some(SharedFact {
                    from_agent: m.from_agent.clone(),
                    category: category.to_string(),
                    key: key.to_string(),
                    value: value.to_string(),
                    timestamp: m.timestamp,
                })
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
        results.sort_by_key(|x| std::cmp::Reverse(x.2));
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

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max.saturating_sub(3)])
    }
}

fn agents_dir() -> Result<PathBuf, String> {
    let dir = crate::core::data_dir::lean_ctx_data_dir()?;
    Ok(dir.join("agents"))
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

#[derive(Debug, Clone)]
pub struct SharedFact {
    pub from_agent: String,
    pub category: String,
    pub key: String,
    pub value: String,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRole {
    Coder,
    Reviewer,
    Planner,
    Explorer,
    Debugger,
    Tester,
    Orchestrator,
}

impl AgentRole {
    pub fn from_str_loose(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "review" | "reviewer" | "code_review" => Self::Reviewer,
            "plan" | "planner" | "architect" => Self::Planner,
            "explore" | "explorer" | "research" => Self::Explorer,
            "debug" | "debugger" => Self::Debugger,
            "test" | "tester" | "qa" => Self::Tester,
            "orchestrator" | "coordinator" | "manager" => Self::Orchestrator,
            _ => Self::Coder,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ContextDepthConfig {
    pub max_files_full: usize,
    pub max_files_signatures: usize,
    pub preferred_mode: &'static str,
    pub include_graph: bool,
    pub include_knowledge: bool,
    pub include_gotchas: bool,
    pub context_budget_ratio: f64,
}

impl ContextDepthConfig {
    pub fn for_role(role: AgentRole) -> Self {
        match role {
            AgentRole::Coder => Self {
                max_files_full: 5,
                max_files_signatures: 15,
                preferred_mode: "full",
                include_graph: true,
                include_knowledge: true,
                include_gotchas: true,
                context_budget_ratio: 0.7,
            },
            AgentRole::Reviewer => Self {
                max_files_full: 3,
                max_files_signatures: 20,
                preferred_mode: "signatures",
                include_graph: true,
                include_knowledge: true,
                include_gotchas: true,
                context_budget_ratio: 0.5,
            },
            AgentRole::Planner => Self {
                max_files_full: 1,
                max_files_signatures: 10,
                preferred_mode: "map",
                include_graph: true,
                include_knowledge: true,
                include_gotchas: false,
                context_budget_ratio: 0.3,
            },
            AgentRole::Explorer => Self {
                max_files_full: 2,
                max_files_signatures: 8,
                preferred_mode: "map",
                include_graph: true,
                include_knowledge: false,
                include_gotchas: false,
                context_budget_ratio: 0.4,
            },
            AgentRole::Debugger => Self {
                max_files_full: 8,
                max_files_signatures: 5,
                preferred_mode: "full",
                include_graph: false,
                include_knowledge: true,
                include_gotchas: true,
                context_budget_ratio: 0.8,
            },
            AgentRole::Tester => Self {
                max_files_full: 4,
                max_files_signatures: 10,
                preferred_mode: "full",
                include_graph: false,
                include_knowledge: false,
                include_gotchas: true,
                context_budget_ratio: 0.6,
            },
            AgentRole::Orchestrator => Self {
                max_files_full: 0,
                max_files_signatures: 5,
                preferred_mode: "map",
                include_graph: true,
                include_knowledge: true,
                include_gotchas: false,
                context_budget_ratio: 0.2,
            },
        }
    }

    pub fn mode_for_rank(&self, rank: usize) -> &'static str {
        if rank < self.max_files_full {
            "full"
        } else if rank < self.max_files_full + self.max_files_signatures {
            "signatures"
        } else {
            "map"
        }
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

    #[test]
    fn share_and_receive_knowledge() {
        let mut reg = AgentRegistry::new();
        let facts = vec![
            ("db_type".to_string(), "postgres".to_string()),
            ("api_version".to_string(), "v3".to_string()),
        ];
        reg.share_knowledge("agent-a", "architecture", &facts);

        let received = reg.receive_shared_knowledge("agent-b");
        assert_eq!(received.len(), 2);
        assert_eq!(received[0].category, "architecture");
        assert_eq!(received[0].key, "db_type");
        assert_eq!(received[0].value, "postgres");
        assert_eq!(received[1].key, "api_version");
    }

    #[test]
    fn shared_knowledge_not_received_by_sender() {
        let mut reg = AgentRegistry::new();
        reg.share_knowledge(
            "agent-a",
            "config",
            &[("port".to_string(), "8080".to_string())],
        );
        let received = reg.receive_shared_knowledge("agent-a");
        assert!(received.is_empty());
    }

    #[test]
    fn role_from_str_loose_variants() {
        assert_eq!(AgentRole::from_str_loose("review"), AgentRole::Reviewer);
        assert_eq!(AgentRole::from_str_loose("reviewer"), AgentRole::Reviewer);
        assert_eq!(AgentRole::from_str_loose("plan"), AgentRole::Planner);
        assert_eq!(AgentRole::from_str_loose("debug"), AgentRole::Debugger);
        assert_eq!(AgentRole::from_str_loose("test"), AgentRole::Tester);
        assert_eq!(AgentRole::from_str_loose("qa"), AgentRole::Tester);
        assert_eq!(
            AgentRole::from_str_loose("orchestrator"),
            AgentRole::Orchestrator
        );
        assert_eq!(AgentRole::from_str_loose("unknown"), AgentRole::Coder);
        assert_eq!(AgentRole::from_str_loose(""), AgentRole::Coder);
    }

    #[test]
    fn context_depth_coder_vs_orchestrator() {
        let coder = ContextDepthConfig::for_role(AgentRole::Coder);
        let orch = ContextDepthConfig::for_role(AgentRole::Orchestrator);
        assert!(coder.max_files_full > orch.max_files_full);
        assert!(coder.context_budget_ratio > orch.context_budget_ratio);
    }

    #[test]
    fn context_depth_debugger_more_full() {
        let debugger = ContextDepthConfig::for_role(AgentRole::Debugger);
        let planner = ContextDepthConfig::for_role(AgentRole::Planner);
        assert!(debugger.max_files_full > planner.max_files_full);
        assert!(debugger.context_budget_ratio > planner.context_budget_ratio);
    }

    #[test]
    fn mode_for_rank_degrades() {
        let cfg = ContextDepthConfig::for_role(AgentRole::Coder);
        assert_eq!(cfg.mode_for_rank(0), "full");
        assert_eq!(cfg.mode_for_rank(cfg.max_files_full), "signatures");
        assert_eq!(
            cfg.mode_for_rank(cfg.max_files_full + cfg.max_files_signatures),
            "map"
        );
    }
}
