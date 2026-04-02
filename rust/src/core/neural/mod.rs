//! Neural context compression — trained models replacing heuristic filters.
//!
//! Feature-gated under `#[cfg(feature = "neural")]`.
//! When an ONNX model is present, switches from heuristic to neural scoring.
//! Falls back gracefully to heuristic mode when no model is available.

pub mod attention_learned;
pub mod cache_alignment;
pub mod context_reorder;
pub mod line_scorer;
pub mod token_optimizer;

use std::path::PathBuf;

use attention_learned::LearnedAttention;
use line_scorer::NeuralLineScorer;
use token_optimizer::TokenOptimizer;

pub struct NeuralEngine {
    line_scorer: Option<NeuralLineScorer>,
    token_optimizer: TokenOptimizer,
    attention: LearnedAttention,
}

impl NeuralEngine {
    pub fn load() -> Self {
        let model_dir = Self::model_directory();

        let line_scorer = if model_dir.join("line_importance.onnx").exists() {
            match NeuralLineScorer::load(&model_dir.join("line_importance.onnx")) {
                Ok(scorer) => {
                    tracing::info!("Neural line scorer loaded from {:?}", model_dir);
                    Some(scorer)
                }
                Err(e) => {
                    tracing::warn!(
                        "Failed to load neural line scorer: {e}. Using heuristic fallback."
                    );
                    None
                }
            }
        } else {
            tracing::debug!("No ONNX model found, using heuristic line scoring");
            None
        };

        let token_optimizer = TokenOptimizer::load_or_default(&model_dir);
        let attention = LearnedAttention::load_or_default(&model_dir);

        Self {
            line_scorer,
            token_optimizer,
            attention,
        }
    }

    pub fn score_line(&self, line: &str, position: f64, task_keywords: &[String]) -> f64 {
        if let Some(ref scorer) = self.line_scorer {
            scorer.score_line(line, position, task_keywords)
        } else {
            self.heuristic_score(line, position)
        }
    }

    pub fn optimize_line(&self, line: &str) -> String {
        self.token_optimizer.optimize_line(line)
    }

    pub fn attention_weight(&self, position: f64) -> f64 {
        self.attention.weight(position)
    }

    pub fn has_neural_model(&self) -> bool {
        self.line_scorer.is_some()
    }

    fn heuristic_score(&self, line: &str, position: f64) -> f64 {
        let structural = super::attention_model::structural_importance(line);
        let positional = self.attention.weight(position);
        (structural * positional).sqrt()
    }

    fn model_directory() -> PathBuf {
        if let Ok(dir) = std::env::var("LEAN_CTX_MODELS_DIR") {
            return PathBuf::from(dir);
        }

        if let Some(data_dir) = dirs::data_dir() {
            return data_dir.join("lean-ctx").join("models");
        }

        PathBuf::from("models")
    }
}
