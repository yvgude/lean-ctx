use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

const MAX_FINDINGS: usize = 20;
const MAX_DECISIONS: usize = 10;
const MAX_FILES: usize = 50;
#[allow(dead_code)]
const MAX_PROGRESS: usize = 30;
#[allow(dead_code)]
const MAX_NEXT_STEPS: usize = 10;
const BATCH_SAVE_INTERVAL: u32 = 5;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SessionState {
    pub id: String,
    pub version: u32,
    pub started_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub project_root: Option<String>,
    pub task: Option<TaskInfo>,
    pub findings: Vec<Finding>,
    pub decisions: Vec<Decision>,
    pub files_touched: Vec<FileTouched>,
    pub test_results: Option<TestSnapshot>,
    pub progress: Vec<ProgressEntry>,
    pub next_steps: Vec<String>,
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

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct SessionStats {
    pub total_tool_calls: u32,
    pub total_tokens_saved: u64,
    pub total_tokens_input: u64,
    pub cache_hits: u32,
    pub files_read: u32,
    pub commands_run: u32,
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
            task: None,
            findings: Vec::new(),
            decisions: Vec::new(),
            files_touched: Vec::new(),
            test_results: None,
            progress: Vec::new(),
            next_steps: Vec::new(),
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

    #[allow(dead_code)]
    pub fn set_test_results(&mut self, command: &str, passed: u32, failed: u32, total: u32) {
        self.test_results = Some(TestSnapshot {
            command: command.to_string(),
            passed,
            failed,
            total,
            timestamp: Utc::now(),
        });
        self.increment();
    }

    #[allow(dead_code)]
    pub fn add_progress(&mut self, action: &str, detail: Option<&str>) {
        self.progress.push(ProgressEntry {
            action: action.to_string(),
            detail: detail.map(|s| s.to_string()),
            timestamp: Utc::now(),
        });
        while self.progress.len() > MAX_PROGRESS {
            self.progress.remove(0);
        }
        self.increment();
    }

    pub fn record_tool_call(&mut self, tokens_saved: u64, tokens_input: u64) {
        self.stats.total_tool_calls += 1;
        self.stats.total_tokens_saved += tokens_saved;
        self.stats.total_tokens_input += tokens_input;
    }

    pub fn record_cache_hit(&mut self) {
        self.stats.cache_hits += 1;
    }

    pub fn record_command(&mut self) {
        self.stats.commands_run += 1;
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

    pub fn load_by_id(id: &str) -> Option<Self> {
        let dir = sessions_dir()?;
        let path = dir.join(format!("{id}.json"));
        let json = std::fs::read_to_string(&path).ok()?;
        serde_json::from_str(&json).ok()
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

        summaries.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
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
#[allow(dead_code)]
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
    dirs::home_dir().map(|h| h.join(".lean-ctx").join("sessions"))
}

fn generate_session_id() -> String {
    let now = Utc::now();
    let ts = now.format("%Y%m%d-%H%M%S").to_string();
    let random: u32 = (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos())
        % 10000;
    format!("{ts}-{random:04}")
}

fn shorten_path(path: &str) -> String {
    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() <= 2 {
        return path.to_string();
    }
    let last_two: Vec<&str> = parts.iter().rev().take(2).copied().collect();
    format!("…/{}/{}", last_two[1], last_two[0])
}
