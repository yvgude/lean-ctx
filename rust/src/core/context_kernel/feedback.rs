//! Outcome feedback collection for Context Kernel learning.

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use super::types::{ContextReceiptV1, ReceiptOutcome};

const LEARNING_RATE: f64 = 0.1;
const DEFAULT_PROVIDER_WEIGHT: f64 = 1.0;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FeedbackEntry {
    pub plan_id: String,
    pub outcome: String,
    pub provider_scores: HashMap<String, f64>,
    pub timestamp_epoch: u64,
}

pub struct FeedbackCollector {
    log_path: PathBuf,
    provider_weights: HashMap<String, f64>,
}

impl FeedbackCollector {
    pub fn new(log_path: PathBuf) -> Self {
        Self {
            log_path,
            provider_weights: HashMap::new(),
        }
    }

    pub fn default_for_project(_project_root: &str) -> Self {
        let cache_root = crate::core::paths::cache_dir()
            .unwrap_or_else(|_| {
                std::env::var_os("HOME")
                    .map_or_else(|| PathBuf::from("."), PathBuf::from)
                    .join(".cache")
                    .join("lean-ctx")
            })
            .join("kernel");
        Self::new(cache_root.join("feedback.jsonl"))
    }

    pub fn record_outcome(&mut self, receipt: &ContextReceiptV1) {
        let score = outcome_score(&receipt.outcome);
        let provider_scores: HashMap<String, f64> = receipt
            .feedback_attribution
            .keys()
            .map(|provider| (provider.clone(), score))
            .collect();

        for (provider, provider_score) in &provider_scores {
            update_weight(&mut self.provider_weights, provider, *provider_score);
        }

        let entry = FeedbackEntry {
            plan_id: receipt.plan_id.clone(),
            outcome: outcome_name(&receipt.outcome).to_owned(),
            provider_scores,
            timestamp_epoch: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |duration| duration.as_secs()),
        };

        let Some(parent) = self.log_path.parent() else {
            return;
        };
        if fs::create_dir_all(parent).is_err() {
            return;
        }
        let Ok(serialized) = serde_json::to_string(&entry) else {
            return;
        };
        if let Ok(mut file) = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_path)
        {
            let _ = writeln!(file, "{serialized}");
        }
    }

    pub fn provider_weight(&self, provider: &str) -> f64 {
        self.provider_weights
            .get(provider)
            .copied()
            .unwrap_or(DEFAULT_PROVIDER_WEIGHT)
    }

    pub fn load_weights(&mut self) {
        self.provider_weights.clear();
        let Ok(contents) = fs::read_to_string(&self.log_path) else {
            return;
        };

        for line in contents.lines() {
            if let Ok(entry) = serde_json::from_str::<FeedbackEntry>(line) {
                for (provider, score) in entry.provider_scores {
                    update_weight(&mut self.provider_weights, &provider, score);
                }
            }
        }
    }
}

pub fn record_kernel_feedback(project_root: &str, receipt: &ContextReceiptV1) {
    let mut collector = FeedbackCollector::default_for_project(project_root);
    collector.load_weights();
    collector.record_outcome(receipt);
}

fn update_weight(weights: &mut HashMap<String, f64>, provider: &str, score: f64) {
    let current = weights
        .get(provider)
        .copied()
        .unwrap_or(DEFAULT_PROVIDER_WEIGHT);
    weights.insert(
        provider.to_owned(),
        current * (1.0 - LEARNING_RATE) + score * LEARNING_RATE,
    );
}

fn outcome_score(outcome: &ReceiptOutcome) -> f64 {
    match outcome {
        ReceiptOutcome::Accepted => 1.0,
        ReceiptOutcome::Partial => 0.5,
        ReceiptOutcome::Unknown => 0.2,
        ReceiptOutcome::Rejected => 0.0,
    }
}

