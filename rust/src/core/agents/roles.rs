use super::ScratchpadEntry;
use crate::core::a2a::message::{A2AMessage, MessageCategory};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AgentRole {
    Coder,
    Reviewer,
    Planner,
    Explorer,
    Debugger,
    Tester,
    Orchestrator,
}

impl AgentRole {
    pub(crate) fn from_str_loose(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "review" | "reviewer" | "code_review" => Self::Reviewer,
            "plan" | "planner" | "architect" => Self::Planner,
            "explore" | "explorer" | "research" => Self::Explorer,
            "debug" | "debugger" => Self::Debugger,
            "test" | "tester" | "qa" => Self::Tester,
            "orchestrator" | "coordinator" | "manager" => Self::Orchestrator,
            _ => Self::Coder,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ContextDepthConfig {
    pub max_files_full: usize,
    pub max_files_signatures: usize,
    pub preferred_mode: &'static str,
    pub context_budget_ratio: f64,
}

impl ContextDepthConfig {
    pub(crate) fn for_role(role: AgentRole) -> Self {
        match role {
            AgentRole::Coder => Self {
                max_files_full: 5,
                max_files_signatures: 15,
                preferred_mode: "full",
                context_budget_ratio: 0.7,
            },
            AgentRole::Reviewer => Self {
                max_files_full: 3,
                max_files_signatures: 20,
                preferred_mode: "signatures",
                context_budget_ratio: 0.5,
            },
            AgentRole::Planner => Self {
                max_files_full: 1,
                max_files_signatures: 10,
                preferred_mode: "map",
                context_budget_ratio: 0.3,
            },
            AgentRole::Explorer => Self {
                max_files_full: 2,
                max_files_signatures: 8,
                preferred_mode: "map",
                context_budget_ratio: 0.4,
            },
            AgentRole::Debugger => Self {
                max_files_full: 8,
                max_files_signatures: 5,
                preferred_mode: "full",
                context_budget_ratio: 0.8,
            },
            AgentRole::Tester => Self {
                max_files_full: 4,
                max_files_signatures: 10,
                preferred_mode: "full",
                context_budget_ratio: 0.6,
            },
            AgentRole::Orchestrator => Self {
                max_files_full: 0,
                max_files_signatures: 5,
                preferred_mode: "map",
                context_budget_ratio: 0.2,
            },
        }
    }
}

impl From<ScratchpadEntry> for A2AMessage {
    fn from(entry: ScratchpadEntry) -> Self {
        Self {
            id: entry.id,
            from_agent: entry.from_agent,
            to_agent: entry.to_agent,
            task_id: entry.task_id,
            category: MessageCategory::parse_str(&entry.category),
            priority: entry.priority,
            privacy: entry.privacy,
            content: entry.message,
            metadata: entry.metadata,
            project_root: entry.project_root,
            timestamp: entry.timestamp,
            read_by: entry.read_by,
            expires_at: entry.expires_at,
        }
    }
}

impl From<A2AMessage> for ScratchpadEntry {
    fn from(msg: A2AMessage) -> Self {
        Self {
            id: msg.id,
            from_agent: msg.from_agent,
            to_agent: msg.to_agent,
            task_id: msg.task_id,
            category: msg.category.to_string(),
            priority: msg.priority,
            privacy: msg.privacy,
            message: msg.content,
            metadata: msg.metadata,
            project_root: msg.project_root,
            timestamp: msg.timestamp,
            read_by: msg.read_by,
            expires_at: msg.expires_at,
        }
    }
}
