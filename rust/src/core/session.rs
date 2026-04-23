use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::core::intent_protocol::{IntentRecord, IntentSource};

const MAX_FINDINGS: usize = 20;
const MAX_DECISIONS: usize = 10;
const MAX_FILES: usize = 50;
const MAX_EVIDENCE: usize = 500;
const BATCH_SAVE_INTERVAL: u32 = 5;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SessionState {
    pub id: String,
    pub version: u32,
    pub started_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub project_root: Option<String>,
    #[serde(default)]
    pub shell_cwd: Option<String>,
    pub task: Option<TaskInfo>,
    pub findings: Vec<Finding>,
    pub decisions: Vec<Decision>,
    pub files_touched: Vec<FileTouched>,
    pub test_results: Option<TestSnapshot>,
    pub progress: Vec<ProgressEntry>,
    pub next_steps: Vec<String>,
    #[serde(default)]
    pub evidence: Vec<EvidenceRecord>,
    #[serde(default)]
    pub intents: Vec<IntentRecord>,
    #[serde(default)]
    pub active_structured_intent: Option<crate::core::intent_engine::StructuredIntent>,
    pub stats: SessionStats,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TaskInfo {
    pub description: String,
    pub intent: Option<String>,
    pub progress_pct: Option<u8>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Finding {
    pub file: Option<String>,
    pub line: Option<u32>,
    pub summary: String,
    pub timestamp: DateTime<Utc>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Decision {
    pub summary: String,
    pub rationale: Option<String>,
    pub timestamp: DateTime<Utc>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct FileTouched {
    pub path: String,
    pub file_ref: Option<String>,
    pub read_count: u32,
    pub modified: bool,
    pub last_mode: String,
    pub tokens: usize,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TestSnapshot {
    pub command: String,
    pub passed: u32,
    pub failed: u32,
    pub total: u32,
    pub timestamp: DateTime<Utc>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ProgressEntry {
    pub action: String,
    pub detail: Option<String>,
    pub timestamp: DateTime<Utc>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    ToolCall,
    Manual,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct EvidenceRecord {
    pub kind: EvidenceKind,
    pub key: String,
    pub value: Option<String>,
    pub tool: Option<String>,
    pub input_md5: Option<String>,
    pub output_md5: Option<String>,
    pub agent_id: Option<String>,
    pub client_name: Option<String>,
    pub timestamp: DateTime<Utc>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct SessionStats {
    pub total_tool_calls: u32,
    pub total_tokens_saved: u64,
    pub total_tokens_input: u64,
    pub cache_hits: u32,
    pub files_read: u32,
    pub commands_run: u32,
    pub intents_inferred: u32,
    pub intents_explicit: u32,
    pub unsaved_changes: u32,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
struct LatestPointer {
    id: String,
}

impl Default for SessionState {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionState {
    pub fn new() -> Self {
        let now = Utc::now();
        Self {
            id: generate_session_id(),
            version: 0,
            started_at: now,
            updated_at: now,
            project_root: None,
            shell_cwd: None,
            task: None,
            findings: Vec::new(),
            decisions: Vec::new(),
            files_touched: Vec::new(),
            test_results: None,
            progress: Vec::new(),
            next_steps: Vec::new(),
            evidence: Vec::new(),
            intents: Vec::new(),
            active_structured_intent: None,
            stats: SessionStats::default(),
        }
    }

    pub fn increment(&mut self) {
        self.version += 1;
        self.updated_at = Utc::now();
        self.stats.unsaved_changes += 1;
    }

    pub fn should_save(&self) -> bool {
        self.stats.unsaved_changes >= BATCH_SAVE_INTERVAL
    }

    pub fn set_task(&mut self, description: &str, intent: Option<&str>) {
        self.task = Some(TaskInfo {
            description: description.to_string(),
            intent: intent.map(|s| s.to_string()),
            progress_pct: None,
        });

        let touched: Vec<String> = self.files_touched.iter().map(|f| f.path.clone()).collect();
        let si = if touched.is_empty() {
            crate::core::intent_engine::StructuredIntent::from_query(description)
        } else {
            crate::core::intent_engine::StructuredIntent::from_query_with_session(
                description,
                &touched,
            )
        };
        if si.confidence >= 0.7 {
            self.active_structured_intent = Some(si);
        }

        self.increment();
    }

    pub fn add_finding(&mut self, file: Option<&str>, line: Option<u32>, summary: &str) {
        self.findings.push(Finding {
            file: file.map(|s| s.to_string()),
            line,
            summary: summary.to_string(),
            timestamp: Utc::now(),
        });
        while self.findings.len() > MAX_FINDINGS {
            self.findings.remove(0);
        }
        self.increment();
    }

    pub fn add_decision(&mut self, summary: &str, rationale: Option<&str>) {
        self.decisions.push(Decision {
            summary: summary.to_string(),
            rationale: rationale.map(|s| s.to_string()),
            timestamp: Utc::now(),
        });
        while self.decisions.len() > MAX_DECISIONS {
            self.decisions.remove(0);
        }
        self.increment();
    }

    pub fn touch_file(&mut self, path: &str, file_ref: Option<&str>, mode: &str, tokens: usize) {
        if let Some(existing) = self.files_touched.iter_mut().find(|f| f.path == path) {
            existing.read_count += 1;
            existing.last_mode = mode.to_string();
            existing.tokens = tokens;
            if let Some(r) = file_ref {
                existing.file_ref = Some(r.to_string());
            }
        } else {
            self.files_touched.push(FileTouched {
                path: path.to_string(),
                file_ref: file_ref.map(|s| s.to_string()),
                read_count: 1,
                modified: false,
                last_mode: mode.to_string(),
                tokens,
            });
            while self.files_touched.len() > MAX_FILES {
                self.files_touched.remove(0);
            }
        }
        self.stats.files_read += 1;
        self.increment();
    }

    pub fn mark_modified(&mut self, path: &str) {
        if let Some(existing) = self.files_touched.iter_mut().find(|f| f.path == path) {
            existing.modified = true;
        }
        self.increment();
    }

    pub fn record_tool_call(&mut self, tokens_saved: u64, tokens_input: u64) {
        self.stats.total_tool_calls += 1;
        self.stats.total_tokens_saved += tokens_saved;
        self.stats.total_tokens_input += tokens_input;
    }

    pub fn record_intent(&mut self, mut intent: IntentRecord) {
        if intent.occurrences == 0 {
            intent.occurrences = 1;
        }

        if let Some(last) = self.intents.last_mut() {
            if last.fingerprint() == intent.fingerprint() {
                last.occurrences = last.occurrences.saturating_add(intent.occurrences);
                last.timestamp = intent.timestamp;
                match intent.source {
                    IntentSource::Inferred => self.stats.intents_inferred += 1,
                    IntentSource::Explicit => self.stats.intents_explicit += 1,
                }
                self.increment();
                return;
            }
        }

        match intent.source {
            IntentSource::Inferred => self.stats.intents_inferred += 1,
            IntentSource::Explicit => self.stats.intents_explicit += 1,
        }

        self.intents.push(intent);
        while self.intents.len() > crate::core::budgets::INTENTS_PER_SESSION_LIMIT {
            self.intents.remove(0);
        }
        self.increment();
    }

    pub fn record_tool_receipt(
        &mut self,
        tool: &str,
        action: Option<&str>,
        input_md5: &str,
        output_md5: &str,
        agent_id: Option<&str>,
        client_name: Option<&str>,
    ) {
        let now = Utc::now();
        let mut push = |key: String| {
            self.evidence.push(EvidenceRecord {
                kind: EvidenceKind::ToolCall,
                key,
                value: None,
                tool: Some(tool.to_string()),
                input_md5: Some(input_md5.to_string()),
                output_md5: Some(output_md5.to_string()),
                agent_id: agent_id.map(|s| s.to_string()),
                client_name: client_name.map(|s| s.to_string()),
                timestamp: now,
            });
        };

        push(format!("tool:{tool}"));
        if let Some(a) = action {
            push(format!("tool:{tool}:{a}"));
        }
        while self.evidence.len() > MAX_EVIDENCE {
            self.evidence.remove(0);
        }
        self.increment();
    }

    pub fn record_manual_evidence(&mut self, key: &str, value: Option<&str>) {
        self.evidence.push(EvidenceRecord {
            kind: EvidenceKind::Manual,
            key: key.to_string(),
            value: value.map(|s| s.to_string()),
            tool: None,
            input_md5: None,
            output_md5: None,
            agent_id: None,
            client_name: None,
            timestamp: Utc::now(),
        });
        while self.evidence.len() > MAX_EVIDENCE {
            self.evidence.remove(0);
        }
        self.increment();
    }

    pub fn has_evidence_key(&self, key: &str) -> bool {
        self.evidence.iter().any(|e| e.key == key)
    }

    pub fn record_cache_hit(&mut self) {
        self.stats.cache_hits += 1;
    }

    pub fn record_command(&mut self) {
        self.stats.commands_run += 1;
    }

    /// Returns the effective working directory for shell commands.
    /// Priority: explicit cwd arg > session shell_cwd > project_root > process cwd
    pub fn effective_cwd(&self, explicit_cwd: Option<&str>) -> String {
        if let Some(cwd) = explicit_cwd {
            if !cwd.is_empty() && cwd != "." {
                return cwd.to_string();
            }
        }
        if let Some(ref cwd) = self.shell_cwd {
            return cwd.clone();
        }
        if let Some(ref root) = self.project_root {
            return root.clone();
        }
        std::env::current_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| ".".to_string())
    }

    /// Updates shell_cwd by detecting `cd` in the command.
    /// Handles: `cd /abs/path`, `cd rel/path` (relative to current cwd),
    /// `cd ..`, and chained commands like `cd foo && ...`.
    pub fn update_shell_cwd(&mut self, command: &str) {
        let base = self.effective_cwd(None);
        if let Some(new_cwd) = extract_cd_target(command, &base) {
            let path = std::path::Path::new(&new_cwd);
            if path.exists() && path.is_dir() {
                self.shell_cwd = Some(
                    crate::core::pathutil::safe_canonicalize_or_self(path)
                        .to_string_lossy()
                        .to_string(),
                );
            }
        }
    }

    pub fn format_compact(&self) -> String {
        let duration = self.updated_at - self.started_at;
        let hours = duration.num_hours();
        let mins = duration.num_minutes() % 60;
        let duration_str = if hours > 0 {
            format!("{hours}h {mins}m")
        } else {
            format!("{mins}m")
        };

        let mut lines = Vec::new();
        lines.push(format!(
            "SESSION v{} | {} | {} calls | {} tok saved",
            self.version, duration_str, self.stats.total_tool_calls, self.stats.total_tokens_saved
        ));

        if let Some(ref task) = self.task {
            let pct = task
                .progress_pct
                .map_or(String::new(), |p| format!(" [{p}%]"));
            lines.push(format!("Task: {}{pct}", task.description));
        }

        if let Some(ref root) = self.project_root {
            lines.push(format!("Root: {}", shorten_path(root)));
        }

        if !self.findings.is_empty() {
            let items: Vec<String> = self
                .findings
                .iter()
                .rev()
                .take(5)
                .map(|f| {
                    let loc = match (&f.file, f.line) {
                        (Some(file), Some(line)) => format!("{}:{line}", shorten_path(file)),
                        (Some(file), None) => shorten_path(file),
                        _ => String::new(),
                    };
                    if loc.is_empty() {
                        f.summary.clone()
                    } else {
                        format!("{loc} \u{2014} {}", f.summary)
                    }
                })
                .collect();
            lines.push(format!(
                "Findings ({}): {}",
                self.findings.len(),
                items.join(" | ")
            ));
        }

        if !self.decisions.is_empty() {
            let items: Vec<&str> = self
                .decisions
                .iter()
                .rev()
                .take(3)
                .map(|d| d.summary.as_str())
                .collect();
            lines.push(format!("Decisions: {}", items.join(" | ")));
        }

        if !self.files_touched.is_empty() {
            let items: Vec<String> = self
                .files_touched
                .iter()
                .rev()
                .take(10)
                .map(|f| {
                    let status = if f.modified { "mod" } else { &f.last_mode };
                    let r = f.file_ref.as_deref().unwrap_or("?");
                    format!("[{r} {} {status}]", shorten_path(&f.path))
                })
                .collect();
            lines.push(format!(
                "Files ({}): {}",
                self.files_touched.len(),
                items.join(" ")
            ));
        }

        if let Some(ref tests) = self.test_results {
            lines.push(format!(
                "Tests: {}/{} pass ({})",
                tests.passed, tests.total, tests.command
            ));
        }

        if !self.next_steps.is_empty() {
            lines.push(format!("Next: {}", self.next_steps.join(" | ")));
        }

        lines.join("\n")
    }

    pub fn build_compaction_snapshot(&self) -> String {
        const MAX_SNAPSHOT_BYTES: usize = 2048;

        let mut sections: Vec<(u8, String)> = Vec::new();

        if let Some(ref task) = self.task {
            let pct = task
                .progress_pct
                .map_or(String::new(), |p| format!(" [{p}%]"));
            sections.push((1, format!("<task>{}{pct}</task>", task.description)));
        }

        if !self.files_touched.is_empty() {
            let modified: Vec<&str> = self
                .files_touched
                .iter()
                .filter(|f| f.modified)
                .map(|f| f.path.as_str())
                .collect();
            let read_only: Vec<&str> = self
                .files_touched
                .iter()
                .filter(|f| !f.modified)
                .take(10)
                .map(|f| f.path.as_str())
                .collect();
            let mut files_section = String::new();
            if !modified.is_empty() {
                files_section.push_str(&format!("Modified: {}", modified.join(", ")));
            }
            if !read_only.is_empty() {
                if !files_section.is_empty() {
                    files_section.push_str(" | ");
                }
                files_section.push_str(&format!("Read: {}", read_only.join(", ")));
            }
            sections.push((1, format!("<files>{files_section}</files>")));
        }

        if !self.decisions.is_empty() {
            let items: Vec<&str> = self.decisions.iter().map(|d| d.summary.as_str()).collect();
            sections.push((2, format!("<decisions>{}</decisions>", items.join(" | "))));
        }

        if !self.findings.is_empty() {
            let items: Vec<String> = self
                .findings
                .iter()
                .rev()
                .take(5)
                .map(|f| f.summary.clone())
                .collect();
            sections.push((2, format!("<findings>{}</findings>", items.join(" | "))));
        }

        if !self.progress.is_empty() {
            let items: Vec<String> = self
                .progress
                .iter()
                .rev()
                .take(5)
                .map(|p| {
                    let detail = p.detail.as_deref().unwrap_or("");
                    if detail.is_empty() {
                        p.action.clone()
                    } else {
                        format!("{}: {detail}", p.action)
                    }
                })
                .collect();
            sections.push((2, format!("<progress>{}</progress>", items.join(" | "))));
        }

        if let Some(ref tests) = self.test_results {
            sections.push((
                3,
                format!(
                    "<tests>{}/{} pass ({})</tests>",
                    tests.passed, tests.total, tests.command
                ),
            ));
        }

        if !self.next_steps.is_empty() {
            sections.push((
                3,
                format!("<next_steps>{}</next_steps>", self.next_steps.join(" | ")),
            ));
        }

        sections.push((
            4,
            format!(
                "<stats>calls={} saved={}tok</stats>",
                self.stats.total_tool_calls, self.stats.total_tokens_saved
            ),
        ));

        sections.sort_by_key(|(priority, _)| *priority);

        let mut snapshot = String::from("<session_snapshot>\n");
        for (_, section) in &sections {
            if snapshot.len() + section.len() + 25 > MAX_SNAPSHOT_BYTES {
                break;
            }
            snapshot.push_str(section);
            snapshot.push('\n');
        }
        snapshot.push_str("</session_snapshot>");
        snapshot
    }

    pub fn save_compaction_snapshot(&self) -> Result<String, String> {
        let snapshot = self.build_compaction_snapshot();
        let dir = sessions_dir().ok_or("cannot determine home directory")?;
        if !dir.exists() {
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        }
        let path = dir.join(format!("{}_snapshot.txt", self.id));
        std::fs::write(&path, &snapshot).map_err(|e| e.to_string())?;
        Ok(snapshot)
    }

    pub fn load_compaction_snapshot(session_id: &str) -> Option<String> {
        let dir = sessions_dir()?;
        let path = dir.join(format!("{session_id}_snapshot.txt"));
        std::fs::read_to_string(&path).ok()
    }

    pub fn load_latest_snapshot() -> Option<String> {
        let dir = sessions_dir()?;
        let mut snapshots: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(&dir)
            .ok()?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().to_string_lossy().ends_with("_snapshot.txt"))
            .filter_map(|e| {
                let meta = e.metadata().ok()?;
                let modified = meta.modified().ok()?;
                Some((modified, e.path()))
            })
            .collect();

        snapshots.sort_by_key(|x| std::cmp::Reverse(x.0));
        snapshots
            .first()
            .and_then(|(_, path)| std::fs::read_to_string(path).ok())
    }

    /// Build a compact resume block for post-compaction injection.
    /// Max ~500 tokens. Includes task, decisions, files, and archive references.
    pub fn build_resume_block(&self) -> String {
        let mut parts: Vec<String> = Vec::new();

        if let Some(ref root) = self.project_root {
            let short = root.rsplit('/').next().unwrap_or(root);
            parts.push(format!("Project: {short}"));
        }

        if let Some(ref task) = self.task {
            let pct = task
                .progress_pct
                .map_or(String::new(), |p| format!(" [{p}%]"));
            parts.push(format!("Task: {}{pct}", task.description));
        }

        if !self.decisions.is_empty() {
            let items: Vec<&str> = self
                .decisions
                .iter()
                .rev()
                .take(5)
                .map(|d| d.summary.as_str())
                .collect();
            parts.push(format!("Decisions: {}", items.join("; ")));
        }

        if !self.files_touched.is_empty() {
            let modified: Vec<&str> = self
                .files_touched
                .iter()
                .filter(|f| f.modified)
                .take(10)
                .map(|f| f.path.as_str())
                .collect();
            if !modified.is_empty() {
                parts.push(format!("Modified: {}", modified.join(", ")));
            }
        }

        if !self.next_steps.is_empty() {
            let steps: Vec<&str> = self.next_steps.iter().take(3).map(|s| s.as_str()).collect();
            parts.push(format!("Next: {}", steps.join("; ")));
        }

        let archives = super::archive::list_entries(Some(&self.id));
        if !archives.is_empty() {
            let hints: Vec<String> = archives
                .iter()
                .take(5)
                .map(|a| format!("{}({})", a.id, a.tool))
                .collect();
            parts.push(format!("Archives: {}", hints.join(", ")));
        }

        parts.push(format!(
            "Stats: {} calls, {} tok saved",
            self.stats.total_tool_calls, self.stats.total_tokens_saved
        ));

        format!(
            "--- SESSION RESUME (post-compaction) ---\n{}\n---",
            parts.join("\n")
        )
    }

    pub fn save(&mut self) -> Result<(), String> {
        let dir = sessions_dir().ok_or("cannot determine home directory")?;
        if !dir.exists() {
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        }

        let path = dir.join(format!("{}.json", self.id));
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;

        let tmp = dir.join(format!(".{}.json.tmp", self.id));
        std::fs::write(&tmp, &json).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;

        let pointer = LatestPointer {
            id: self.id.clone(),
        };
        let pointer_json = serde_json::to_string(&pointer).map_err(|e| e.to_string())?;
        let latest_path = dir.join("latest.json");
        let latest_tmp = dir.join(".latest.json.tmp");
        std::fs::write(&latest_tmp, &pointer_json).map_err(|e| e.to_string())?;
        std::fs::rename(&latest_tmp, &latest_path).map_err(|e| e.to_string())?;

        self.stats.unsaved_changes = 0;
        Ok(())
    }

    pub fn load_latest() -> Option<Self> {
        let dir = sessions_dir()?;
        let latest_path = dir.join("latest.json");
        let pointer_json = std::fs::read_to_string(&latest_path).ok()?;
        let pointer: LatestPointer = serde_json::from_str(&pointer_json).ok()?;
        Self::load_by_id(&pointer.id)
    }

    pub fn load_latest_for_project_root(project_root: &str) -> Option<Self> {
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

    pub fn load_by_id(id: &str) -> Option<Self> {
        let dir = sessions_dir()?;
        let path = dir.join(format!("{id}.json"));
        let json = std::fs::read_to_string(&path).ok()?;
        let session: Self = serde_json::from_str(&json).ok()?;
        Some(normalize_loaded_session(session))
    }

    pub fn list_sessions() -> Vec<SessionSummary> {
        let dir = match sessions_dir() {
            Some(d) => d,
            None => return Vec::new(),
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
                if let Ok(json) = std::fs::read_to_string(&path) {
                    if let Ok(session) = serde_json::from_str::<SessionState>(&json) {
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
        }

        summaries.sort_by_key(|x| std::cmp::Reverse(x.updated_at));
        summaries
    }

    pub fn cleanup_old_sessions(max_age_days: i64) -> u32 {
        let dir = match sessions_dir() {
            Some(d) => d,
            None => return 0,
        };

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
                if let Ok(json) = std::fs::read_to_string(&path) {
                    if let Ok(session) = serde_json::from_str::<SessionState>(&json) {
                        if session.updated_at < cutoff && std::fs::remove_file(&path).is_ok() {
                            removed += 1;
                        }
                    }
                }
            }
        }

        removed
    }
}

#[derive(Debug, Clone)]
pub struct SessionSummary {
    pub id: String,
    pub started_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub version: u32,
    pub task: Option<String>,
    pub tool_calls: u32,
    pub tokens_saved: u64,
}

fn sessions_dir() -> Option<PathBuf> {
    crate::core::data_dir::lean_ctx_data_dir()
        .ok()
        .map(|d| d.join("sessions"))
}

fn generate_session_id() -> String {
    static COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let now = Utc::now();
    let ts = now.format("%Y%m%d-%H%M%S").to_string();
    let nanos = now.timestamp_subsec_micros();
    let seq = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("{ts}-{nanos:06}s{seq}")
}

/// Extracts the `cd` target from a command string.
/// Handles patterns like `cd /foo`, `cd foo && bar`, `cd ../dir; cmd`, etc.
fn extract_cd_target(command: &str, base_cwd: &str) -> Option<String> {
    let first_cmd = command
        .split("&&")
        .next()
        .unwrap_or(command)
        .split(';')
        .next()
        .unwrap_or(command)
        .trim();

    if !first_cmd.starts_with("cd ") && first_cmd != "cd" {
        return None;
    }

    let target = first_cmd.strip_prefix("cd")?.trim();
    if target.is_empty() || target == "~" {
        return dirs::home_dir().map(|h| h.to_string_lossy().to_string());
    }

    let target = target.trim_matches('"').trim_matches('\'');
    let path = std::path::Path::new(target);

    if path.is_absolute() {
        Some(target.to_string())
    } else {
        let base = std::path::Path::new(base_cwd);
        let joined = base.join(target).to_string_lossy().to_string();
        Some(joined.replace('\\', "/"))
    }
}

fn shorten_path(path: &str) -> String {
    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() <= 2 {
        return path.to_string();
    }
    let last_two: Vec<&str> = parts.iter().rev().take(2).copied().collect();
    format!("…/{}/{}", last_two[1], last_two[0])
}

fn normalize_loaded_session(mut session: SessionState) -> SessionState {
    if matches!(session.project_root.as_deref(), Some(r) if r.trim().is_empty()) {
        session.project_root = None;
    }
    if matches!(session.shell_cwd.as_deref(), Some(c) if c.trim().is_empty()) {
        session.shell_cwd = None;
    }

    // Heal stale project_root caused by agent/temp working directories.
    // If project_root doesn't look like a real project root but shell_cwd does, prefer shell_cwd.
    if let (Some(ref root), Some(ref cwd)) = (&session.project_root, &session.shell_cwd) {
        let root_p = std::path::Path::new(root);
        let cwd_p = std::path::Path::new(cwd);
        let root_looks_real = has_project_marker(root_p);
        let cwd_looks_real = has_project_marker(cwd_p);

        if !root_looks_real && cwd_looks_real && is_agent_or_temp_dir(root_p) {
            session.project_root = Some(cwd.clone());
        }
    }

    session
}

fn session_matches_project_root(session: &SessionState, target_root: &std::path::Path) -> bool {
    if let Some(root) = session.project_root.as_deref() {
        let root_path =
            crate::core::pathutil::safe_canonicalize_or_self(std::path::Path::new(root));
        if root_path == target_root {
            return true;
        }
        if has_project_marker(&root_path) {
            return false;
        }
    }

    if let Some(cwd) = session.shell_cwd.as_deref() {
        let cwd_path = crate::core::pathutil::safe_canonicalize_or_self(std::path::Path::new(cwd));
        return cwd_path == target_root || cwd_path.starts_with(target_root);
    }

    false
}

fn has_project_marker(dir: &std::path::Path) -> bool {
    const MARKERS: &[&str] = &[
        ".git",
        ".lean-ctx.toml",
        "Cargo.toml",
        "package.json",
        "go.mod",
        "pyproject.toml",
        ".planning",
    ];
    MARKERS.iter().any(|m| dir.join(m).exists())
}

fn is_agent_or_temp_dir(dir: &std::path::Path) -> bool {
    let s = dir.to_string_lossy();
    s.contains("/.claude")
        || s.contains("/.codex")
        || s.contains("/var/folders/")
        || s.contains("/tmp/")
        || s.contains("\\.claude")
        || s.contains("\\.codex")
        || s.contains("\\AppData\\Local\\Temp")
        || s.contains("\\Temp\\")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_cd_absolute_path() {
        let result = extract_cd_target("cd /usr/local/bin", "/home/user");
        assert_eq!(result, Some("/usr/local/bin".to_string()));
    }

    #[test]
    fn extract_cd_relative_path() {
        let result = extract_cd_target("cd subdir", "/home/user");
        assert_eq!(result, Some("/home/user/subdir".to_string()));
    }

    #[test]
    fn extract_cd_with_chained_command() {
        let result = extract_cd_target("cd /tmp && ls", "/home/user");
        assert_eq!(result, Some("/tmp".to_string()));
    }

    #[test]
    fn extract_cd_with_semicolon() {
        let result = extract_cd_target("cd /tmp; ls", "/home/user");
        assert_eq!(result, Some("/tmp".to_string()));
    }

    #[test]
    fn extract_cd_parent_dir() {
        let result = extract_cd_target("cd ..", "/home/user/project");
        assert_eq!(result, Some("/home/user/project/..".to_string()));
    }

    #[test]
    fn extract_cd_no_cd_returns_none() {
        let result = extract_cd_target("ls -la", "/home/user");
        assert!(result.is_none());
    }

    #[test]
    fn extract_cd_bare_cd_goes_home() {
        let result = extract_cd_target("cd", "/home/user");
        assert!(result.is_some());
    }

    #[test]
    fn effective_cwd_explicit_takes_priority() {
        let mut session = SessionState::new();
        session.project_root = Some("/project".to_string());
        session.shell_cwd = Some("/project/src".to_string());
        assert_eq!(session.effective_cwd(Some("/explicit")), "/explicit");
    }

    #[test]
    fn effective_cwd_shell_cwd_second_priority() {
        let mut session = SessionState::new();
        session.project_root = Some("/project".to_string());
        session.shell_cwd = Some("/project/src".to_string());
        assert_eq!(session.effective_cwd(None), "/project/src");
    }

    #[test]
    fn effective_cwd_project_root_third_priority() {
        let mut session = SessionState::new();
        session.project_root = Some("/project".to_string());
        assert_eq!(session.effective_cwd(None), "/project");
    }

    #[test]
    fn effective_cwd_dot_ignored() {
        let mut session = SessionState::new();
        session.project_root = Some("/project".to_string());
        assert_eq!(session.effective_cwd(Some(".")), "/project");
    }

    #[test]
    fn compaction_snapshot_includes_task() {
        let mut session = SessionState::new();
        session.set_task("fix auth bug", None);
        let snapshot = session.build_compaction_snapshot();
        assert!(snapshot.contains("<task>fix auth bug</task>"));
        assert!(snapshot.contains("<session_snapshot>"));
        assert!(snapshot.contains("</session_snapshot>"));
    }

    #[test]
    fn compaction_snapshot_includes_files() {
        let mut session = SessionState::new();
        session.touch_file("src/auth.rs", None, "full", 500);
        session.files_touched[0].modified = true;
        session.touch_file("src/main.rs", None, "map", 100);
        let snapshot = session.build_compaction_snapshot();
        assert!(snapshot.contains("auth.rs"));
        assert!(snapshot.contains("<files>"));
    }

    #[test]
    fn compaction_snapshot_includes_decisions() {
        let mut session = SessionState::new();
        session.add_decision("Use JWT RS256", None);
        let snapshot = session.build_compaction_snapshot();
        assert!(snapshot.contains("JWT RS256"));
        assert!(snapshot.contains("<decisions>"));
    }

    #[test]
    fn compaction_snapshot_respects_size_limit() {
        let mut session = SessionState::new();
        session.set_task("a]task", None);
        for i in 0..100 {
            session.add_finding(
                Some(&format!("file{i}.rs")),
                Some(i),
                &format!("Finding number {i} with some detail text here"),
            );
        }
        let snapshot = session.build_compaction_snapshot();
        assert!(snapshot.len() <= 2200);
    }

    #[test]
    fn compaction_snapshot_includes_stats() {
        let mut session = SessionState::new();
        session.stats.total_tool_calls = 42;
        session.stats.total_tokens_saved = 10000;
        let snapshot = session.build_compaction_snapshot();
        assert!(snapshot.contains("calls=42"));
        assert!(snapshot.contains("saved=10000"));
    }
}
