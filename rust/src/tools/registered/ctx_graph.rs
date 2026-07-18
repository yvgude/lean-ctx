use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput, get_str, get_usize};
use crate::tool_defs::tool_def;

pub struct CtxGraphTool;

impl McpTool for CtxGraphTool {
    fn name(&self) -> &'static str {
        "ctx_graph"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_graph",
            "File-level dependency graph queries.\n\
             action=symbol path=\"file.rs::fnName\" returns the DEFINITION (not usages — \
             use ctx_search for references). neighbors=imports±direction, \
             impact=reverse-dep blast radius, path from→to=dependency chain, \
             diff since=HEAD~1=git change impact, diagram kind=deps|calls (Mermaid).\n\
             For understanding code use ctx_compose FIRST.",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "description": "build|related|symbol|impact|status|enrich|context|diagram|neighbors|path|explain|diff"
                    },
                    "path": {
                        "type": "string",
                        "description": "Path; file::symbol for symbol action"
                    },
                    "to": { "type": "string", "description": "Target file (action=path)" },
                    "depth": { "type": "integer" },
                    "kind": { "type": "string", "description": "diagram: deps|calls" },
                    "format": { "type": "string", "description": "text|json" },
                    "since": { "type": "string", "description": "Git ref (default HEAD~1)" },
                    "project_root": { "type": "string" }
                },
                "required": ["action"],
                "allOf": [
                    { "if": { "properties": { "action": { "enum": ["related", "symbol", "impact", "neighbors", "explain", "path"] } }, "required": ["action"] }, "then": { "required": ["path"] } },
                    { "if": { "properties": { "action": { "const": "path" } }, "required": ["action"] }, "then": { "required": ["path", "to"] } }
                ]
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

        let path = if action == "diagram" {
            get_str(args, "path")
        } else if let Some(p) = ctx.resolved_path("path") {
            Some(p.to_string())
        } else if let Some(err) = ctx
            .path_error("path")
            .filter(|_| get_str(args, "path").is_some())
        {
            return Err(ErrorData::invalid_params(format!("path: {err}"), None));
        } else {
            None
        };

        let root = if let Some(p) = ctx.resolved_path("project_root") {
            p.to_string()
        } else if let Some(err) = ctx.path_error("project_root") {
            return Err(ErrorData::invalid_params(
                format!("project_root: {err}"),
                None,
            ));
        } else {
            ctx.project_root.clone()
        };
        let depth = get_usize(args, "depth").map(|d| d.min(64));
        let kind = get_str(args, "kind");
        let format = get_str(args, "format");
        // `since` is a git ref, not a filesystem path — read it raw (no PathJail).
        let since = get_str(args, "since");
        let to = if let Some(p) = ctx.resolved_path("to") {
            Some(p.to_string())
        } else if let Some(err) = ctx
            .path_error("to")
            .filter(|_| get_str(args, "to").is_some())
        {
            return Err(ErrorData::invalid_params(format!("to: {err}"), None));
        } else {
            None
        };

        let cache = ctx
            .cache
            .as_ref()
            .ok_or_else(|| ErrorData::internal_error("cache not available", None))?;
        let Some(mut guard) = crate::server::bounded_lock::write(cache, "ctx_graph") else {
            return Ok(ToolOutput::simple(
                "[graph cache temporarily unavailable — retry in a moment]".to_string(),
            ));
        };
        let result = crate::tools::ctx_graph::handle(
            &action,
            path.as_deref(),
            &root,
            &mut guard,
            ctx.crp_mode,
            depth,
            kind.as_deref(),
            to.as_deref(),
            format.as_deref(),
            since.as_deref(),
        );

        Ok(ToolOutput {
            text: result,
            original_tokens: 0,
            saved_tokens: 0,
            mode: Some(action),
            path: None,
            changed: false,
            shell_outcome: None,
            content_blocks: None,
        })
    }
}
