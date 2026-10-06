// SPDX-License-Identifier: Apache-2.0

//! Unified Context Kernel integration for tool hot-paths.

use super::accounting_fix::{PostDeliveryAccounting, compute_honest_accounting};
use super::activation::{
    KernelModeConfig, load_config, should_suppress_in_mode, supplement_budget,
};
use super::bridge::{KernelEnrichment, kernel_enrich};

/// Result of the unified kernel integration for a single hot-path call.
#[derive(Debug, Clone)]
pub struct KernelIntegration {
    /// Text to append to the tool output (budget-capped, deduplicated).
    pub supplement: Option<String>,
    /// Whether the content should be suppressed (already delivered, enforce mode).
    pub suppress: bool,
    /// Honest token accounting for this call.
    pub accounting: PostDeliveryAccounting,
    /// Budget tokens consumed by the kernel.
    pub budget_used: usize,
}

/// Unified kernel integration for all hot-paths.
///
/// Call this instead of manually orchestrating kernel enrichment, activation,
/// and accounting. The returned supplement is already bounded by the active
/// kernel budget.
pub fn kernel_integrate(
    query: &str,
    project_root: &str,
    original_tokens: usize,
    compressed_tokens: usize,
) -> KernelIntegration {
    let config = load_config(project_root);
    let budget = supplement_budget(&config);
    let enrichment = kernel_enrich(query, project_root, budget);

    finish_integration(config.mode, original_tokens, compressed_tokens, enrichment)
}

/// Returns the stable delimiter placed before appended kernel context.
pub fn format_integration_header() -> &'static str {
    "\n--- kernel context ---\n"
}

/// Returns the kernel token overhead recorded for an integration.
pub fn integration_overhead(integration: &KernelIntegration) -> usize {
    integration.budget_used
}

fn finish_integration(
    mode: KernelModeConfig,
    original_tokens: usize,
    compressed_tokens: usize,
    enrichment: Option<KernelEnrichment>,
) -> KernelIntegration {
    let (supplement, budget_used) = match enrichment {
        Some(enrichment) => {
            let budget_used = crate::core::tokens::count_tokens(&enrichment.blocks);
            (Some(enrichment.blocks), budget_used)
        }
        None => (None, 0),
    };
    let accounting = compute_honest_accounting(original_tokens, compressed_tokens, budget_used, 0);

    KernelIntegration {
        supplement,
        suppress: should_suppress_in_mode(mode),
        accounting,
        budget_used,
    }
}

/// Bridge: integrate kernel context for an MCP tool call.
///
/// Unlike [`kernel_integrate`], which takes raw token counts, this function
/// takes headers and determines the optimal integration strategy based on
/// the client's coverage class and profile.
pub fn integrate_for_mcp(
    query: &str,
    project_root: &str,
    headers: &[(String, String)],
    original_tokens: usize,
    compressed_tokens: usize,
) -> KernelIntegration {
    let profile = super::client_profile::detect_from_headers(headers);
    let coverage = profile.coverage;

    if !super::coverage_class::is_addressable(coverage) {
        return KernelIntegration {
            supplement: None,
            suppress: false,
            accounting: compute_honest_accounting(original_tokens, compressed_tokens, 0, 0),
            budget_used: 0,
        };
    }

    kernel_integrate(query, project_root, original_tokens, compressed_tokens)
}

/// Returns the coverage-aware kernel budget for an MCP request.
pub fn mcp_kernel_budget(headers: &[(String, String)]) -> usize {
    let profile = super::client_profile::detect_from_headers(headers);
    let broker = super::context_broker::ContextBroker::new(profile);
    broker.compute_budget().kernel_tokens
}

#[cfg(test)]
pub mod tests {
    use std::collections::HashMap;
    use std::fs;

    use super::{
        KernelIntegration, finish_integration, integrate_for_mcp, integration_overhead,
        kernel_integrate, mcp_kernel_budget,
    };
    use crate::core::context_kernel::accounting_fix::detect_negative_savings;
    use crate::core::context_kernel::activation::KernelModeConfig;
    use crate::core::context_kernel::bridge::{KernelEnrichment, enrichment_from_plan};
    use crate::core::context_kernel::enforce::KernelMode;
    use crate::core::context_kernel::types::{ContextPlanV1, PlanBudget};

