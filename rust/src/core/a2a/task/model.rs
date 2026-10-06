// SPDX-License-Identifier: Apache-2.0

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use super::authority::{AgentArtifactRefV1, TaskDescriptorV1};
use super::{MAX_HISTORY, TASK_DESCRIPTOR_VERSION, TASK_STATUS_VERSION};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum TaskState {
    Created,
    Working,
    InputRequired,
    Completed,
    Failed,
    Canceled,
}

impl std::fmt::Display for TaskState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TaskState::Created => write!(f, "created"),
            TaskState::Working => write!(f, "working"),
            TaskState::InputRequired => write!(f, "input-required"),
            TaskState::Completed => write!(f, "completed"),
            TaskState::Failed => write!(f, "failed"),
            TaskState::Canceled => write!(f, "canceled"),
        }
    }
}

impl TaskState {
    pub fn parse_str(s: &str) -> Option<Self> {
        match s {
            "created" => Some(Self::Created),
            "working" => Some(Self::Working),
            "input-required" | "input_required" => Some(Self::InputRequired),
            "completed" => Some(Self::Completed),
            "failed" => Some(Self::Failed),
            "canceled" | "cancelled" => Some(Self::Canceled),
            _ => None,
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Canceled)
    }

    pub fn can_transition_to(&self, next: &TaskState) -> bool {
        match self {
            TaskState::Created => matches!(
                next,
                TaskState::Working | TaskState::Canceled | TaskState::Failed
            ),
            TaskState::Working => matches!(
                next,
                TaskState::InputRequired
                    | TaskState::Completed
                    | TaskState::Failed
                    | TaskState::Canceled
            ),
            TaskState::InputRequired => matches!(
                next,
                TaskState::Working | TaskState::Canceled | TaskState::Failed
            ),
            TaskState::Completed | TaskState::Failed | TaskState::Canceled => false,
        }
    }
}

/// Versioned task status as published on the A2A wire.
///
/// `state` keeps the established lower-case A2A spelling (`created`,
/// `input-required`, …) instead of the Rust variant name, so adding the
/// version field does not change any value an existing client parses, and the
/// persisted [`TaskState`] representation in the store stays untouched.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TaskStatusV1 {
    pub schema_version: u32,
    #[serde(with = "task_state_wire")]
    pub state: TaskState,
    pub timestamp: DateTime<Utc>,
}

mod task_state_wire {
    use super::TaskState;
    use serde::{Deserialize, Deserializer, Serializer};

