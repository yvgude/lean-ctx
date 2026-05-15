use std::collections::HashMap;

use rmcp::model::Tool;

use super::tool_trait::McpTool;

/// Central registry mapping tool names to their trait-based handlers.
/// Replaces the match-cascade dispatch for migrated tools while
/// coexisting with the legacy dispatch for tools not yet migrated.
pub struct ToolRegistry {
    tools: HashMap<&'static str, Box<dyn McpTool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    pub fn register(&mut self, tool: Box<dyn McpTool>) {
        self.tools.insert(tool.name(), tool);
    }

    pub fn get(&self, name: &str) -> Option<&dyn McpTool> {
        self.tools.get(name).map(AsRef::as_ref)
    }

    pub fn contains(&self, name: &str) -> bool {
        self.tools.contains_key(name)
    }

    /// Returns MCP Tool definitions for all registered tools.
    /// Used by `list_tools` to expose schemas to clients.
    pub fn tool_defs(&self) -> Vec<Tool> {
        let mut defs: Vec<Tool> = self.tools.values().map(|t| t.tool_def()).collect();
        defs.sort_by(|a, b| a.name.as_ref().cmp(b.name.as_ref()));
        defs
    }

    /// Returns tool definitions filtered by the dynamic tool state.
    /// Only includes tools whose category is currently active.
    pub fn active_tool_defs(&self) -> Vec<Tool> {
        let state = super::dynamic_tools::global().lock().unwrap();
        let mut defs: Vec<Tool> = self
            .tools
            .values()
            .filter(|t| state.is_tool_active(t.name()))
            .map(|t| t.tool_def())
            .collect();
        defs.sort_by(|a, b| a.name.as_ref().cmp(b.name.as_ref()));
        defs
    }

    pub fn len(&self) -> usize {
        self.tools.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    pub fn names(&self) -> Vec<&'static str> {
        let mut names: Vec<_> = self.tools.keys().copied().collect();
        names.sort_unstable();
        names
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Register all trait-based tools. Called once during server startup.
/// Tools are added here as they are migrated from the legacy dispatch.
pub fn build_registry() -> ToolRegistry {
    let mut registry = ToolRegistry::new();

    use crate::tools::registered;
    registry.register(Box::new(registered::ctx_tree::CtxTreeTool));
    registry.register(Box::new(registered::ctx_benchmark::CtxBenchmarkTool));
    registry.register(Box::new(registered::ctx_analyze::CtxAnalyzeTool));
    registry.register(Box::new(registered::ctx_discover::CtxDiscoverTool));
    registry.register(Box::new(registered::ctx_response::CtxResponseTool));
    registry.register(Box::new(registered::ctx_heatmap::CtxHeatmapTool));
    registry.register(Box::new(registered::ctx_verify::CtxVerifyTool));
    registry.register(Box::new(registered::ctx_outline::CtxOutlineTool));
    registry.register(Box::new(registered::ctx_cost::CtxCostTool));
    registry.register(Box::new(registered::ctx_gain::CtxGainTool));
    registry.register(Box::new(registered::ctx_expand::CtxExpandTool));
    registry.register(Box::new(registered::ctx_routes::CtxRoutesTool));
    registry.register(Box::new(registered::ctx_callgraph::CtxCallgraphTool));
    registry.register(Box::new(registered::ctx_refactor::CtxRefactorTool));
    registry.register(Box::new(registered::ctx_symbol::CtxSymbolTool));
    registry.register(Box::new(
        registered::ctx_discover_tools::CtxDiscoverToolsTool,
    ));
    registry.register(Box::new(registered::ctx_review::CtxReviewTool));
    registry.register(Box::new(registered::ctx_provider::CtxProviderTool));
    registry.register(Box::new(registered::ctx_impact::CtxImpactTool));
    registry.register(Box::new(registered::ctx_architecture::CtxArchitectureTool));
    registry.register(Box::new(registered::ctx_smells::CtxSmellsTool));
    registry.register(Box::new(registered::ctx_pack::CtxPackTool));
    registry.register(Box::new(registered::ctx_index::CtxIndexTool));
    registry.register(Box::new(registered::ctx_artifacts::CtxArtifactsTool));
    registry.register(Box::new(
        registered::ctx_compress_memory::CtxCompressMemoryTool,
    ));
    registry.register(Box::new(registered::ctx_read::CtxReadTool));
    registry.register(Box::new(registered::ctx_multi_read::CtxMultiReadTool));
    registry.register(Box::new(registered::ctx_smart_read::CtxSmartReadTool));
    registry.register(Box::new(registered::ctx_delta::CtxDeltaTool));
    registry.register(Box::new(registered::ctx_edit::CtxEditTool));
    registry.register(Box::new(registered::ctx_fill::CtxFillTool));
    registry.register(Box::new(registered::ctx_shell::CtxShellTool));
    registry.register(Box::new(registered::ctx_search::CtxSearchTool));
    registry.register(Box::new(registered::ctx_execute::CtxExecuteTool));

    // Utility tools (migrated from dispatch/utility_tools.rs)
    registry.register(Box::new(registered::ctx_compress::CtxCompressTool));
    registry.register(Box::new(registered::ctx_metrics::CtxMetricsTool));
    registry.register(Box::new(registered::ctx_radar::CtxRadarTool));
    registry.register(Box::new(registered::ctx_dedup::CtxDedupTool));
    registry.register(Box::new(registered::ctx_intent::CtxIntentTool));
    registry.register(Box::new(registered::ctx_context::CtxContextTool));
    registry.register(Box::new(registered::ctx_graph::CtxGraphTool));
    registry.register(Box::new(registered::ctx_proof::CtxProofTool));
    registry.register(Box::new(registered::ctx_cache::CtxCacheTool));
    registry.register(Box::new(registered::ctx_retrieve::CtxRetrieveTool));
    registry.register(Box::new(registered::ctx_overview::CtxOverviewTool));
    registry.register(Box::new(registered::ctx_preload::CtxPreloadTool));
    registry.register(Box::new(registered::ctx_prefetch::CtxPrefetchTool));
    registry.register(Box::new(
        registered::ctx_semantic_search::CtxSemanticSearchTool,
    ));
    registry.register(Box::new(registered::ctx_feedback::CtxFeedbackTool));
    registry.register(Box::new(registered::ctx_control::CtxControlTool));
    registry.register(Box::new(registered::ctx_plan::CtxPlanTool));
    registry.register(Box::new(registered::ctx_compile::CtxCompileTool));

    // Session tools (migrated from legacy dispatch)
    registry.register(Box::new(registered::ctx_session::CtxSessionTool));
    registry.register(Box::new(registered::ctx_knowledge::CtxKnowledgeTool));
    registry.register(Box::new(registered::ctx_agent::CtxAgentTool));
    registry.register(Box::new(registered::ctx_share::CtxShareTool));
    registry.register(Box::new(registered::ctx_task::CtxTaskTool));
    registry.register(Box::new(registered::ctx_handoff::CtxHandoffTool));
    registry.register(Box::new(registered::ctx_workflow::CtxWorkflowTool));

    registry
}
