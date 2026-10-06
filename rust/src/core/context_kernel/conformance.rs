// SPDX-License-Identifier: Apache-2.0

#[cfg(test)]
mod tests {
    use crate::core::context_field::{ContextItemId, TokenBudget};
    use crate::core::context_kernel::orchestrator::ContextKernel;
    use crate::core::context_kernel::policy::{ContextPolicy, PolicyFilter};
    use crate::core::context_kernel::types::{
        ContextObjectKind, ContextObjectV1, ReceiptOutcome, RetrievalContext, SensitivityLevel,
    };

    fn test_candidate(
        source: &str,
        sensitivity: SensitivityLevel,
        tokens: usize,
    ) -> ContextObjectV1 {
        ContextObjectV1 {
            id: ContextItemId(format!("test:{source}")),
            kind: ContextObjectKind::Fact,
            source: source.to_owned(),
            sensitivity,
            token_estimate: tokens,
            ..ContextObjectV1::default()
        }
    }

    #[test]
    fn plan_receipt_roundtrip() {
        let project_root = std::env::temp_dir().join("lean-ctx-kernel-conformance");
        let project_root_text = project_root.to_string_lossy();
        let kernel = ContextKernel::for_project(project_root_text.as_ref());
        let context = RetrievalContext {
            query: "context kernel conformance".to_owned(),
            task: Some("verify plan receipt roundtrip".to_owned()),
            project_root: project_root_text.into_owned(),
            budget: TokenBudget {
                total: 1_000,
                used: 0,
            },
            max_candidates: 10,
        };

        let plan = kernel.plan(&context).expect("valid conformance plan");
        let receipt = kernel.record_receipt(&plan, 64, ReceiptOutcome::Accepted);

        assert_eq!(receipt.plan_id, plan.plan_id);
        assert!(receipt.delivered_tokens > 0);
    }

    #[test]
    fn policy_filters_sensitive_candidates() {
        let candidates: Vec<ContextObjectV1> = vec![
            test_candidate("public", SensitivityLevel::Public, 20),
            test_candidate("restricted", SensitivityLevel::Restricted, 20),
        ];
        let policy = ContextPolicy {
            max_sensitivity: SensitivityLevel::Internal,
            allowed_sources: None,
            blocked_sources: Vec::new(),
            budget_cap_tokens: None,
            retention_days: None,
        };
        let filter = PolicyFilter::new(policy);

        let filtered = filter.apply(candidates);

        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].source, "public");
        assert_eq!(filtered[0].sensitivity, SensitivityLevel::Public);
    }
}