    pub(super) fn serialize<S: Serializer>(
        state: &TaskState,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&state.to_string())
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<TaskState, D::Error> {
        let raw = String::deserialize(deserializer)?;
        TaskState::parse_str(&raw)
            .ok_or_else(|| serde::de::Error::custom(format!("unknown task state: {raw}")))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskMessage {
    pub role: String,
    pub parts: Vec<TaskPart>,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum TaskPart {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "data")]
    Data { mime_type: String, data: String },
    #[serde(rename = "file")]
    File {
        name: String,
        mime_type: Option<String>,
        data: Option<String>,
        uri: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskTransition {
    pub from: TaskState,
    pub to: TaskState,
    pub timestamp: DateTime<Utc>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub from_agent: String,
    pub to_agent: String,
    pub state: TaskState,
    pub description: String,
    pub messages: Vec<TaskMessage>,
    pub artifacts: Vec<TaskPart>,
    pub history: Vec<TaskTransition>,
    pub metadata: HashMap<String, String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub tenant_id: Option<String>,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub action: Option<String>,
    #[serde(default)]
    pub idempotency_key: Option<String>,
    #[serde(default)]
    pub authority_key_id: Option<String>,
    #[serde(default)]
    pub descriptor_digest: Option<String>,
    #[serde(default)]
    pub artifact_refs: Vec<AgentArtifactRefV1>,
}

impl Task {
    pub fn new(from_agent: &str, to_agent: &str, description: &str) -> Self {
        let now = Utc::now();
        let id = format!("task-{}", uuid::Uuid::new_v4());

        Self {
            id,
            from_agent: from_agent.to_string(),
            to_agent: to_agent.to_string(),
            state: TaskState::Created,
            description: description.to_string(),
            messages: vec![TaskMessage {
                role: from_agent.to_string(),
                parts: vec![TaskPart::Text {
                    text: description.to_string(),
                }],
                timestamp: now,
            }],
            artifacts: Vec::new(),
            history: vec![TaskTransition {
                from: TaskState::Created,
                to: TaskState::Created,
                timestamp: now,
                reason: Some("task created".to_string()),
            }],
            metadata: HashMap::new(),
            created_at: now,
            updated_at: now,
            tenant_id: None,
            project_id: None,
            action: None,
            idempotency_key: None,
            authority_key_id: None,
            descriptor_digest: None,
            artifact_refs: Vec::new(),
        }
    }

    pub(super) fn from_remote_descriptor(descriptor: &TaskDescriptorV1, task_id: String) -> Self {
        let now = Utc::now();
        let parts = vec![TaskPart::Text {
            text: descriptor.description.clone(),
        }];
        let mut metadata = HashMap::new();
        metadata.insert(
            "schema_version".to_string(),
            TASK_DESCRIPTOR_VERSION.to_string(),
        );
        metadata.insert("action".to_string(), descriptor.action.clone());
        Self {
            id: task_id,
            from_agent: descriptor.sender.clone(),
            to_agent: descriptor.recipient.clone(),
            state: TaskState::Created,
            description: descriptor.description.clone(),
            messages: vec![TaskMessage {
                role: descriptor.sender.clone(),
                parts,
                timestamp: now,
            }],
            artifacts: Vec::new(),
            history: vec![TaskTransition {
                from: TaskState::Created,
                to: TaskState::Created,
                timestamp: now,
                reason: Some("remote task materialized".to_string()),
            }],
            metadata,
            created_at: now,
            updated_at: now,
            tenant_id: Some(descriptor.tenant_id.clone()),
            project_id: Some(descriptor.project_id.clone()),
            action: Some(descriptor.action.clone()),
            idempotency_key: Some(descriptor.idempotency_key.clone()),
            authority_key_id: Some(descriptor.key_id.clone()),
            descriptor_digest: Some(descriptor.content_digest()),
            artifact_refs: descriptor.artifact_refs.clone(),
        }
    }

    pub fn status_v1(&self) -> TaskStatusV1 {
        TaskStatusV1 {
            schema_version: TASK_STATUS_VERSION,
            state: self.state.clone(),
            timestamp: self.updated_at,
        }
    }

    pub fn transition(&mut self, new_state: TaskState, reason: Option<&str>) -> Result<(), String> {
        if self.history.len() >= MAX_HISTORY {
            return Err("task transition history limit exceeded".to_string());
        }
        // Keep one slot for cancellation/failure, not unbounded further work.
        if self.history.len() == MAX_HISTORY - 1 && !new_state.is_terminal() {
            return Err("task transition history reserved for terminal state".to_string());
        }
        if !self.state.can_transition_to(&new_state) {
            return Err(format!(
                "invalid transition: {} → {}",
                self.state, new_state
            ));
        }

        let now = Utc::now();
        // Equal timestamps are valid; a backward clock must not poison reload.
        if now < self.updated_at {
            return Err("task clock precedes stored state".to_string());
        }
        self.history.push(TaskTransition {
            from: self.state.clone(),
            to: new_state.clone(),
            timestamp: now,
            reason: reason.map(std::string::ToString::to_string),
        });

        self.state = new_state;
        self.updated_at = now;
        Ok(())
    }

    pub fn add_message(&mut self, role: &str, parts: Vec<TaskPart>) {
        self.messages.push(TaskMessage {
            role: role.to_string(),
            parts,
            timestamp: Utc::now(),
        });
        self.updated_at = Utc::now();
    }

    pub fn add_artifact(&mut self, artifact: TaskPart) {
        self.artifacts.push(artifact);
        self.updated_at = Utc::now();
    }
}
