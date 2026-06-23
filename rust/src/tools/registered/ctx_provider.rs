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
            "Query GitHub, GitLab, Jira, Postgres, MCP bridges, custom REST.\n\
             WORKFLOW: action=list first to discover configured providers.\n\
             ANTIPATTERN: not for file content — use ctx_compose/ctx_read instead.\n\
             provider=id (github|gitlab|jira|mcp:<name>); resource=issues|pull_requests.\n\
             Data flows through consolidation pipeline; results searchable via ctx_semantic_search.",
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
