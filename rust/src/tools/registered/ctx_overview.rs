use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput, get_str};
use crate::tool_defs::tool_def;

pub struct CtxOverviewTool;

impl McpTool for CtxOverviewTool {
    fn name(&self) -> &'static str {
        "ctx_overview"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_overview",
            "WORKFLOW: call at session START before ctx_compose/ctx_read.\n\
            ANTIPATTERN: NOT for source code — structure only. Use ctx_compose for code understanding.\n\
            Project map — task='your goal' scopes files by relevance (PageRank on symbol graph).\n\
            High-level structure only, no source body. ~10x cheaper than ctx_compose.",
            json!({
                "type": "object",
                "properties": {
                    "task": {
                        "type": "string",
                        "description": "Short English task for relevance scoring"
                    },
                    "path": {
                        "type": "string",
                        "description": "Project root"
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
        let task = get_str(args, "task");

        let resolved_path = if get_str(args, "path").is_some() {
            if let Some(p) = ctx.resolved_path("path") {
                Some(p.to_string())
            } else if let Some(err) = ctx.path_error("path") {
                return Err(ErrorData::invalid_params(format!("path: {err}"), None));
            } else {
                None
            }
        } else if let Some(ref session) = ctx.session {
            let guard = crate::server::bounded_lock::read(session, "ctx_overview:session");
            guard.as_ref().and_then(|g| g.project_root.clone())
        } else {
            None
        };

        let cache = ctx
            .cache
            .as_ref()
            .ok_or_else(|| ErrorData::internal_error("cache not available", None))?;
        let Some(guard) = crate::server::bounded_lock::read(cache, "ctx_overview:cache") else {
            return Ok(ToolOutput::simple(
                "[overview temporarily unavailable — cache busy]".to_string(),
            ));
        };
        let (result, _complete) = crate::tools::ctx_overview::handle(
            &guard,
            task.as_deref(),
            resolved_path.as_deref(),
            ctx.crp_mode,
        );

        Ok(ToolOutput {
            text: result,
            original_tokens: 0,
            saved_tokens: 0,
            mode: Some("overview".to_string()),
            path: None,
            changed: false,
            shell_outcome: None,
            content_blocks: None,
        })
    }
}
