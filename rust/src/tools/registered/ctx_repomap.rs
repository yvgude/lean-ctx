//! MCP wrapper for `ctx_repomap` — Personalized `PageRank` repo map.

use rmcp::ErrorData;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput, get_int, get_str_array};
use crate::tool_defs::tool_def;

pub struct CtxRepomapTool;

const DEFAULT_MAX_TOKENS: usize = 2048;

impl McpTool for CtxRepomapTool {
    fn name(&self) -> &'static str {
        "ctx_repomap"
    }

    fn tool_def(&self) -> rmcp::model::Tool {
        tool_def(
            "ctx_repomap",
            "PageRank symbol map ranked by structural importance + session relevance.\n\
             WORKFLOW: call for codebase-wide orientation at session start.\n\
             ANTIPATTERN: not for task-scoped views — use ctx_overview instead.\n\
             focus_files=['path/*.rs'] boosts specific areas; max_tokens controls size\n\
             (default 2048). Saves tokens vs reading all files individually.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Project root" },
                    "max_tokens": { "type": "integer", "description": "Token budget", "default": 2048 },
                    "focus_files": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Boost ranking for relative paths"
                    }
                }
            }),
        )
    }

    fn handle(
        &self,
        args: &Map<String, Value>,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ErrorData> {
        let project_root = ctx
            .resolved_path("path")
            .map_or_else(|| ctx.project_root.clone(), String::from);

        if project_root.is_empty() {
            return Err(ErrorData::invalid_params(
                "No project root available. Provide 'path' or ensure a project is open.",
                None,
            ));
        }

        let max_tokens =
            get_int(args, "max_tokens").map_or(DEFAULT_MAX_TOKENS, |v| v.max(100) as usize);

        let focus_files = get_str_array(args, "focus_files").unwrap_or_default();
        let session_files = extract_session_files(ctx);

        let result = crate::tools::ctx_repomap::handle(
            &project_root,
            max_tokens,
            &focus_files,
            &session_files,
        );

        let original_tokens = crate::core::tokens::count_tokens(&result);

        Ok(ToolOutput {
            text: result,
            original_tokens,
            saved_tokens: 0,
            mode: Some("repomap".to_string()),
            path: Some(project_root),
            changed: false,
            shell_outcome: None,
        })
    }
}

fn extract_session_files(ctx: &ToolContext) -> Vec<String> {
    let Some(ref session_arc) = ctx.session else {
        return Vec::new();
    };

    let Ok(session) = session_arc.try_read() else {
        return Vec::new();
    };

    session
        .files_touched
        .iter()
        .map(|f| f.path.clone())
        .collect()
}
