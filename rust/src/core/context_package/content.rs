use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::core::knowledge::{ConsolidatedInsight, KnowledgeFact, ProjectPattern};

use super::graph_model::ContextGraph;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PackageContent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub knowledge: Option<KnowledgeLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph: Option<GraphLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patterns: Option<PatternsLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gotchas: Option<GotchasLayer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_graph: Option<ContextGraph>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeLayer {
    pub facts: Vec<KnowledgeFact>,
    pub patterns: Vec<ProjectPattern>,
    pub insights: Vec<ConsolidatedInsight>,
    pub exported_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphLayer {
    pub nodes: Vec<GraphNodeExport>,
    pub edges: Vec<GraphEdgeExport>,
    pub exported_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphNodeExport {
    pub kind: String,
    pub name: String,
    pub file_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_start: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_end: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphEdgeExport {
    pub source_path: String,
    pub source_name: String,
    pub target_path: String,
    pub target_name: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionLayer {
    pub task_description: Option<String>,
    pub findings: Vec<SessionFinding>,
    pub decisions: Vec<SessionDecision>,
    pub next_steps: Vec<String>,
    pub files_touched: Vec<String>,
    pub exported_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionFinding {
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionDecision {
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatternsLayer {
    pub patterns: Vec<ProjectPattern>,
    pub exported_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GotchasLayer {
    pub gotchas: Vec<GotchaExport>,
    pub exported_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GotchaExport {
    pub id: String,
    pub category: String,
    pub severity: String,
    pub trigger: String,
    pub resolution: String,
    #[serde(default)]
    pub file_patterns: Vec<String>,
    pub confidence: f32,
}

impl PackageContent {
    #[must_use]
    pub fn active_layer_count(&self) -> usize {
        let mut n = 0;
        if self.knowledge.is_some() {
            n += 1;
        }
        if self.graph.is_some() {
            n += 1;
        }
        if self.session.is_some() {
            n += 1;
        }
        if self.patterns.is_some() {
            n += 1;
        }
        if self.gotchas.is_some() {
            n += 1;
        }
        if self.context_graph.is_some() {
            n += 1;
        }
        n
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.active_layer_count() == 0
    }

    #[must_use]
    pub fn estimated_token_count(&self) -> usize {
        let json = serde_json::to_string(self).unwrap_or_default();
        json.len() / 4
    }
}
