// SPDX-License-Identifier: Apache-2.0
use std::path::Path;

use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput, get_str};
use lean_ctx::tool_defs::tool_def;

pub struct CtxIndexTool;

impl McpTool for CtxIndexTool {
    fn name(&self) -> &'static str {
        "ctx_index"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_index",
            "Index orchestration — manage code graph index.\n\
             WORKFLOW: status → build → build-full (escalate if stale).\n\
             ANTI-PATTERN: build-full is expensive — use incremental build first.\n\
             Actions: status (state), build (incremental), build-full (rebuild),\n\
             why (is `path` indexed? if not, the rule that dropped it).",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["status", "build", "build-full", "why"],
                        "description": "status|build|build-full|why"
                    },
                    "project_root": {
                        "type": "string",
                        "description": "Project root"
                    },
                    "path": {
                        "type": "string",
                        "description": "File to diagnose (action=why)"
                    }
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
        let action = get_str(args, "action")
            .ok_or_else(|| ErrorData::invalid_params("action is required", None))?;
        let root = if let Some(p) = ctx
            .resolved_path("project_root")
            .or(ctx.resolved_path("root"))
        {
            p
        } else if let Some(err) = ctx.path_error("project_root").or(ctx.path_error("root")) {
            return Err(ErrorData::invalid_params(
                format!("project_root: {err}"),
                None,
            ));
        } else {
            &ctx.project_root
        };

        if action == "why" {
            // The jail-resolved form when available; `explain` itself refuses
            // anything outside `root` before reading content.
            let file = if let Some(p) = ctx.resolved_path("path") {
                p.to_string()
            } else if let Some(err) = ctx.path_error("path") {
                return Err(ErrorData::invalid_params(format!("path: {err}"), None));
            } else {
                get_str(args, "path").ok_or_else(|| {
                    ErrorData::invalid_params("path is required for action=why", None)
                })?
            };
            let coverage = lean_ctx::core::bm25_index::coverage::explain(Path::new(root), &file);
            return Ok(ToolOutput::simple(
                lean_ctx::core::bm25_index::coverage::render(&coverage),
            ));
        }

        let result = crate::tools::ctx_index::handle(&action, Path::new(root));

        // #420: `build-full` is an explicit "make everything fresh". The CLI path
        // flushes the running daemon's read cache via `notify_cache_clear()`; the
        // MCP tool runs in the process that owns this session's `SessionCache`, so
        // clear it in-process here. Otherwise `ctx_read` map/signatures keep
        // serving pre-rebuild output from the long-lived cache.
        if action == "build-full"
            && let Some(cache) = ctx.cache.as_ref()
            && let Some(mut guard) = crate::server::bounded_lock::write(cache, "ctx_index")
        {
            guard.clear();
        }

        Ok(ToolOutput::simple(result))
    }
}
