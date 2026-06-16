use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{
    McpTool, ToolContext, ToolOutput, get_str, get_str_array, get_usize,
};
use crate::tool_defs::tool_def;

pub struct CtxMultiRepoTool;

impl McpTool for CtxMultiRepoTool {
    fn name(&self) -> &'static str {
        "ctx_multi_repo"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_multi_repo",
            "Multi-repo management: add/remove roots, cross-repo search with Reciprocal Rank Fusion (RRF). Enables searching across multiple project directories simultaneously.",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["add_root", "remove_root", "list_roots", "search", "status", "save_config"],
                        "description": "Action to perform"
                    },
                    "path": {
                        "type": "string",
                        "description": "Repository path (for add_root/remove_root)"
                    },
                    "alias": {
                        "type": "string",
                        "description": "Short alias for the repo (for add_root). Auto-derived from directory name if omitted."
                    },
                    "query": {
                        "type": "string",
                        "description": "Search query (for search action)"
                    },
                    "roots": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Filter search to specific roots by alias or path (for search). Omit to search all."
                    },
                    "max_results": {
                        "type": "integer",
                        "description": "Maximum results to return (default: 20)"
                    }
                },
                "required": ["action"]
            }),
        )
    }

    fn handle(
        &self,
        args: &Map<String, Value>,
        _ctx: &ToolContext,
    ) -> Result<ToolOutput, ErrorData> {
        let action = get_str(args, "action")
            .ok_or_else(|| ErrorData::invalid_params("action is required", None))?;

        let path = get_str(args, "path");
        let alias = get_str(args, "alias");
        let query = get_str(args, "query");
        let roots_filter = get_str_array(args, "roots");
        let max_results = get_usize(args, "max_results").unwrap_or(20).min(1000);

        let (result, original_tokens) = crate::tools::ctx_multi_repo::handle(
            &action,
            path.as_deref(),
            alias.as_deref(),
            query.as_deref(),
            roots_filter.as_deref(),
            max_results,
        );

        if result.starts_with("ERROR:") {
            return Err(ErrorData::invalid_params(result, None));
        }

        let sent = crate::core::tokens::count_tokens(&result);
        let saved = original_tokens.saturating_sub(sent);

        Ok(ToolOutput {
            text: result,
            original_tokens,
            saved_tokens: saved,
            mode: Some("multi_repo".to_string()),
            path,
            changed: action == "add_root" || action == "remove_root",
            shell_outcome: None,
        })
    }
}
