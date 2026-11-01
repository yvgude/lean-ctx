// SPDX-License-Identifier: Apache-2.0

//! Stable task metadata used to group outcome observations.

use serde::{Deserialize, Serialize};

use super::contracts::TaskClass;

/// Task context captured alongside an outcome evaluation.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TaskFingerprint {
    pub language: Option<String>,
    pub task_class: TaskClass,
    pub complexity: String,
    pub repo_size: Option<String>,
    pub context_warmth: Option<String>,
    pub changed_loc: Option<u32>,
    pub tests_available: bool,
    pub risk_class: Option<String>,
    pub agent: Option<String>,
    pub model: Option<String>,
    pub provider: Option<String>,
}

impl TaskFingerprint {
    /// Create a fingerprint with only its class and complexity known.
    pub fn new(task_class: TaskClass, complexity: impl Into<String>) -> Self {
        Self {
            language: None,
            task_class,
            complexity: complexity.into(),
            repo_size: None,
            context_warmth: None,
            changed_loc: None,
            tests_available: false,
            risk_class: None,
            agent: None,
            model: None,
            provider: None,
        }
    }

    /// Produce a deterministic, human-readable grouping key.
    pub fn grouping_key(&self) -> String {
        format!(
            "{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}",
            self.language.as_deref().unwrap_or("unknown"),
            self.task_class,
            self.complexity,
            self.repo_size.as_deref().unwrap_or("unknown"),
            self.context_warmth.as_deref().unwrap_or("unknown"),
            self.changed_loc
                .map_or_else(|| "unknown".to_owned(), |value| value.to_string()),
            self.tests_available,
            self.risk_class.as_deref().unwrap_or("unknown"),
            self.agent.as_deref().unwrap_or("unknown"),
            self.model.as_deref().unwrap_or("unknown"),
            self.provider.as_deref().unwrap_or("unknown"),
        )
    }
}

impl Default for TaskFingerprint {
    fn default() -> Self {
        Self::new(TaskClass::BugFix, "unknown")
    }
}
