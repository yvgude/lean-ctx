//! Privacy-preserving aggregate outcome learning for model routing.
//!
//! Raw task metrics are converted to a one-way task-shape fingerprint before
//! they enter this module's persistent store.  The store deliberately has no
//! fields for prompts, paths, source, people, teams, or organizations.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::f64::consts::TAU;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

/// The smallest number of comparable outcomes required before routing advice
/// is returned.  This avoids recommendations based on individual customers.
pub const MINIMUM_SAMPLE_SIZE: u32 = 10;

const RNG_INCREMENT: u64 = 0x9e37_79b9_7f4a_7c15;

/// Result class reported by an agent run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OutcomeClass {
    Success,
    PartialSuccess,
    Failure,
    Timeout,
}

impl OutcomeClass {
    fn is_success(self) -> bool {
        matches!(self, Self::Success)
    }
}

/// Raw, local-only measurements supplied by the caller.
///
/// `task_class` may be locally meaningful.  It is hashed by [`anonymize`]
/// and is never retained by [`CrossCustomerLearning`].
#[derive(Debug, Clone)]
pub struct AgentMetrics {
    pub task_class: String,
    pub token_count: u64,
    pub tool_count: u32,
    pub model_used: String,
    pub reasoning_budget: u32,
    pub tokens_consumed: u64,
    pub outcome: OutcomeClass,
    pub latency_ms: u64,
    pub timestamp: u64,
}

/// The complete, privacy-safe persistence schema for one outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnonymizedOutcome {
    pub task_fingerprint: String,
    pub model_used: String,
    pub reasoning_budget: u32,
    pub tokens_consumed: u64,
    pub outcome: OutcomeClass,
    pub latency_ms: u64,
    pub timestamp: u64,
}

/// A routing recommendation generated from aggregate outcomes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Recommendation {
    pub suggested_model: String,
    pub suggested_budget: u32,
    pub confidence: f32,
    pub sample_size: u32,
}

/// Summary suitable for a fleet or dashboard view.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LearningStats {
    pub total_outcomes: u64,
    pub unique_fingerprints: u64,
    pub models_tracked: u64,
    pub avg_success_rate: f32,
    /// Keys are task fingerprints, never raw task classes.
    pub best_model_per_class: HashMap<String, String>,
}

#[derive(Debug, Clone, Default)]
struct BudgetEvidence {
    successes: u32,
    failures: u32,
}

impl BudgetEvidence {
    fn observations(&self) -> u32 {
        self.successes.saturating_add(self.failures)
    }

    fn posterior_mean(&self) -> f32 {
        (self.successes as f32 + 1.0) / (self.observations() as f32 + 2.0)
    }
}

#[derive(Debug, Clone, Default)]
struct ModelEvidence {
    successes: u32,
    failures: u32,
    budgets: BTreeMap<u32, BudgetEvidence>,
}

impl ModelEvidence {
    fn observations(&self) -> u32 {
        self.successes.saturating_add(self.failures)
    }

    fn posterior_mean(&self) -> f32 {
        (self.successes as f32 + 1.0) / (self.observations() as f32 + 2.0)
    }

    fn preferred_budget(&self) -> u32 {
        self.budgets
            .iter()
            .max_by(|(left_budget, left), (right_budget, right)| {
                left.posterior_mean()
                    .total_cmp(&right.posterior_mean())
                    .then_with(|| left.observations().cmp(&right.observations()))
                    .then_with(|| right_budget.cmp(left_budget))
            })
            .map(|(budget, _)| *budget)
            .unwrap_or_default()
    }
}

/// In-memory aggregate intelligence backed by append-only JSONL persistence.
pub struct CrossCustomerLearning {
    outcomes: Vec<AnonymizedOutcome>,
    evidence: HashMap<(String, String), ModelEvidence>,
    persistence_path: PathBuf,
    rng_state: AtomicU64,
}

