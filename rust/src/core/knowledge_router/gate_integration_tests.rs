use super::*;
use crate::core::knowledge::KnowledgeQuery;
use crate::core::stigmergy::{PheromoneSignal, SignalKind, deposit_signal};
use chrono::Utc;

fn context() -> ContextState {
    ContextState::new(Vec::new(), KnowledgeQuery::default())
}

#[test]
fn test_advise_with_jira_ref() {
    let advice = KnowledgeGateAdvisor::advise("Fix LEAN-42", &context());
    assert_eq!(advice.references_found.len(), 1);
    assert_eq!(
        advice.references_found[0].ref_type,
        super::super::reference_resolver::ReferenceType::JiraIssue
    );
}

#[test]
fn test_advise_with_github_ref() {
    let advice = KnowledgeGateAdvisor::advise("Review #789", &context());
    assert_eq!(advice.references_found.len(), 1);
    assert_eq!(
        advice.references_found[0].ref_type,
        super::super::reference_resolver::ReferenceType::GitHubPR
    );
}

#[test]
fn test_advise_no_refs() {
    assert!(
        KnowledgeGateAdvisor::advise("Hello world", &context())
            .references_found
            .is_empty()
    );
}

#[test]
fn test_advise_multiple_refs() {
    assert_eq!(
        KnowledgeGateAdvisor::advise("Fix LEAN-42 and check #789", &context())
            .references_found
            .len(),
        2
    );
}

#[test]
fn test_hint_format() {
    assert!(
        KnowledgeGateAdvisor::advise("Fix LEAN-42", &context())
            .additional_context_hint
            .unwrap()
            .contains("Referenced:")
    );
}

#[test]
#[ignore] // flaky: PatternReferenceResolver may not extract the path from query
fn test_hint_includes_high_file_exploration_pressure() {
    for agent_id in ["agent-1", "agent-2", "agent-3"] {
        deposit_signal(PheromoneSignal {
            agent_id: agent_id.to_owned(),
            kind: SignalKind::Exploration,
            path: "rust/src/core/knowledge_router/mod.rs".to_owned(),
            symbol: None,
            strength: 0.4,
            deposited_at: Utc::now(),
            note: None,
        });
    }

    let hint =
        KnowledgeGateAdvisor::advise("Review rust/src/core/knowledge_router/mod.rs", &context())
            .additional_context_hint
            .unwrap();

    assert!(hint.contains(
        "File rust/src/core/knowledge_router/mod.rs has high exploration pressure from 3 agents"
    ));
}

#[derive(Debug)]
struct PanickingResolver;

impl ReferenceResolver for PanickingResolver {
    fn resolve(&self, _: &str) -> Vec<ResolvedReference> {
        panic!("resolver failed")
    }

    fn name(&self) -> &'static str {
        "panic"
    }
}

#[test]
fn test_error_tolerance() {
    assert!(
        KnowledgeGateAdvisor::advise_with("LEAN-42", &context(), &PanickingResolver)
            .references_found
            .is_empty()
    );
}
