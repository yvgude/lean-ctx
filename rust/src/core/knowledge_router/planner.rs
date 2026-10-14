use super::{
    reference_resolver::{ReferenceType, ResolvedReference},
    source_manifest::SourceManifestEntry,
};
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContextCandidate {
    pub candidate_id: String,
    pub source_id: String,
    pub kind: String,
    pub relevance_milli: u16,
    pub estimated_tokens: u64,
    pub reference: Option<String>,
}
#[derive(Debug, Clone, Copy, Default)]
pub struct QueryPlanner;
impl QueryPlanner {
    pub fn plan(
        references: &[ResolvedReference],
        manifests: &[SourceManifestEntry],
        budget_tokens: u64,
    ) -> Vec<ContextCandidate> {
        let mut candidates = references
            .iter()
            .enumerate()
            .filter_map(|(index, reference)| {
                manifests
                    .iter()
                    .find(|manifest| manifest.source_id == reference.source_id)
                    .map(|manifest| ContextCandidate {
                        candidate_id: format!("candidate-{index}-{}", manifest.source_id),
                        source_id: manifest.source_id.clone(),
                        kind: kind_for(reference.ref_type).into(),
                        relevance_milli: reference.confidence_milli,
                        estimated_tokens: estimated_tokens(reference.ref_type),
                        reference: Some(reference.identifier.clone()),
                    })
            })
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| {
            right
                .relevance_milli
                .cmp(&left.relevance_milli)
                .then_with(|| left.candidate_id.cmp(&right.candidate_id))
        });
        let mut used: u64 = 0;
        candidates
            .into_iter()
            .filter(|candidate| {
                if used.saturating_add(candidate.estimated_tokens) > budget_tokens {
                    return false;
                }
                used += candidate.estimated_tokens;
                true
            })
            .collect()
    }
}
fn kind_for(reference: ReferenceType) -> &'static str {
    match reference {
        ReferenceType::JiraIssue | ReferenceType::GitHubIssue => "issue",
        ReferenceType::GitHubPR => "pull_request",
        ReferenceType::FilePath => "file",
        ReferenceType::Function => "function",
        ReferenceType::Url => "url",
    }
}
fn estimated_tokens(reference: ReferenceType) -> u64 {
    match reference {
        ReferenceType::FilePath => 256,
        _ => 384,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::knowledge_router::{
        reference_resolver::{ReferenceType, ResolvedReference},
        source_manifest::builtin_manifests,
    };
    fn reference(source: &str, confidence: u16) -> ResolvedReference {
        ResolvedReference {
            ref_type: ReferenceType::JiraIssue,
            identifier: "LEAN-1".into(),
            source_id: source.into(),
            confidence_milli: confidence,
        }
    }
    #[test]
    fn plans_matching_sources() {
        assert_eq!(
            QueryPlanner::plan(&[reference("jira", 900)], &builtin_manifests(), 400).len(),
            1
        );
    }
    #[test]
    fn respects_budget() {
        {
            let found = &QueryPlanner::plan(&[reference("jira", 900)], &builtin_manifests(), 383);
            assert!(found.is_empty(), "{found:?}");
        }
    }
    #[test]
    fn sorts_by_relevance() {
        let candidates = QueryPlanner::plan(
            &[reference("jira", 800), reference("github", 950)],
            &builtin_manifests(),
            1_000,
        );
        assert_eq!(candidates[0].source_id, "github");
    }
    #[test]
    fn skips_unknown_sources() {
        {
            let found = &QueryPlanner::plan(&[reference("other", 900)], &builtin_manifests(), 400);
            assert!(found.is_empty(), "{found:?}");
        }
    }
}