impl Default for CrossCustomerLearning {
    fn default() -> Self {
        Self::new()
    }
}

impl CrossCustomerLearning {
    /// Opens the default local outcome store, loading valid prior entries.
    pub fn new() -> Self {
        Self::with_path(default_persistence_path())
    }

    /// Opens an explicit store path.  Primarily useful for isolated deployments
    /// and tests; normal callers should use [`Self::new`].
    pub fn with_path(path: impl Into<PathBuf>) -> Self {
        let persistence_path = path.into();
        let mut learning = Self {
            outcomes: Vec::new(),
            evidence: HashMap::new(),
            persistence_path,
            rng_state: AtomicU64::new(RNG_INCREMENT),
        };
        learning.load();
        learning
    }

    /// Converts local metrics into the only data shape eligible for persistence.
    pub fn anonymize(raw_metrics: &AgentMetrics) -> AnonymizedOutcome {
        AnonymizedOutcome {
            task_fingerprint: Self::task_fingerprint(
                &raw_metrics.task_class,
                raw_metrics.token_count,
                raw_metrics.tool_count,
            ),
            model_used: canonical_model_label(&raw_metrics.model_used),
            reasoning_budget: raw_metrics.reasoning_budget,
            tokens_consumed: raw_metrics.tokens_consumed,
            outcome: raw_metrics.outcome,
            latency_ms: raw_metrics.latency_ms,
            timestamp: raw_metrics.timestamp,
        }
    }

    /// Creates a deterministic fingerprint from task shape, without retaining
    /// the raw task class, input, file path, or identifier.
    pub fn task_fingerprint(task_class: &str, token_count: u64, tool_count: u32) -> String {
        let material = format!(
            "{}|{}|{}",
            task_class.trim().to_ascii_lowercase(),
            token_count_bucket(token_count),
            tool_count_bucket(tool_count)
        );
        blake3::hash(material.as_bytes()).to_hex().to_string()
    }

    /// Learns a single anonymized result and appends it to the JSONL store.
    ///
    /// Callers must pass the result of [`Self::anonymize`]; this API accepts the
    /// already-anonymous type so centrally received outcomes need not contain raw
    /// customer metrics.
    pub fn learn(&mut self, outcome: &AnonymizedOutcome) {
        self.record(outcome);
        self.outcomes.push(outcome.clone());
        let _ = self.persist(outcome);
    }

    /// Uses Thompson sampling over Beta(successes + 1, failures + 1) posteriors.
    pub fn recommend(&self, task_fingerprint: &str) -> Option<Recommendation> {
        let sample_size = self.sample_size(task_fingerprint);
        if sample_size < MINIMUM_SAMPLE_SIZE {
            return None;
        }

        let mut models: Vec<_> = self
            .evidence
            .iter()
            .filter(|((fingerprint, _), _)| fingerprint == task_fingerprint)
            .collect();
        models.sort_by(|((_, left), _), ((_, right), _)| left.cmp(right));

        let (_, model, evidence) = models.into_iter().fold(
            None,
            |best: Option<(f64, &String, &ModelEvidence)>, ((_, model), evidence)| {
                let sampled = self.sample_beta(
                    f64::from(evidence.successes) + 1.0,
                    f64::from(evidence.failures) + 1.0,
                );
                match best {
                    Some((best_sample, _, _)) if best_sample >= sampled => best,
                    _ => Some((sampled, model, evidence)),
                }
            },
        )?;

        Some(Recommendation {
            suggested_model: model.clone(),
            suggested_budget: evidence.preferred_budget(),
            confidence: evidence.posterior_mean(),
            sample_size,
        })
    }

