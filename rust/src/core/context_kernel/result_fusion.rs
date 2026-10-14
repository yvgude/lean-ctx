//! Fusion of child-agent results into one attributed report.

use std::collections::HashSet;

/// A child agent's result with its confidence and quality metadata.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ChildResult {
    pub agent_id: String,
    pub receipt_ref: String,
    pub evidence: Vec<String>,
    pub confidence: f64,
    pub quality_score: f64,
    pub contradicts: Vec<String>,
}

/// How to merge multiple child results.
#[derive(Debug, Clone, Copy, Default, serde::Serialize, serde::Deserialize)]
pub enum FusionStrategy {
    /// Take the result with highest confidence.
    #[default]
    BestConfidence,
    /// Take the result agreed upon by majority (>50% same evidence).
    MajorityVote,
    /// Weighted merge by quality score multiplied by confidence.
    WeightedMerge,
}

/// Result of fusing child results.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct FusionReport {
    pub winning_agent: String,
    pub merged_evidence: Vec<String>,
    pub conflicts: Vec<Conflict>,
    pub total_confidence: f64,
    pub attribution_per_child: Vec<ChildAttribution>,
}

/// An explicit contradiction between two child agents.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Conflict {
    pub agent_a: String,
    pub agent_b: String,
    pub reason: String,
}

/// A child's normalized contribution to a fusion report.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ChildAttribution {
    pub agent_id: String,
    pub weight: f64,
    pub evidence_contributed: usize,
}

/// Fuse child results according to the selected strategy.
pub fn fuse_results(results: &[ChildResult], strategy: FusionStrategy) -> FusionReport {
    if results.is_empty() {
        return FusionReport::default();
    }

    let conflicts = detect_contradictions(results);
    let (winning_index, selected, weights, total_confidence) = match strategy {
        FusionStrategy::BestConfidence => best_confidence(results),
        FusionStrategy::MajorityVote => majority_vote(results),
        FusionStrategy::WeightedMerge => weighted_merge(results),
    };
    let merged_evidence = merge_evidence(results, &selected);
    let attribution_per_child = results
        .iter()
        .enumerate()
        .map(|(index, result)| ChildAttribution {
            agent_id: result.agent_id.clone(),
            weight: weights[index],
            evidence_contributed: if selected.contains(&index) {
                result.evidence.iter().collect::<HashSet<_>>().len()
            } else {
                0
            },
        })
        .collect();

    FusionReport {
        winning_agent: results[winning_index].agent_id.clone(),
        merged_evidence,
        conflicts,
        total_confidence,
        attribution_per_child,
    }
}

/// Extract and deduplicate all explicit contradiction pairs.
pub fn detect_contradictions(results: &[ChildResult]) -> Vec<Conflict> {
    let mut seen = HashSet::new();
    let mut conflicts = Vec::new();
    for result in results {
        for contradicted in &result.contradicts {
            if contradicted == &result.agent_id {
                continue;
            }
            let pair = if result.agent_id <= *contradicted {
                (result.agent_id.clone(), contradicted.clone())
            } else {
                (contradicted.clone(), result.agent_id.clone())
            };
            if seen.insert(pair) {
                conflicts.push(Conflict {
                    agent_a: result.agent_id.clone(),
                    agent_b: contradicted.clone(),
                    reason: "explicit contradiction".to_owned(),
                });
            }
        }
    }
    conflicts
}

fn best_confidence(results: &[ChildResult]) -> (usize, Vec<usize>, Vec<f64>, f64) {
    let winner = highest_score_index(results, |result| result.confidence);
    let mut weights = vec![0.0; results.len()];
    weights[winner] = 1.0;
    (winner, vec![winner], weights, results[winner].confidence)
}

fn majority_vote(results: &[ChildResult]) -> (usize, Vec<usize>, Vec<f64>, f64) {
    let groups = overlap_groups(results);
    let mut selected = groups[0].clone();
    for group in groups.into_iter().skip(1) {
        let group_confidence = confidence_sum(results, &group);
        let selected_confidence = confidence_sum(results, &selected);
        if group.len() > selected.len()
            || (group.len() == selected.len() && group_confidence > selected_confidence)
        {
            selected = group;
        }
    }
    let winner = selected.iter().copied().fold(selected[0], |best, index| {
        if results[index].confidence > results[best].confidence {
            index
        } else {
            best
        }
    });
    let weight = 1.0 / selected.len() as f64;
    let mut weights = vec![0.0; results.len()];
    for index in &selected {
        weights[*index] = weight;
    }
    let confidence = confidence_sum(results, &selected) / selected.len() as f64;
    (winner, selected, weights, confidence)
}

