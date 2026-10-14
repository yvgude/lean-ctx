//! Budget-aware reduction of tool schemas exposed to clients.

use std::cmp::Reverse;

use super::client_profile::ClientEfficiencyProfile;

/// Schema metadata for one client-visible tool.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ToolSchema {
    /// Tool name exposed to the client.
    pub name: String,
    /// Human-readable explanation of the tool.
    pub description: String,
    /// JSON-encoded parameter schema.
    pub parameters_json: String,
    /// Estimated tokens consumed by the complete schema.
    pub token_count: usize,
    /// Selection priority, where larger values rank first.
    pub priority: u8,
    /// Stability tier used to filter the tool surface.
    pub category: ToolCategory,
}

/// Stability tier for a client-visible tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum ToolCategory {
    /// Tool belongs to the essential supported surface.
    #[default]
    Core,
    /// Tool belongs to the optional supported surface.
    Extended,
    /// Tool is available for evaluation.
    Experimental,
    /// Tool is retained only for compatibility.
    Deprecated,
}

/// Metrics and tool names produced by surface optimization.
#[derive(Debug, Clone)]
pub struct SurfaceReduction {
    /// Number of schemas before optimization.
    pub original_count: usize,
    /// Number of selected schemas.
    pub reduced_count: usize,
    /// Combined schema tokens before optimization.
    pub original_tokens: usize,
    /// Combined tokens for selected schemas.
    pub reduced_tokens: usize,
    /// Tokens removed from the client-visible surface.
    pub tokens_saved: usize,
    /// Percentage of original schema tokens removed.
    pub savings_pct: f64,
    /// Selected tool names in selection order.
    pub selected_tools: Vec<String>,
    /// Removed tool names in original order.
    pub removed_tools: Vec<String>,
}

/// Applies count, token, and lifecycle limits to a tool surface.
pub struct ToolSurfaceOptimizer {
    max_tools: usize,
    max_schema_tokens: usize,
    exclude_deprecated: bool,
}

impl ToolSurfaceOptimizer {
    /// Creates an optimizer that excludes deprecated tools by default.
    #[must_use]
    pub const fn new(max_tools: usize, max_schema_tokens: usize) -> Self {
        Self {
            max_tools,
            max_schema_tokens,
            exclude_deprecated: true,
        }
    }

    /// Creates an optimizer using the tool budget advertised by a client profile.
    #[must_use]
    pub fn from_profile(profile: &ClientEfficiencyProfile) -> Self {
        Self::new(
            profile.tool_budget.max_tools,
            profile.tool_budget.max_schema_tokens,
        )
    }

    /// Configures whether deprecated tools are removed before selection.
    #[must_use]
    pub const fn exclude_deprecated(mut self, exclude: bool) -> Self {
        self.exclude_deprecated = exclude;
        self
    }

    /// Selects the highest-priority schemas that fit both configured budgets.
    #[must_use]
    pub fn optimize(&self, schemas: &[ToolSchema]) -> SurfaceReduction {
        let original_tokens: usize = schemas.iter().map(|schema| schema.token_count).sum();
        let mut ranked = schemas
            .iter()
            .enumerate()
            .filter(|(_, schema)| {
                !self.exclude_deprecated || schema.category != ToolCategory::Deprecated
            })
            .collect::<Vec<_>>();
        ranked.sort_unstable_by_key(|(_, schema)| (Reverse(schema.priority), schema.token_count));

        let mut selected = vec![false; schemas.len()];
        let mut selected_tools = Vec::with_capacity(self.max_tools.min(ranked.len()));
        let mut reduced_tokens = 0usize;
        for (index, schema) in ranked {
            if selected_tools.len() == self.max_tools {
                break;
            }
            if reduced_tokens.saturating_add(schema.token_count) <= self.max_schema_tokens {
                reduced_tokens += schema.token_count;
                selected[index] = true;
                selected_tools.push(schema.name.clone());
            }
        }

        let removed_tools = schemas
            .iter()
            .enumerate()
            .filter(|(index, _)| !selected[*index])
            .map(|(_, schema)| schema.name.clone())
            .collect();
        let tokens_saved = original_tokens.saturating_sub(reduced_tokens);
        let savings_pct = if original_tokens == 0 {
            0.0
        } else {
            tokens_saved as f64 * 100.0 / original_tokens as f64
        };

        SurfaceReduction {
            original_count: schemas.len(),
            reduced_count: selected_tools.len(),
            original_tokens,
            reduced_tokens,
            tokens_saved,
            savings_pct,
            selected_tools,
            removed_tools,
        }
    }
}

/// Returns a compact copy of a tool schema and recalculates its token estimate.
#[must_use]
pub fn compress_schema(schema: &ToolSchema) -> ToolSchema {
    let description = schema.description.chars().take(100).collect::<String>();
    let parameters_json = strip_json_whitespace(&schema.parameters_json);
    let token_count = (schema.name.len() + description.len() + parameters_json.len()).div_ceil(4);
    ToolSchema {
        name: schema.name.clone(),
        description,
        parameters_json,
        token_count,
        priority: schema.priority,
        category: schema.category,
    }
}

/// Formats headline surface-reduction metrics for logs and diagnostics.
#[must_use]
pub fn format_reduction_summary(reduction: &SurfaceReduction) -> String {
    format!(
        "Reduced {}→{} tools, saved {} tokens ({:.1}%)",
        reduction.original_count,
        reduction.reduced_count,
        reduction.tokens_saved,
        reduction.savings_pct
    )
}

