pub mod accuracy_tracker;
pub mod calibration;
#[cfg(test)]
mod calibration_tests;
pub mod confidence;
pub mod distillation;
#[cfg(test)]
mod distillation_tests;
pub mod fusion;
pub mod markdown;
pub mod model_loader;
pub mod profile;
pub mod rules;
pub mod semantic_analyzer;
pub mod validation;
#[cfg(test)]
mod validation_tests;

use profile::TaskProfileLocal;
use std::thread;
use std::{fmt, sync::Arc};

/// Analyzes task inputs into ranked profile hypotheses.
pub trait TaskAnalyzer: std::fmt::Debug + Send + Sync {
    fn analyze(&self, input: &TaskAnalysisInput) -> Result<ProfileHypothesis, TriageError>;
    fn name(&self) -> &'static str;

    /// Marks an observation-only analyzer that may run beside rules in shadow mode.
    fn shadow_enabled(&self) -> bool {
        false
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
/// Captures the signals used to classify a task.
pub struct TaskAnalysisInput {
    pub query: String,
    pub files_touched: Vec<String>,
    pub active_diagnostics: usize,
    pub session_context: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
/// Describes one task-profile classification hypothesis.
pub struct ProfileHypothesis {
    pub profile: TaskProfileLocal,
    pub confidence_milli: u16,
    pub backend: TriageBackendLocal,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
/// Identifies the backend that produced a triage hypothesis.
pub enum TriageBackendLocal {
    #[default]
    Rules,
    Semantic,
    Hybrid,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Represents a task-triage failure.
pub enum TriageError {
    NoSignal,
    ModelUnavailable,
    InternalError(String),
}

#[derive(Debug, Clone)]
/// Orchestrates analyzers to select the strongest task profile.
pub struct TriageEngine {
    pub analyzers: Vec<Arc<dyn TaskAnalyzer>>,
}

impl fmt::Display for TriageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoSignal => write!(f, "no triage signal available"),
            Self::ModelUnavailable => write!(f, "triage model unavailable"),
            Self::InternalError(error) => write!(f, "triage internal error: {error}"),
        }
    }
}

impl std::error::Error for TriageError {}

impl TriageEngine {
    pub fn new(analyzers: Vec<Box<dyn TaskAnalyzer>>) -> Self {
        Self {
            analyzers: analyzers.into_iter().map(Arc::from).collect(),
        }
    }

    pub fn with_rules() -> Self {
        Self::new(vec![Box::new(rules::RuleTriageBackend)])
    }

    #[allow(clippy::match_same_arms)]
    pub fn analyze(&self, input: &TaskAnalysisInput) -> Result<ProfileHypothesis, TriageError> {
        if let Some((rules, semantic)) = self.shadow_pair() {
            let (rules_result, semantic_result) = thread::scope(|scope| {
                let rules_task = scope.spawn(|| rules.analyze(input));
                let semantic_task = scope.spawn(|| semantic.analyze(input));
                (rules_task.join(), semantic_task.join())
            });
            let rules_result = rules_result.map_err(|_| {
                TriageError::InternalError("rules triage worker panicked".to_string())
            })?;
            let semantic_result = semantic_result.map_err(|_| {
                TriageError::InternalError("semantic triage worker panicked".to_string())
            })?;
            if let (Ok(rules), Ok(semantic)) = (&rules_result, &semantic_result) {
                if let Err(error) = accuracy_tracker::record_comparison(input, rules, semantic) {
                    tracing::warn!(%error, "failed to persist semantic triage shadow comparison");
                }
            }
            // Rules remain the sole execution authority while semantic is shadowed.
            return rules_result;
        }
        let mut best: Option<ProfileHypothesis> = None;
        let mut first_error = None;
        for analyzer in &self.analyzers {
            match analyzer.analyze(input) {
                Ok(candidate)
                    if best.as_ref().is_none_or(|current: &ProfileHypothesis| {
                        candidate.confidence_milli > current.confidence_milli
                    }) =>
                {
                    best = Some(candidate);
                }
                Ok(_) => {}
                Err(error) if first_error.is_none() => {
                    first_error = Some(error);
                }
                Err(_) => {}
            }
        }
        best.ok_or_else(|| first_error.unwrap_or(TriageError::NoSignal))
    }

    fn shadow_pair(&self) -> Option<(&Arc<dyn TaskAnalyzer>, &Arc<dyn TaskAnalyzer>)> {
        let rules = self
            .analyzers
            .iter()
            .find(|analyzer| analyzer.name() == "rules")?;
        let semantic = self
            .analyzers
            .iter()
            .find(|analyzer| analyzer.name() == "semantic" && analyzer.shadow_enabled())?;
        Some((rules, semantic))
    }
}

impl Default for TriageEngine {
    fn default() -> Self {
        Self::with_rules()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct FixedAnalyzer {
        name: &'static str,
        result: Result<ProfileHypothesis, TriageError>,
        shadow: bool,
    }

    impl TaskAnalyzer for FixedAnalyzer {
        fn analyze(&self, _: &TaskAnalysisInput) -> Result<ProfileHypothesis, TriageError> {
            self.result.clone()
        }

        fn name(&self) -> &'static str {
            self.name
        }
        fn shadow_enabled(&self) -> bool {
            self.shadow
        }
    }

    fn hypothesis(intent: &str, backend: TriageBackendLocal) -> ProfileHypothesis {
        ProfileHypothesis {
            profile: TaskProfileLocal {
                intent: intent.into(),
                ..Default::default()
            },
            confidence_milli: 500,
            backend,
        }
    }

    #[test]
    fn shadow_parallel_logs_and_keeps_rules_result() {
        let data = crate::core::data_dir::isolated_data_dir();
        let engine = TriageEngine::new(vec![
            Box::new(FixedAnalyzer {
                name: "rules",
                result: Ok(hypothesis("rules", TriageBackendLocal::Rules)),
                shadow: false,
            }),
            Box::new(FixedAnalyzer {
                name: "semantic",
                result: Ok(hypothesis("semantic", TriageBackendLocal::Semantic)),
                shadow: true,
            }),
        ]);
        let result = engine
            .analyze(&TaskAnalysisInput {
                query: "task".into(),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(result.backend, TriageBackendLocal::Rules);
        assert!(data.path().join("triage_shadow.jsonl").is_file());
    }

    #[test]
    fn shadow_parallel_skips_log_when_semantic_is_unavailable() {
        let data = crate::core::data_dir::isolated_data_dir();
        let engine = TriageEngine::new(vec![
            Box::new(FixedAnalyzer {
                name: "rules",
                result: Ok(hypothesis("rules", TriageBackendLocal::Rules)),
                shadow: false,
            }),
            Box::new(FixedAnalyzer {
                name: "semantic",
                result: Err(TriageError::ModelUnavailable),
                shadow: true,
            }),
        ]);
        engine.analyze(&TaskAnalysisInput::default()).unwrap();
        assert!(!data.path().join("triage_shadow.jsonl").exists());
    }
}