fn weighted_merge(results: &[ChildResult]) -> (usize, Vec<usize>, Vec<f64>, f64) {
    let raw: Vec<f64> = results
        .iter()
        .map(|result| result.quality_score * result.confidence)
        .collect();
    let total: f64 = raw.iter().sum();
    let weights = if total > 0.0 {
        raw.iter().map(|weight| weight / total).collect()
    } else {
        vec![1.0 / results.len() as f64; results.len()]
    };
    let winner = highest_score_index(results, |result| result.quality_score * result.confidence);
    let confidence = results
        .iter()
        .zip(&weights)
        .map(|(result, weight)| result.confidence * weight)
        .sum();
    (winner, (0..results.len()).collect(), weights, confidence)
}

fn highest_score_index(results: &[ChildResult], score: impl Fn(&ChildResult) -> f64) -> usize {
    (1..results.len()).fold(0, |best, index| {
        if score(&results[index]) > score(&results[best]) {
            index
        } else {
            best
        }
    })
}

fn overlap_groups(results: &[ChildResult]) -> Vec<Vec<usize>> {
    let evidence: Vec<HashSet<&str>> = results
        .iter()
        .map(|result| result.evidence.iter().map(String::as_str).collect())
        .collect();
    let mut visited = vec![false; results.len()];
    let mut groups = Vec::new();
    for start in 0..results.len() {
        if visited[start] {
            continue;
        }
        visited[start] = true;
        let mut group = Vec::new();
        let mut stack = vec![start];
        while let Some(current) = stack.pop() {
            group.push(current);
            for candidate in 0..results.len() {
                if !visited[candidate] && !evidence[current].is_disjoint(&evidence[candidate]) {
                    visited[candidate] = true;
                    stack.push(candidate);
                }
            }
        }
        group.sort_unstable();
        groups.push(group);
    }
    groups
}

fn merge_evidence(results: &[ChildResult], selected: &[usize]) -> Vec<String> {
    let mut seen = HashSet::new();
    selected
        .iter()
        .flat_map(|index| &results[*index].evidence)
        .filter(|evidence| seen.insert((*evidence).clone()))
        .cloned()
        .collect()
}

fn confidence_sum(results: &[ChildResult], indices: &[usize]) -> f64 {
    indices.iter().map(|index| results[*index].confidence).sum()
}

#[cfg(test)]
pub mod tests {
    use super::{ChildResult, FusionStrategy, detect_contradictions, fuse_results};

    fn result(id: &str, evidence: &[&str], confidence: f64, quality: f64) -> ChildResult {
        ChildResult {
            agent_id: id.to_owned(),
            receipt_ref: format!("receipt-{id}"),
            evidence: evidence.iter().map(|item| (*item).to_owned()).collect(),
            confidence,
            quality_score: quality,
            contradicts: Vec::new(),
        }
    }

    #[test]
    fn best_confidence_picks_highest() {
        let results = [
            result("a", &["one"], 0.4, 1.0),
            result("b", &["two", "two"], 0.9, 0.5),
            result("c", &["three"], 0.7, 1.0),
        ];
        let report = fuse_results(&results, FusionStrategy::BestConfidence);
        assert_eq!(report.winning_agent, "b");
    }

    #[test]
    fn majority_vote_picks_majority() {
        let results = [
            result("a", &["shared", "a"], 0.5, 1.0),
            result("b", &["shared", "b"], 0.8, 1.0),
            result("c", &["other"], 0.9, 1.0),
        ];
        let report = fuse_results(&results, FusionStrategy::MajorityVote);
        assert_eq!(report.winning_agent, "b");
    }

    #[test]
    fn weighted_merge_attributes_proportionally() {
        let results = [
            result("a", &["one"], 0.5, 1.0),
            result("b", &["two"], 1.0, 1.0),
        ];
        let report = fuse_results(&results, FusionStrategy::WeightedMerge);
        let sum: f64 = report
            .attribution_per_child
            .iter()
            .map(|item| item.weight)
            .sum();
        assert!((sum - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn contradictions_detected() {
        let mut a = result("a", &["one"], 0.5, 1.0);
        a.contradicts.push("b".to_owned());
        let conflicts = detect_contradictions(&[a, result("b", &["two"], 0.5, 1.0)]);
        assert_eq!(conflicts.len(), 1);
    }

    #[test]
    fn single_result_returns_identity() {
        let only = result("only", &["one"], 0.75, 0.8);
        let report = fuse_results(std::slice::from_ref(&only), FusionStrategy::WeightedMerge);
        assert_eq!(report.winning_agent, only.agent_id);
    }

    #[test]
    fn empty_results_returns_empty_report() {
        let report = fuse_results(&[], FusionStrategy::MajorityVote);
        assert!(
            report.winning_agent.is_empty(),
            "{:?}",
            report.winning_agent
        );
    }
}
