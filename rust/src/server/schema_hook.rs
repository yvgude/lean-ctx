//! Server bridge for Context Kernel list-tools schema optimization.

use std::borrow::Cow;

use crate::core::context_kernel::list_tools_opt;

/// Optimizes tool descriptions when schema optimization supports the client.
#[must_use]
pub fn optimize_tools(tools: Vec<rmcp::model::Tool>, client_name: &str) -> Vec<rmcp::model::Tool> {
    if !list_tools_opt::should_optimize_for_client(client_name) {
        return tools;
    }

    let tuples = tools
        .iter()
        .map(|tool| {
            let parameter_count = tool
                .input_schema
                .get("properties")
                .and_then(serde_json::Value::as_object)
                .map_or(0, serde_json::Map::len);
            (
                tool.name.to_string(),
                tool.description.as_deref().unwrap_or("").to_owned(),
                parameter_count,
            )
        })
        .collect();
    // The optimizer re-sorts its entries and may drop some to meet the budget,
    // so its descriptions are matched back by tool name; a tool it dropped
    // keeps its own description and stays in the list.
    let mut optimized: std::collections::HashMap<String, String> =
        list_tools_opt::optimize_descriptions(tuples, client_name)
            .into_iter()
            .map(|(name, description, _)| (name, description))
            .collect();

    tools
        .into_iter()
        .map(|mut tool| {
            if let Some(description) = optimized.remove(tool.name.as_ref()) {
                tool.description = Some(Cow::Owned(description));
            }
            tool
        })
        .collect()
}

/// Returns cumulative list-tools schema optimization statistics.
#[must_use]
pub fn summary() -> list_tools_opt::SchemaOptSummary {
    list_tools_opt::schema_opt_summary()
}

#[cfg(test)]
mod tests {
    use std::sync::MutexGuard;

    use super::list_tools_opt;
    use crate::core::context_kernel::kernel_config::{self, KERNEL_TEST_LOCK, KernelFeatures};

    fn setup() -> MutexGuard<'static, ()> {
        let guard = KERNEL_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        kernel_config::reset_features();
        list_tools_opt::reset();
        guard
    }

    #[test]
    fn disabled_returns_same_count() {
        let _guard = setup();
        let features = KernelFeatures {
            enabled: false,
            ..KernelFeatures::default()
        };
        kernel_config::update_features(features);
        let tools = vec![("tool".to_owned(), "description".to_owned(), 1)];

        assert_eq!(
            list_tools_opt::optimize_descriptions(tools, "cursor").len(),
            1
        );
    }

    #[test]
    fn summary_works() {
        let _guard = setup();
        let summary = list_tools_opt::schema_opt_summary();

        assert_eq!(summary.optimizations_applied, 0);
        assert_eq!(summary.total_tokens_saved, 0);
        assert_eq!(summary.avg_reduction_percent, 0.0);
    }

    /// F7: the optimizer sorts by size and drops entries over budget; every
    /// tool must survive and keep a description that belongs to it.
    #[test]
    fn descriptions_are_paired_by_name_and_no_tool_is_lost() {
        let _guard = setup();
        let tools: Vec<rmcp::model::Tool> = (0..40)
            .map(|i| {
                // Descending size, so the optimizer's ascending sort reverses
                // the order, and large enough that the budget drops some.
                let description = format!("tool-{i}: {}", "x".repeat(8_000 - i * 100));
                rmcp::model::Tool::new(
                    format!("tool_{i}"),
                    description,
                    std::sync::Arc::new(serde_json::Map::new()),
                )
            })
            .collect();

        let optimized = super::optimize_tools(tools, "cursor");

        assert_eq!(optimized.len(), 40, "no tool may be dropped from the list");
        for (i, tool) in optimized.iter().enumerate() {
            assert_eq!(tool.name, format!("tool_{i}"));
            let description = tool.description.as_deref().unwrap_or("");
            assert!(
                description.starts_with(&format!("tool-{i}: ")),
                "{} got a foreign description: {}",
                tool.name,
                &description[..description.len().min(20)]
            );
        }
    }

    #[test]
    fn optimization_flag_check() {
        let _guard = setup();

        assert!(list_tools_opt::should_optimize_for_client("cursor"));
        assert!(!list_tools_opt::should_optimize_for_client(
            "unknown-client"
        ));
    }
}
