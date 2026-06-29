use std::collections::HashMap;
use std::sync::Arc;

use rmcp::model::Tool;

use super::tool_trait::McpTool;

/// Central registry mapping tool names to their trait-based handlers.
/// Every tool is trait-based and resolved here; the earlier
/// match-cascade dispatch has been fully retired.
///
/// Handlers are stored behind `Arc` (not `Box`) so the dispatch layer can hand
/// an owned, `'static` handle to `tokio::task::spawn_blocking`. That lets a
/// blocking handler run on the dedicated blocking pool under a watchdog
/// deadline instead of pinning a scarce core worker via `block_in_place`
/// (#271 — a hung handler must never swallow the JSON-RPC response).
pub struct ToolRegistry {
    tools: HashMap<&'static str, Arc<dyn McpTool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    pub fn register(&mut self, tool: Box<dyn McpTool>) {
        let name = tool.name();
        self.tools.insert(name, Arc::from(tool));
    }

    pub fn get(&self, name: &str) -> Option<&dyn McpTool> {
        self.tools.get(name).map(|t| &**t)
    }

    /// Clone an owned, `'static` handle to a registered tool.
    ///
    /// Unlike [`get`](Self::get), the returned `Arc` can be moved into
    /// `spawn_blocking`, so the dispatch layer can execute the (synchronous)
    /// handler off the async core workers under a watchdog (#271).
    pub fn get_arc(&self, name: &str) -> Option<Arc<dyn McpTool>> {
        self.tools.get(name).cloned()
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
        let Ok(state) = super::dynamic_tools::global().lock() else {
            tracing::warn!("dynamic_tools mutex poisoned in active_tool_defs; returning all");
            return self.tool_defs();
        };
        let mut defs: Vec<Tool> = self
            .tools
            .values()
            .filter(|t| state.is_tool_active(t.name()))
            .map(|t| t.tool_def())
            .collect();
        defs.sort_by(|a, b| a.name.as_ref().cmp(b.name.as_ref()));
        defs
    }

    /// Returns tool definitions filtered by a tool profile.
    /// Only includes tools whose name is enabled by the given profile.
    pub fn profile_tool_defs(
        &self,
        profile: &crate::core::tool_profiles::ToolProfile,
    ) -> Vec<Tool> {
        let mut defs: Vec<Tool> = self
            .tools
            .values()
            .filter(|t| profile.is_tool_enabled(t.name()))
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

/// Number of registered MCP tools — the single source of truth for the
/// "N MCP tools" count shown in `--help`, the README, and the feature catalog.
/// Deriving it here means the count can never drift from the actual registry.
pub fn tool_count() -> usize {
    build_registry().len()
}

/// Register all trait-based tools. Called once during server startup.
/// New tools are added here as their `McpTool` implementation lands.
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
    registry.register(Box::new(registered::ctx_call::CtxCallTool));
    registry.register(Box::new(registered::ctx_callgraph::CtxCallgraphTool));
    registry.register(Box::new(registered::ctx_refactor::CtxRefactorTool));
    registry.register(Box::new(registered::ctx_repomap::CtxRepomapTool));
    registry.register(Box::new(registered::ctx_symbol::CtxSymbolTool));
    registry.register(Box::new(
        registered::ctx_discover_tools::CtxDiscoverToolsTool,
    ));
    registry.register(Box::new(registered::ctx_tools::CtxToolsTool));
    registry.register(Box::new(registered::ctx_review::CtxReviewTool));
    registry.register(Box::new(registered::ctx_provider::CtxProviderTool));
    registry.register(Box::new(registered::ctx_impact::CtxImpactTool));
    registry.register(Box::new(registered::ctx_architecture::CtxArchitectureTool));
    registry.register(Box::new(registered::ctx_smells::CtxSmellsTool));
    registry.register(Box::new(registered::ctx_quality::CtxQualityTool));
    registry.register(Box::new(registered::ctx_pack::CtxPackTool));
    registry.register(Box::new(registered::ctx_plugins::CtxPluginsTool));
    registry.register(Box::new(registered::ctx_rules::CtxRulesTool));
    registry.register(Box::new(registered::ctx_index::CtxIndexTool));
    registry.register(Box::new(registered::ctx_artifacts::CtxArtifactsTool));
    registry.register(Box::new(
        registered::ctx_compress_memory::CtxCompressMemoryTool,
    ));
    registry.register(Box::new(registered::ctx_read::CtxReadTool));
    registry.register(Box::new(registered::ctx_multi_read::CtxMultiReadTool));
    registry.register(Box::new(registered::ctx_multi_repo::CtxMultiRepoTool));
    registry.register(Box::new(registered::ctx_smart_read::CtxSmartReadTool));
    registry.register(Box::new(registered::ctx_delta::CtxDeltaTool));
    registry.register(Box::new(registered::ctx_edit::CtxEditTool));
    registry.register(Box::new(registered::ctx_patch::CtxPatchTool));
    registry.register(Box::new(registered::ctx_fill::CtxFillTool));
    registry.register(Box::new(registered::ctx_glob::CtxGlobTool));
    registry.register(Box::new(registered::ctx_shell::CtxShellTool));
    registry.register(Box::new(registered::shell_alias::ShellAliasTool));
    registry.register(Box::new(registered::ctx_search::CtxSearchTool));
    registry.register(Box::new(registered::ctx_url_read::CtxUrlReadTool));
    registry.register(Box::new(registered::ctx_git_read::CtxGitReadTool));
    registry.register(Box::new(registered::ctx_checkpoint::CtxCheckpointTool));
    registry.register(Box::new(registered::ctx_compose::CtxComposeTool));
    registry.register(Box::new(registered::ctx_explore::CtxExploreTool));
    registry.register(Box::new(registered::ctx_execute::CtxExecuteTool));

    // Utility tools (migrated from dispatch/utility_tools.rs)
    registry.register(Box::new(registered::ctx_compress::CtxCompressTool));
    registry.register(Box::new(registered::ctx_compare::CtxCompareTool));
    registry.register(Box::new(registered::ctx_metrics::CtxMetricsTool));
    registry.register(Box::new(registered::ctx_radar::CtxRadarTool));
    registry.register(Box::new(registered::ctx_dedup::CtxDedupTool));
    registry.register(Box::new(registered::ctx_intent::CtxIntentTool));
    registry.register(Box::new(registered::ctx_context::CtxContextTool));
    registry.register(Box::new(registered::ctx_graph::CtxGraphTool));
    registry.register(Box::new(registered::ctx_proof::CtxProofTool));
    registry.register(Box::new(registered::ctx_cache::CtxCacheTool));
    registry.register(Box::new(registered::ctx_ledger::CtxLedgerTool));
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
    registry.register(Box::new(registered::ctx_skillify::CtxSkillifyTool));
    registry.register(Box::new(registered::ctx_summary::CtxSummaryTool));
    registry.register(Box::new(
        registered::ctx_transcript_compact::CtxTranscriptCompactTool,
    ));
    registry.register(Box::new(registered::ctx_package::CtxPackageTool));
    registry.register(Box::new(registered::ctx_task::CtxTaskTool));
    registry.register(Box::new(registered::ctx_handoff::CtxHandoffTool));
    registry.register(Box::new(registered::ctx_workflow::CtxWorkflowTool));
    registry.register(Box::new(registered::ctx_load_tools::CtxLoadToolsTool));

    register_plugin_tools(&mut registry);

    registry
}

