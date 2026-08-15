//! Runtime routing from task references to bounded context bundles.
pub mod context_bundle;
pub mod gate_integration;
pub mod manifest_builder;
pub mod planner;
pub mod provider_bridge;
pub mod receipt;
pub mod reference_resolver;
pub mod source_manifest;

#[cfg(test)]
#[path = "knowledge_router_tests.rs"]
mod knowledge_router_tests;

use crate::core::task_spine::TaskProfileLocal;
pub use context_bundle::{BundleStrategy, ContextBundle};
pub use manifest_builder::{
    ProviderConfig, build_manifests_for_provider_ids, build_manifests_from_config, detect_config,
};
pub use planner::{ContextCandidate, QueryPlanner};
pub use provider_bridge::{ProviderBridge, ProviderResolution};
pub use receipt::KnowledgeReceipt;
pub use reference_resolver::{PatternReferenceResolver, ReferenceResolver, ResolvedReference};
pub use source_manifest::{SourceManifestEntry, builtin_manifests};
use std::sync::Arc;

#[derive(Debug, Clone, Default)]
/// Routes task references into bounded knowledge bundles.
pub struct KnowledgeRouter {
    pub manifests: Vec<SourceManifestEntry>,
    pub resolvers: Vec<Arc<dyn ReferenceResolver>>,
}

#[derive(Debug, Clone, Default)]
/// Contains candidates, bundle, and receipt from knowledge routing.
pub struct RoutingResult {
    pub candidates: Vec<ContextCandidate>,
    pub bundle: ContextBundle,
    pub receipt: KnowledgeReceipt,
}

/// Router-derived hints for request-local compression.
///
/// References selected by the router identify message content that should be
/// preserved verbatim so a compressor does not discard the context required to
/// resolve the referenced source.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContextAdvice {
    protected_references: Vec<String>,
}

impl ContextAdvice {
    pub fn is_empty(&self) -> bool {
        self.protected_references.is_empty()
    }

    pub fn protects(&self, content: &str) -> bool {
        self.protected_references
            .iter()
            .any(|reference| content.contains(reference))
    }
}

impl KnowledgeRouter {
    pub fn route(
        &self,
        task_id: &str,
        query: &str,
        profile: &TaskProfileLocal,
        providers: &[SourceManifestEntry],
        bridge: Option<&ProviderBridge<'_>>,
    ) -> RoutingResult {
        let mut references = self
            .resolvers
            .iter()
            .flat_map(|resolver| resolver.resolve(query))
            .collect::<Vec<_>>();
        let dynamic_manifests = bridge.map(|bridge| {
            let config = detect_config();
            let mut manifests = build_manifests_from_config(&config);
            let configured = manifests
                .iter()
                .any(|manifest| manifest.source_id != "local_files");
            if !configured {
                manifests = builtin_manifests();
            }
            let available = bridge.available_provider_ids();
            for manifest in build_manifests_for_provider_ids(&available) {
                if !manifests
                    .iter()
                    .any(|existing| existing.source_id == manifest.source_id)
                {
                    manifests.push(manifest);
                }
            }
            let ids = available.iter().map(String::as_str).collect::<Vec<_>>();
            let resolutions = bridge.resolve_from_providers(&references, &ids);
            for (reference, resolution) in references.iter_mut().zip(resolutions) {
                if resolution.resolved {
                    reference.source_id = resolution.provider_id;
                } else {
                    reference.source_id.clear();
                }
            }
            manifests
        });
        let manifests = if let Some(manifests) = dynamic_manifests.as_deref() {
            manifests
        } else if providers.is_empty() {
            &self.manifests
        } else {
            providers
        };
        let budget = u64::from(profile.context_need_milli).max(250) * 4;
        let candidates = QueryPlanner::plan(&references, manifests, budget);
        let strategy = if profile.risk_signal == lean_ctx_protocol::RiskClass::Low {
            BundleStrategy::Enriched
        } else {
            BundleStrategy::Governed
        };
        let bundle = context_bundle::create_bundle(task_id, &candidates, strategy);
        let receipt = receipt::create_receipt(task_id, &bundle, &candidates, budget);
        RoutingResult {
            candidates,
            bundle,
            receipt,
        }
    }

