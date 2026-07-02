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
            "Multi-repository — add, remove, search project directories.\n\
             WORKFLOW: list_roots → add_root/remove_root → search.\n\
             ANTI-PATTERN: not for single-repo projects — use ctx_search.\n\
             Actions: add_root|remove_root|list_roots|search|status|save_config.\n\
             Cross-repo search runs hybrid retrieval per root (BM25+dense+SPLADE)\n\
             and merges with RRF; mode=\"bm25\" forces lexical-only.",
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
                    },
                    "mode": {
                        "type": "string",
                        "enum": ["hybrid", "bm25"],
                        "description": "Per-root ranking signal (default: hybrid; bm25 = lexical only)"
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
        let mode = get_str(args, "mode");

        let (result, original_tokens) = crate::tools::ctx_multi_repo::handle(
            &action,
            path.as_deref(),
            alias.as_deref(),
            query.as_deref(),
            roots_filter.as_deref(),
            max_results,
            mode.as_deref(),
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
