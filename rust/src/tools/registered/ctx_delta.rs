use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput, require_resolved_path};
use crate::tool_defs::tool_def;

pub struct CtxDeltaTool;

impl McpTool for CtxDeltaTool {
    fn name(&self) -> &'static str {
        "ctx_delta"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_delta",
            "Incremental diff since last read — shows only changed lines after you edit.\n\
             WORKFLOW: ctx_read(mode=full) -> edit -> ctx_delta (no re-read needed).\n\
             Use INSTEAD of re-reading the whole file after modifications — it returns only changed\n\
             lines and avoids resending unchanged content. Path must have a prior ctx_read in this session\'s cache.\n\
             ctx_read(mode=diff) diffs this same cache, not git; for HEAD use git diff.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File path" }
                },
                "required": ["path"]
            }),
        )
    }

    fn handle(
        &self,
        args: &Map<String, Value>,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ErrorData> {
        let path = require_resolved_path(ctx, args, "path")?;

        {
            let cache_lock = ctx
                .cache
                .as_ref()
                .ok_or_else(|| ErrorData::internal_error("cache not available", None))?;
            let Some(mut cache) =
                crate::server::bounded_lock::write(cache_lock, "ctx_delta cache write")
            else {
                crate::core::io_health::record_freeze();
                return Err(ErrorData::internal_error(
                    "cache busy (ctx_delta) — retry in a moment",
                    None,
                ));
            };
            let output = crate::tools::ctx_delta::handle(&mut cache, &path);
            let original = cache.get(&path).map_or(0, |e| e.original_tokens);
            let tokens = crate::core::tokens::count_tokens(&output);
            drop(cache);

            if let Some(session_lock) = ctx.session.as_ref() {
                let mut session = session_lock.blocking_write();
                session.mark_modified(&path);
            }

            let saved = original.saturating_sub(tokens);
            Ok(ToolOutput {
                text: output,
                original_tokens: original,
                saved_tokens: saved,
                mode: Some("delta".to_string()),
                path: Some(path),
                changed: false,
                shell_outcome: None,
                content_blocks: None,
            })
        }
    }
}
