use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::core::a2a::message::{MessagePriority, PrivacyLevel};
use crate::ipc::process::ProcessIdentity;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct AgentRegistry {
    pub agents: Vec<AgentEntry>,
    pub scratchpad: Vec<ScratchpadEntry>,
    #[serde(default)]
    pub logical_sessions: Vec<LogicalSessionPresence>,
    #[serde(default)]
    pub logical_session_telemetry_seen: bool,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct LogicalSessionPresence {
    pub source: String,
    pub workspace: String,
    pub session_id: String,
    pub opened_at: DateTime<Utc>,
    pub last_heartbeat: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct AgentEntry {
    pub agent_id: String,
    pub agent_type: String,
    pub role: Option<String>,
    pub project_root: String,
    pub started_at: DateTime<Utc>,
    pub last_active: DateTime<Utc>,
    pub pid: u32,
    /// PID alone is not an identity: operating systems reuse it. The immutable
    /// process start marker prevents a retired MCP record from becoming "live"
    /// again when an unrelated process receives the same PID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_identity: Option<ProcessIdentity>,
    /// Optional explicit binding to the durable identity registry. Generated
    /// presence IDs remain independent and unchanged for legacy callers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub durable_identity_id: Option<String>,
    pub status: AgentStatus,
    pub status_message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) enum AgentStatus {
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
    #[serde(default)]
    pub task_id: Option<String>,
    pub category: String,
    #[serde(default)]
    pub priority: MessagePriority,
    #[serde(default)]
    pub privacy: PrivacyLevel,
    pub message: String,
    #[serde(default)]
    pub metadata: HashMap<String, String>,
    #[serde(default)]
    pub project_root: Option<String>,
    pub timestamp: DateTime<Utc>,
    pub read_by: Vec<String>,
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
}