    /// Convert routing candidates into bounded, request-local compression
    /// hints.  This deliberately exposes no provider payload: callers only
    /// receive the references whose surrounding message text must survive
    /// compression.
    pub fn context_advice(
        &self,
        task_id: &str,
        query: &str,
        profile: &TaskProfileLocal,
        providers: &[SourceManifestEntry],
        bridge: Option<&ProviderBridge<'_>>,
    ) -> ContextAdvice {
        let mut protected_references = self
            .route(task_id, query, profile, providers, bridge)
            .candidates
            .into_iter()
            .filter_map(|candidate| candidate.reference)
            .filter(|reference| !reference.is_empty())
            .collect::<Vec<_>>();
        protected_references.sort();
        protected_references.dedup();

        ContextAdvice {
            protected_references,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::knowledge_router::reference_resolver::ReferenceType;
    use crate::core::providers::registry::global_registry;
    use crate::core::task_spine::TaskProfileLocal;

    fn reference(ref_type: ReferenceType, identifier: &str) -> ResolvedReference {
        ResolvedReference {
            ref_type,
            identifier: identifier.into(),
            source_id: String::new(),
            confidence_milli: 900,
        }
    }

    #[test]
    fn test_provider_bridge_jira() {
        let result = ProviderBridge::new(global_registry())
            .resolve_from_providers(&[reference(ReferenceType::JiraIssue, "LEAN-42")], &["jira"]);
        assert_eq!(result[0].provider_id, "jira");
    }

    #[test]
    fn test_provider_bridge_github() {
        let result = ProviderBridge::new(global_registry())
            .resolve_from_providers(&[reference(ReferenceType::GitHubPR, "#42")], &["github"]);
        assert_eq!(result[0].estimated_tokens, 1_000);
    }

    #[test]
    fn test_provider_bridge_no_provider() {
        assert!(
            ProviderBridge::new(global_registry())
                .resolve_from_providers(
                    &[reference(ReferenceType::Url, "https://example.com")],
                    &[]
                )
                .is_empty()
        );
    }

    #[test]
    fn test_manifest_builder_empty() {
        assert_eq!(
            build_manifests_from_config(&ProviderConfig::default()).len(),
            1
        );
    }

    #[test]
    fn test_manifest_builder_full() {
        let config = ProviderConfig {
            jira_configured: true,
            github_configured: true,
            gitlab_configured: true,
            postgres_configured: false,
            custom_providers: vec![],
        };
        assert_eq!(build_manifests_from_config(&config).len(), 4);
    }

    #[test]
    fn test_router_with_bridge() {
        let router = KnowledgeRouter {
            manifests: builtin_manifests(),
            resolvers: vec![Arc::new(PatternReferenceResolver)],
        };
        let profile = TaskProfileLocal::default();
        let bridge = ProviderBridge::new(global_registry());
        let result = router.route(
            "task",
            "review src/core/knowledge_router/mod.rs",
            &profile,
            &[],
            Some(&bridge),
        );
        assert!(!result.candidates.is_empty());
    }

    #[test]
    fn context_advice_preserves_router_reference() {
        let router = KnowledgeRouter {
            manifests: builtin_manifests(),
            resolvers: vec![Arc::new(PatternReferenceResolver)],
        };
        let profile = TaskProfileLocal::default();
        let advice = router.context_advice(
            "task",
            "review src/core/knowledge_router/mod.rs",
            &profile,
            &builtin_manifests(),
            None,
        );

        assert!(advice.protects("Please review src/core/knowledge_router/mod.rs"));
    }
}
