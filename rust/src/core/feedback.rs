use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

use serde::{Deserialize, Serialize};

const FEEDBACK_FLUSH_SECS: u64 = 60;

static FEEDBACK_BUFFER: Mutex<Option<(FeedbackStore, Instant)>> = Mutex::new(None);

/// Feedback loop for learning optimal compression parameters.
///
/// Tracks compression outcomes per session and learns which
/// threshold combinations lead to fewer turns and higher success rates.

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CompressionOutcome {
    pub session_id: String,
    pub language: String,
    pub entropy_threshold: f64,
    pub jaccard_threshold: f64,
    pub total_turns: u32,
    pub tokens_saved: u64,
    pub tokens_original: u64,
    pub cache_hits: u32,
    pub total_reads: u32,
    pub task_completed: bool,
    pub timestamp: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FeedbackStore {
    pub outcomes: Vec<CompressionOutcome>,
    pub learned_thresholds: HashMap<String, LearnedThresholds>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LearnedThresholds {
    pub entropy: f64,
    pub jaccard: f64,
    pub sample_count: u32,
    pub avg_efficiency: f64,
}

impl FeedbackStore {
    pub fn load() -> Self {
        let guard = FEEDBACK_BUFFER
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((ref store, _)) = *guard {
            return store.clone();
        }
        drop(guard);

        let path = feedback_path();
        if path.exists() {
            if let Ok(content) = std::fs::read_to_string(&path) {
                if let Ok(store) = serde_json::from_str::<FeedbackStore>(&content) {
                    return store;
                }
            }
        }
        Self::default()
    }

    fn save_to_disk(&self) {
        let path = feedback_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(path, json);
        }
    }

    pub fn save(&self) {
        self.save_to_disk();
    }

    pub fn flush() {
        let guard = FEEDBACK_BUFFER
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((ref store, _)) = *guard {
            store.save_to_disk();
        }
    }

    pub fn record_outcome(&mut self, outcome: CompressionOutcome) {
        let lang = outcome.language.clone();
        self.outcomes.push(outcome);

        if self.outcomes.len() > 200 {
            self.outcomes.drain(0..self.outcomes.len() - 200);
        }

        self.update_learned_thresholds(&lang);

        let mut guard = FEEDBACK_BUFFER
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let should_flush = match *guard {
            Some((_, ref last)) => last.elapsed().as_secs() >= FEEDBACK_FLUSH_SECS,
            None => true,
        };
        *guard = Some((
            self.clone(),
            guard.as_ref().map_or_else(Instant::now, |(_, t)| *t),
        ));
        if should_flush {
            self.save_to_disk();
            if let Some((_, ref mut t)) = *guard {
                *t = Instant::now();
            }
        }
    }

    fn update_learned_thresholds(&mut self, language: &str) {
        let relevant: Vec<&CompressionOutcome> = self
            .outcomes
            .iter()
            .filter(|o| o.language == language && o.task_completed)
            .collect();

        if relevant.len() < 5 {
            return; // not enough data to learn
        }

        // Find the threshold combination that maximizes efficiency
        // Efficiency = tokens_saved / tokens_original * (1 / total_turns)
        let mut best_entropy = 1.0;
        let mut best_jaccard = 0.7;
        let mut best_efficiency = 0.0;

        for outcome in &relevant {
            let compression_ratio = if outcome.tokens_original > 0 {
                outcome.tokens_saved as f64 / outcome.tokens_original as f64
            } else {
                0.0
            };
            let turn_efficiency = 1.0 / (outcome.total_turns.max(1) as f64);
            let efficiency = compression_ratio * 0.6 + turn_efficiency * 0.4;

            if efficiency > best_efficiency {
                best_efficiency = efficiency;
                best_entropy = outcome.entropy_threshold;
                best_jaccard = outcome.jaccard_threshold;
            }
        }

        // Weighted average with current learned values for stability
        let entry = self
            .learned_thresholds
            .entry(language.to_string())
            .or_insert(LearnedThresholds {
                entropy: best_entropy,
                jaccard: best_jaccard,
                sample_count: 0,
                avg_efficiency: 0.0,
            });

        let momentum = 0.7;
        let old_entropy = entry.entropy;
        let old_jaccard = entry.jaccard;
        entry.entropy = entry.entropy * momentum + best_entropy * (1.0 - momentum);
        entry.jaccard = entry.jaccard * momentum + best_jaccard * (1.0 - momentum);
        entry.sample_count = relevant.len() as u32;
        entry.avg_efficiency = best_efficiency;

        if (old_entropy - entry.entropy).abs() > 0.01 || (old_jaccard - entry.jaccard).abs() > 0.01
        {
            crate::core::events::emit(crate::core::events::EventKind::ThresholdShift {
                language: language.to_string(),
                old_entropy,
                new_entropy: entry.entropy,
                old_jaccard,
                new_jaccard: entry.jaccard,
            });
        }
    }

    pub fn get_learned_entropy(&self, language: &str) -> Option<f64> {
        self.learned_thresholds.get(language).map(|t| t.entropy)
    }

    pub fn get_learned_jaccard(&self, language: &str) -> Option<f64> {
        self.learned_thresholds.get(language).map(|t| t.jaccard)
    }

    pub fn format_report(&self) -> String {
        let mut lines = vec![String::from("Feedback Loop Report")];
        lines.push(format!("Total outcomes tracked: {}", self.outcomes.len()));
        lines.push(String::new());

        if self.learned_thresholds.is_empty() {
            lines.push(
                "No learned thresholds yet (need 5+ completed sessions per language).".to_string(),
            );
        } else {
            lines.push("Learned Thresholds:".to_string());
            for (lang, t) in &self.learned_thresholds {
                lines.push(format!(
                    "  {lang}: entropy={:.2} jaccard={:.2} (n={}, eff={:.1}%)",
                    t.entropy,
                    t.jaccard,
                    t.sample_count,
                    t.avg_efficiency * 100.0
                ));
            }
        }

        lines.join("\n")
    }
}

fn feedback_path() -> std::path::PathBuf {
    crate::core::data_dir::lean_ctx_data_dir()
        .unwrap_or_else(|_| std::path::PathBuf::from("."))
        .join("feedback.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_store_loads() {
        let store = FeedbackStore::default();
        assert!(store.outcomes.is_empty());
        assert!(store.learned_thresholds.is_empty());
    }

    #[test]
    fn learned_thresholds_need_minimum_samples() {
        let mut store = FeedbackStore::default();
        for i in 0..3 {
            store.record_outcome(CompressionOutcome {
                session_id: format!("s{i}"),
                language: "rs".to_string(),
                entropy_threshold: 0.85,
                jaccard_threshold: 0.72,
                total_turns: 5,
                tokens_saved: 1000,
                tokens_original: 2000,
                cache_hits: 3,
                total_reads: 10,
                task_completed: true,
                timestamp: String::new(),
            });
        }
        assert!(store.get_learned_entropy("rs").is_none()); // only 3, need 5
    }
}
