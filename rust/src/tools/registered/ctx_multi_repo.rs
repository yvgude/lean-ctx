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
            "Multi-repository management — add, remove, and search across project directories.\n\
             action=add_root|remove_root|list_roots|search. Cross-repo search uses Reciprocal\n\
             Rank Fusion (RRF) to merge results from multiple repos. query=search term;\n\
             roots=filter to specific repos. max_results limits output (default 20).",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["add_root", "remove_root", "list_roots", "search", "status", "save_config"],
                        "description": "add_root|remove_root|list_roots|search|status|save_config"
                    },
                    "path": {
                        "type": "string",
                        "description": "Repo path"
                    },
                    "alias": {
                        "type": "string",
                        "description": "Short alias (auto-derived if omitted)"
                    },
                    "query": {
                        "type": "string",
                        "description": "Search query (for search action)"
                    },
                    "roots": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Filter to specific repos by alias/path"
                    },
                    "max_results": {
                        "type": "integer",
                        "description": "Max results"
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
