//! The portable context-package bundle format (#293).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::core::session::{Decision, FileTouched, Finding, TaskInfo, TestSnapshot};
use crate::core::session_summary::SummaryRecord;

pub const FORMAT_VERSION: u32 = 1;

/// A portable, self-contained context package.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextPackage {
    pub format_version: u32,
    pub created_at: DateTime<Utc>,
    pub project_root: String,
    pub session_id: String,
    pub metadata: PackageMetadata,
    pub session: SessionSlice,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub summaries: Vec<SummaryRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub knowledge: Vec<KnowledgeFact>,
}

/// Human-readable metadata about the package.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageMetadata {
    pub agent_id: Option<String>,
    pub description: Option<String>,
    pub tool_calls: u32,
    pub tokens_saved: u64,
}

/// The essential slice of session state to restore.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSlice {
    pub task: Option<TaskInfo>,
    pub findings: Vec<Finding>,
    pub decisions: Vec<Decision>,
    pub files: Vec<FileTouched>,
    pub next_steps: Vec<String>,
    pub test_results: Option<TestSnapshot>,
}

/// A knowledge fact (compact, portable representation).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeFact {
    pub category: String,
    pub key: String,
    pub value: String,
    pub confidence: f32,
    pub created_at: DateTime<Utc>,
}

impl ContextPackage {
    #[must_use]
    pub fn is_compatible(&self) -> bool {
        self.format_version <= FORMAT_VERSION
    }

    #[must_use]
    pub fn summary_line(&self) -> String {
        let desc = self
            .metadata
            .description
            .as_deref()
            .or(self.session.task.as_ref().map(|t| t.description.as_str()))
            .unwrap_or("(no description)");
        format!(
            "[{}] {} — {} files, {} decisions, {} summaries, {} facts",
            self.session_id
                .split('-')
                .next()
                .unwrap_or(&self.session_id),
            desc,
            self.session.files.len(),
            self.session.decisions.len(),
            self.summaries.len(),
            self.knowledge.len()
        )
    }
}