    fn enrichment(blocks: String) -> KernelEnrichment {
        let budget_used = crate::core::tokens::count_tokens(&blocks);
        enrichment_from_plan(
            ContextPlanV1 {
                plan_id: "test-plan".to_owned(),
                intent: "test".to_owned(),
                budget: PlanBudget {
                    total_tokens: 150,
                    used_tokens: budget_used,
                    remaining_tokens: 150_usize.saturating_sub(budget_used),
                },
                selected: Vec::new(),
                excluded: Vec::new(),
                deferred: Vec::new(),
                provider_stats: HashMap::new(),
                origins: Default::default(),
            },
            blocks,
            budget_used,
            KernelMode::Shadow,
        )
        .expect("nonempty accounting fixture")
    }

    fn integration(
        mode: KernelModeConfig,
        original_tokens: usize,
        compressed_tokens: usize,
        blocks: Option<String>,
    ) -> KernelIntegration {
        finish_integration(
            mode,
            original_tokens,
            compressed_tokens,
            blocks.map(enrichment),
        )
    }

    #[test]
    fn integrate_caps_budget_at_150() {
        let root = tempfile::tempdir().expect("temporary project root");
        fs::write(
            root.path().join(".lean-ctx.toml"),
            "[kernel]\nmax_supplement_tokens = 500\n",
        )
        .expect("kernel config should be writable");

        let result = kernel_integrate(
            "query with no project candidates",
            root.path().to_str().expect("UTF-8 project path"),
            1_000,
            500,
        );

        assert!(result.budget_used <= 150);
    }

    #[test]
    fn integrate_accounting_honest() {
        let result = integration(
            KernelModeConfig::Shadow,
            100,
            40,
            Some("kernel context".to_owned()),
        );

        assert_eq!(result.accounting.kernel_overhead_tokens, result.budget_used);
        assert_eq!(
            result.accounting.delivered_tokens,
            40 + integration_overhead(&result)
        );
    }

    #[test]
    fn integrate_shadow_no_suppress() {
        let result = integration(KernelModeConfig::Shadow, 100, 50, None);

        assert!(!result.suppress);
    }

    #[test]
    fn integrate_no_enrichment_zero_overhead() {
        let result = integration(KernelModeConfig::Enforce, 100, 50, None);

        assert_eq!(result.budget_used, 0);
        assert_eq!(result.accounting.kernel_overhead_tokens, 0);
        assert!(result.supplement.is_none());
    }

    #[test]
    fn integrate_negative_savings_detected() {
        let result = integration(
            KernelModeConfig::Shadow,
            10,
            10,
            Some("kernel overhead exceeds original input size".repeat(20)),
        );

        assert!(detect_negative_savings(&result.accounting));
    }

    #[test]
    fn mcp_unmanaged_no_enrichment() {
        let root = tempfile::tempdir().expect("temporary project root");
        let headers = vec![("x-coverage-class".to_owned(), "unmanaged".to_owned())];

        let result = integrate_for_mcp(
            "query with no project candidates",
            root.path().to_str().expect("UTF-8 project path"),
            &headers,
            100,
            50,
        );

        assert_eq!(result.budget_used, 0);
        assert!(result.supplement.is_none());
    }

    #[test]
    fn mcp_full_inline_enriches() {
        let root = tempfile::tempdir().expect("temporary project root");
        let project_root = root.path().to_str().expect("UTF-8 project path");
        let headers = vec![("x-coverage-class".to_owned(), "full_inline".to_owned())];
        let expected = kernel_integrate("bridge query", project_root, 100, 50);

        let result = integrate_for_mcp("bridge query", project_root, &headers, 100, 50);

        assert_eq!(result.supplement, expected.supplement);
        assert_eq!(result.budget_used, expected.budget_used);
        assert_eq!(result.suppress, expected.suppress);
    }

    #[test]
    fn mcp_budget_from_profile() {
        let headers = vec![("x-context-window".to_owned(), "64000".to_owned())];

        assert_eq!(mcp_kernel_budget(&headers), 6_400);
    }
}
