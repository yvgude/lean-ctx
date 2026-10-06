#[cfg(test)]
mod golden_workloads {
    use std::collections::HashMap;

    use super::super::orchestrator::ContextKernel;
    use super::super::types::{
        CandidateProvider, ContextObjectKind, ContextObjectV1, Freshness, RetrievalContext,
        SensitivityLevel, SideEffectPolicy,
    };
    use crate::core::context_field::{ContextItemId, Provenance, TokenBudget, ViewCosts};

    fn test_object(
        id: &str,
        kind: ContextObjectKind,
        title: &str,
        confidence: f32,
        tokens: usize,
    ) -> ContextObjectV1 {
        ContextObjectV1 {
            id: ContextItemId::from_provider("golden", id),
            kind,
            source: "test".to_owned(),
            content_ref: format!("ref:{id}"),
            title: title.to_owned(),
            content: Some(format!("Content for {title}")),
            freshness: Freshness::default(),
            confidence,
            sensitivity: SensitivityLevel::Internal,
            token_estimate: tokens,
            view_costs: ViewCosts::default(),
            provenance: Provenance::default(),
            semantic_fingerprint: None,
            metadata: HashMap::new(),
        }
    }

    struct GoldenProvider {
        name: String,
        candidates: Vec<ContextObjectV1>,
    }

    impl CandidateProvider for GoldenProvider {
        fn provider_id(&self) -> &str {
            &self.name
        }

        fn candidates(&self, _ctx: &RetrievalContext) -> Vec<ContextObjectV1> {
            self.candidates.clone()
        }

        fn side_effect_policy(&self) -> SideEffectPolicy {
            SideEffectPolicy::ReadOnly
        }
    }

    fn retrieval_ctx(query: &str, budget: usize) -> RetrievalContext {
        RetrievalContext {
            query: query.to_owned(),
            task: Some(query.to_owned()),
            project_root: "/tmp/golden-test".to_owned(),
            budget: TokenBudget {
                total: budget,
                used: 0,
            },
            max_candidates: 20,
        }
    }

    fn object_id(id: &str) -> String {
        ContextItemId::from_provider("golden", id).to_string()
    }

    #[test]
    fn golden_auth_bug_hunt() {
        let knowledge = GoldenProvider {
            name: "golden.knowledge".to_owned(),
            candidates: vec![
                test_object(
                    "auth-middleware",
                    ContextObjectKind::Fact,
                    "auth middleware uses JWT",
                    0.95,
                    100,
                ),
                test_object(
                    "rate-limiter",
                    ContextObjectKind::Fact,
                    "rate limiter config",
                    0.10,
                    100,
                ),
            ],
        };
        let memory = GoldenProvider {
            name: "golden.memory".to_owned(),
            candidates: vec![
                test_object(
                    "auth-bypass",
                    ContextObjectKind::Episode,
                    "fixed auth bypass in v2.3",
                    0.75,
                    100,
                ),
                test_object(
                    "database-migration",
                    ContextObjectKind::Episode,
                    "database migration v1.0",
                    0.10,
                    100,
                ),
                test_object(
                    "auth-debug-sequence",
                    ContextObjectKind::Procedure,
                    "auth debug sequence",
                    0.95,
                    100,
                ),
            ],
        };
        let kernel = ContextKernel::new(vec![Box::new(knowledge), Box::new(memory)]);
        let plan = kernel
            .plan(&retrieval_ctx("auth middleware bypass debug sequence", 300))
            .expect("valid auth workload plan");

        assert!(
            plan.selected
                .iter()
                .any(|entry| entry.object_id == object_id("auth-middleware"))
        );
        assert!(
            plan.selected
                .iter()
                .any(|entry| entry.object_id == object_id("auth-bypass"))
        );
        assert!(
            plan.excluded
                .iter()
                .any(|entry| entry.object_id == object_id("rate-limiter"))
        );
        assert!(
            plan.excluded
                .iter()
                .any(|entry| entry.object_id == object_id("database-migration"))
        );
        assert!(plan.budget.used_tokens <= plan.budget.total_tokens);
    }

    #[test]
    fn golden_caching_feature() {
        let provider = GoldenProvider {
            name: "golden.caching".to_owned(),
            candidates: vec![
                test_object(
                    "cache-invalidation",
                    ContextObjectKind::Procedure,
                    "cache invalidation pattern",
                    0.95,
                    100,
                ),
                test_object(
                    "redis-pool",
                    ContextObjectKind::Fact,
                    "Redis connection pool config",
                    0.75,
                    100,
                ),
                test_object(
                    "current-cache-task",
                    ContextObjectKind::SessionItem,
                    "current task: add caching",
                    0.95,
                    100,
                ),
                test_object(
                    "unrelated-refactoring",
                    ContextObjectKind::Episode,
                    "unrelated refactoring",
                    0.10,
                    100,
                ),
            ],
        };
        let kernel = ContextKernel::new(vec![Box::new(provider)]);
        let plan = kernel
            .plan(&retrieval_ctx(
                "cache invalidation Redis connection pool current task add caching",
                300,
            ))
            .expect("valid caching workload plan");

        for id in ["cache-invalidation", "redis-pool", "current-cache-task"] {
            assert!(
                plan.selected
                    .iter()
                    .any(|entry| entry.object_id == object_id(id)),
                "expected {id} to be selected"
            );
        }
        assert!(
            plan.excluded
                .iter()
                .any(|entry| entry.object_id == object_id("unrelated-refactoring"))
        );
        assert!(plan.budget.used_tokens <= plan.budget.total_tokens);
    }

    #[test]
    fn golden_budget_constraint() {
        let provider = GoldenProvider {
            name: "golden.budget".to_owned(),
            candidates: vec![
                test_object("item-a", ContextObjectKind::Fact, "item A", 0.9, 100),
                test_object("item-b", ContextObjectKind::Fact, "item B", 0.8, 100),
                test_object("item-c", ContextObjectKind::Fact, "item C", 0.7, 100),
            ],
        };
        let kernel = ContextKernel::new(vec![Box::new(provider)]);
        let plan = kernel
            .plan(&retrieval_ctx("item", 200))
            .expect("valid budget workload plan");

        assert_eq!(plan.selected.len(), 2);
        assert!(
            plan.selected
                .iter()
                .any(|entry| entry.object_id == object_id("item-a"))
        );
        assert!(
            plan.selected
                .iter()
                .any(|entry| entry.object_id == object_id("item-b"))
        );
        assert!(
            plan.deferred
                .iter()
                .any(|entry| entry.object_id == object_id("item-c"))
                || plan
                    .excluded
                    .iter()
                    .any(|entry| entry.object_id == object_id("item-c"))
        );
        assert!(plan.budget.used_tokens <= 200);
    }
}
