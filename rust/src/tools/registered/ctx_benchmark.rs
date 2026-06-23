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
            "Benchmark compression modes — measures token savings across all available modes for a file or project.\n\
            WORKFLOW: use BEFORE ctx_read to pick the optimal compression strategy.\n\
            Provide a file path, or use action=project for project-wide results.\n\
            ANTIPATTERN: NOT for production profiling — measures compression, not runtime performance.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File path to benchmark (required for per-file mode)" },
                    "action": { "type": "string", "description": "Benchmark scope: omit for per-file, \"project\" for project-wide" },
                    "format": { "type": "string", "description": "Output format for project benchmarks: json|markdown|terminal (default terminal)" }
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
