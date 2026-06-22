use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput, get_str};
use crate::tool_defs::tool_def;

pub struct CtxIntentTool;

impl McpTool for CtxIntentTool {
    fn name(&self) -> &'static str {
        "ctx_intent"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_intent",
            "Submit task goals as JSON or short text — server infers from tool calls.\n\
             ANTI-PATTERN: not needed for simple tasks.\n\
             query=task|JSON; format=json for JSON output; project_root=scope.",
            json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Compact JSON intent or short text" },
                    "project_root": { "type": "string", "description": "Project root" },
                    "format": { "type": "string", "description": "Output format (omit for default, \"json\" for JSON route)" }
                },
                "required": ["query"]
            }),
        )
    }

    fn handle(
        &self,
        args: &Map<String, Value>,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ErrorData> {
        let query = get_str(args, "query")
            .ok_or_else(|| ErrorData::invalid_params("query is required", None))?;
        let root = if let Some(p) = ctx.resolved_path("project_root") {
            p.to_string()
        } else if let Some(err) = ctx.path_error("project_root") {
            return Err(ErrorData::invalid_params(
                format!("project_root: {err}"),
                None,
            ));
        } else {
            ".".to_string()
        };
        let format = get_str(args, "format");

        let cache = ctx
            .cache
            .as_ref()
            .ok_or_else(|| ErrorData::internal_error("cache not available", None))?;
        let Some(mut cache_guard) = crate::server::bounded_lock::write(cache, "ctx_intent:cache")
        else {
            return Ok(ToolOutput::simple(
                "[intent unavailable — cache busy, retry]".to_string(),
            ));
        };
        let output = crate::tools::ctx_intent::handle(
            &mut cache_guard,
            &query,
            &root,
            ctx.crp_mode,
            format.as_deref(),
        );
        drop(cache_guard);

        if let Some(ref session) = ctx.session
            && let Some(mut session_guard) =
                crate::server::bounded_lock::write(session, "ctx_intent:session")
        {
            session_guard.set_task(&query, Some("intent"));
        }

        Ok(ToolOutput {
            text: output,
            original_tokens: 0,
            saved_tokens: 0,
            mode: Some("semantic".to_string()),
            path: None,
            changed: false,
            shell_outcome: None,
        })
    }
}
