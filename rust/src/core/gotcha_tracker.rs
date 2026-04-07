use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;

const MAX_GOTCHAS: usize = 100;
const MAX_SESSION_LOGS: usize = 20;
const MAX_PENDING: usize = 10;
const PENDING_TIMEOUT_SECS: i64 = 900; // 15 minutes
const DECAY_ARCHIVE_THRESHOLD: f32 = 0.15;
const PROMOTION_CONFIDENCE: f32 = 0.9;
const PROMOTION_OCCURRENCES: u32 = 5;
const PROMOTION_SESSIONS: usize = 3;
const PROMOTION_PREVENTED: u32 = 2;

// ---------------------------------------------------------------------------
// Core types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum GotchaCategory {
    Build,
    Test,
    Config,
    Runtime,
    Dependency,
    Platform,
    Convention,
    Security,
}

impl GotchaCategory {
    pub fn from_str_loose(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "build" | "compile" => Self::Build,
            "test" => Self::Test,
            "config" | "configuration" => Self::Config,
            "runtime" => Self::Runtime,
            "dependency" | "dep" | "deps" => Self::Dependency,
            "platform" | "os" => Self::Platform,
            "convention" | "style" | "lint" => Self::Convention,
            "security" | "sec" => Self::Security,
            _ => Self::Convention,
        }
    }

    pub fn short_label(&self) -> &'static str {
        match self {
            Self::Build => "build",
            Self::Test => "test",
            Self::Config => "config",
            Self::Runtime => "runtime",
            Self::Dependency => "dep",
            Self::Platform => "platform",
            Self::Convention => "conv",
            Self::Security => "sec",
        }
    }
}

impl std::fmt::Display for GotchaCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.short_label())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum GotchaSeverity {
    Critical,
    Warning,
    Info,
}

impl GotchaSeverity {
    pub fn multiplier(&self) -> f32 {
        match self {
            Self::Critical => 1.5,
            Self::Warning => 1.0,
            Self::Info => 0.7,
        }
    }

