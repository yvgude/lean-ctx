use lean_ctx_time::UtcDateTime;
use serde::{Deserialize, Serialize};

use super::{CheckpointId, DecisionId, ProvenanceId};

fn default_schema_version() -> u32 {
    1
}

fn default_observed_at() -> UtcDateTime {
    UtcDateTime::now()
}

/// How directly an edit observation was established.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ObservationConfidence {
    Verified,
    #[default]
    Observed,
    Partial,
}

/// Whether a checkpoint could be connected to one or more file observations.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum CheckpointLinkState {
    Linked,
    #[default]
    Orphaned,
}

/// Backwards-compatible concise name for [`CheckpointLinkState`].
pub type LinkState = CheckpointLinkState;

/// Durable evidence that a tool changed, or observed a change to, one file.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProvenanceRecord {
    #[serde(default)]
    pub id: ProvenanceId,
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    #[serde(default)]
    pub project_hash: String,
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub agent_id: String,
    /// Stable operation fingerprint used to make retried observations idempotent.
    #[serde(default)]
    pub operation_id: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub tool: String,
    #[serde(default = "default_observed_at")]
    pub observed_at: UtcDateTime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_sha256: Option<String>,
    #[serde(default)]
    pub lines_added: u64,
    #[serde(default)]
    pub lines_removed: u64,
    #[serde(default)]
    pub confidence: ObservationConfidence,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decision_ids: Vec<DecisionId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint_id: Option<CheckpointId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ledger_event_id: Option<String>,
}

impl Default for ProvenanceRecord {
    fn default() -> Self {
        Self {
            id: String::new(),
            schema_version: default_schema_version(),
            project_hash: String::new(),
            session_id: String::new(),
            agent_id: String::new(),
            operation_id: String::new(),
            path: String::new(),
            tool: String::new(),
            observed_at: default_observed_at(),
            before_sha256: None,
            after_sha256: None,
            lines_added: 0,
            lines_removed: 0,
            confidence: ObservationConfidence::default(),
            decision_ids: Vec::new(),
            checkpoint_id: None,
            ledger_event_id: None,
        }
    }
}

/// A durable bridge between a git commit and the file observations it contains.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CheckpointRecord {
    #[serde(default)]
    pub id: CheckpointId,
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub commit_sha: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patch_id: Option<String>,
    #[serde(default)]
    pub link_state: CheckpointLinkState,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provenance_ids: Vec<ProvenanceId>,
    #[serde(default = "default_observed_at")]
    pub observed_at: UtcDateTime,
    #[serde(default)]
    pub files_touched: u64,
    #[serde(default)]
    pub insertions: u64,
    #[serde(default)]
    pub deletions: u64,
}

impl Default for CheckpointRecord {
    fn default() -> Self {
        Self {
            id: String::new(),
            session_id: String::new(),
            commit_sha: String::new(),
            patch_id: None,
            link_state: CheckpointLinkState::default(),
            provenance_ids: Vec::new(),
            observed_at: default_observed_at(),
            files_touched: 0,
            insertions: 0,
            deletions: 0,
        }
    }
}
