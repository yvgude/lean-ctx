use super::{context_bundle::ContextBundle, planner::ContextCandidate};
use lean_ctx_time::UtcDateTime;
use std::collections::BTreeSet;
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KnowledgeReceipt {
    pub receipt_id: String,
    pub task_id: String,
    pub bundle_id: String,
    pub budget_tokens: u64,
    pub materialized_tokens: u64,
    pub candidates_considered: u32,
    pub candidates_selected: u32,
    pub sources_used: Vec<String>,
    pub strategy: String,
    pub timestamp: String,
}
pub fn create_receipt(
    task_id: &str,
    bundle: &ContextBundle,
    all_candidates: &[ContextCandidate],
    budget: u64,
) -> KnowledgeReceipt {
    let selected = bundle.candidates.iter().collect::<BTreeSet<_>>();
    let sources_used = all_candidates
        .iter()
        .filter(|candidate| selected.contains(&candidate.candidate_id))
        .map(|candidate| candidate.source_id.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    KnowledgeReceipt {
        receipt_id: format!("receipt-{task_id}-{}", bundle.candidates.len()),
        task_id: task_id.into(),
        bundle_id: bundle.bundle_id.clone(),
        budget_tokens: budget,
        materialized_tokens: bundle.total_tokens.min(budget),
        candidates_considered: all_candidates.len() as u32,
        candidates_selected: bundle.candidates.len() as u32,
        sources_used,
        strategy: format!("{:?}", bundle.strategy).to_lowercase(),
        timestamp: UtcDateTime::now().to_rfc3339(),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::knowledge_router::{
        context_bundle::{BundleStrategy, create_bundle},
        planner::ContextCandidate,
    };
    fn candidate(id: &str, source: &str) -> ContextCandidate {
        ContextCandidate {
            candidate_id: id.into(),
            source_id: source.into(),
            kind: "file".into(),
            relevance_milli: 900,
            estimated_tokens: 100,
            reference: None,
        }
    }
    #[test]
    fn receipt_accounts_for_selected_candidates() {
        let all = vec![candidate("a", "local_files"), candidate("b", "jira")];
        let bundle = create_bundle("task", &all[..1], BundleStrategy::Enriched);
        let receipt = create_receipt("task", &bundle, &all, 200);
        assert_eq!(receipt.candidates_selected, 1);
        assert_eq!(receipt.materialized_tokens, 100);
    }
    #[test]
    fn receipt_deduplicates_sources() {
        let all = vec![candidate("a", "jira"), candidate("b", "jira")];
        let receipt = create_receipt(
            "task",
            &create_bundle("task", &all, BundleStrategy::Enriched),
            &all,
            200,
        );
        assert_eq!(receipt.sources_used, ["jira"]);
    }
}
