//! Weighted A/B experiments for model-routing variants.

use std::collections::HashMap;

use super::routing_quality::RoutingOutcome;

const SUCCESS_THRESHOLD: f64 = 0.8;

/// A model-routing variant participating in an experiment.
#[derive(Clone, Debug, PartialEq)]
pub struct ExperimentVariant {
    pub name: String,
    pub model: String,
    pub weight: f64,
}

/// Aggregate evaluation for the best-performing experiment variant.
#[derive(Clone, Debug, PartialEq)]
pub struct ExperimentResult {
    pub winner: String,
    pub success_rate: f64,
    pub avg_savings: f64,
    pub sample_size: usize,
}

/// Collects routing outcomes and evaluates model variants.
#[derive(Debug)]
pub struct RoutingExperiment {
    pub name: String,
    variants: Vec<ExperimentVariant>,
    outcomes: HashMap<String, Vec<RoutingOutcome>>,
}

impl RoutingExperiment {
    /// Creates an experiment and normalizes its variant weights.
    pub fn new(name: &str, mut variants: Vec<ExperimentVariant>) -> Self {
        normalize_weights(&mut variants);
        let outcomes = variants
            .iter()
            .map(|variant| (variant.name.clone(), Vec::new()))
            .collect();

        Self {
            name: name.to_owned(),
            variants,
            outcomes,
        }
    }

    /// Selects a variant using its normalized weight.
    pub fn select_variant(&self) -> Option<&ExperimentVariant> {
        let total_weight = self
            .variants
            .iter()
            .map(|variant| variant.weight)
            .sum::<f64>();
        if total_weight <= 0.0 || !total_weight.is_finite() {
            return None;
        }

        let mut target = random_unit_interval() * total_weight;
        for variant in &self.variants {
            if variant.weight > 0.0 {
                if target < variant.weight {
                    return Some(variant);
                }
                target -= variant.weight;
            }
        }

        self.variants
            .iter()
            .rev()
            .find(|variant| variant.weight > 0.0)
    }

    /// Records an outcome under the selected variant name.
    pub fn record_outcome(&mut self, variant_name: &str, outcome: RoutingOutcome) {
        self.outcomes
            .entry(variant_name.to_owned())
            .or_default()
            .push(outcome);
    }

    /// Returns the variant with the best success-rate and savings score.
    pub fn evaluate(&self) -> Option<ExperimentResult> {
        let mut best_score = None;
        let mut result = None;

        for variant in &self.variants {
            let Some(outcomes) = self.outcomes.get(&variant.name) else {
                continue;
            };
            if outcomes.is_empty() {
                continue;
            }

            let sample_size = outcomes.len();
            let success_rate = outcomes
                .iter()
                .filter(|outcome| {
                    outcome
                        .quality_score
                        .is_some_and(|score| score >= SUCCESS_THRESHOLD)
                })
                .count() as f64
                / sample_size as f64;
            let avg_savings = outcomes
                .iter()
                .map(|outcome| outcome.tokens_saved as f64)
                .sum::<f64>()
                / sample_size as f64;
            let score = success_rate * avg_savings;

            if best_score.is_none_or(|current| score > current) {
                best_score = Some(score);
                result = Some(ExperimentResult {
                    winner: variant.name.clone(),
                    success_rate,
                    avg_savings,
                    sample_size,
                });
            }
        }

        result
    }

    /// Returns the number of outcomes recorded for a variant.
    pub fn sample_count(&self, variant_name: &str) -> usize {
        self.outcomes.get(variant_name).map_or(0, Vec::len)
    }

    /// Returns whether every configured variant has enough observations.
    pub fn is_conclusive(&self, min_samples: usize) -> bool {
        !self.variants.is_empty()
            && self
                .variants
                .iter()
                .all(|variant| self.sample_count(&variant.name) >= min_samples)
    }
}

