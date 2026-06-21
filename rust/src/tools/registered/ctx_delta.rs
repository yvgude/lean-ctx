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
             Use INSTEAD of re-reading the whole file after modifications — saves 90%+ tokens\n\
             on unchanged content. Path must have a prior ctx_read in this session\'s cache.\n\
             For the full git diff against HEAD, use ctx_read(path, mode=diff) instead.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Absolute file path" }
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

        tokio::task::block_in_place(|| {
            let cache_lock = ctx
                .cache
                .as_ref()
                .ok_or_else(|| ErrorData::internal_error("cache not available", None))?;
            let timeout_dur =
                crate::core::io_health::adaptive_timeout(std::time::Duration::from_secs(10));
            let Ok(mut cache) = tokio::runtime::Handle::current()
                .block_on(tokio::time::timeout(timeout_dur, cache_lock.write()))
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
            })
        })
    }
}
