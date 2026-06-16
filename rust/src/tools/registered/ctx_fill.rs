use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{
    McpTool, ToolContext, ToolOutput, get_str, get_str_array, get_usize,
};
use crate::tool_defs::tool_def;

pub struct CtxFillTool;

impl McpTool for CtxFillTool {
    fn name(&self) -> &'static str {
        "ctx_fill"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_fill",
            "Budget-aware context fill — auto-selects compression per file within token limit.",
            json!({
                "type": "object",
                "properties": {
                    "paths": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "File paths to consider"
                    },
                    "budget": {
                        "type": "integer",
                        "description": "Maximum token budget to fill"
                    },
                    "task": {
                        "type": "string",
                        "description": "Optional task (short English preferred) for intent-driven pruning"
                    }
                },
                "required": ["paths", "budget"]
            }),
        )
    }

    fn handle(
        &self,
        args: &Map<String, Value>,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ErrorData> {
        let raw_paths = get_str_array(args, "paths")
            .ok_or_else(|| ErrorData::invalid_params("paths array is required", None))?;
        let budget = get_usize(args, "budget")
            .ok_or_else(|| ErrorData::invalid_params("budget is required (non-negative)", None))?;
        let task = get_str(args, "task");

        tokio::task::block_in_place(|| {
            let session_lock = ctx
                .session
                .as_ref()
                .ok_or_else(|| ErrorData::internal_error("session not available", None))?;
            let cache_lock = ctx
                .cache
                .as_ref()
                .ok_or_else(|| ErrorData::internal_error("cache not available", None))?;

            let mut paths = Vec::with_capacity(raw_paths.len());
            {
                let session = session_lock.blocking_read();
                for p in &raw_paths {
                    match super::resolve_path_sync(&session, p) {
                        Ok(resolved) => paths.push(resolved),
                        Err(e) => {
                            return Err(ErrorData::invalid_params(e, None));
                        }
                    }
                }
            }

            let timeout_dur =
                crate::core::io_health::adaptive_timeout(std::time::Duration::from_secs(10));
            let Ok(mut cache) = tokio::runtime::Handle::current()
                .block_on(tokio::time::timeout(timeout_dur, cache_lock.write()))
            else {
                crate::core::io_health::record_freeze();
                return Err(ErrorData::internal_error(
                    "cache busy (ctx_fill) — retry in a moment",
                    None,
                ));
            };
            let output = crate::tools::ctx_fill::handle(
                &mut cache,
                &paths,
                budget,
                ctx.crp_mode,
                task.as_deref(),
            );
            drop(cache);

            Ok(ToolOutput {
                text: output,
                original_tokens: 0,
                saved_tokens: 0,
                mode: Some(format!("budget:{budget}")),
                path: None,
                changed: false,
                shell_outcome: None,
            })
        })
    }
}