    /// Returns aggregate fleet statistics.  `best_model_per_class` is indexed
    /// by privacy-safe task fingerprint rather than the discarded raw class.
    pub fn aggregate_stats(&self) -> LearningStats {
        let total_outcomes = self.outcomes.len() as u64;
        let unique_fingerprints: HashSet<_> = self
            .outcomes
            .iter()
            .map(|outcome| outcome.task_fingerprint.as_str())
            .collect();
        let models_tracked: HashSet<_> = self
            .outcomes
            .iter()
            .map(|outcome| outcome.model_used.as_str())
            .collect();
        let successes = self
            .outcomes
            .iter()
            .filter(|outcome| outcome.outcome.is_success())
            .count() as f32;
        let avg_success_rate = if total_outcomes == 0 {
            0.0
        } else {
            successes / total_outcomes as f32
        };

        let mut best_model_per_class = HashMap::new();
        for fingerprint in &unique_fingerprints {
            if let Some(model) = self.best_model(fingerprint) {
                best_model_per_class.insert((*fingerprint).to_owned(), model);
            }
        }

        LearningStats {
            total_outcomes,
            unique_fingerprints: unique_fingerprints.len() as u64,
            models_tracked: models_tracked.len() as u64,
            avg_success_rate,
            best_model_per_class,
        }
    }

    /// Describes the persistence contract for compliance and dashboard audits.
    pub fn privacy_report(&self) -> String {
        "Stored only: one-way task-shape fingerprint, canonical model label, reasoning budget, \
tokens consumed, outcome class, latency, and timestamp. Never stored: user content, file \
paths, source code, team names, organization names, raw task classes, or raw token/tool \
counts. Fingerprints hash only task class plus token-count and tool-count buckets."
            .to_owned()
    }

    /// Exports an anonymized JSON dashboard summary.
    pub fn export_insights(&self) -> Value {
        let stats = self.aggregate_stats();
        json!({
            "total_outcomes": stats.total_outcomes,
            "unique_fingerprints": stats.unique_fingerprints,
            "models_tracked": stats.models_tracked,
            "avg_success_rate": stats.avg_success_rate,
            "best_model_per_class": stats.best_model_per_class,
            "minimum_sample_size": MINIMUM_SAMPLE_SIZE,
            "privacy_report": self.privacy_report(),
        })
    }

    fn load(&mut self) {
        let Ok(file) = fs::File::open(&self.persistence_path) else {
            return;
        };
        for line in BufReader::new(file).lines().map_while(Result::ok) {
            if let Ok(outcome) = serde_json::from_str::<AnonymizedOutcome>(&line) {
                self.record(&outcome);
                self.outcomes.push(outcome);
            }
        }
    }

    fn persist(&self, outcome: &AnonymizedOutcome) -> std::io::Result<()> {
        if let Some(parent) = self.persistence_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.persistence_path)?;
        serde_json::to_writer(&mut file, outcome)
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        file.write_all(b"\n")
    }

    fn record(&mut self, outcome: &AnonymizedOutcome) {
        let evidence = self
            .evidence
            .entry((outcome.task_fingerprint.clone(), outcome.model_used.clone()))
            .or_default();
        let budget = evidence
            .budgets
            .entry(outcome.reasoning_budget)
            .or_default();
        if outcome.outcome.is_success() {
            evidence.successes = evidence.successes.saturating_add(1);
            budget.successes = budget.successes.saturating_add(1);
        } else {
            evidence.failures = evidence.failures.saturating_add(1);
            budget.failures = budget.failures.saturating_add(1);
        }
    }

    fn sample_size(&self, task_fingerprint: &str) -> u32 {
        self.evidence
            .iter()
            .filter(|((fingerprint, _), _)| fingerprint == task_fingerprint)
            .map(|(_, evidence)| evidence.observations())
            .sum()
    }

    fn best_model(&self, fingerprint: &str) -> Option<String> {
        self.evidence
            .iter()
            .filter(|((candidate, _), _)| candidate == fingerprint)
            .max_by(|((_, left_model), left), ((_, right_model), right)| {
                left.posterior_mean()
                    .total_cmp(&right.posterior_mean())
                    .then_with(|| left.observations().cmp(&right.observations()))
                    .then_with(|| right_model.cmp(left_model))
            })
            .map(|((_, model), _)| model.clone())
    }

    fn sample_beta(&self, alpha: f64, beta: f64) -> f64 {
        let left = self.sample_gamma(alpha);
        let right = self.sample_gamma(beta);
        left / (left + right)
    }

    fn sample_gamma(&self, shape: f64) -> f64 {
        debug_assert!(shape >= 1.0);
        let d = shape - 1.0 / 3.0;
        let c = (1.0 / (9.0 * d)).sqrt();
        loop {
            let x = self.sample_standard_normal();
            let base = 1.0 + c * x;
            if base <= 0.0 {
                continue;
            }
            let v = base * base * base;
            let uniform = self.next_unit();
            if uniform < 1.0 - 0.0331 * x.powi(4)
                || uniform.ln() < 0.5 * x * x + d * (1.0 - v + v.ln())
            {
                return d * v;
            }
        }
    }

    fn sample_standard_normal(&self) -> f64 {
        let radius = (-2.0 * self.next_unit().ln()).sqrt();
        radius * (TAU * self.next_unit()).cos()
    }

    fn next_unit(&self) -> f64 {
        let mut value = self.rng_state.fetch_add(RNG_INCREMENT, Ordering::Relaxed);
        value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        value ^= value >> 31;
        ((value >> 11) as f64 + 1.0) / ((1_u64 << 53) as f64 + 2.0)
    }
}

