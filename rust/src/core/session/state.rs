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
/// #717: max time an unsaved change may linger before it is flushed even
/// below the batch threshold — the dashboard reads session JSONs from disk,
/// so a slow trickle of tool calls must still become visible promptly.
pub(crate) const SESSION_FLUSH_INTERVAL: std::time::Duration = std::time::Duration::from_mins(1);

/// `TaskInfo.intent` of a task `auto_infer_task` built from touched-file
/// patterns. A label, never a stated task (#1590).
pub(crate) const INFERRED_TASK_INTENT: &str = "inferred";
/// `TaskInfo.intent` of a task `auto_infer_task` built from the working tree's
/// uncommitted files. Describes state, never a stated task.
pub(crate) const GIT_TASK_INTENT: &str = "git";

/// Whether a task intent marks an `auto_infer_task` label (re-derivable, never
/// a stated task). A plan title is a stated task and stays authoritative.
pub(crate) fn is_auto_task_intent(intent: Option<&str>) -> bool {
    matches!(intent, Some(INFERRED_TASK_INTENT | GIT_TASK_INTENT))
}

/// How many file names a git-derived task description lists before `+N more`.
const GIT_TASK_MAX_FILES: usize = 3;

/// Describe uncommitted files by name — `Uncommitted changes: entropy.rs,
/// call_graph.rs (+2 more)` — instead of a `git diff --stat` summary whose only
/// words were "files changed, insertions, deletions". `None` when clean.
pub(super) fn describe_uncommitted_files<'a>(
    paths: impl IntoIterator<Item = &'a str>,
) -> Option<String> {
    let names: Vec<&str> = paths
        .into_iter()
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(|p| p.rsplit('/').next().unwrap_or(p))
        .collect();
    if names.is_empty() {
        return None;
    }
    let listed = names[..names.len().min(GIT_TASK_MAX_FILES)].join(", ");
    let more = names.len().saturating_sub(GIT_TASK_MAX_FILES);
    Some(if more > 0 {
        format!("Uncommitted changes: {listed} (+{more} more)")
    } else {
        format!("Uncommitted changes: {listed}")
    })
}

/// Converts durable session signals into cross-session knowledge facts.
pub(crate) fn extract_session_facts(
    session: &SessionState,
) -> Vec<crate::core::knowledge::KnowledgeFact> {
    session
        .derived_facts()
        .into_iter()
        .map(|(fact, _)| fact)
        .collect()
}

impl SessionState {
    /// [`extract_session_facts`], each fact with the finding it is built from
    /// when it is built from exactly one: the `auto:blocker` facts. A store
    /// that keeps such a fact carries the finding's source binding over to it
    /// (`consolidation_engine::import_session_into`). A decision and a
    /// touched-file record have no source binding to carry: `None`.
    pub(crate) fn derived_facts(
        &self,
    ) -> Vec<(crate::core::knowledge::KnowledgeFact, Option<&Finding>)> {
        derive_session_facts(self)
    }
}

fn derive_session_facts(
    session: &SessionState,
) -> Vec<(crate::core::knowledge::KnowledgeFact, Option<&Finding>)> {
    let mut facts = Vec::new();

    facts.extend(session.decisions.iter().filter_map(|decision| {
        let summary = decision.summary.trim();
        if summary.is_empty() {
            return None;
        }
        let value = decision
            .rationale
            .as_deref()
            .filter(|rationale| !rationale.trim().is_empty())
            .map_or_else(
                || summary.to_owned(),
                |rationale| format!("{summary} — {rationale}"),
            );
        Some((
            auto_session_fact("auto:decision", value, session, decision.timestamp, 0.9),
            None,
        ))
    }));

    facts.extend(
        session
            .files_touched
            .iter()
            .filter(|file| file.modified)
            .map(|file| {
                let summary = file
                    .summary
                    .as_deref()
                    .filter(|summary| !summary.trim().is_empty())
                    .map_or_else(String::new, |summary| format!("; {summary}"));
                let fact = auto_session_fact(
                    "auto:pattern",
                    format!(
                        "Modified {} (mode: {}, reads: {}){summary}",
                        file.path, file.last_mode, file.read_count
                    ),
                    session,
                    session.updated_at,
                    0.8,
                );
                (fact, None)
            }),
    );

    facts.extend(session.findings.iter().filter_map(|finding| {
        let summary = finding.summary.trim();
        if summary.is_empty() || crate::core::auto_findings::is_activity_record(summary) {
            return None;
        }
        let normalized = summary.to_ascii_lowercase();
        let label = if ["resolved", "fixed", "solved", "unblocked", "passed"]
            .iter()
            .any(|keyword| normalized.contains(keyword))
        {
            "Resolution"
        } else if [
            "block", "fail", "error", "unable", "cannot", "missing", "timeout", "denied",
        ]
        .iter()
        .any(|keyword| normalized.contains(keyword))
        {
            "Blocker"
        } else {
            "Finding"
        };
        let location = finding.file.as_deref().map_or_else(String::new, |file| {
            finding
                .line
                .map_or_else(|| format!("{file}: "), |line| format!("{file}:{line}: "))
        });
        let fact = auto_session_fact(
            "auto:blocker",
            format!("{label}: {location}{summary}"),
            session,
            finding.timestamp,
            0.75,
        );
        Some((fact, Some(finding)))
    }));

    facts
}

