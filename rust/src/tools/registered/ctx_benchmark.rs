use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput, get_str, require_resolved_path};
use crate::tool_defs::tool_def;

pub struct CtxBenchmarkTool;

impl McpTool for CtxBenchmarkTool {
    fn name(&self) -> &'static str {
        "ctx_benchmark"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_benchmark",
            "Benchmark compression modes — measures token savings across all available modes for a file or project. Provide a file path, or use action=project format=json|markdown for project-wide results.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "action": { "type": "string" },
                    "format": { "type": "string" }
                },
                "required": ["path"]
            }),
        )
    }

    fn handle(
        &self,
        args: &Map<String, Value>,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ErrorData> {
        let path = require_resolved_path(ctx, args, "path")?;

        let action = get_str(args, "action").unwrap_or_default();
        let result = if action == "project" {
            let fmt = get_str(args, "format").unwrap_or_default();
            let bench = crate::core::benchmark::run_project_benchmark(&path);
            match fmt.as_str() {
                "json" => crate::core::benchmark::format_json(&bench),
                "markdown" | "md" => crate::core::benchmark::format_markdown(&bench),
                _ => crate::core::benchmark::format_terminal(&bench),
            }
        } else {
            crate::tools::ctx_benchmark::handle(&path, crate::tools::CrpMode::effective())
        };

        Ok(ToolOutput::simple(result))
    }
}