/// Convenience form of [`CrossCustomerLearning::anonymize`].
pub fn anonymize(raw_metrics: &AgentMetrics) -> AnonymizedOutcome {
    CrossCustomerLearning::anonymize(raw_metrics)
}

fn default_persistence_path() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".local/share/lean-ctx/learning/outcomes.jsonl")
}

fn token_count_bucket(token_count: u64) -> &'static str {
    match token_count {
        0..=499 => "tiny",
        500..=1_999 => "small",
        2_000..=7_999 => "medium",
        8_000..=31_999 => "large",
        _ => "huge",
    }
}

fn tool_count_bucket(tool_count: u32) -> &'static str {
    match tool_count {
        0 => "none",
        1..=2 => "few",
        3..=10 => "several",
        _ => "many",
    }
}

fn canonical_model_label(model: &str) -> String {
    let lower = model.trim().to_ascii_lowercase();
    let mut candidates = lower.split(|character| matches!(character, '/' | '\\' | ':' | '@' | ' '));
    let candidate = candidates
        .find(|candidate| is_public_model_label(candidate))
        .unwrap_or_default();
    if candidate.is_empty() {
        format!("model-{}", blake3::hash(lower.as_bytes()).to_hex())
    } else {
        candidate.to_owned()
    }
}

fn is_public_model_label(candidate: &str) -> bool {
    let known_family = [
        "gpt", "o1", "o3", "o4", "claude", "gemini", "llama", "mistral", "deepseek", "qwen",
        "codex",
    ]
    .iter()
    .any(|prefix| candidate == *prefix || candidate.starts_with(&format!("{prefix}-")));
    known_family
        && candidate.len() <= 64
        && candidate.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'.')
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "lean-ctx-cross-customer-learning-{name}-{}-{}.jsonl",
            std::process::id(),
            blake3::hash(name.as_bytes()).to_hex()
        ))
    }

    fn outcome(
        fingerprint: &str,
        model: &str,
        budget: u32,
        result: OutcomeClass,
    ) -> AnonymizedOutcome {
        AnonymizedOutcome {
            task_fingerprint: fingerprint.to_owned(),
            model_used: model.to_owned(),
            reasoning_budget: budget,
            tokens_consumed: 100,
            outcome: result,
            latency_ms: 20,
            timestamp: 1,
        }
    }

    #[test]
    fn anonymization_removes_all_pii() {
        let raw = AgentMetrics {
            task_class: "review /Users/alice/acme/secret.rs for Team Phoenix".to_owned(),
            token_count: 2_500,
            tool_count: 3,
            model_used: "Acme/gpt-5.4".to_owned(),
            reasoning_budget: 4_000,
            tokens_consumed: 2_100,
            outcome: OutcomeClass::Success,
            latency_ms: 42,
            timestamp: 123,
        };

        let anonymized = anonymize(&raw);
        let stored = serde_json::to_string(&anonymized).unwrap();
        for forbidden in ["alice", "acme", "secret.rs", "phoenix", "/users"] {
            assert!(!stored.to_ascii_lowercase().contains(forbidden));
        }
        assert_eq!(anonymized.model_used, "gpt-5.4");
        assert_eq!(anonymized.task_fingerprint.len(), 64);
    }

    #[test]
    fn thompson_sampling_converges_to_best_model() {
        let path = test_path("converges");
        let _ = fs::remove_file(&path);
        let mut learning = CrossCustomerLearning::with_path(&path);
        let fingerprint = "routing-shape";
        for _ in 0..100 {
            learning.learn(&outcome(
                fingerprint,
                "model-a",
                4_000,
                OutcomeClass::Success,
            ));
            learning.learn(&outcome(
                fingerprint,
                "model-b",
                4_000,
                OutcomeClass::Failure,
            ));
        }

        let best_count = (0..25)
            .filter(|_| learning.recommend(fingerprint).unwrap().suggested_model == "model-a")
            .count();
        assert!(best_count >= 24);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn minimum_sample_size_is_enforced() {
        let path = test_path("minimum-samples");
        let _ = fs::remove_file(&path);
        let mut learning = CrossCustomerLearning::with_path(&path);
        for _ in 0..MINIMUM_SAMPLE_SIZE - 1 {
            learning.learn(&outcome("shape", "model-a", 1_000, OutcomeClass::Success));
        }
        assert!(learning.recommend("shape").is_none());

        learning.learn(&outcome("shape", "model-a", 1_000, OutcomeClass::Success));
        assert!(learning.recommend("shape").is_some());
        let _ = fs::remove_file(path);
    }

    #[test]
    fn recommendation_improves_with_more_data() {
        let path = test_path("improves");
        let _ = fs::remove_file(&path);
        let mut learning = CrossCustomerLearning::with_path(&path);
        for _ in 0..10 {
            learning.learn(&outcome("shape", "model-a", 8_000, OutcomeClass::Success));
            learning.learn(&outcome("shape", "model-b", 2_000, OutcomeClass::Failure));
        }
        let initial = learning.recommend("shape").unwrap();
        assert_eq!(initial.suggested_model, "model-a");
        for _ in 0..50 {
            learning.learn(&outcome("shape", "model-a", 8_000, OutcomeClass::Success));
        }
        let improved = learning.recommend("shape").unwrap();
        assert_eq!(improved.suggested_model, "model-a");
        assert!(improved.confidence > initial.confidence);
        assert_eq!(improved.suggested_budget, 8_000);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn fingerprint_bucketing_is_deterministic() {
        let first = CrossCustomerLearning::task_fingerprint("code-review", 499, 2);
        let same_bucket = CrossCustomerLearning::task_fingerprint("CODE-REVIEW", 1, 1);
        let next_bucket = CrossCustomerLearning::task_fingerprint("code-review", 500, 2);
        assert_eq!(first, same_bucket);
        assert_ne!(first, next_bucket);
    }

    #[test]
    fn outcomes_persist_as_jsonl() {
        let path = test_path("persistence");
        let _ = fs::remove_file(&path);
        let mut learning = CrossCustomerLearning::with_path(&path);
        learning.learn(&outcome("shape", "model-a", 1_000, OutcomeClass::Success));
        drop(learning);

        let restored = CrossCustomerLearning::with_path(&path);
        assert_eq!(restored.aggregate_stats().total_outcomes, 1);
        let _ = fs::remove_file(path);
    }
}