fn auto_session_fact(
    category: &str,
    value: String,
    session: &SessionState,
    timestamp: chrono::DateTime<Utc>,
    confidence: f32,
) -> crate::core::knowledge::KnowledgeFact {
    let key = value.clone();
    crate::core::knowledge::KnowledgeFact {
        anchors: Vec::new(),
        review: None,
        capture: Default::default(),
        origin: crate::core::knowledge::FactOrigin::Unverified,
        category: category.to_owned(),
        key,
        value,
        source_session: session.id.clone(),
        confidence,
        created_at: timestamp,
        last_confirmed: timestamp,
        retrieval_count: 0,
        last_retrieved: None,
        valid_from: None,
        valid_until: None,
        supersedes: None,
        confirmation_count: 1,
        feedback_up: 0,
        feedback_down: 0,
        last_feedback: None,
        privacy: Default::default(),
        sensitivity: Default::default(),
        imported_from: None,
        archetype: Default::default(),
        fidelity: None,
        revision_count: 0,
        status: None,
        validity: None,
    }
}

impl Default for SessionState {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionState {
    /// Creates a new session with a unique ID and current timestamp.
    pub fn new() -> Self {
        let now = Utc::now();
        Self {
            canonical_checkpoint: None,
            save_gate: Default::default(),
            last_save_failed: false,
            id: generate_session_id(),
            version: 0,
            started_at: now,
            updated_at: now,
            project_root: None,
            shell_cwd: None,
            task: None,
            findings: Vec::new(),
            decisions: Vec::new(),
            handoff_context: Vec::new(),
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
            last_aaak_hash: None,
            extra_roots: Vec::new(),
            live_zone: super::types::LiveZoneSessionState::default(),
            playbook: super::playbook::Playbook::default(),
            last_semantic_query: None,
            last_flush: None,
        }
        .with_compression_from_config()
    }

