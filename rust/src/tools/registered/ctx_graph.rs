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
            "Graph queries — find dependencies, relationships, and symbols.\n\
             action=symbol path=\"file.rs::fnName\" returns the source (NOT usages).\n\
             action=neighbors path=\"file.rs\" shows import neighbors with direction & confidence.\n\
             action=impact path=\"file.rs\" shows reverse dependency tree (blast radius).\n\
             action=path from→to shows shortest dependency chain between two files.\n\
             action=diff since=HEAD~1 for git change impact.\n\
             action=diagram kind=deps|calls renders a Mermaid diagram.\n\
             For understanding code, use ctx_compose FIRST. Use ctx_graph for targeted structural queries.\n\
             ANTIPATTERN: symbol returns only the DEFINITION — not usages. For REFERENCES use grep or ctx_compose.",
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
                    "depth": { "type": "integer", "description": "Traversal depth" },
                    "kind": { "type": "string", "description": "diagram: deps|calls" },
                    "format": { "type": "string", "description": "text|json" },
                    "since": { "type": "string", "description": "Git ref for action=diff (default HEAD~1)" },
                    "project_root": { "type": "string", "description": "Project root" }
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
        })
    }
}
