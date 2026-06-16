use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput, get_str};
use crate::tool_defs::tool_def;

pub struct CtxCallTool;

impl McpTool for CtxCallTool {
    fn name(&self) -> &'static str {
        "ctx_call"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_call",
            "Invoke any non-core lean-ctx tool by name.\n\
             arch: ctx_architecture, ctx_impact, ctx_callgraph, ctx_refactor, ctx_symbol, ctx_routes, ctx_smells\n\
             debug: ctx_benchmark, ctx_verify, ctx_analyze, ctx_profile, ctx_review\n\
             memory: ctx_semantic_search, ctx_artifacts\n\
             batch: ctx_fill, ctx_execute, ctx_pack, ctx_plan, ctx_compile\n\
             agent: ctx_agent, ctx_share, ctx_task, ctx_handoff, ctx_workflow\n\
             util: ctx_compress, ctx_cache, ctx_metrics, ctx_dedup, ctx_cost, ctx_heatmap, ctx_preload\n\
             Discover more: name=ctx_discover_tools, arguments={query}.",
            json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "Tool name" },
                    "arguments": { "type": "object", "description": "Arguments for the tool" }
                },
                "required": ["name"]
            }),
        )
    }

    fn handle(
        &self,
        args: &Map<String, Value>,
        _ctx: &ToolContext,
    ) -> Result<ToolOutput, ErrorData> {
        let name = get_str(args, "name")
            .ok_or_else(|| ErrorData::invalid_params("'name' is required", None))?;

        if name == "ctx_call" {
            return Err(ErrorData::invalid_params(
                "ctx_call cannot invoke itself",
                None,
            ));
        }

        Err(ErrorData::internal_error(
            format!(
                "ctx_call dispatch for '{name}' must be handled by the async dispatch layer. \
                 If you see this error, the tool was routed to the sync handler by mistake."
            ),
            None,
        ))
    }
}
