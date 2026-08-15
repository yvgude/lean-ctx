//! MCP ingress task lineage and thread-local propagation.

use chrono::Utc;
use lean_ctx_protocol::{
    AgentId, ProjectId, SessionId, TaskComplexity, TaskEnvelopeV1, TaskId, TaskProfileV1, TraceId,
};
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

pub type TaskProfileLocal = TaskProfileV1;

thread_local! {
    static CURRENT: RefCell<Option<TaskEnvelopeV1>> = const { RefCell::new(None) };
}

/// Parent task id retained for the lifetime of each active MCP session.
static SESSION_LINEAGES: OnceLock<Arc<Mutex<HashMap<String, String>>>> = OnceLock::new();

#[derive(Debug, Clone, Default)]
/// Maintains task-envelope lineage for the current execution thread.
pub struct TaskSpine {
    /// Root task of the current session, absent when creating that root task.
    pub parent_id: Option<String>,
}

impl TaskSpine {
    pub fn create_envelope(query: &str, session_id: &str, agent_id: &str) -> TaskEnvelopeV1 {
        let project = crate::core::session::SessionState::load_latest()
            .and_then(|s| s.project_root)
            .unwrap_or_else(|| "unknown-project".to_owned());
        let task_id = TaskId::try_from(format!("mcp-task-{}", uuid::Uuid::new_v4()))
            .expect("generated task id is valid");
        let spine = Self::for_session(session_id, task_id.as_str());
        let envelope = TaskEnvelopeV1 {
            schema_version: TaskEnvelopeV1::SCHEMA_VERSION,
            task_id,
            trace_id: TraceId::try_from(format!("trace-{}", uuid::Uuid::new_v4()))
                .expect("generated trace id is valid"),
            project_id: ProjectId::try_from(project).unwrap_or_else(|_| {
                ProjectId::try_from("unknown-project").expect("fallback project id is valid")
            }),
            session_id: SessionId::try_from(session_id.to_owned())
                .expect("MCP session id is valid"),
            agent_id: AgentId::try_from(agent_id.to_owned()).expect("MCP agent id is valid"),
            complexity: TaskComplexity::Unknown,
            created_at: Utc::now().to_rfc3339(),
            parent_task_id: spine
                .parent_id
                .clone()
                .map(TaskId::try_from)
                .transpose()
                .expect("stored parent task id is valid"),
            tenant_id: None,
            intent: (!query.trim().is_empty()).then(|| query.to_owned()),
            task_class: None,
            risk_class: None,
            quality_requirement_milli: None,
            cost_budget_micros: None,
            latency_budget_ms: None,
            data_classification: None,
            region_policy_ref: None,
            model_policy_ref: None,
            context_state_ref: None,
            outcome_contract_ref: None,
        };
        CURRENT.with(|current| *current.borrow_mut() = Some(envelope.clone()));
        envelope
    }

    fn for_session(session_id: &str, task_id: &str) -> Self {
        if session_id.trim().is_empty() {
            return Self::default();
        }

        let mut lineages = Self::session_lineages()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let parent_id = match lineages.entry(session_id.to_owned()) {
            std::collections::hash_map::Entry::Occupied(parent) => Some(parent.get().clone()),
            std::collections::hash_map::Entry::Vacant(session) => {
                session.insert(task_id.to_owned());
                None
            }
        };
        Self { parent_id }
    }

    fn session_lineages() -> &'static Arc<Mutex<HashMap<String, String>>> {
        SESSION_LINEAGES.get_or_init(|| Arc::new(Mutex::new(HashMap::new())))
    }

    pub fn enrich_from_triage(envelope: &mut TaskEnvelopeV1, profile: &TaskProfileLocal) {
        envelope.intent = Some(profile.primary_intent.clone());
        envelope.task_class = Some(profile.task_class.clone());
        envelope.complexity = profile.complexity;
        envelope.risk_class = Some(profile.risk_signal);
        CURRENT.with(|current| *current.borrow_mut() = Some(envelope.clone()));
    }

    pub fn task_id() -> Option<String> {
        CURRENT.with(|current| {
            current
                .borrow()
                .as_ref()
                .map(|e| e.task_id.as_str().to_owned())
        })
    }

    pub fn current() -> Option<TaskEnvelopeV1> {
        CURRENT.with(|current| current.borrow().clone())
    }

    pub fn set(envelope: TaskEnvelopeV1) {
        CURRENT.with(|current| *current.borrow_mut() = Some(envelope));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_envelope_created_on_dispatch() {
        let envelope =
            TaskSpine::create_envelope("query", "session-task-spine", "agent-task-spine");
        assert_eq!(
            TaskSpine::task_id().as_deref(),
            Some(envelope.task_id.as_str())
        );
    }

    #[test]
    fn test_envelope_has_valid_task_id() {
        let first = TaskSpine::create_envelope("one", "session-task-spine", "agent-task-spine");
        let second = TaskSpine::create_envelope("two", "session-task-spine", "agent-task-spine");
        assert!(!first.task_id.as_str().is_empty());
        assert_ne!(first.task_id, second.task_id);
    }

    #[test]
    fn test_enrich_from_triage() {
        let mut envelope =
            TaskSpine::create_envelope("query", "session-task-spine", "agent-task-spine");
        let profile = TaskProfileV1 {
            primary_intent: "implement".into(),
            task_class: "coding".into(),
            complexity: TaskComplexity::High,
            scope: Default::default(),
            context_need_milli: 0,
            reasoning_need_milli: 0,
            risk_signal: lean_ctx_protocol::RiskClass::High,
            confidence_milli: 0,
            keywords: vec![],
            language_hints: vec![],
        };
        TaskSpine::enrich_from_triage(&mut envelope, &profile);
        assert_eq!(envelope.intent.as_deref(), Some("implement"));
        assert_eq!(envelope.task_class.as_deref(), Some("coding"));
        assert_eq!(envelope.complexity, TaskComplexity::High);
        assert_eq!(
            envelope.risk_class,
            Some(lean_ctx_protocol::RiskClass::High)
        );
    }

    #[test]
    fn first_session_task_is_parent_of_subsequent_tasks() {
        let session_id = format!("lineage-session-{}", uuid::Uuid::new_v4());
        let parent = TaskSpine::create_envelope("first", &session_id, "agent");
        let child = TaskSpine::create_envelope("second", &session_id, "agent");

        assert!(parent.parent_task_id.is_none());
        assert_eq!(
            child.parent_task_id.as_ref().map(|id| id.as_str()),
            Some(parent.task_id.as_str())
        );
        assert_eq!(
            TaskSpine::current()
                .as_ref()
                .map(|envelope| envelope.task_id.as_str()),
            Some(child.task_id.as_str())
        );
    }
}
