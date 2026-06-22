use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{
    McpTool, ToolContext, ToolOutput, get_int, get_str, get_str_array,
};
use crate::tool_defs::tool_def;

pub struct CtxPrefetchTool;

impl McpTool for CtxPrefetchTool {
    fn name(&self) -> &'static str {
        "ctx_prefetch"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_prefetch",
            "WORKFLOW: call BEFORE context-heavy operations to minimize latency.\n\
            ANTIPATTERN: NOT for normal reads — only for proactive cache warming.\n\
            Prewarms cache for blast radius files via graph + task signals.\n\
            task=description; changed_files=paths for blast radius;\n\
            budget_tokens=soft budget (default 3000); max_files=limit (default 10).\n\
            Saves latency (not tokens): preloads files before needed.",
            json!({
                "type": "object",
                "properties": {
                    "root": { "type": "string", "description": "Project root directory" },
                    "task": { "type": "string", "description": "Task description for relevance scoring" },
                    "changed_files": { "type": "array", "items": { "type": "string" }, "description": "Changed file paths for computing blast radius" },
                    "budget_tokens": { "type": "integer", "description": "Soft token budget (default: 3000)" },
                    "max_files": { "type": "integer", "description": "Max files to prefetch (default: 10)" }
                }
            }),
        )
    }

    fn handle(
        &self,
        args: &Map<String, Value>,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ErrorData> {
        let root = if get_str(args, "root").is_some() {
            if let Some(p) = ctx.resolved_path("root") {
                p.to_string()
            } else if let Some(err) = ctx.path_error("root") {
                return Err(ErrorData::invalid_params(format!("root: {err}"), None));
            } else {
                ctx.project_root.clone()
            }
        } else if let Some(ref session) = ctx.session {
            let guard = tokio::task::block_in_place(|| session.blocking_read());
            guard
                .project_root
                .clone()
                .unwrap_or_else(|| ".".to_string())
        } else {
            ".".to_string()
        };

        let task = get_str(args, "task");
        let changed_files = get_str_array(args, "changed_files");
        let budget_tokens = get_int(args, "budget_tokens").map_or(3000, |n| n.max(0) as usize);
        let max_files = get_int(args, "max_files").map(|n| n.max(1) as usize);

        let resolved_changed: Option<Vec<String>> = changed_files.map(|files| {
            files
                .iter()
                .map(|p| ctx.resolve_path_sync(p).unwrap_or_else(|_| p.clone()))
                .collect()
        });

        let cache = ctx
            .cache
            .as_ref()
            .ok_or_else(|| ErrorData::internal_error("cache not available", None))?;
        let Some(mut guard) = crate::server::bounded_lock::write(cache, "ctx_prefetch") else {
            return Ok(ToolOutput::simple(
                "[prefetch skipped — cache busy, retry in a moment]".to_string(),
            ));
        };
        let result = crate::tools::ctx_prefetch::handle(
            &mut guard,
            &root,
            task.as_deref(),
            resolved_changed.as_deref(),
            budget_tokens,
            max_files,
            ctx.crp_mode,
        );

        Ok(ToolOutput {
            text: result,
            original_tokens: 0,
            saved_tokens: 0,
            mode: Some("prefetch".to_string()),
            path: None,
            changed: false,
            shell_outcome: None,
        })
    }
}