fn normalize_weights(variants: &mut [ExperimentVariant]) {
    let total_weight = variants
        .iter()
        .map(|variant| {
            if variant.weight.is_finite() && variant.weight > 0.0 {
                variant.weight
            } else {
                0.0
            }
        })
        .sum::<f64>();

    if total_weight.is_finite() && total_weight > 0.0 {
        for variant in variants {
            variant.weight = if variant.weight.is_finite() && variant.weight > 0.0 {
                variant.weight / total_weight
            } else {
                0.0
            };
        }
    } else {
        for variant in variants {
            variant.weight = 0.0;
        }
    }
}

fn random_unit_interval() -> f64 {
    let mut bytes = [0_u8; 8];
    if getrandom::fill(&mut bytes).is_err() {
        tracing::warn!("secure random source unavailable; using midpoint selection");
        return 0.5;
    }

    let value = u64::from_le_bytes(bytes) >> 11;
    value as f64 / (1_u64 << 53) as f64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::ocla::routing_quality::RoutingDecision;

    fn variant(name: &str, weight: f64) -> ExperimentVariant {
        ExperimentVariant {
            name: name.into(),
            model: format!("model-{name}"),
            weight,
        }
    }

    fn outcome(quality_score: Option<f64>, tokens_saved: u64) -> RoutingOutcome {
        RoutingOutcome {
            decision: RoutingDecision {
                decision_id: "experiment-decision".into(),
                original_model: "baseline".into(),
                routed_model: "candidate".into(),
                reason: "experiment".into(),
                timestamp: "2026-01-01T00:00:00Z".into(),
            },
            quality_score,
            tokens_saved,
            latency_delta_ms: 0,
        }
    }

    #[test]
    fn select_variant_respects_weights() {
        let experiment = RoutingExperiment::new(
            "weight-test",
            vec![variant("heavy", 0.9), variant("light", 0.1)],
        );
        let heavy = (0..1_000)
            .filter(|_| {
                experiment
                    .select_variant()
                    .is_some_and(|selected| selected.name == "heavy")
            })
            .count();

        assert!(heavy > 700, "heavy variant selected {heavy} times");
    }

    #[test]
    fn evaluate_picks_best_variant() {
        let mut experiment = RoutingExperiment::new(
            "evaluation-test",
            vec![variant("A", 0.5), variant("B", 0.5)],
        );
        for _ in 0..5 {
            experiment.record_outcome("A", outcome(Some(1.0), 100));
            experiment.record_outcome("B", outcome(Some(0.2), 100));
        }

        let result = experiment.evaluate().expect("outcomes should evaluate");
        assert_eq!(result.winner, "A");
        assert_eq!(result.success_rate, 1.0);
        assert_eq!(result.avg_savings, 100.0);
        assert_eq!(result.sample_size, 5);
    }

    #[test]
    fn empty_experiment_returns_none() {
        let experiment = RoutingExperiment::new("empty", vec![variant("A", 1.0)]);

        assert_eq!(experiment.evaluate(), None);
    }

    #[test]
    fn is_conclusive_requires_min_samples() {
        let mut experiment = RoutingExperiment::new(
            "conclusive-test",
            vec![variant("A", 0.5), variant("B", 0.5)],
        );

        assert!(!experiment.is_conclusive(1));
        experiment.record_outcome("A", outcome(Some(1.0), 10));
        experiment.record_outcome("B", outcome(Some(1.0), 10));
        assert!(!experiment.is_conclusive(2));
        experiment.record_outcome("A", outcome(Some(1.0), 10));
        experiment.record_outcome("B", outcome(Some(1.0), 10));
        assert!(experiment.is_conclusive(2));
    }

    #[test]
    fn record_and_retrieve() {
        let mut experiment = RoutingExperiment::new("record-test", vec![variant("A", 1.0)]);

        experiment.record_outcome("A", outcome(Some(1.0), 10));
        experiment.record_outcome("A", outcome(Some(0.0), 5));

        assert_eq!(experiment.sample_count("A"), 2);
        assert_eq!(experiment.sample_count("missing"), 0);
    }
}
