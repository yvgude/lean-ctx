//! BuiltinAnalyzer — wraps ModePredictor for read-only efficiency analysis.
//!
//! LEARN phase: observes compression outcomes, recommends modes, never mutates.
//! Emits `IntentClassified` events to OclaBus when analysis is performed.

use crate::core::mode_predictor::{FileSignature, ModePredictor};
use crate::core::ocla::{EfficiencyAnalyzer, EfficiencyEntry};
use crate::core::ocla_bus::{self, OclaEvent};

use std::sync::Mutex;

/// Built-in OCLA EfficiencyAnalyzer wrapping the existing ModePredictor.
pub struct BuiltinAnalyzer {
    predictor: Mutex<ModePredictor>,
}

impl BuiltinAnalyzer {
    pub fn new() -> Self {
        Self {
            predictor: Mutex::new(ModePredictor::new()),
        }
    }

    pub fn with_predictor(predictor: ModePredictor) -> Self {
        Self {
            predictor: Mutex::new(predictor),
        }
    }
}

impl Default for BuiltinAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl EfficiencyAnalyzer for BuiltinAnalyzer {
    fn recommend_mode(&self, path: &str) -> Option<String> {
        let predictor = self
            .predictor
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let sig = FileSignature::from_path(path, 0);
        let mode = predictor.predict_best_mode(&sig);

        if mode.is_some() {
            ocla_bus::emit(OclaEvent::IntentClassified {
                tier: "analysis".into(),
                confidence: 0.8,
                reasoning: format!("mode prediction for {path}"),
            });
        }

        mode
    }

    fn efficiency_score(&self, path: &str) -> f64 {
        let predictor = self
            .predictor
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let sig = FileSignature::from_path(path, 0);
        predictor
            .predict_best_mode(&sig)
            .map(|_| 0.8)
            .unwrap_or(0.5)
    }

    fn summary(&self) -> Vec<EfficiencyEntry> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn analyzer_is_read_only() {
        let analyzer = BuiltinAnalyzer::new();
        let _ = analyzer.recommend_mode("src/main.rs");
        let _ = analyzer.efficiency_score("src/lib.rs");
        let summary = analyzer.summary();
        assert!(summary.is_empty(), "new analyzer has no history");
    }

    #[test]
    fn default_score_is_neutral() {
        let analyzer = BuiltinAnalyzer::new();
        let score = analyzer.efficiency_score("unknown/file.xyz");
        assert!((score - 0.5).abs() < f64::EPSILON, "unknown file = 0.5");
    }
}
