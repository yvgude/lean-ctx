#![allow(clippy::all, unused)]
use std::sync::Arc;

use super::{
    BundleStrategy, KnowledgeRouter, PatternReferenceResolver, ProviderBridge, QueryPlanner,
    ReferenceResolver, ResolvedReference, build_manifests_for_provider_ids, builtin_manifests,
    context_bundle::create_bundle,
};
use crate::core::knowledge_router::reference_resolver::ReferenceType;
use crate::core::providers::{
    ContextProvider, ProviderItem, ProviderParams, ProviderRegistry, ProviderResult,
};
use crate::core::task_spine::TaskProfileLocal;

#[derive(Debug)]
struct RecordingProvider;

impl ContextProvider for RecordingProvider {
    fn id(&self) -> &'static str {
        "github"
    }
    fn display_name(&self) -> &'static str {
        "Recording GitHub"
    }
    fn supported_actions(&self) -> &[&str] {
        &["issues", "pull_requests"]
    }

    fn execute(&self, action: &str, params: &ProviderParams) -> Result<ProviderResult, String> {
        Ok(ProviderResult {
            provider: self.id().into(),
            resource_type: action.into(),
            items: vec![ProviderItem {
                id: params.id.clone().unwrap_or_default(),
                title: params.query.clone().unwrap_or_default(),
                ..ProviderItem::default()
            }],
            total_count: Some(1),
            truncated: false,
        })
    }

    fn is_available(&self) -> bool {
        true
    }
}

#[test]
fn resolves_jira_github_and_file_references_from_task_text() {
    let references = PatternReferenceResolver
        .resolve("Implement LEAN-42 from GitHub PR #77 in src/core/knowledge_router/mod.rs");

    assert!(references.iter().any(|reference| {
        reference.ref_type == ReferenceType::JiraIssue && reference.identifier == "LEAN-42"
    }));
    assert!(references.iter().any(|reference| {
        reference.ref_type == ReferenceType::GitHubPR && reference.identifier == "#77"
    }));
    assert!(references.iter().any(|reference| {
        reference.ref_type == ReferenceType::FilePath
            && reference.identifier == "src/core/knowledge_router/mod.rs"
    }));
}

#[test]
fn planner_produces_a_budget_bounded_plan() {
    let references = resolved_references();
    let budget = 800;
    let candidates = QueryPlanner::plan(&references, &builtin_manifests(), budget);

    assert!(!candidates.is_empty(), "`candidates` is empty");
    assert!(
        candidates
            .iter()
            .map(|candidate| candidate.estimated_tokens)
            .sum::<u64>()
            <= budget
    );
    assert!(
        candidates
            .iter()
            .all(|candidate| candidate.reference.is_some())
    );
}

#[test]
fn context_bundle_assembles_resolved_references() {
    let candidates = QueryPlanner::plan(&resolved_references(), &builtin_manifests(), 2_000);
    let bundle = create_bundle("router-task", &candidates, BundleStrategy::Enriched);

    assert_eq!(bundle.candidates.len(), candidates.len());
    assert_eq!(
        bundle.total_tokens,
        candidates
            .iter()
            .map(|candidate| candidate.estimated_tokens)
            .sum::<u64>()
    );
    assert!(bundle.coverage_milli > 0);
}

#[test]
fn knowledge_router_exit_gate_resolves_jira_into_auditable_context_receipt() {
    let task_id = "routing-task-LEAN-42";
    let query = "Implement LEAN-42 using src/core/knowledge_router/mod.rs";
    let router = KnowledgeRouter {
        manifests: builtin_manifests(),
        resolvers: vec![Arc::new(PatternReferenceResolver)],
    };

    let result = router.route(task_id, query, &TaskProfileLocal::default(), &[], None);

    assert!(
        result
            .candidates
            .iter()
            .any(|candidate| candidate.reference.as_deref() == Some("LEAN-42"))
    );
    assert!(
        result.bundle.total_tokens > 0
            && result.bundle.total_tokens <= result.receipt.budget_tokens,
        "context bundle must account for tokens within its planned budget"
    );
    assert_eq!(result.receipt.task_id, task_id);
    assert_eq!(result.receipt.bundle_id, result.bundle.bundle_id);
    assert_eq!(
        result.receipt.materialized_tokens,
        result.bundle.total_tokens.min(result.receipt.budget_tokens)
    );
    assert_eq!(
        result.receipt.candidates_selected as usize,
        result.bundle.candidates.len()
    );
    assert!(
        result
            .receipt
            .sources_used
            .iter()
            .any(|source| source == "jira")
    );
    assert!(
        chrono::DateTime::parse_from_rfc3339(&result.receipt.timestamp).is_ok(),
        "context receipt must retain an auditable creation time"
    );
}

#[test]
fn bridge_executes_the_resolved_context_provider() {
    let registry = ProviderRegistry::new();
    registry.register(Arc::new(RecordingProvider));
    let reference = reference(ReferenceType::GitHubPR, "#77", "github", 900);
    let bridge = ProviderBridge::new(&registry);
    let resolution = bridge.resolve_from_providers(&[reference.clone()], &["github"]);

    let result = bridge.fetch_reference(&reference, &resolution[0]).unwrap();
    assert_eq!(result.resource_type, "pull_requests");
    assert_eq!(result.items[0].id, "77");
    assert_eq!(result.items[0].title, "#77");
}

#[test]
fn manifest_builder_covers_all_registered_provider_kinds() {
    let providers = [
        "github",
        "gitlab",
        "jira",
        "postgres",
        "mcp-bridge",
        "config-provider",
        "wasm-provider",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    let manifests = build_manifests_for_provider_ids(&providers);

    assert_eq!(manifests.len(), providers.len() + 1);
    for provider in providers {
        assert!(
            manifests
                .iter()
                .any(|manifest| manifest.source_id == provider && manifest.is_valid())
        );
    }
}

fn resolved_references() -> Vec<ResolvedReference> {
    vec![
        reference(ReferenceType::JiraIssue, "LEAN-42", "jira", 950),
        reference(ReferenceType::GitHubPR, "#77", "github", 900),
        reference(
            ReferenceType::FilePath,
            "src/core/knowledge_router/mod.rs",
            "local_files",
            800,
        ),
    ]
}

fn reference(
    ref_type: ReferenceType,
    identifier: &str,
    source_id: &str,
    confidence_milli: u16,
) -> ResolvedReference {
    ResolvedReference {
        ref_type,
        identifier: identifier.into(),
        source_id: source_id.into(),
        confidence_milli,
    }
}
