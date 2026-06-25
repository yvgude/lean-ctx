use chrono::Utc;

use crate::core::intent_protocol::{IntentRecord, IntentSource};

use super::paths::{extract_cd_target, generate_session_id};
#[allow(clippy::wildcard_imports)]
use super::types::*;

const MAX_FINDINGS: usize = 20;
const MAX_DECISIONS: usize = 10;
const MAX_FILES: usize = 50;
const MAX_EVIDENCE: usize = 500;
pub(crate) const BATCH_SAVE_INTERVAL: u32 = 5;

impl Default for SessionState {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionState {
    /// Creates a new session with a unique ID and current timestamp.
    #[must_use]
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
            terse_mode: false,
            compression_level: String::new(),
            last_consolidate_ts: None,
            extra_roots: Vec::new(),
            wakeup_manifest: Vec::new(),
            playbook: super::playbook::Playbook::default(),
            last_semantic_query: None,
        }
        .with_compression_from_config()
    }

    fn with_compression_from_config(mut self) -> Self {
        let cfg = crate::core::config::Config::load();
        let level = crate::core::config::CompressionLevel::effective(&cfg);
        self.compression_level = level.label().to_string();
        self.terse_mode = level.is_active();
        self
    }

    /// Bumps the version counter and marks the session as dirty.
    pub fn increment(&mut self) {
        self.version += 1;
        self.updated_at = Utc::now();
        self.stats.unsaved_changes += 1;
    }

    /// Returns `true` if enough changes have accumulated to warrant a disk save.
    #[must_use]
    pub fn should_save(&self) -> bool {
        self.stats.unsaved_changes >= BATCH_SAVE_INTERVAL
    }

    /// Sets the active task and infers a structured intent from the description.
    pub fn set_task(&mut self, description: &str, intent: Option<&str>) {
        self.task = Some(TaskInfo {
            description: description.to_string(),
            intent: intent.map(std::string::ToString::to_string),
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

    /// Auto-infers the task from available context (plans, git diff, file patterns).
    /// Only sets if no explicit task is already set or it's stale.
    pub fn auto_infer_task(&mut self) {
        // Don't overwrite explicitly set tasks
        if self.task.is_some() {
            return;
        }

        // Source 1: Active .cursor/plans/*.plan.md
        if let Some(task_from_plan) = Self::infer_task_from_plans() {
            self.set_task(&task_from_plan, Some("plan"));
            return;
        }

        // Source 2: git diff summary
        if let Some(ref root) = self.project_root
            && let Some(task_from_git) = Self::infer_task_from_git(root)
        {
            self.set_task(&task_from_git, Some("git"));
            return;
        }

        // Source 3: File patterns from intent engine
        if self.files_touched.len() >= 3 {
            let touched: Vec<String> = self.files_touched.iter().map(|f| f.path.clone()).collect();
            let intent = crate::core::intent_engine::StructuredIntent::from_file_patterns(&touched);
            if intent.confidence >= 0.5 {
                let dirs: std::collections::HashSet<&str> = touched
                    .iter()
                    .filter_map(|f| std::path::Path::new(f).parent()?.to_str())
                    .collect();
                let primary_dir = dirs.iter().next().unwrap_or(&".");
                let desc = format!("Working on {} ({})", primary_dir, intent.task_type.as_str());
                self.set_task(&desc, Some("inferred"));
            }
        }
    }

    fn infer_task_from_plans() -> Option<String> {
        let plans_dir = std::path::Path::new(".cursor/plans");
        if !plans_dir.exists() {
            return None;
        }

        let mut newest: Option<(std::time::SystemTime, String)> = None;
        if let Ok(entries) = std::fs::read_dir(plans_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.to_string_lossy().ends_with(".plan.md") {
                    continue;
                }
                let mtime = entry.metadata().ok()?.modified().ok()?;
                let content = std::fs::read_to_string(&path).ok()?;

                // Check if plan has active (pending/in_progress) todos
                let has_active =
                    content.contains("status: pending") || content.contains("status: in_progress");
                if !has_active {
                    continue;
                }

                // Extract plan name from frontmatter
                let name = content
                    .lines()
                    .find(|l| l.starts_with("name:"))
                    .map_or("Unknown Plan", |l| {
                        l.trim_start_matches("name:").trim().trim_matches('"')
                    });

                let better = newest.as_ref().is_none_or(|(t, _)| mtime > *t);
                if better {
                    newest = Some((mtime, name.to_string()));
                }
            }
        }

        newest.map(|(_, name)| name)
    }

    fn infer_task_from_git(project_root: &str) -> Option<String> {
        let output = std::process::Command::new("git")
            .args(["diff", "--stat", "--no-color"])
            .current_dir(project_root)
            .output()
            .ok()?;

        if !output.status.success() {
            return None;
        }

        let stat = String::from_utf8_lossy(&output.stdout);
        let lines: Vec<&str> = stat.lines().collect();
        if lines.is_empty() {
            return None;
        }

        // Last line typically has "N files changed, M insertions(+), K deletions(-)"
        let summary_line = lines.last()?;
        if !summary_line.contains("changed") {
            return None;
        }

        // Find common directory prefix
        let file_lines: Vec<&str> = lines[..lines.len() - 1].to_vec();
        let dirs: std::collections::HashSet<&str> = file_lines
            .iter()
            .filter_map(|l| {
                let path = l.split('|').next()?.trim();
                std::path::Path::new(path).parent()?.to_str()
            })
            .collect();

        let primary = if dirs.len() == 1 {
            dirs.into_iter().next().unwrap_or(".")
        } else {
            "multiple dirs"
        };

        Some(format!("Modified: {} in {}", summary_line.trim(), primary))
    }

    /// Records a finding (discovery or observation) in the session log.
    pub fn add_finding(&mut self, file: Option<&str>, line: Option<u32>, summary: &str) {
        let (summary_clean, _) =
            crate::core::secret_detection::scan_and_redact_from_config(summary);
        self.findings.push(Finding {
            file: file.map(std::string::ToString::to_string),
            line,
            summary: summary_clean,
            timestamp: Utc::now(),
        });
        while self.findings.len() > MAX_FINDINGS {
            self.findings.remove(0);
        }
        self.increment();
    }

    /// Records a design or implementation decision with optional rationale.
    pub fn add_decision(&mut self, summary: &str, rationale: Option<&str>) {
        let (summary_clean, _) =
            crate::core::secret_detection::scan_and_redact_from_config(summary);
        let rationale_clean =
            rationale.map(|r| crate::core::secret_detection::scan_and_redact_from_config(r).0);
        self.decisions.push(Decision {
            summary: summary_clean,
            rationale: rationale_clean,
            timestamp: Utc::now(),
        });
        while self.decisions.len() > MAX_DECISIONS {
            self.decisions.remove(0);
        }
        self.increment();
    }

    /// Records a file read/access in the session, incrementing its read count.
    pub fn touch_file(&mut self, path: &str, file_ref: Option<&str>, mode: &str, tokens: usize) {
        if let Some(existing) = self.files_touched.iter_mut().find(|f| f.path == path) {
            existing.read_count += 1;
            existing.last_mode = mode.to_string();
            existing.tokens = tokens;
            if let Some(r) = file_ref {
                existing.file_ref = Some(r.to_string());
            }
        } else {
            let item_id = crate::core::context_field::ContextItemId::from_file(path);
            self.files_touched.push(FileTouched {
                path: path.to_string(),
                file_ref: file_ref.map(std::string::ToString::to_string),
                read_count: 1,
                modified: false,
                last_mode: mode.to_string(),
                tokens,
                stale: false,
                context_item_id: Some(item_id.to_string()),
                summary: None,
            });
            while self.files_touched.len() > MAX_FILES {
                self.files_touched.remove(0);
            }
        }
        self.stats.files_read += 1;
        self.increment();
    }

    /// Marks a previously touched file as modified (written to).
    pub fn mark_modified(&mut self, path: &str) {
        if let Some(existing) = self.files_touched.iter_mut().find(|f| f.path == path) {
            existing.modified = true;
        }
        self.increment();
    }

    /// Sets a one-line content summary for a touched file (max 80 chars).
    pub fn set_file_summary(&mut self, path: &str, summary: &str) {
        if let Some(existing) = self.files_touched.iter_mut().find(|f| f.path == path) {
            let truncated = if summary.len() > 80 {
                format!("{}…", &summary[..79])
            } else {
                summary.to_string()
            };
            existing.summary = Some(truncated);
        }
    }

    /// Increments the tool call counter and accumulates token savings.
    pub fn record_tool_call(&mut self, tokens_saved: u64, tokens_input: u64) {
        self.stats.total_tool_calls += 1;
        self.stats.total_tokens_saved += tokens_saved;
        self.stats.total_tokens_input += tokens_input;
    }

    /// Records an inferred or explicit intent, coalescing consecutive duplicates.
    pub fn record_intent(&mut self, mut intent: IntentRecord) {
        if intent.occurrences == 0 {
            intent.occurrences = 1;
        }

        if let Some(last) = self.intents.last_mut()
            && last.fingerprint() == intent.fingerprint()
        {
            last.occurrences = last.occurrences.saturating_add(intent.occurrences);
            last.timestamp = intent.timestamp;
            match intent.source {
                IntentSource::Inferred => self.stats.intents_inferred += 1,
                IntentSource::Explicit => self.stats.intents_explicit += 1,
            }
            self.increment();
            return;
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

    /// Appends an auditable evidence record for a tool invocation.
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
                agent_id: agent_id.map(std::string::ToString::to_string),
                client_name: client_name.map(std::string::ToString::to_string),
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

    /// Appends a manual (non-tool) evidence record to the audit log.
    pub fn record_manual_evidence(&mut self, key: &str, value: Option<&str>) {
        self.evidence.push(EvidenceRecord {
            kind: EvidenceKind::Manual,
            key: key.to_string(),
            value: value.map(std::string::ToString::to_string),
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

    /// Returns `true` if an evidence record with the given key exists.
    #[must_use]
    pub fn has_evidence_key(&self, key: &str) -> bool {
        self.evidence.iter().any(|e| e.key == key)
    }

    /// Increments the session-level cache hit counter.
    pub fn record_cache_hit(&mut self) {
        self.stats.cache_hits += 1;
    }

    /// Increments the session-level command counter.
    pub fn record_command(&mut self) {
        self.stats.commands_run += 1;
    }

    /// Returns the effective working directory for shell commands.
    /// Priority: explicit cwd arg > session `shell_cwd` > `project_root` > process cwd.
    /// Explicit CWD and stored `shell_cwd` are jail-checked against the project root
    /// to prevent MCP clients from escaping the workspace.
    #[must_use]
    pub fn effective_cwd(&self, explicit_cwd: Option<&str>) -> String {
        let root = self.project_root.as_deref().unwrap_or(".");
        if let Some(cwd) = explicit_cwd
            && !cwd.is_empty()
            && cwd != "."
        {
            return Self::jail_cwd(cwd, root);
        }
        if let Some(ref cwd) = self.shell_cwd {
            return cwd.clone();
        }
        if let Some(ref r) = self.project_root {
            return r.clone();
        }
        std::env::current_dir()
            .map_or_else(|_| ".".to_string(), |p| p.to_string_lossy().to_string())
    }

    /// Verifies that `candidate` is within the project jail.
    /// Falls back to `fallback_root` if the candidate escapes.
    fn jail_cwd(candidate: &str, fallback_root: &str) -> String {
        let p = std::path::Path::new(candidate);
        match crate::core::pathjail::jail_path(p, std::path::Path::new(fallback_root)) {
            Ok(jailed) => jailed.to_string_lossy().to_string(),
            Err(_) => fallback_root.to_string(),
        }
    }

    /// Updates `shell_cwd` by detecting `cd` in the command.
    /// Handles: `cd /abs/path`, `cd rel/path` (relative to current cwd),
    /// `cd ..`, and chained commands like `cd foo && ...`.
    /// The new CWD is jail-checked against the project root.
    pub fn update_shell_cwd(&mut self, command: &str) {
        let base = self.effective_cwd(None);
        if let Some(new_cwd) = extract_cd_target(command, &base) {
            let path = std::path::Path::new(&new_cwd);
            if path.exists() && path.is_dir() {
                let canonical = crate::core::pathutil::safe_canonicalize_or_self(path)
                    .to_string_lossy()
                    .to_string();
                let root = self.project_root.as_deref().unwrap_or(".");
                if crate::core::pathjail::jail_path(
                    std::path::Path::new(&canonical),
                    std::path::Path::new(root),
                )
                .is_ok()
                {
                    self.shell_cwd = Some(canonical);
                }
            }
        }
    }
}