    pub fn refresh_handoff_context(&mut self) {
        let Some(project_root) = self.project_root.clone() else {
            self.handoff_context.clear();
            return;
        };

        let session_id = self.id.clone();
        self.handoff_context = crate::core::knowledge::ProjectKnowledge::load(&project_root)
            .map(|knowledge| {
                // The handoff context is printed with its fact values: only
                // facts that may be delivered now are copied into it
                // (`ProjectKnowledge::delivery_filter`).
                let deliverable = knowledge.delivery_filter();
                knowledge
                    .recent_decisions(10)
                    .into_iter()
                    .filter(|fact| fact.source_session != session_id && deliverable(fact))
                    .collect()
            })
            .unwrap_or_default();
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

    /// Returns `true` if enough changes have accumulated to warrant a disk
    /// save — or, since #717, if any change has waited longer than
    /// `SESSION_FLUSH_INTERVAL`: the 5-change batch alone left slow
    /// sessions invisible to the dashboard (stuck "idle") for the whole
    /// batch window. A fresh in-memory session flushes its first change
    /// immediately so new activity surfaces at once.
    pub fn should_save(&self) -> bool {
        if self.stats.unsaved_changes == 0 {
            return false;
        }
        if self.stats.unsaved_changes >= BATCH_SAVE_INTERVAL {
            return true;
        }
        match self.last_flush {
            Some(t) => t.elapsed() >= SESSION_FLUSH_INTERVAL,
            None => true,
        }
    }

    /// Sets the active task and infers a structured intent from the description.
    pub fn set_task(&mut self, description: &str, intent: Option<&str>) {
        let (description_clean, _) =
            crate::core::secret_detection::scan_and_redact_from_config(description);
        // A canonical title edit continues the admitted task, not a new task ID.
        let progress_pct = if self.canonical_checkpoint.is_some() {
            self.task.as_ref().and_then(|task| task.progress_pct)
        } else {
            None
        };
        self.task = Some(TaskInfo {
            description: description_clean.clone(),
            intent: intent.map(std::string::ToString::to_string),
            progress_pct,
        });
        // An auto-inferred label is not a stated task: its words must not
        // become the structured intent that steers entropy/IB reads.
        if is_auto_task_intent(intent) {
            self.increment();
            return;
        }

        let touched: Vec<String> = self.files_touched.iter().map(|f| f.path.clone()).collect();
        let si = if touched.is_empty() {
            crate::core::intent_engine::StructuredIntent::from_query(&description_clean)
        } else {
            crate::core::intent_engine::StructuredIntent::from_query_with_session(
                &description_clean,
                &touched,
            )
        };
        if si.confidence >= 0.7 {
            self.active_structured_intent = Some(si);
        }

        self.increment();
    }

    /// Auto-infers the task from available context (plans, git diff, file patterns).
    /// Never overwrites a stated task; an earlier *auto-inferred* one is
    /// re-derived, so a label from a past session or a since-committed working
    /// tree does not linger as "the task" forever.
    pub fn auto_infer_task(&mut self) {
        if self
            .task
            .as_ref()
            .is_some_and(|t| !is_auto_task_intent(t.intent.as_deref()))
        {
            return;
        }
        let inferred = self.infer_task();
        let unchanged = match (&self.task, &inferred) {
            (Some(t), Some((desc, intent))) => {
                t.description == *desc && t.intent.as_deref() == Some(*intent)
            }
            (None, None) => true,
            _ => false,
        };
        if unchanged {
            return;
        }
        if let Some((desc, intent)) = inferred {
            self.set_task(&desc, Some(intent));
        } else {
            // Nothing to infer any more (plan done, tree committed): a stale
            // auto label must not outlive the state it described.
            self.task = None;
            self.increment();
        }
    }

    /// The first available auto source: active plan, uncommitted files,
    /// touched-file patterns. `(description, intent)`.
    fn infer_task(&self) -> Option<(String, &'static str)> {
        if let Some(task_from_plan) = Self::infer_task_from_plans() {
            return Some((task_from_plan, "plan"));
        }
        if let Some(ref root) = self.project_root
            && let Some(task_from_git) = Self::infer_task_from_git(root)
        {
            return Some((task_from_git, GIT_TASK_INTENT));
        }
        if self.files_touched.len() < 3 {
            return None;
        }
        let touched: Vec<String> = self.files_touched.iter().map(|f| f.path.clone()).collect();
        let intent = crate::core::intent_engine::StructuredIntent::from_file_patterns(&touched);
        if intent.confidence < 0.5 {
            return None;
        }
        let dirs: std::collections::BTreeSet<&str> = touched
            .iter()
            .filter_map(|f| std::path::Path::new(f).parent()?.to_str())
            .collect();
        let primary_dir = dirs.iter().next().unwrap_or(&".");
        Some((
            format!("Working on {} ({})", primary_dir, intent.task_type.as_str()),
            INFERRED_TASK_INTENT,
        ))
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
            .args(["diff", "--name-only", "--no-color"])
            .current_dir(project_root)
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        describe_uncommitted_files(String::from_utf8_lossy(&output.stdout).lines())
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
        self.stats.day_mut(Utc::now()).tool_calls += 1;
        if self.stats.client.is_none() {
            let client = crate::core::client_capabilities::current().client_id;
            if client != "unknown" {
                self.stats.client = Some(client);
            }
        }
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
        self.record_tool_receipt_with_task(
            tool,
            action,
            input_md5,
            output_md5,
            agent_id,
            client_name,
            None,
        );
    }

    /// Records a tool receipt with optional MCP task lineage.
    pub fn record_tool_receipt_with_task(
        &mut self,
        tool: &str,
        action: Option<&str>,
        input_md5: &str,
        output_md5: &str,
        agent_id: Option<&str>,
        client_name: Option<&str>,
        task_id: Option<&str>,
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
                task_id: task_id.map(std::string::ToString::to_string),
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
            task_id: None,
            timestamp: Utc::now(),
        });
        while self.evidence.len() > MAX_EVIDENCE {
            self.evidence.remove(0);
        }
        self.increment();
    }

    /// Returns `true` if an evidence record with the given key exists.
    pub fn has_evidence_key(&self, key: &str) -> bool {
        self.evidence.iter().any(|e| e.key == key)
    }

    /// Increments the session-level cache hit counter.
    pub fn record_cache_hit(&mut self) {
        self.stats.cache_hits += 1;
        self.stats.day_mut(Utc::now()).cache_hits += 1;
    }

    /// Increments the session-level command counter.
    pub fn record_command(&mut self) {
        self.stats.commands_run += 1;
    }

