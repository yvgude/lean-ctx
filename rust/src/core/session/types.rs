use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::core::intent_protocol::IntentRecord;

/// Persistent session state tracking task, findings, files, decisions, and stats.
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
    /// When true, resume / compaction prompts encourage concise model replies.
    #[serde(default)]
    pub terse_mode: bool,
    /// Unified compression level label (off/lite/standard/max).
    #[serde(default)]
    pub compression_level: String,
    /// Watermark: timestamp of last auto-consolidation to prevent duplicate knowledge entries.
    #[serde(default)]
    pub last_consolidate_ts: Option<DateTime<Utc>>,
    /// Extra project roots for multi-root workspaces.
    /// Populated from config `extra_roots` and/or MCP `roots/list`.
    #[serde(default)]
    pub extra_roots: Vec<String>,
    /// LITM placement manifest (#539): what the last wakeup injection placed
    /// where, so explicit re-recalls can be scored as placement misses.
    #[serde(default)]
    pub wakeup_manifest: Vec<ManifestEntry>,
    /// ACE delta playbook (#541): incremental, stable-ID checkpoint entries —
    /// grown by `ctx_compress`, never rewritten (anti context-collapse).
    #[serde(default)]
    pub playbook: super::playbook::Playbook,
    /// Last `ctx_semantic_search` query (#542): fallback query source for
    /// query-conditioned IB compression when no explicit task is set.
    #[serde(default)]
    pub last_semantic_query: Option<String>,
}

/// One item placed by the wakeup/instructions builder, used for LITM
/// placement calibration (#539).
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ManifestEntry {
    pub key: String,
    /// "begin" | "end"
    pub position: String,
    /// LITM profile name active when placed ("claude" | "gpt" | "gemini").
    pub profile: String,
    /// Set once this entry was re-recalled (counted as a miss).
    #[serde(default)]
    pub missed: bool,
}

/// Description of the current task being worked on, with optional progress tracking.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TaskInfo {
    pub description: String,
    pub intent: Option<String>,
    pub progress_pct: Option<u8>,
}

/// A discovery or observation recorded during the session.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Finding {
    pub file: Option<String>,
    pub line: Option<u32>,
    pub summary: String,
    pub timestamp: DateTime<Utc>,
}

/// A design or implementation decision made during the session.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Decision {
    pub summary: String,
    pub rationale: Option<String>,
    pub timestamp: DateTime<Utc>,
}

/// A file that was read or modified during the session.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct FileTouched {
    pub path: String,
    pub file_ref: Option<String>,
    pub read_count: u32,
    pub modified: bool,
    pub last_mode: String,
    pub tokens: usize,
    #[serde(default)]
    pub stale: bool,
    #[serde(default)]
    pub context_item_id: Option<String>,
    /// One-line summary of file purpose/content (max 80 chars)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

/// Snapshot of a test run with pass/fail counts.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TestSnapshot {
    pub command: String,
    pub passed: u32,
    pub failed: u32,
    pub total: u32,
    pub timestamp: DateTime<Utc>,
}

/// A timestamped progress entry describing an action taken.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ProgressEntry {
    pub action: String,
    pub detail: Option<String>,
    pub timestamp: DateTime<Utc>,
}

/// Source of an evidence record: automatic tool call or manual agent entry.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    ToolCall,
    Manual,
}

/// An auditable record of a tool invocation or manual observation.
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

/// Aggregate counters for the session: tool calls, token savings, cache hits.
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
pub(crate) struct LatestPointer {
    pub(crate) id: String,
}

/// Pre-serialized session data ready for background disk I/O.
/// Created by `SessionState::prepare_save()` while holding the write lock,
/// then written via `write_to_disk()` after the lock is released.
pub struct PreparedSave {
    pub(crate) dir: PathBuf,
    pub(crate) id: String,
    pub(crate) json: String,
    pub(crate) pointer_json: String,
    pub(crate) compaction_snapshot: Option<String>,
}

/// Lightweight summary of a session for listing purposes.
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
