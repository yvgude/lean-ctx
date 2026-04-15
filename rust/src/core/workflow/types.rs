use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct WorkflowSpec {
    pub name: String,
    pub description: Option<String>,
    pub initial: String,
    pub states: Vec<StateSpec>,
    pub transitions: Vec<TransitionSpec>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct StateSpec {
    pub name: String,
    pub description: Option<String>,
    #[serde(default)]
    pub allowed_tools: Option<Vec<String>>,
    #[serde(default)]
    pub requires_evidence: Option<Vec<String>>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TransitionSpec {
    pub from: String,
    pub to: String,
    pub description: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct WorkflowRun {
    pub spec: WorkflowSpec,
    pub current: String,
    pub started_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub transitions: Vec<TransitionRecord>,
    #[serde(default)]
    pub evidence: Vec<EvidenceItem>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TransitionRecord {
    pub from: String,
    pub to: String,
    pub note: Option<String>,
    pub timestamp: DateTime<Utc>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct EvidenceItem {
    pub key: String,
    pub value: Option<String>,
    pub timestamp: DateTime<Utc>,
}

impl WorkflowSpec {
    pub fn state(&self, name: &str) -> Option<&StateSpec> {
        self.states.iter().find(|s| s.name == name)
    }

    pub fn builtin_plan_code_test() -> Self {
        Self {
            name: "plan_code_test".to_string(),
            description: Some(
                "Minimal workflow with evidence-gated transitions (planning→coding→testing→done)."
                    .to_string(),
            ),
            initial: "planning".to_string(),
            states: vec![
                StateSpec {
                    name: "planning".to_string(),
                    description: Some("Define approach + acceptance criteria.".to_string()),
                    allowed_tools: Some(vec![
                        "ctx".to_string(),
                        "ctx_workflow".to_string(),
                        "ctx_overview".to_string(),
                        "ctx_search".to_string(),
                        "ctx_graph".to_string(),
                        "ctx_intent".to_string(),
                        "ctx_semantic_search".to_string(),
                        "ctx_tree".to_string(),
                        "ctx_read".to_string(),
                        "ctx_multi_read".to_string(),
                        "ctx_session".to_string(),
                        "ctx_knowledge".to_string(),
                        "ctx_agent".to_string(),
                        "ctx_share".to_string(),
                        "ctx_task".to_string(),
                    ]),
                    requires_evidence: None,
                },
                StateSpec {
                    name: "coding".to_string(),
                    description: Some("Implement changes.".to_string()),
                    allowed_tools: Some(vec![
                        "ctx".to_string(),
                        "ctx_workflow".to_string(),
                        "ctx_read".to_string(),
                        "ctx_multi_read".to_string(),
                        "ctx_search".to_string(),
                        "ctx_tree".to_string(),
                        "ctx_edit".to_string(),
                        "ctx_delta".to_string(),
                        "ctx_graph".to_string(),
                        "ctx_outline".to_string(),
                        "ctx_symbol".to_string(),
                    ]),
                    requires_evidence: Some(vec!["tool:ctx_read".to_string()]),
                },
                StateSpec {
                    name: "testing".to_string(),
                    description: Some("Run tests and verify behavior.".to_string()),
                    allowed_tools: Some(vec![
                        "ctx".to_string(),
                        "ctx_workflow".to_string(),
                        "ctx_shell".to_string(),
                        "ctx_metrics".to_string(),
                        "ctx_cost".to_string(),
                        "ctx_heatmap".to_string(),
                    ]),
                    requires_evidence: Some(vec!["tool:ctx_shell".to_string()]),
                },
                StateSpec {
                    name: "done".to_string(),
                    description: Some("Complete.".to_string()),
                    allowed_tools: Some(vec!["ctx".to_string(), "ctx_workflow".to_string()]),
                    requires_evidence: Some(vec!["tool:ctx_shell".to_string()]),
                },
            ],
            transitions: vec![
                TransitionSpec {
                    from: "planning".to_string(),
                    to: "coding".to_string(),
                    description: Some("Plan ready; start coding.".to_string()),
                },
                TransitionSpec {
                    from: "coding".to_string(),
                    to: "testing".to_string(),
                    description: Some("Implementation complete; run tests.".to_string()),
                },
                TransitionSpec {
                    from: "testing".to_string(),
                    to: "done".to_string(),
                    description: Some("Tests passed; wrap up.".to_string()),
                },
            ],
        }
    }
}

impl WorkflowRun {
    pub fn new(spec: WorkflowSpec) -> Self {
        let now = Utc::now();
        let current = spec.initial.clone();
        Self {
            spec,
            current,
            started_at: now,
            updated_at: now,
            transitions: Vec::new(),
            evidence: Vec::new(),
        }
    }

    pub fn add_manual_evidence(&mut self, key: &str, value: Option<&str>) {
        self.evidence.push(EvidenceItem {
            key: key.to_string(),
            value: value.map(|v| v.to_string()),
            timestamp: Utc::now(),
        });
        self.updated_at = Utc::now();
    }
}