    /// Returns the effective working directory for shell commands.
    /// Priority: explicit cwd arg > session shell_cwd > project_root > process cwd.
    /// Explicit CWD and stored shell_cwd are jail-checked against the project root
    /// to prevent MCP clients from escaping the workspace.
    pub fn effective_cwd(&self, explicit_cwd: Option<&str>) -> String {
        self.effective_cwd_checked(explicit_cwd).0
    }

    /// Like [`Self::effective_cwd`], but also reports *why* an explicit `cwd`
    /// request was rejected by the project-root jail and silently replaced with
    /// the project root (#629).
    ///
    /// The jail itself is deliberate sandboxing (it stops MCP clients escaping
    /// the workspace) and must stay — the only gap was that the substitution was
    /// silent, so a caller running `pwd && ls` in what they think is dir A
    /// actually ran in the project root with no indication why. Callers that
    /// surface output to a human/agent (e.g. `ctx_shell`) use the returned
    /// `Option<String>` reason to append a one-line hint instead of letting the
    /// swap pass unnoticed; `effective_cwd` keeps the original lossless behaviour.
    pub fn effective_cwd_checked(&self, explicit_cwd: Option<&str>) -> (String, Option<String>) {
        let root = self.project_root.as_deref().unwrap_or(".");
        if let Some(cwd) = explicit_cwd
            && !cwd.is_empty()
            && cwd != "."
        {
            return Self::jail_cwd(cwd, root);
        }
        if let Some(ref cwd) = self.shell_cwd {
            return (cwd.clone(), None);
        }
        if let Some(ref r) = self.project_root {
            return (r.clone(), None);
        }
        (
            std::env::current_dir()
                .map_or_else(|_| ".".to_string(), |p| p.to_string_lossy().to_string()),
            None,
        )
    }

    /// The directory a command is to RUN in, or the reason there is none.
    ///
    /// Unlike [`Self::effective_cwd_checked`] nothing is substituted: an
    /// explicit `cwd` the project jail rejects is an error, so the command is
    /// not executed, backgrounded or cached under a directory the caller did
    /// not name. The stored shell cwd is held to the same jail — it was
    /// accepted when it was stored, but the project root may have changed
    /// since. Without a project root there is no jail to hold it to, and the
    /// stored value is used as before.
    pub fn execution_cwd(&self, explicit_cwd: Option<&str>) -> Result<String, String> {
        let jailed = |candidate: &str, root: &str| {
            crate::core::pathjail::jail_path(
                std::path::Path::new(candidate),
                std::path::Path::new(root),
            )
            .map(|path| path.to_string_lossy().to_string())
            .map_err(|reason| reason.to_string())
        };
        if let Some(cwd) = explicit_cwd
            && !cwd.is_empty()
            && cwd != "."
        {
            return jailed(cwd, self.project_root.as_deref().unwrap_or("."));
        }
        match (self.shell_cwd.as_deref(), self.project_root.as_deref()) {
            (Some(cwd), Some(root)) => jailed(cwd, root),
            (Some(cwd), None) => Ok(cwd.to_string()),
            (None, Some(root)) => Ok(root.to_string()),
            (None, None) => Ok(std::env::current_dir()
                .map_or_else(|_| ".".to_string(), |p| p.to_string_lossy().to_string())),
        }
    }

    /// Verifies that `candidate` is within the project jail.
    ///
    /// Falls back to `fallback_root` if the candidate escapes, returning the
    /// jail-rejection reason as the second tuple element so callers can surface
    /// it instead of silently substituting the root (#629). `None` means the
    /// candidate was accepted as-is.
    fn jail_cwd(candidate: &str, fallback_root: &str) -> (String, Option<String>) {
        let p = std::path::Path::new(candidate);
        match crate::core::pathjail::jail_path(p, std::path::Path::new(fallback_root)) {
            Ok(jailed) => (jailed.to_string_lossy().to_string(), None),
            Err(reason) => (fallback_root.to_string(), Some(reason.to_string())),
        }
    }

    /// Persist an explicit, jail-accepted `cwd` argument as the live shell
    /// cwd (#707). `update_shell_cwd` only tracks `cd` inside the command
    /// text, but clients that switch checkouts mid-session (Claude Code
    /// after `EnterWorktree`) pass the new directory as the `cwd` *param* of
    /// every subsequent call — without persisting it, the worktree-divergence
    /// detection in path resolution never sees the switch. Callers must pass
    /// a cwd that already passed the project-root jail.
    pub fn note_explicit_cwd(&mut self, cwd: &str) {
        let path = std::path::Path::new(cwd);
        if path.is_absolute() && path.is_dir() {
            self.shell_cwd = Some(
                crate::core::pathutil::safe_canonicalize_or_self(path)
                    .to_string_lossy()
                    .to_string(),
            );
        }
    }

    /// Updates shell_cwd by detecting `cd` in the command.
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
