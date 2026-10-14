//! Wire types for transferring reference-only context between agents.

use std::collections::{HashMap, HashSet};
use std::time::{SystemTime, UNIX_EPOCH};

/// Wire-compatible context capsule for multi-agent transfer.
/// Contains only references and metadata — never raw content payloads.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ContextCapsuleV1 {
    /// Content-derived capsule identifier.
    pub capsule_id: String,
    /// Content-addressed reference to the capsule manifest.
    pub manifest_ref: String,
    /// Selected content references, without payloads.
    pub selected_refs: Vec<String>,
    /// Intent the transferred context supports.
    pub intent: String,
    /// Total token budget assigned to the capsule.
    pub budget_tokens: u64,
    /// Token budget still available to recipients.
    pub budget_remaining: u64,
    /// Policy version used to construct the capsule.
    pub policy_version: String,
    /// Capsule sensitivity classification.
    pub sensitivity: String,
    /// Agent that owns the capsule.
    pub owner_agent: String,
    /// Agents permitted to receive the capsule.
    pub target_agents: Vec<String>,
    /// Parent capsule identifier for transfer chains.
    pub parent_capsule: Option<String>,
    /// Creation time as seconds since the Unix epoch.
    pub created_at_epoch: u64,
}

/// Represents the difference between a base capsule and an updated one.
/// Used to minimize transfer size in multi-agent chains.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DeltaTransfer {
    /// Identifier of the capsule this delta applies to.
    pub base_ref: String,
    /// Content references added by the update.
    pub added_refs: Vec<String>,
    /// Content references removed by the update.
    pub removed_refs: Vec<String>,
    /// Signed change to the capsule token budget.
    pub budget_delta: i64,
    /// Opaque references to findings produced since the base capsule.
    pub new_findings: Vec<String>,
}

/// Cross-capsule reference overlap and deduplication metrics.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DedupReport {
    /// References present in more than one sibling capsule.
    pub shared_refs: Vec<String>,
    /// References present only in the corresponding capsule.
    pub unique_per_capsule: Vec<Vec<String>>,
    /// Fraction of reference entries removable through deduplication.
    pub dedup_ratio: f64,
}

impl ContextCapsuleV1 {
    /// Creates a reference-only capsule with a deterministic content-derived ID.
    pub fn new(intent: &str, refs: Vec<String>, budget: u64, owner: &str) -> Self {
        let capsule_id = capsule_id(intent, &refs);
        Self {
            manifest_ref: capsule_id.clone(),
            capsule_id,
            selected_refs: refs,
            intent: intent.to_owned(),
            budget_tokens: budget,
            budget_remaining: budget,
            policy_version: "v1".to_owned(),
            sensitivity: "internal".to_owned(),
            owner_agent: owner.to_owned(),
            target_agents: Vec::new(),
            parent_capsule: None,
            created_at_epoch: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |duration| duration.as_secs()),
        }
    }
}
impl DeltaTransfer {
    /// Returns `true` if the delta has fewer total refs than the updated capsule.
    ///
    /// When this returns `false`, callers should send the full capsule instead
    /// of the delta, since the disjoint-update case can produce a delta that
    /// is larger than the full capsule.
    pub fn is_efficient(&self, updated_ref_count: usize) -> bool {
        self.added_refs.len() + self.removed_refs.len() < updated_ref_count
    }
}

/// Computes the reference and budget difference between two capsules.
pub fn compute_delta(base: &ContextCapsuleV1, updated: &ContextCapsuleV1) -> DeltaTransfer {
    let base_refs: HashSet<&str> = base.selected_refs.iter().map(String::as_str).collect();
    let updated_refs: HashSet<&str> = updated.selected_refs.iter().map(String::as_str).collect();

    DeltaTransfer {
        base_ref: base.capsule_id.clone(),
        added_refs: updated
            .selected_refs
            .iter()
            .filter(|reference| !base_refs.contains(reference.as_str()))
            .cloned()
            .collect(),
        removed_refs: base
            .selected_refs
            .iter()
            .filter(|reference| !updated_refs.contains(reference.as_str()))
            .cloned()
            .collect(),
        budget_delta: signed_difference(updated.budget_tokens, base.budget_tokens),
        new_findings: Vec::new(),
    }
}

/// Applies a reference and budget delta to a base capsule.
pub fn apply_delta(base: &ContextCapsuleV1, delta: &DeltaTransfer) -> ContextCapsuleV1 {
    let removed: HashSet<&str> = delta.removed_refs.iter().map(String::as_str).collect();
    let mut selected_refs: Vec<String> = base
        .selected_refs
        .iter()
        .filter(|reference| !removed.contains(reference.as_str()))
        .cloned()
        .collect();
    let mut present: HashSet<String> = selected_refs.iter().cloned().collect();
    for reference in &delta.added_refs {
        if present.insert(reference.clone()) {
            selected_refs.push(reference.clone());
        }
    }

    let mut updated = base.clone();
    updated.budget_tokens = apply_signed(base.budget_tokens, delta.budget_delta);
    updated.budget_remaining = apply_signed(base.budget_remaining, delta.budget_delta);
    updated.selected_refs = selected_refs;
    updated.capsule_id = capsule_id(&updated.intent, &updated.selected_refs);
    updated.manifest_ref.clone_from(&updated.capsule_id);
    updated
}

