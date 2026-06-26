use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput, get_bool, get_str, get_usize};
use crate::tool_defs::tool_def;

pub struct CtxExploreTool;

impl McpTool for CtxExploreTool {
    fn name(&self) -> &'static str {
        "ctx_explore"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_explore",
            "Iterative, deterministic code exploration → compact file:line citations.\n\
             Runs a bounded multi-turn loop (BM25 + static call/import graph + AST symbols)\n\
             and returns a <final_answer> block of `path:start-end` spans instead of bodies.\n\
             USE WHEN: locating WHERE behavior lives across many files, cheaply.\n\
             vs ctx_compose: compose inlines bodies in one shot; explore returns citations\n\
             over N turns (far fewer tokens). citation=true emits only the block.",
            json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Natural-language question or symbol names" },
                    "path": { "type": "string", "description": "Project root" },
                    "max_turns": { "type": "integer", "description": "Exploration depth (1-8, default 3)" },
                    "citation": { "type": "boolean", "description": "Emit only the <final_answer> citation block" }
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
        let path = if let Some(p) = ctx.resolved_path("path") {
            p.to_string()
        } else if let Some(err) = ctx.path_error("path") {
            return Err(ErrorData::invalid_params(format!("path: {err}"), None));
        } else {
            ctx.project_root.clone()
        };

        let opts = crate::tools::ctx_explore::ExploreOptions::new(
            get_usize(args, "max_turns"),
            get_bool(args, "citation").unwrap_or(false),
        );

        let outcome = tokio::task::block_in_place(|| {
            crate::tools::ctx_explore::handle(&query, &path, ctx.crp_mode, &opts)
        });

        if outcome.text.starts_with("ERROR") {
            return Err(ErrorData::invalid_params(outcome.text, None));
        }

        Ok(ToolOutput {
            text: outcome.text,
            original_tokens: outcome.tokens,
            saved_tokens: 0,
            mode: Some("explore".to_string()),
            path: Some(path),
            changed: false,
            shell_outcome: None,
        })
    }
}
