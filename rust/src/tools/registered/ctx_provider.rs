use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput};
use crate::tool_defs::tool_def;

pub struct CtxProviderTool;

impl McpTool for CtxProviderTool {
    fn name(&self) -> &'static str {
        "ctx_provider"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_provider",
            "External context providers (GitHub, GitLab, Jira, Postgres, MCP, custom REST).",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "description": "discover|list|status|refresh|configure|query|mcp_resources|gitlab_issues|gitlab_issue|gitlab_mrs|gitlab_pipelines"
                    },
                    "provider": {
                        "type": "string",
                        "description": "Provider ID (github|gitlab|jira|mcp:<name>). Required for query"
                    },
                    "resource": {
                        "type": "string",
                        "description": "query: e.g. issues, pull_requests. configure: paths|template|show"
                    },
                    "mode": { "type": "string", "description": "query output: compact|chunks" },
                    "state": { "type": "string", "description": "open|closed|merged|all" },
                    "labels": { "type": "string", "description": "Comma-separated label filter" },
                    "iid": { "type": "integer", "description": "Issue/MR IID" },
                    "status": { "type": "string", "description": "Pipeline status filter" },
                    "limit": { "type": "integer", "description": "Max results (default 20)" }
                },
                "required": ["action"]
            }),
        )
    }

    fn handle(
        &self,
        args: &Map<String, Value>,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ErrorData> {
        let result = crate::tools::ctx_provider::handle(args, ctx);
        Ok(ToolOutput::simple(result))
    }
}