/// Finds references shared across sibling capsules and reports transfer savings.
pub fn dedup_siblings(capsules: &[ContextCapsuleV1]) -> DedupReport {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for capsule in capsules {
        let refs: HashSet<&str> = capsule.selected_refs.iter().map(String::as_str).collect();
        for reference in refs {
            *counts.entry(reference).or_default() += 1;
        }
    }

    let mut seen = HashSet::new();
    let shared_refs = capsules
        .iter()
        .flat_map(|capsule| &capsule.selected_refs)
        .filter(|reference| counts.get(reference.as_str()).copied().unwrap_or(0) > 1)
        .filter(|reference| seen.insert(reference.as_str()))
        .cloned()
        .collect();
    let unique_per_capsule = capsules
        .iter()
        .map(|capsule| {
            capsule
                .selected_refs
                .iter()
                .filter(|reference| counts.get(reference.as_str()).copied().unwrap_or(0) == 1)
                .cloned()
                .collect()
        })
        .collect();
    let total_refs: usize = capsules
        .iter()
        .map(|capsule| capsule.selected_refs.len())
        .sum();
    let distinct_refs = counts.len();
    let dedup_ratio = if total_refs == 0 {
        0.0
    } else {
        (total_refs.saturating_sub(distinct_refs)) as f64 / total_refs as f64
    };

    DedupReport {
        shared_refs,
        unique_per_capsule,
        dedup_ratio,
    }
}

fn capsule_id(intent: &str, refs: &[String]) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(&(intent.len() as u64).to_le_bytes());
    hasher.update(intent.as_bytes());
    for reference in refs {
        hasher.update(&(reference.len() as u64).to_le_bytes());
        hasher.update(reference.as_bytes());
    }
    format!("blake3:{}", hasher.finalize().to_hex())
}

fn signed_difference(updated: u64, base: u64) -> i64 {
    if updated >= base {
        i64::try_from(updated - base).unwrap_or(i64::MAX)
    } else {
        -i64::try_from(base - updated).unwrap_or(i64::MAX)
    }
}

fn apply_signed(value: u64, delta: i64) -> u64 {
    if delta >= 0 {
        value.saturating_add(delta.unsigned_abs())
    } else {
        value.saturating_sub(delta.unsigned_abs())
    }
}

#[cfg(test)]
pub mod tests {
    use super::{ContextCapsuleV1, apply_delta, compute_delta, dedup_siblings};

    fn capsule(refs: &[&str], budget: u64) -> ContextCapsuleV1 {
        ContextCapsuleV1::new(
            "handoff",
            refs.iter()
                .map(|reference| (*reference).to_owned())
                .collect(),
            budget,
            "agent-a",
        )
    }

    #[test]
    fn new_capsule_has_content_addressed_id() {
        let first = capsule(&["ref-a", "ref-b"], 100);
        let second = capsule(&["ref-a", "ref-b"], 100);

        assert_eq!(first.capsule_id, second.capsule_id);
        assert!(first.capsule_id.starts_with("blake3:"));
        assert_eq!(first.manifest_ref, first.capsule_id);
    }

    #[test]
    fn compute_delta_finds_added_removed() {
        let delta = compute_delta(&capsule(&["A", "B"], 100), &capsule(&["B", "C"], 80));

        assert_eq!(delta.added_refs, ["C"]);
        assert_eq!(delta.removed_refs, ["A"]);
        assert_eq!(delta.budget_delta, -20);
    }

    #[test]
    fn apply_delta_reconstructs_capsule() {
        let base = capsule(&["A", "B"], 100);
        let updated = capsule(&["B", "C"], 80);

        assert_eq!(apply_delta(&base, &compute_delta(&base, &updated)), updated);
    }

    #[test]
    fn delta_smaller_than_full() {
        let base = capsule(&["ref-a", "ref-b", "ref-c"], 100);
        let updated = capsule(&["ref-a", "ref-b", "ref-d"], 90);
        let delta = compute_delta(&base, &updated);

        assert!(
            serde_json::to_vec(&delta).unwrap().len() < serde_json::to_vec(&updated).unwrap().len()
        );
    }

    #[test]
    fn dedup_siblings_finds_shared_refs() {
        let capsules = [
            capsule(&["ref1", "ref2", "only-a"], 100),
            capsule(&["ref1", "ref2", "only-b"], 100),
            capsule(&["ref1", "ref2", "only-c"], 100),
        ];
        let report = dedup_siblings(&capsules);

        assert_eq!(report.shared_refs, ["ref1", "ref2"]);
        assert_eq!(report.unique_per_capsule[0], ["only-a"]);
        assert!((report.dedup_ratio - 4.0 / 9.0).abs() < f64::EPSILON);
    }

    #[test]
    fn empty_capsule_delta_is_identity() {
        let base = capsule(&[], 100);
        let delta = compute_delta(&base, &base);

        assert!(delta.added_refs.is_empty(), "{:?}", delta.added_refs);
        assert!(delta.removed_refs.is_empty(), "{:?}", delta.removed_refs);
        assert_eq!(apply_delta(&base, &delta), base);
    }
}