    pub fn prefix(&self) -> &'static str {
        match self {
            Self::Critical => "!",
            Self::Warning => "!",
            Self::Info => "",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GotchaSource {
    AutoDetected { command: String, exit_code: i32 },
    AgentReported { session_id: String },
    CrossSessionCorrelated { sessions: Vec<String> },
    Promoted { from_knowledge_key: String },
}

impl GotchaSource {
    pub fn decay_rate(&self) -> f32 {
        match self {
            Self::Promoted { .. } => 0.01,
            Self::AgentReported { .. } => 0.02,
            Self::CrossSessionCorrelated { .. } => 0.03,
            Self::AutoDetected { .. } => 0.05,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Gotcha {
    pub id: String,
    pub category: GotchaCategory,
    pub severity: GotchaSeverity,
    pub trigger: String,
    pub resolution: String,
    pub file_patterns: Vec<String>,
    pub occurrences: u32,
    pub session_ids: Vec<String>,
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
    pub confidence: f32,
    pub source: GotchaSource,
    pub prevented_count: u32,
    pub tags: Vec<String>,
}

impl Gotcha {
    pub fn new(
        category: GotchaCategory,
        severity: GotchaSeverity,
        trigger: &str,
        resolution: &str,
        source: GotchaSource,
        session_id: &str,
    ) -> Self {
        let now = Utc::now();
        let confidence = match &source {
            GotchaSource::AgentReported { .. } => 0.9,
            GotchaSource::CrossSessionCorrelated { .. } => 0.85,
            GotchaSource::AutoDetected { .. } => 0.6,
            GotchaSource::Promoted { .. } => 0.95,
        };
        Self {
            id: gotcha_id(trigger, &category),
            category,
            severity,
            trigger: trigger.to_string(),
            resolution: resolution.to_string(),
            file_patterns: Vec::new(),
            occurrences: 1,
            session_ids: vec![session_id.to_string()],
            first_seen: now,
            last_seen: now,
            confidence,
            source,
            prevented_count: 0,
            tags: Vec::new(),
        }
    }

    pub fn merge_with(&mut self, other: &Gotcha) {
        self.occurrences += other.occurrences;
        for sid in &other.session_ids {
            if !self.session_ids.contains(sid) {
                self.session_ids.push(sid.clone());
            }
        }
        for fp in &other.file_patterns {
            if !self.file_patterns.contains(fp) {
                self.file_patterns.push(fp.clone());
            }
        }
        if other.last_seen > self.last_seen {
            self.last_seen = other.last_seen;
            self.resolution = other.resolution.clone();
        }
        self.confidence = self.confidence.max(other.confidence);
    }

    pub fn is_promotable(&self) -> bool {
        self.confidence >= PROMOTION_CONFIDENCE
            && self.occurrences >= PROMOTION_OCCURRENCES
            && self.session_ids.len() >= PROMOTION_SESSIONS
            && self.prevented_count >= PROMOTION_PREVENTED
    }
}

// ---------------------------------------------------------------------------
// Pending errors (in-memory, not persisted)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingError {
    pub error_signature: String,
    pub category: GotchaCategory,
    pub severity: GotchaSeverity,
    pub command: String,
    pub exit_code: i32,
    pub files_at_error: Vec<String>,
    pub timestamp: DateTime<Utc>,
    pub raw_snippet: String,
    pub session_id: String,
}

impl PendingError {
    pub fn is_expired(&self) -> bool {
        (Utc::now() - self.timestamp).num_seconds() > PENDING_TIMEOUT_SECS
    }
}

// ---------------------------------------------------------------------------
// Session error log (for cross-session correlation)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionErrorLog {
    pub session_id: String,
    pub timestamp: DateTime<Utc>,
    pub errors: Vec<ErrorEntry>,
    pub fixes: Vec<FixEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorEntry {
    pub signature: String,
    pub command: String,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FixEntry {
    pub error_signature: String,
    pub resolution: String,
    pub files_changed: Vec<String>,
    pub timestamp: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Stats
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GotchaStats {
    pub total_errors_detected: u64,
    pub total_fixes_correlated: u64,
    pub total_prevented: u64,
    pub gotchas_promoted: u64,
    pub gotchas_decayed: u64,
}

// ---------------------------------------------------------------------------
// GotchaStore
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GotchaStore {
    pub project_hash: String,
    pub gotchas: Vec<Gotcha>,
    #[serde(default)]
    pub error_log: Vec<SessionErrorLog>,
    #[serde(default)]
    pub stats: GotchaStats,
    pub updated_at: DateTime<Utc>,

    #[serde(skip)]
    pub pending_errors: Vec<PendingError>,
}

impl GotchaStore {
    pub fn new(project_hash: &str) -> Self {
        Self {
            project_hash: project_hash.to_string(),
            gotchas: Vec::new(),
            error_log: Vec::new(),
            stats: GotchaStats::default(),
            updated_at: Utc::now(),
            pending_errors: Vec::new(),
        }
    }

    pub fn load(project_root: &str) -> Self {
        let hash = hash_project(project_root);
        let path = gotcha_path(&hash);
        if let Ok(content) = std::fs::read_to_string(&path) {
            if let Ok(mut store) = serde_json::from_str::<GotchaStore>(&content) {
                store.apply_decay();
                store.pending_errors = Vec::new();
                return store;
            }
        }
        Self::new(&hash)
    }

    pub fn save(&self, project_root: &str) -> Result<(), String> {
        let hash = hash_project(project_root);
        let path = gotcha_path(&hash);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let tmp = path.with_extension("tmp");
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(&tmp, &json).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
        Ok(())
    }

    // -- Detection ----------------------------------------------------------

    pub fn detect_error(
        &mut self,
        output: &str,
        command: &str,
        exit_code: i32,
        files_touched: &[String],
        session_id: &str,
    ) -> bool {
        self.pending_errors.retain(|p| !p.is_expired());

        let Some(detected) = detect_error_pattern(output, command, exit_code) else {
            return false;
        };

        let signature = normalize_error_signature(&detected.raw_message);
        let snippet = output.chars().take(500).collect::<String>();

        self.pending_errors.push(PendingError {
            error_signature: signature.clone(),
            category: detected.category,
            severity: detected.severity,
            command: command.to_string(),
            exit_code,
            files_at_error: files_touched.to_vec(),
            timestamp: Utc::now(),
            raw_snippet: snippet,
            session_id: session_id.to_string(),
        });

        if self.pending_errors.len() > MAX_PENDING {
            self.pending_errors.remove(0);
        }

        self.log_error(session_id, &signature, command);
        self.stats.total_errors_detected += 1;
        true
    }

    pub fn try_resolve_pending(
        &mut self,
        command: &str,
        files_touched: &[String],
        session_id: &str,
    ) -> Option<Gotcha> {
        self.pending_errors.retain(|p| !p.is_expired());

        let cmd_base = command_base(command);
        let idx = self
            .pending_errors
            .iter()
            .position(|p| command_base(&p.command) == cmd_base)?;

        let pending = self.pending_errors.remove(idx);

        let changed_files: Vec<String> = files_touched
            .iter()
            .filter(|f| !pending.files_at_error.contains(f))
            .cloned()
            .collect();

        let resolution = if changed_files.is_empty() {
            format!("Fixed after re-running {}", cmd_base)
        } else {
            format!("Fixed by editing: {}", changed_files.join(", "))
        };

        let mut gotcha = Gotcha::new(
            pending.category,
            pending.severity,
            &pending.error_signature,
            &resolution,
            GotchaSource::AutoDetected {
                command: command.to_string(),
                exit_code: pending.exit_code,
            },
            session_id,
        );
        gotcha.file_patterns = changed_files.clone();

        self.add_or_merge(gotcha.clone());
        self.log_fix(
            session_id,
            &pending.error_signature,
            &resolution,
            &changed_files,
        );
        self.stats.total_fixes_correlated += 1;
        self.updated_at = Utc::now();

        Some(gotcha)
    }

    // -- Agent-reported -----------------------------------------------------

    pub fn report_gotcha(
        &mut self,
        trigger: &str,
        resolution: &str,
        category: &str,
        severity: &str,
        session_id: &str,
    ) -> Option<&Gotcha> {
        let cat = GotchaCategory::from_str_loose(category);
        let sev = match severity.to_lowercase().as_str() {
            "critical" => GotchaSeverity::Critical,
            "info" => GotchaSeverity::Info,
            _ => GotchaSeverity::Warning,
        };
        let id = gotcha_id(trigger, &cat);
        let gotcha = Gotcha::new(
            cat,
            sev,
            trigger,
            resolution,
            GotchaSource::AgentReported {
                session_id: session_id.to_string(),
            },
            session_id,
        );
        self.add_or_merge(gotcha);
        self.updated_at = Utc::now();
        self.gotchas.iter().find(|g| g.id == id)
    }

    // -- Add / Merge --------------------------------------------------------

    fn add_or_merge(&mut self, new: Gotcha) {
        if let Some(existing) = self.gotchas.iter_mut().find(|g| g.id == new.id) {
            existing.merge_with(&new);
        } else {
            self.gotchas.push(new);
            if self.gotchas.len() > MAX_GOTCHAS {
                self.gotchas.sort_by(|a, b| {
                    b.confidence
                        .partial_cmp(&a.confidence)
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
                self.gotchas.truncate(MAX_GOTCHAS);
            }
        }
    }

    // -- Cross-Session ------------------------------------------------------

    fn log_error(&mut self, session_id: &str, signature: &str, command: &str) {
        let log = self.get_or_create_session_log(session_id);
        log.errors.push(ErrorEntry {
            signature: signature.to_string(),
            command: command.to_string(),
            timestamp: Utc::now(),
        });
    }

    fn log_fix(&mut self, session_id: &str, error_sig: &str, resolution: &str, files: &[String]) {
        let log = self.get_or_create_session_log(session_id);
        log.fixes.push(FixEntry {
            error_signature: error_sig.to_string(),
            resolution: resolution.to_string(),
            files_changed: files.to_vec(),
            timestamp: Utc::now(),
        });
    }

    fn get_or_create_session_log(&mut self, session_id: &str) -> &mut SessionErrorLog {
        if !self.error_log.iter().any(|l| l.session_id == session_id) {
            self.error_log.push(SessionErrorLog {
                session_id: session_id.to_string(),
                timestamp: Utc::now(),
                errors: Vec::new(),
                fixes: Vec::new(),
            });
            if self.error_log.len() > MAX_SESSION_LOGS {
                self.error_log.remove(0);
            }
        }
        self.error_log
            .iter_mut()
            .find(|l| l.session_id == session_id)
            .expect("session log must exist after push")
    }

    pub fn cross_session_boost(&mut self) {
        let mut sig_sessions: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();

        for log in &self.error_log {
            for err in &log.errors {
                sig_sessions
                    .entry(err.signature.clone())
                    .or_default()
                    .push(log.session_id.clone());
            }
        }

        for gotcha in &mut self.gotchas {
            if let Some(sessions) = sig_sessions.get(&gotcha.trigger) {
                let unique: Vec<String> = sessions
                    .iter()
                    .filter(|s| !gotcha.session_ids.contains(s))
                    .cloned()
                    .collect();
                if !unique.is_empty() {
                    let boost = 0.15 * unique.len() as f32;
                    gotcha.confidence = (gotcha.confidence + boost).min(0.95);
                    for s in unique {
                        gotcha.session_ids.push(s);
                    }
                    gotcha.source = GotchaSource::CrossSessionCorrelated {
                        sessions: gotcha.session_ids.clone(),
                    };
                }
            }
        }
    }

    // -- Decay --------------------------------------------------------------

    pub fn apply_decay(&mut self) {
        let now = Utc::now();
        let mut decayed = 0u64;

        for gotcha in &mut self.gotchas {
            let days_since = (now - gotcha.last_seen).num_days().max(0) as f32;
            if days_since < 1.0 {
                continue;
            }
            let base_rate = gotcha.source.decay_rate();
            let occurrence_factor = 1.0 / (1.0 + gotcha.occurrences as f32 * 0.1);
            let decay = base_rate * occurrence_factor * (days_since / 7.0);
            gotcha.confidence = (gotcha.confidence - decay).max(0.0);
        }

        let before = self.gotchas.len();
        self.gotchas
            .retain(|g| g.confidence >= DECAY_ARCHIVE_THRESHOLD);
        decayed += (before - self.gotchas.len()) as u64;

        self.stats.gotchas_decayed += decayed;
    }

    // -- Promotion ----------------------------------------------------------

    pub fn check_promotions(&mut self) -> Vec<(String, String, String, f32)> {
        let mut promoted = Vec::new();
        for gotcha in &self.gotchas {
            if gotcha.is_promotable() {
                promoted.push((
                    gotcha.category.to_string(),
                    gotcha.trigger.clone(),
                    gotcha.resolution.clone(),
                    gotcha.confidence,
                ));
            }
        }
        self.stats.gotchas_promoted += promoted.len() as u64;
        promoted
    }

    // -- Universal Gotchas --------------------------------------------------

    pub fn extract_universal(&self) -> Vec<Gotcha> {
        self.gotchas
            .iter()
            .filter(|g| {
                g.category == GotchaCategory::Platform
                    && g.occurrences >= 10
                    && g.session_ids.len() >= 5
            })
            .cloned()
            .collect()
    }

    // -- Relevance scoring --------------------------------------------------

    pub fn top_relevant(&self, files_touched: &[String], limit: usize) -> Vec<&Gotcha> {
        let mut scored: Vec<(&Gotcha, f32)> = self
            .gotchas
            .iter()
            .map(|g| (g, relevance_score(g, files_touched)))
            .filter(|(_, s)| *s > 0.5)
            .collect();

        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.into_iter().take(limit).map(|(g, _)| g).collect()
    }

    pub fn format_injection_block(&self, files_touched: &[String]) -> String {
        let relevant = self.top_relevant(files_touched, 7);
        if relevant.is_empty() {
            return String::new();
        }

        let mut lines = Vec::with_capacity(relevant.len() + 2);
        lines.push("--- PROJECT GOTCHAS (do NOT repeat these mistakes) ---".to_string());

        for g in &relevant {
            let prefix = g.severity.prefix();
            let label = g.category.short_label();
            let sessions = g.session_ids.len();
            let age = format_age(g.last_seen);

            let source_hint = match &g.source {
                GotchaSource::AgentReported { .. } => ", agent-confirmed".to_string(),
                GotchaSource::CrossSessionCorrelated { .. } => {
                    format!(", across {} sessions", sessions)
                }
                GotchaSource::AutoDetected { .. } => ", auto-detected".to_string(),
                GotchaSource::Promoted { .. } => ", proven".to_string(),
            };

            let prevented = if g.prevented_count > 0 {
                format!(", prevented {}x", g.prevented_count)
            } else {
                String::new()
            };

            lines.push(format!("[{prefix}{label}] {}", g.trigger));
            lines.push(format!(
                "  FIX: {} (seen {}x{}{}, {})",
                g.resolution, g.occurrences, source_hint, prevented, age
            ));
        }

        lines.push("---".to_string());
        lines.join("\n")
    }

    // -- Prevention tracking ------------------------------------------------

    pub fn mark_prevented(&mut self, gotcha_id: &str) {
        if let Some(g) = self.gotchas.iter_mut().find(|g| g.id == gotcha_id) {
            g.prevented_count += 1;
            g.confidence = (g.confidence + 0.05).min(0.99);
            self.stats.total_prevented += 1;
        }
    }

    // -- CLI ----------------------------------------------------------------

    pub fn format_list(&self) -> String {
        if self.gotchas.is_empty() {
            return "No gotchas recorded for this project.".to_string();
        }

        let mut out = Vec::new();
        out.push(format!("  {} active gotchas\n", self.gotchas.len()));

        let mut sorted = self.gotchas.clone();
        sorted.sort_by(|a, b| {
            b.confidence
                .partial_cmp(&a.confidence)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        for g in &sorted {
            let prefix = g.severity.prefix();
            let label = g.category.short_label();
            let conf = (g.confidence * 100.0) as u32;
            let source = match &g.source {
                GotchaSource::AutoDetected { .. } => "auto",
                GotchaSource::AgentReported { .. } => "agent",
                GotchaSource::CrossSessionCorrelated { .. } => "cross-session",
                GotchaSource::Promoted { .. } => "promoted",
            };
            out.push(format!(
                "  [{prefix}{label:8}] {} ({}x, {} sessions, {source}, confidence: {conf}%)",
                truncate_str(&g.trigger, 60),
                g.occurrences,
                g.session_ids.len(),
            ));
            out.push(format!(
                "             FIX: {}",
                truncate_str(&g.resolution, 70)
            ));
            if g.prevented_count > 0 {
                out.push(format!("             Prevented: {}x", g.prevented_count));
            }
            out.push(String::new());
        }

        out.push(format!(
            "  Stats: {} errors detected | {} fixes correlated | {} prevented",
            self.stats.total_errors_detected,
            self.stats.total_fixes_correlated,
            self.stats.total_prevented,
        ));

        out.join("\n")
    }

    pub fn clear(&mut self) {
        self.gotchas.clear();
        self.pending_errors.clear();
        self.updated_at = Utc::now();
    }
}

// ---------------------------------------------------------------------------
// Error pattern detection
// ---------------------------------------------------------------------------

pub struct DetectedError {
    pub category: GotchaCategory,
    pub severity: GotchaSeverity,
    pub raw_message: String,
}

pub fn detect_error_pattern(output: &str, command: &str, exit_code: i32) -> Option<DetectedError> {
    let cmd_lower = command.to_lowercase();
    let out_lower = output.to_lowercase();

    // Rust / Cargo
    if cmd_lower.starts_with("cargo ") || cmd_lower.contains("rustc") {
        if let Some(msg) = extract_pattern(output, r"error\[E\d{4}\]: .+") {
            return Some(DetectedError {
                category: GotchaCategory::Build,
                severity: GotchaSeverity::Critical,
                raw_message: msg,
            });
        }
        if out_lower.contains("cannot find") || out_lower.contains("mismatched types") {
            return Some(DetectedError {
                category: GotchaCategory::Build,
                severity: GotchaSeverity::Critical,
                raw_message: extract_first_error_line(output),
            });
        }
        if out_lower.contains("test result: failed") || out_lower.contains("failures:") {
            return Some(DetectedError {
                category: GotchaCategory::Test,
                severity: GotchaSeverity::Critical,
                raw_message: extract_first_error_line(output),
            });
        }
    }

    // npm / pnpm / yarn
    if (cmd_lower.starts_with("npm ")
        || cmd_lower.starts_with("pnpm ")
        || cmd_lower.starts_with("yarn "))
        && (out_lower.contains("err!") || out_lower.contains("eresolve"))
    {
        return Some(DetectedError {
            category: GotchaCategory::Dependency,
            severity: GotchaSeverity::Critical,
            raw_message: extract_first_error_line(output),
        });
    }

    // Node.js
    if cmd_lower.starts_with("node ") || cmd_lower.contains("tsx ") || cmd_lower.contains("ts-node")
    {
        for pat in &[
            "syntaxerror",
            "typeerror",
            "referenceerror",
            "cannot find module",
        ] {
            if out_lower.contains(pat) {
                return Some(DetectedError {
                    category: GotchaCategory::Runtime,
                    severity: GotchaSeverity::Critical,
                    raw_message: extract_first_error_line(output),
                });
            }
        }
    }

    // Python
    if (cmd_lower.starts_with("python")
        || cmd_lower.starts_with("pip ")
        || cmd_lower.starts_with("uv "))
        && (out_lower.contains("traceback")
            || out_lower.contains("importerror")
            || out_lower.contains("modulenotfounderror"))
    {
        return Some(DetectedError {
            category: GotchaCategory::Runtime,
            severity: GotchaSeverity::Critical,
            raw_message: extract_first_error_line(output),
        });
    }

    // Go
    if cmd_lower.starts_with("go ")
        && (out_lower.contains("cannot use") || out_lower.contains("undefined:"))
    {
        return Some(DetectedError {
            category: GotchaCategory::Build,
            severity: GotchaSeverity::Critical,
            raw_message: extract_first_error_line(output),
        });
    }

    // TypeScript / tsc
    if cmd_lower.contains("tsc") || cmd_lower.contains("typescript") {
        if let Some(msg) = extract_pattern(output, r"TS\d{4}: .+") {
            return Some(DetectedError {
                category: GotchaCategory::Build,
                severity: GotchaSeverity::Critical,
                raw_message: msg,
            });
        }
    }

    // Docker
    if cmd_lower.starts_with("docker ")
        && out_lower.contains("error")
        && (out_lower.contains("failed to") || out_lower.contains("copy failed"))
    {
        return Some(DetectedError {
            category: GotchaCategory::Build,
            severity: GotchaSeverity::Critical,
            raw_message: extract_first_error_line(output),
        });
    }

    // Git
    if cmd_lower.starts_with("git ")
        && (out_lower.contains("conflict")
            || out_lower.contains("rejected")
            || out_lower.contains("diverged"))
    {
        return Some(DetectedError {
            category: GotchaCategory::Config,
            severity: GotchaSeverity::Warning,
            raw_message: extract_first_error_line(output),
        });
    }

    // pytest
    if cmd_lower.contains("pytest") && (out_lower.contains("failed") || out_lower.contains("error"))
    {
        return Some(DetectedError {
            category: GotchaCategory::Test,
            severity: GotchaSeverity::Critical,
            raw_message: extract_first_error_line(output),
        });
    }

    // Jest / Vitest
    if (cmd_lower.contains("jest") || cmd_lower.contains("vitest"))
        && (out_lower.contains("fail") || out_lower.contains("typeerror"))
    {
        return Some(DetectedError {
            category: GotchaCategory::Test,
            severity: GotchaSeverity::Critical,
            raw_message: extract_first_error_line(output),
        });
    }

    // Make / CMake
    if (cmd_lower.starts_with("make") || cmd_lower.contains("cmake"))
        && out_lower.contains("error")
        && (out_lower.contains("undefined reference") || out_lower.contains("no rule"))
    {
        return Some(DetectedError {
            category: GotchaCategory::Build,
            severity: GotchaSeverity::Critical,
            raw_message: extract_first_error_line(output),
        });
    }

    // Generic: non-zero exit + substantial stderr
    if exit_code != 0
        && output.len() > 50
        && (out_lower.contains("error")
            || out_lower.contains("fatal")
            || out_lower.contains("failed"))
    {
        return Some(DetectedError {
            category: GotchaCategory::Runtime,
            severity: GotchaSeverity::Warning,
            raw_message: extract_first_error_line(output),
        });
    }

    None
}

// ---------------------------------------------------------------------------
// Signature normalization
// ---------------------------------------------------------------------------

pub fn normalize_error_signature(raw: &str) -> String {
    let mut sig = raw.to_string();

    // Remove absolute paths: /Users/foo/bar/src/main.rs -> src/main.rs
    sig = regex_replace(&sig, r"(/[A-Za-z][\w.-]*/)+", "");

    // Remove Windows paths: C:\Users\foo\bar\ -> ""
    sig = regex_replace(&sig, r"[A-Z]:\\[\w\\.-]+\\", "");

    // Remove line:col numbers: :42:13 -> :_:_
    sig = regex_replace(&sig, r":\d+:\d+", ":_:_");
    sig = regex_replace(&sig, r"line \d+", "line _");

    // Collapse whitespace
    sig = regex_replace(&sig, r"\s+", " ");

    // Truncate
    if sig.len() > 200 {
        sig.truncate(200);
    }

    sig.trim().to_string()
}

// ---------------------------------------------------------------------------
// Relevance scoring
// ---------------------------------------------------------------------------

pub fn relevance_score(gotcha: &Gotcha, files_touched: &[String]) -> f32 {
    let mut score: f32 = 0.0;

    // 1. Basis: occurrences * confidence (max 10)
    score += (gotcha.occurrences as f32 * gotcha.confidence).min(10.0);

    // 2. Recency boost (exponential decay, half-life 1 week)
    let hours_ago = (Utc::now() - gotcha.last_seen).num_hours().max(0) as f32;
    score += 5.0 * (-hours_ago / 168.0).exp();

    // 3. File overlap
    let overlap = gotcha
        .file_patterns
        .iter()
        .filter(|fp| {
            files_touched
                .iter()
                .any(|ft| ft.contains(fp.as_str()) || fp.contains(ft.as_str()))
        })
        .count();
    score += overlap as f32 * 3.0;

    // 4. Severity multiplier
    score *= gotcha.severity.multiplier();

    // 5. Cross-session bonus
    if gotcha.session_ids.len() >= 3 {
        score *= 1.3;
    }

    // 6. Prevention track record
    if gotcha.prevented_count > 0 {
        score *= 1.2;
    }

    score
}

// ---------------------------------------------------------------------------
// Universal gotchas (cross-project)
// ---------------------------------------------------------------------------

pub fn load_universal_gotchas() -> Vec<Gotcha> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    let path = home.join(".lean-ctx").join("universal-gotchas.json");
    if let Ok(content) = std::fs::read_to_string(&path) {
        serde_json::from_str(&content).unwrap_or_default()
    } else {
        Vec::new()
    }
}

pub fn save_universal_gotchas(gotchas: &[Gotcha]) -> Result<(), String> {
    let Some(home) = dirs::home_dir() else {
        return Err("Cannot determine home directory".into());
    };
    let path = home.join(".lean-ctx").join("universal-gotchas.json");
    let tmp = path.with_extension("tmp");
    let json = serde_json::to_string_pretty(gotchas).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, &json).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn gotcha_path(project_hash: &str) -> PathBuf {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    home.join(".lean-ctx")
        .join("knowledge")
        .join(project_hash)
        .join("gotchas.json")
}

fn hash_project(root: &str) -> String {
    let mut hasher = DefaultHasher::new();
    root.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn gotcha_id(trigger: &str, category: &GotchaCategory) -> String {
    let mut hasher = DefaultHasher::new();
    trigger.hash(&mut hasher);
    category.short_label().hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn command_base(cmd: &str) -> String {
    let parts: Vec<&str> = cmd.split_whitespace().collect();
    if parts.len() >= 2 {
        format!("{} {}", parts[0], parts[1])
    } else {
        parts.first().unwrap_or(&"").to_string()
    }
}

fn extract_pattern(text: &str, pattern: &str) -> Option<String> {
    let re = regex::Regex::new(pattern).ok()?;
    re.find(text).map(|m| m.as_str().to_string())
}

fn extract_first_error_line(output: &str) -> String {
    for line in output.lines() {
        let ll = line.to_lowercase();
        if ll.contains("error") || ll.contains("failed") || ll.contains("traceback") {
            let trimmed = line.trim();
            if trimmed.len() > 200 {
                return trimmed[..200].to_string();
            }
            return trimmed.to_string();
        }
    }
    output.lines().next().unwrap_or("unknown error").to_string()
}

fn regex_replace(text: &str, pattern: &str, replacement: &str) -> String {
    match regex::Regex::new(pattern) {
        Ok(re) => re.replace_all(text, replacement).to_string(),
        Err(_) => text.to_string(),
    }
}

fn format_age(dt: DateTime<Utc>) -> String {
    let diff = Utc::now() - dt;
    let hours = diff.num_hours();
    if hours < 1 {
        format!("{}m ago", diff.num_minutes().max(1))
    } else if hours < 24 {
        format!("{}h ago", hours)
    } else {
        format!("{}d ago", diff.num_days())
    }
}

fn truncate_str(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max.saturating_sub(3)])
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_cargo_error() {
        let output = r#"error[E0507]: cannot move out of `self.field` which is behind a shared reference
   --> src/server.rs:42:13"#;
        let result = detect_error_pattern(output, "cargo build", 1);
        assert!(result.is_some());
        let d = result.unwrap();
        assert_eq!(d.category, GotchaCategory::Build);
        assert_eq!(d.severity, GotchaSeverity::Critical);
        assert!(d.raw_message.contains("E0507"));
    }

    #[test]
    fn detect_npm_error() {
        let output = "npm ERR! ERESOLVE unable to resolve dependency tree";
        let result = detect_error_pattern(output, "npm install", 1);
        assert!(result.is_some());
        assert_eq!(result.unwrap().category, GotchaCategory::Dependency);
    }

    #[test]
    fn detect_python_traceback() {
        let output = "Traceback (most recent call last):\n  File \"app.py\", line 5\nImportError: No module named 'flask'";
        let result = detect_error_pattern(output, "python app.py", 1);
        assert!(result.is_some());
        assert_eq!(result.unwrap().category, GotchaCategory::Runtime);
    }

    #[test]
    fn detect_typescript_error() {
        let output =
            "src/index.ts(10,5): error TS2339: Property 'foo' does not exist on type 'Bar'.";
        let result = detect_error_pattern(output, "npx tsc", 1);
        assert!(result.is_some());
        assert_eq!(result.unwrap().category, GotchaCategory::Build);
    }

    #[test]
    fn detect_go_error() {
        let output = "./main.go:15:2: undefined: SomeFunc";
        let result = detect_error_pattern(output, "go build", 1);
        assert!(result.is_some());
    }

    #[test]
    fn detect_jest_failure() {
        let output = "FAIL src/app.test.ts\n  TypeError: Cannot read properties of undefined";
        let result = detect_error_pattern(output, "npx jest", 1);
        assert!(result.is_some());
        assert_eq!(result.unwrap().category, GotchaCategory::Test);
    }

    #[test]
    fn no_false_positive_on_success() {
        let output = "Compiling lean-ctx v2.17.2\nFinished release target(s) in 30s";
        let result = detect_error_pattern(output, "cargo build --release", 0);
        assert!(result.is_none());
    }

    #[test]
    fn normalize_signature_strips_paths() {
        let raw = "error[E0507]: cannot move out of /Users/foo/project/src/main.rs:42:13";
        let sig = normalize_error_signature(raw);
        assert!(!sig.contains("/Users/foo"));
        assert!(sig.contains("E0507"));
        assert!(sig.contains(":_:_"));
    }

    #[test]
    fn gotcha_store_add_and_merge() {
        let mut store = GotchaStore::new("testhash");
        let g1 = Gotcha::new(
            GotchaCategory::Build,
            GotchaSeverity::Critical,
            "error E0507",
            "use clone",
            GotchaSource::AutoDetected {
                command: "cargo build".into(),
                exit_code: 1,
            },
            "s1",
        );
        store.add_or_merge(g1.clone());
        assert_eq!(store.gotchas.len(), 1);

        let g2 = Gotcha::new(
            GotchaCategory::Build,
            GotchaSeverity::Critical,
            "error E0507",
            "use ref pattern",
            GotchaSource::AutoDetected {
                command: "cargo build".into(),
                exit_code: 1,
            },
            "s2",
        );
        store.add_or_merge(g2);
        assert_eq!(store.gotchas.len(), 1);
        assert_eq!(store.gotchas[0].occurrences, 2);
        assert_eq!(store.gotchas[0].session_ids.len(), 2);
    }

    #[test]
    fn gotcha_store_detect_and_resolve() {
        let mut store = GotchaStore::new("testhash");

        let error_output = "error[E0507]: cannot move out of `self.name`";
        let detected = store.detect_error(error_output, "cargo build", 1, &[], "s1");
        assert!(detected);
        assert_eq!(store.pending_errors.len(), 1);

        let resolved =
            store.try_resolve_pending("cargo build --release", &["src/main.rs".into()], "s1");
        assert!(resolved.is_some());
        assert_eq!(store.gotchas.len(), 1);
        assert!(store.gotchas[0].resolution.contains("src/main.rs"));
    }

    #[test]
    fn agent_report_gotcha() {
        let mut store = GotchaStore::new("testhash");
        let g = store
            .report_gotcha(
                "Use thiserror not anyhow",
                "Derive thiserror::Error in library code",
                "convention",
                "warning",
                "s1",
            )
            .expect("gotcha should be retained in empty store");
        assert_eq!(g.confidence, 0.9);
        assert_eq!(g.category, GotchaCategory::Convention);
    }

    #[test]
    fn decay_reduces_confidence() {
        let mut store = GotchaStore::new("testhash");
        let mut g = Gotcha::new(
            GotchaCategory::Build,
            GotchaSeverity::Warning,
            "test error",
            "test fix",
            GotchaSource::AutoDetected {
                command: "test".into(),
                exit_code: 1,
            },
            "s1",
        );
        g.last_seen = Utc::now() - chrono::Duration::days(30);
        g.confidence = 0.5;
        store.gotchas.push(g);

        store.apply_decay();
        assert!(store.gotchas[0].confidence < 0.5);
    }

    #[test]
    fn decay_archives_low_confidence() {
        let mut store = GotchaStore::new("testhash");
        let mut g = Gotcha::new(
            GotchaCategory::Build,
            GotchaSeverity::Info,
            "old error",
            "old fix",
            GotchaSource::AutoDetected {
                command: "test".into(),
                exit_code: 1,
            },
            "s1",
        );
        g.last_seen = Utc::now() - chrono::Duration::days(90);
        g.confidence = 0.16;
        store.gotchas.push(g);

        store.apply_decay();
        assert!(store.gotchas.is_empty());
    }

    #[test]
    fn relevance_score_higher_for_recent() {
        let recent = Gotcha::new(
            GotchaCategory::Build,
            GotchaSeverity::Critical,
            "error A",
            "fix A",
            GotchaSource::AutoDetected {
                command: "test".into(),
                exit_code: 1,
            },
            "s1",
        );
        let mut old = recent.clone();
        old.last_seen = Utc::now() - chrono::Duration::days(14);

        let score_recent = relevance_score(&recent, &[]);
        let score_old = relevance_score(&old, &[]);
        assert!(score_recent > score_old);
    }

    #[test]
    fn relevance_score_file_overlap_boost() {
        let mut g = Gotcha::new(
            GotchaCategory::Build,
            GotchaSeverity::Warning,
            "error B",
            "fix B",
            GotchaSource::AutoDetected {
                command: "test".into(),
                exit_code: 1,
            },
            "s1",
        );
        g.file_patterns = vec!["src/server.rs".to_string()];

        let with_overlap = relevance_score(&g, &["src/server.rs".to_string()]);
        let without_overlap = relevance_score(&g, &["src/other.rs".to_string()]);
        assert!(with_overlap > without_overlap);
    }

    #[test]
    fn cross_session_boost_increases_confidence() {
        let mut store = GotchaStore::new("testhash");
        let mut g = Gotcha::new(
            GotchaCategory::Build,
            GotchaSeverity::Critical,
            "recurring error",
            "recurring fix",
            GotchaSource::AutoDetected {
                command: "cargo build".into(),
                exit_code: 1,
            },
            "s1",
        );
        g.confidence = 0.6;
        store.gotchas.push(g);

        store.error_log.push(SessionErrorLog {
            session_id: "s2".into(),
            timestamp: Utc::now(),
            errors: vec![ErrorEntry {
                signature: "recurring error".into(),
                command: "cargo build".into(),
                timestamp: Utc::now(),
            }],
            fixes: vec![],
        });
        store.error_log.push(SessionErrorLog {
            session_id: "s3".into(),
            timestamp: Utc::now(),
            errors: vec![ErrorEntry {
                signature: "recurring error".into(),
                command: "cargo build".into(),
                timestamp: Utc::now(),
            }],
            fixes: vec![],
        });

        store.cross_session_boost();
        assert!(store.gotchas[0].confidence > 0.6);
        assert!(store.gotchas[0].session_ids.len() >= 3);
    }

    #[test]
    fn promotion_criteria() {
        let mut g = Gotcha::new(
            GotchaCategory::Convention,
            GotchaSeverity::Warning,
            "use thiserror",
            "derive thiserror::Error",
            GotchaSource::AgentReported {
                session_id: "s1".into(),
            },
            "s1",
        );
        g.confidence = 0.95;
        g.occurrences = 6;
        g.session_ids = vec!["s1".into(), "s2".into(), "s3".into()];
        g.prevented_count = 3;
        assert!(g.is_promotable());

        g.occurrences = 2;
        assert!(!g.is_promotable());
    }

    #[test]
    fn format_injection_block_empty() {
        let store = GotchaStore::new("testhash");
        assert!(store.format_injection_block(&[]).is_empty());
    }

    #[test]
    fn format_injection_block_with_gotchas() {
        let mut store = GotchaStore::new("testhash");
        store.add_or_merge(Gotcha::new(
            GotchaCategory::Build,
            GotchaSeverity::Critical,
            "cargo E0507",
            "use clone",
            GotchaSource::AutoDetected {
                command: "cargo build".into(),
                exit_code: 1,
            },
            "s1",
        ));

        let block = store.format_injection_block(&[]);
        assert!(block.contains("PROJECT GOTCHAS"));
        assert!(block.contains("cargo E0507"));
        assert!(block.contains("use clone"));
    }
}