fn strip_json_whitespace(json: &str) -> String {
    let mut compact = String::with_capacity(json.len());
    let mut in_string = false;
    let mut escaped = false;
    for character in json.chars() {
        if in_string {
            compact.push(character);
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                in_string = false;
            }
        } else if character == '"' {
            in_string = true;
            compact.push(character);
        } else if !character.is_whitespace() {
            compact.push(character);
        }
    }
    compact
}

/// Bridge: optimize MCP tool schemas for the current request profile.
///
/// Called by the MCP server to reduce tool schema tokens based on
/// the client's efficiency profile and broker decisions.
#[must_use]
pub fn optimize_for_request(
    headers: &[(String, String)],
    schemas: &[ToolSchema],
) -> SurfaceReduction {
    let profile = super::client_profile::detect_from_headers(headers);
    let optimizer = ToolSurfaceOptimizer::from_profile(&profile);
    optimizer.optimize(schemas)
}

/// Returns the token savings from tool surface optimization.
#[must_use]
pub const fn tool_savings_tokens(reduction: &SurfaceReduction) -> usize {
    reduction.tokens_saved
}

/// Returns true if tool surface optimization would save significant tokens.
#[must_use]
pub fn should_optimize_tools(headers: &[(String, String)], tool_count: usize) -> bool {
    let profile = super::client_profile::detect_from_headers(headers);
    tool_count > profile.tool_budget.max_tools
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::core::context_kernel::client_profile::{ProfileBuilder, ToolBudget};

    fn schema(name: &str, priority: u8, tokens: usize, category: ToolCategory) -> ToolSchema {
        ToolSchema {
            name: name.to_owned(),
            description: "description".to_owned(),
            parameters_json: r#"{ "type": "object" }"#.to_owned(),
            token_count: tokens,
            priority,
            category,
        }
    }

    #[test]
    fn optimize_within_budget() {
        let schemas = (0..10)
            .map(|index| schema(&format!("tool-{index}"), index, 10, ToolCategory::Core))
            .collect::<Vec<_>>();
        assert_eq!(
            ToolSurfaceOptimizer::new(5, 100)
                .optimize(&schemas)
                .reduced_count,
            5
        );
    }

    #[test]
    fn optimize_by_priority() {
        let schemas = vec![
            schema("low", 1, 10, ToolCategory::Core),
            schema("high", 9, 10, ToolCategory::Core),
        ];
        assert_eq!(
            ToolSurfaceOptimizer::new(1, 10)
                .optimize(&schemas)
                .selected_tools,
            ["high"]
        );
    }

    #[test]
    fn optimize_excludes_deprecated() {
        let schemas = vec![schema("old", 10, 1, ToolCategory::Deprecated)];
        let reduction = ToolSurfaceOptimizer::new(1, 10).optimize(&schemas);
        assert!(
            reduction.selected_tools.is_empty(),
            "{:?}",
            reduction.selected_tools
        );
        assert_eq!(reduction.removed_tools, ["old"]);
    }

    #[test]
    fn optimize_respects_token_budget() {
        let schemas = vec![
            schema("a", 3, 6, ToolCategory::Core),
            schema("b", 2, 5, ToolCategory::Core),
            schema("c", 1, 4, ToolCategory::Core),
        ];
        let reduction = ToolSurfaceOptimizer::new(3, 10).optimize(&schemas);
        assert!(reduction.reduced_tokens <= 10);
        assert_eq!(reduction.selected_tools, ["a", "c"]);
    }

    #[test]
    fn compress_strips_whitespace() {
        let compact = compress_schema(&schema("a", 1, 10, ToolCategory::Core));
        assert_eq!(compact.parameters_json, r#"{"type":"object"}"#);
    }

    #[test]
    fn compress_truncates_description() {
        let mut input = schema("a", 1, 100, ToolCategory::Core);
        input.description = "é".repeat(101);
        assert_eq!(compress_schema(&input).description.chars().count(), 100);
    }

    #[test]
    fn from_profile_uses_budget() {
        let profile = ProfileBuilder::new("client")
            .tool_budget(ToolBudget {
                max_tools: 1,
                max_schema_tokens: 5,
            })
            .build();
        let optimizer = ToolSurfaceOptimizer::from_profile(&profile);
        assert_eq!(optimizer.max_tools, 1);
        assert_eq!(optimizer.max_schema_tokens, 5);
    }

    #[test]
    fn savings_pct_correct() {
        let schemas = vec![
            schema("a", 2, 25, ToolCategory::Core),
            schema("b", 1, 75, ToolCategory::Core),
        ];
        let reduction = ToolSurfaceOptimizer::new(1, 100).optimize(&schemas);
        assert_eq!(reduction.tokens_saved, 75);
        assert!((reduction.savings_pct - 75.0).abs() < f64::EPSILON);
    }

    #[test]
    fn optimize_for_request_reduces() {
        let schemas = (0..20)
            .map(|index| schema(&format!("tool-{index}"), 1, 1_000, ToolCategory::Core))
            .collect::<Vec<_>>();
        let reduction = optimize_for_request(&[], &schemas);
        assert!(reduction.reduced_count < schemas.len());
        assert!(tool_savings_tokens(&reduction) > 0);
    }

    #[test]
    fn should_optimize_over_budget() {
        assert!(should_optimize_tools(&[], 65));
    }

    #[test]
    fn should_optimize_under_budget() {
        assert!(!should_optimize_tools(&[], 3));
    }
}