/// Append manifest-declared plugin tools (EPIC 12.11) without forking the
/// registry. Only enabled plugins contribute; a tool whose name collides with a
/// native tool is skipped (native tools win) so a plugin can never shadow core
/// behavior. No-op when no plugins are installed.
fn register_plugin_tools(registry: &mut ToolRegistry) {
    for spec in crate::core::plugins::PluginManager::tool_specs() {
        if registry.contains(&spec.name) {
            tracing::warn!(
                "plugin '{}' tool '{}' collides with a native tool; skipping",
                spec.plugin_name,
                spec.name
            );
            continue;
        }
        registry.register(Box::new(
            crate::tools::registered::plugin_tool::PluginTool::from_spec(spec),
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_arc_returns_owned_handle_for_known_tool() {
        let registry = build_registry();
        let arc = registry.get_arc("ctx_tree");
        assert!(arc.is_some(), "ctx_tree must be registered");
        assert_eq!(arc.unwrap().name(), "ctx_tree");
    }

    #[test]
    fn get_arc_is_none_for_unknown_tool() {
        let registry = build_registry();
        assert!(registry.get_arc("ctx_does_not_exist_xyz").is_none());
    }

    #[test]
    fn get_and_get_arc_agree_for_core_tool() {
        let registry = build_registry();
        assert_eq!(
            registry.get("ctx_read").is_some(),
            registry.get_arc("ctx_read").is_some(),
            "get and get_arc must agree on tool presence"
        );
    }
}
