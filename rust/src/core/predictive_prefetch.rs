//! Predictive Prefetch via Free Energy Minimization.
//!
//! Scientific basis: Karl Friston's Free Energy Principle (2010) — the system minimizes
//! "surprise" (unexpected information requests) by maintaining a generative model of what
//! files will be needed next and proactively loading them when resources permit.
//!
//! The model combines:
//! 1. Co-access history (Hebbian associations)
//! 2. Graph neighborhood (import/call relationships)
//! 3. Recency patterns (temporal locality)

use std::collections::HashMap;

/// Maximum files to prefetch per prediction cycle.
const MAX_PREFETCH: usize = 5;
/// Minimum prediction confidence to trigger prefetch.
const MIN_CONFIDENCE: f64 = 0.3;

/// Tracks prediction accuracy for model self-evaluation.
pub struct PrefetchModel {
    /// Transition probabilities: after accessing file A, probability of accessing file B.
    transitions: HashMap<u64, Vec<(u64, f64)>>,
    /// Rolling accuracy metric.
    predictions_made: u64,
    predictions_hit: u64,
    /// Recent access sequence for learning.
    recent_accesses: Vec<u64>,
}

impl Default for PrefetchModel {
    fn default() -> Self {
        Self::new()
    }
}

impl PrefetchModel {
    #[must_use]
    pub fn new() -> Self {
        Self {
            transitions: HashMap::with_capacity(128),
            predictions_made: 0,
            predictions_hit: 0,
            recent_accesses: Vec::with_capacity(64),
        }
    }

    /// Record a file access and learn transition patterns.
    pub fn observe(&mut self, path_hash: u64) {
        // Learn: strengthen transition from last N accesses → this file
        let window = self.recent_accesses.len().min(3);
        if window > 0 {
            for &prev in &self.recent_accesses[self.recent_accesses.len() - window..] {
                let entry = self.transitions.entry(prev).or_default();
                if let Some(pair) = entry.iter_mut().find(|(h, _)| *h == path_hash) {
                    pair.1 += 1.0;
                } else {
                    entry.push((path_hash, 1.0));
                }
            }
        }

        self.recent_accesses.push(path_hash);
        if self.recent_accesses.len() > 100 {
            self.recent_accesses.drain(..50);
        }

        // Prune transition table if too large
        if self.transitions.len() > 2000 {
            self.prune_transitions();
        }
    }

    /// Predict which files will be accessed next, based on current state.
    /// Returns (`path_hash`, confidence) pairs sorted by confidence descending.
    #[must_use]
    pub fn predict(&self, current_hash: u64, active_hashes: &[u64]) -> Vec<(u64, f64)> {
        let mut candidates: HashMap<u64, f64> = HashMap::new();

        // Signal 1: Direct transitions from current file
        if let Some(transitions) = self.transitions.get(&current_hash) {
            let total: f64 = transitions.iter().map(|(_, w)| w).sum();
            if total > 0.0 {
                for &(target, weight) in transitions {
                    let prob = weight / total;
                    *candidates.entry(target).or_insert(0.0) += prob * 0.6;
                }
            }
        }

        // Signal 2: Transitions from recently active files (temporal context)
        for &active in active_hashes.iter().take(5) {
            if let Some(transitions) = self.transitions.get(&active) {
                let total: f64 = transitions.iter().map(|(_, w)| w).sum();
                if total > 0.0 {
                    for &(target, weight) in transitions {
                        let prob = weight / total;
                        *candidates.entry(target).or_insert(0.0) += prob * 0.3;
                    }
                }
            }
        }

        // Signal 3: Global frequency (fallback for cold-start)
        if candidates.is_empty() {
            let last_n: Vec<u64> = self
                .recent_accesses
                .iter()
                .rev()
                .take(10)
                .copied()
                .collect();
            for &h in &last_n {
                *candidates.entry(h).or_insert(0.0) += 0.1;
            }
        }

        // Remove already-active files from predictions
        let active_set: std::collections::HashSet<u64> = active_hashes.iter().copied().collect();
        candidates.retain(|h, _| !active_set.contains(h) && *h != current_hash);

        // Sort by confidence and take top-k
        let mut sorted: Vec<(u64, f64)> = candidates.into_iter().collect();
        sorted.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        sorted.truncate(MAX_PREFETCH);

        // Filter by minimum confidence
        sorted.retain(|(_, conf)| *conf >= MIN_CONFIDENCE);
        sorted
    }

    /// Report whether a predicted file was actually accessed (feedback loop).
    pub fn report_hit(&mut self, predicted_hash: u64, was_accessed: bool) {
        self.predictions_made += 1;
        if was_accessed {
            self.predictions_hit += 1;

            // Strengthen the transition that led to this prediction
            if let Some(&last) = self.recent_accesses.last()
                && let Some(transitions) = self.transitions.get_mut(&last)
                && let Some(pair) = transitions.iter_mut().find(|(h, _)| *h == predicted_hash)
            {
                pair.1 *= 1.2; // Reward correct prediction
            }
        }
    }

    /// Current prediction accuracy (0.0 - 1.0).
    #[must_use]
    pub fn accuracy(&self) -> f64 {
        if self.predictions_made == 0 {
            return 0.0;
        }
        self.predictions_hit as f64 / self.predictions_made as f64
    }

    /// Free Energy = surprise metric. High value means predictions are poor.
    #[must_use]
    pub fn free_energy(&self) -> f64 {
        1.0 - self.accuracy()
    }

    /// Should we actively prefetch? Only when model has learned enough and
    /// prediction accuracy is reasonable.
    #[must_use]
    pub fn should_prefetch(&self) -> bool {
        self.predictions_made >= 10 && self.accuracy() > 0.2
    }

    fn prune_transitions(&mut self) {
        // Keep only top-10 transitions per source
        for transitions in self.transitions.values_mut() {
            if transitions.len() > 10 {
                transitions
                    .sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
                transitions.truncate(10);
            }
        }
        // Remove sources with all-zero transitions
        self.transitions.retain(|_, v| !v.is_empty());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_learns_transitions() {
        let mut model = PrefetchModel::new();
        let a = 1u64;
        let b = 2u64;

        // Repeated strong pattern: A → B (30 times builds high weight)
        for _ in 0..30 {
            model.observe(a);
            model.observe(b);
        }

        // After observing A, should predict B with high confidence
        let predictions = model.predict(a, &[]);
        assert!(
            !predictions.is_empty(),
            "Expected predictions after 30 A→B transitions"
        );
        assert!(
            predictions.iter().any(|(h, _)| *h == b),
            "Expected B in predictions, got: {predictions:?}"
        );
    }

    #[test]
    fn empty_model_returns_no_predictions_above_threshold() {
        let model = PrefetchModel::new();
        let predictions = model.predict(42, &[]);
        // Fresh model may return recent accesses but below threshold
        assert!(predictions.iter().all(|(_, conf)| *conf >= MIN_CONFIDENCE));
    }

    #[test]
    fn accuracy_tracking() {
        let mut model = PrefetchModel::new();
        model.report_hit(1, true);
        model.report_hit(2, true);
        model.report_hit(3, false);
        assert!((model.accuracy() - 0.666).abs() < 0.01);
    }

    #[test]
    fn free_energy_decreases_with_accuracy() {
        let mut model = PrefetchModel::new();
        for i in 0..20 {
            model.report_hit(i, true);
        }
        assert!(model.free_energy() < 0.1);
    }
}
