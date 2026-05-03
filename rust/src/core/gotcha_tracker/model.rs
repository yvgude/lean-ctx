use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

pub(super) const MAX_GOTCHAS: usize = 100;
pub(super) const MAX_SESSION_LOGS: usize = 20;
pub(super) const MAX_PENDING: usize = 10;
pub(super) const PENDING_TIMEOUT_SECS: i64 = 900; // 15 minutes
pub(super) const DECAY_ARCHIVE_THRESHOLD: f32 = 0.15;
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
            Self::Critical | Self::Warning => "!",
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
            self.resolution.clone_from(&other.resolution);
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

    pub fn clear(&mut self) {
        self.gotchas.clear();
        self.pending_errors.clear();
        self.updated_at = Utc::now();
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

pub(super) fn gotcha_id(trigger: &str, category: &GotchaCategory) -> String {
    let mut hasher = DefaultHasher::new();
    trigger.hash(&mut hasher);
    category.short_label().hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}
