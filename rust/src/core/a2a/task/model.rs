// SPDX-License-Identifier: Apache-2.0

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use super::authority::{AgentArtifactRefV1, TaskDescriptorV1};
use super::{MAX_HISTORY, TASK_DESCRIPTOR_VERSION, TASK_STATUS_VERSION};

use crate::core::context_admission::carrier::{Withheld, admit_for_store};
use crate::core::context_admission::stored::serve_stored;

/// Transition reasons the Engine writes itself. They are fixed text, not
/// caller text: neither admitted on the way in nor withheld on the way out.
const REASON_CREATED: &str = "task created";
const REASON_MATERIALIZED: &str = "remote task materialized";
pub(crate) const REASON_INPUT_RECEIVED: &str = "input received via A2A";
pub(crate) const REASON_CANCELED_VIA_A2A: &str = "canceled via A2A";

fn engine_reason(reason: &str) -> bool {
    matches!(
        reason,
        REASON_CREATED | REASON_MATERIALIZED | REASON_INPUT_RECEIVED | REASON_CANCELED_VIA_A2A
    )
}

/// Stored task text as a new delivery. A task carries no source binding, so
/// this is the originless rule of `context_admission::stored`: withheld while
/// protection is active, re-admitted under the current content rules
/// otherwise.
fn serve_text(text: &str) -> Result<String, Withheld> {
    serve_stored(text, None, None)
}

/// [`serve_text`], with the typed, content-free gateway notice in place of
/// text that is withheld.
fn served_text(text: &str) -> String {
    serve_text(text).unwrap_or_else(|withheld| withheld.notice())
}

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

impl TaskPart {
    /// The part with every caller-chosen string passed through `text`, or the
    /// first refusal.
    fn try_map(&self, text: fn(&str) -> Result<String, Withheld>) -> Result<Self, Withheld> {
        Ok(match self {
            Self::Text { text: value } => Self::Text { text: text(value)? },
            Self::Data { mime_type, data } => Self::Data {
                mime_type: text(mime_type)?,
                data: text(data)?,
            },
            Self::File {
                name,
                mime_type,
                data,
                uri,
            } => Self::File {
                name: text(name)?,
                mime_type: mime_type.as_deref().map(text).transpose()?,
                data: data.as_deref().map(text).transpose()?,
                uri: uri.as_deref().map(text).transpose()?,
            },
        })
    }

    /// NEW caller content on its way into the task store: what the gateway
    /// masks is stored masked; restricted or withheld content is refused and
    /// nothing of the part is stored (E3).
    pub(crate) fn admitted(&self) -> Result<Self, Withheld> {
        self.try_map(admit_for_store)
    }

    /// The stored part as a new delivery. A part that is withheld — whatever
    /// its kind — becomes a text part carrying the gateway notice, in place.
    fn served(&self) -> Self {
        self.try_map(serve_text)
            .unwrap_or_else(|withheld| Self::Text {
                text: withheld.notice(),
            })
    }
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
                reason: Some(REASON_CREATED.to_string()),
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

    /// The description is NEW caller text from the remote peer: it is
    /// admitted for the store here (masked text is stored masked; restricted
    /// or withheld text refuses the task and nothing is materialized).
    pub(super) fn from_remote_descriptor(
        descriptor: &TaskDescriptorV1,
        task_id: String,
    ) -> Result<Self, Withheld> {
        let now = Utc::now();
        let description = admit_for_store(&descriptor.description)?;
        let parts = vec![TaskPart::Text {
            text: description.clone(),
        }];
        let mut metadata = HashMap::new();
        metadata.insert(
            "schema_version".to_string(),
            TASK_DESCRIPTOR_VERSION.to_string(),
        );
        metadata.insert("action".to_string(), descriptor.action.clone());
        Ok(Self {
            id: task_id,
            from_agent: descriptor.sender.clone(),
            to_agent: descriptor.recipient.clone(),
            state: TaskState::Created,
            description,
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
                reason: Some(REASON_MATERIALIZED.to_string()),
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
        })
    }

    /// A copy of the task for delivery to a tool result or a peer.
    ///
    /// Description, message parts, artifacts, metadata values and
    /// caller-typed transition reasons are each served on their own; a part
    /// that is withheld is replaced in place by the typed gateway notice, so
    /// the shape of the task (counts, order, states, timestamps, ids) is what
    /// is stored. The stored task is not changed.
    #[must_use]
    pub fn served(&self) -> Self {
        let mut task = self.clone();
        task.description = self.served_description();
        for message in &mut task.messages {
            for part in &mut message.parts {
                *part = part.served();
            }
        }
        for artifact in &mut task.artifacts {
            *artifact = artifact.served();
        }
        for value in task.metadata.values_mut() {
            *value = served_text(value);
        }
        for transition in &mut task.history {
            if let Some(reason) = transition
                .reason
                .as_mut()
                .filter(|reason| !engine_reason(reason.as_str()))
            {
                *reason = served_text(reason);
            }
        }
        task
    }

    /// The description alone as [`Self::served`] delivers it, for a listing
    /// that prints nothing else of the task's text.
    #[must_use]
    pub fn served_description(&self) -> String {
        served_text(&self.description)
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
        // A reason typed by a caller is new text for the store: admitted
        // before anything changes. The refusal is typed and content-free.
        let reason = match reason {
            Some(reason) if !engine_reason(reason) => {
                Some(admit_for_store(reason).map_err(|withheld| withheld.message("task reason"))?)
            }
            other => other.map(str::to_owned),
        };
        self.history.push(TaskTransition {
            from: self.state.clone(),
            to: new_state.clone(),
            timestamp: now,
            reason,
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

    /// [`Self::add_message`] for NEW caller content: every part is admitted
    /// for the store first ([`TaskPart::admitted`]). One refused part refuses
    /// the message and nothing of it is appended.
    pub(crate) fn append_message(
        &mut self,
        role: &str,
        parts: &[TaskPart],
    ) -> Result<(), Withheld> {
        let parts = parts
            .iter()
            .map(TaskPart::admitted)
            .collect::<Result<Vec<_>, _>>()?;
        self.add_message(role, parts);
        Ok(())
    }

    pub fn add_artifact(&mut self, artifact: TaskPart) {
        self.artifacts.push(artifact);
        self.updated_at = Utc::now();
    }
}