fn outcome_name(outcome: &ReceiptOutcome) -> &'static str {
    match outcome {
        ReceiptOutcome::Accepted => "accepted",
        ReceiptOutcome::Partial => "partial",
        ReceiptOutcome::Unknown => "unknown",
        ReceiptOutcome::Rejected => "rejected",
    }
}

#[cfg(test)]
pub mod tests {
    use std::collections::HashMap;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::FeedbackCollector;
    use crate::core::context_kernel::types::{ContextReceiptV1, ReceiptOutcome};

    static NEXT_TEST_PATH: AtomicUsize = AtomicUsize::new(0);

    fn test_log_path(test_name: &str) -> PathBuf {
        let sequence = NEXT_TEST_PATH.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "lean-ctx-feedback-{}-{test_name}-{sequence}.jsonl",
            std::process::id()
        ))
    }

    fn receipt(outcome: ReceiptOutcome) -> ContextReceiptV1 {
        ContextReceiptV1 {
            receipt_id: "receipt-1".to_owned(),
            plan_id: "plan-1".to_owned(),
            task_id: None,
            delivered_tokens: 100,
            cache_hits: 0,
            cache_misses: 0,
            outcome,
            quality_signals: Vec::new(),
            feedback_attribution: HashMap::from([("files".to_owned(), 1.0)]),
        }
    }

    #[test]
    fn default_for_project_honors_cache_dir_override() {
        let _env_lock = crate::core::data_dir::test_env_lock();
        let home = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();

        let previous_home = std::env::var_os("HOME");
        let previous_cache = std::env::var_os("LEAN_CTX_CACHE_DIR");

        crate::test_env::set_var("HOME", home.path());
        crate::test_env::set_var("LEAN_CTX_CACHE_DIR", cache.path());

        let expected = cache.path().join("kernel").join("feedback.jsonl");
        let legacy = home
            .path()
            .join(".cache")
            .join("lean-ctx")
            .join("kernel")
            .join("feedback.jsonl");

        let mut collector = FeedbackCollector::default_for_project("project");
        collector.record_outcome(&receipt(ReceiptOutcome::Rejected));

        let expected_exists = expected.exists();
        let legacy_exists = legacy.exists();

        match previous_home {
            Some(value) => crate::test_env::set_var("HOME", value),
            None => crate::test_env::remove_var("HOME"),
        }

        match previous_cache {
            Some(value) => crate::test_env::set_var("LEAN_CTX_CACHE_DIR", value),
            None => crate::test_env::remove_var("LEAN_CTX_CACHE_DIR"),
        }

        assert!(
            expected_exists,
            "Context Kernel feedback must honor LEAN_CTX_CACHE_DIR"
        );
        assert!(
            !legacy_exists,
            "Context Kernel feedback must not bypass the central cache resolver"
        );
    }

    #[test]
    fn accepted_outcome_increases_provider_weight() {
        let path = test_log_path("accepted");
        let mut collector = FeedbackCollector::new(path.clone());
        collector.provider_weights.insert("files".to_owned(), 0.5);

        collector.record_outcome(&receipt(ReceiptOutcome::Accepted));

        assert!(collector.provider_weight("files") > 0.5);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejected_outcome_decreases_provider_weight() {
        let path = test_log_path("rejected");
        let mut collector = FeedbackCollector::new(path.clone());

        collector.record_outcome(&receipt(ReceiptOutcome::Rejected));

        assert!(collector.provider_weight("files") < 1.0);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn feedback_persists_to_file() {
        let path = test_log_path("persists");
        let mut collector = FeedbackCollector::new(path.clone());
        collector.record_outcome(&receipt(ReceiptOutcome::Rejected));

        let mut restored = FeedbackCollector::new(path.clone());
        restored.load_weights();

        assert!((restored.provider_weight("files") - 0.9).abs() < f64::EPSILON);
        assert!(fs::read_to_string(&path).is_ok());
        let _ = fs::remove_file(path);
    }
}
