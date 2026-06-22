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
            "External context providers — query data from GitHub, GitLab, Jira, Postgres, MCP\n\
             bridges, and custom REST APIs. Actions: discover|list|status|refresh|configure|\n\
             query|mcp_resources|gitlab_issues. provider=id (github|gitlab|jira|mcp:<name>);\n\
             resource=issues|pull_requests. Data flows through consolidation pipeline.",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "description": "discover|list|status|refresh|configure|query|mcp_resources|gitlab_issues|gitlab_issue|gitlab_mrs|gitlab_pipelines"
                    },
                    "provider": {
                        "type": "string",
                        "description": "github|gitlab|jira|mcp:<name>"
                    },
                    "resource": {
                        "type": "string",
                        "description": "issues|pull_requests|paths|template|show"
                    },
                    "mode": { "type": "string", "description": "compact|chunks" },
                    "state": { "type": "string", "description": "open|closed|merged|all" },
                    "labels": { "type": "string", "description": "Comma-separated labels" },
                    "iid": { "type": "integer", "description": "Issue/MR IID" },
                    "status": { "type": "string", "description": "Pipeline status filter" },
                    "limit": { "type": "integer", "description": "Max results" }
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
