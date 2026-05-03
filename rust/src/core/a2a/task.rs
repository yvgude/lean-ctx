use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
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
}

impl Task {
    pub fn new(from_agent: &str, to_agent: &str, description: &str) -> Self {
        let now = Utc::now();
        let id = format!("task-{}", generate_task_id());

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
        }
    }

    pub fn transition(&mut self, new_state: TaskState, reason: Option<&str>) -> Result<(), String> {
        if !self.state.can_transition_to(&new_state) {
            return Err(format!(
                "invalid transition: {} → {}",
                self.state, new_state
            ));
        }

        self.history.push(TaskTransition {
            from: self.state.clone(),
            to: new_state.clone(),
            timestamp: Utc::now(),
            reason: reason.map(std::string::ToString::to_string),
        });

        self.state = new_state;
        self.updated_at = Utc::now();
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

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TaskStore {
    pub tasks: Vec<Task>,
    pub updated_at: Option<DateTime<Utc>>,
}

impl TaskStore {
    pub fn load() -> Self {
        let Some(path) = task_store_path() else {
            return Self::default();
        };
        match std::fs::read_to_string(&path) {
            Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self) -> std::io::Result<()> {
        let Some(path) = task_store_path() else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "no home dir",
            ));
        };

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let json = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, &json)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    pub fn create_task(&mut self, from: &str, to: &str, description: &str) -> String {
        let task = Task::new(from, to, description);
        let id = task.id.clone();
        self.tasks.push(task);
        self.updated_at = Some(Utc::now());
        id
    }

    pub fn get_task(&self, task_id: &str) -> Option<&Task> {
        self.tasks.iter().find(|t| t.id == task_id)
    }

    pub fn get_task_mut(&mut self, task_id: &str) -> Option<&mut Task> {
        self.tasks.iter_mut().find(|t| t.id == task_id)
    }

    pub fn tasks_for_agent(&self, agent_id: &str) -> Vec<&Task> {
        self.tasks
            .iter()
            .filter(|t| t.to_agent == agent_id || t.from_agent == agent_id)
            .collect()
    }

    pub fn pending_tasks_for(&self, agent_id: &str) -> Vec<&Task> {
        self.tasks
            .iter()
            .filter(|t| t.to_agent == agent_id && !t.state.is_terminal())
            .collect()
    }

    pub fn cleanup_old(&mut self, max_age_hours: u64) {
        let cutoff = Utc::now() - chrono::Duration::hours(max_age_hours as i64);
        self.tasks
            .retain(|t| !t.state.is_terminal() || t.updated_at > cutoff);
    }
}

fn task_store_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".lean-ctx/agents/tasks.json"))
}

fn generate_task_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let rand: u32 = (ts as u32).wrapping_mul(2654435761);
    format!("{ts:x}-{rand:08x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_lifecycle_happy_path() {
        let mut task = Task::new("agent-a", "agent-b", "fix the bug");
        assert_eq!(task.state, TaskState::Created);

        task.transition(TaskState::Working, Some("started"))
            .unwrap();
        assert_eq!(task.state, TaskState::Working);

        task.transition(TaskState::Completed, Some("done")).unwrap();
        assert_eq!(task.state, TaskState::Completed);
        assert_eq!(task.history.len(), 3);
    }

    #[test]
    fn task_lifecycle_with_input_required() {
        let mut task = Task::new("a", "b", "deploy");
        task.transition(TaskState::Working, None).unwrap();
        task.transition(TaskState::InputRequired, Some("need credentials"))
            .unwrap();
        task.transition(TaskState::Working, Some("got them"))
            .unwrap();
        task.transition(TaskState::Completed, None).unwrap();
        assert_eq!(task.history.len(), 5);
    }

    #[test]
    fn invalid_transitions_rejected() {
        let mut task = Task::new("a", "b", "test");
        task.transition(TaskState::Working, None).unwrap();
        task.transition(TaskState::Completed, None).unwrap();

        let err = task.transition(TaskState::Working, None);
        assert!(err.is_err());
    }

    #[test]
    fn task_store_operations() {
        let mut store = TaskStore::default();
        let id = store.create_task("agent-a", "agent-b", "review PR");
        assert_eq!(store.tasks.len(), 1);

        let task = store.get_task(&id).unwrap();
        assert_eq!(task.from_agent, "agent-a");

        let pending = store.pending_tasks_for("agent-b");
        assert_eq!(pending.len(), 1);

        store
            .get_task_mut(&id)
            .unwrap()
            .transition(TaskState::Working, None)
            .unwrap();
        store
            .get_task_mut(&id)
            .unwrap()
            .transition(TaskState::Completed, None)
            .unwrap();

        let pending = store.pending_tasks_for("agent-b");
        assert_eq!(pending.len(), 0);
    }

    #[test]
    fn terminal_states_correct() {
        assert!(TaskState::Completed.is_terminal());
        assert!(TaskState::Failed.is_terminal());
        assert!(TaskState::Canceled.is_terminal());
        assert!(!TaskState::Created.is_terminal());
        assert!(!TaskState::Working.is_terminal());
        assert!(!TaskState::InputRequired.is_terminal());
    }
}
