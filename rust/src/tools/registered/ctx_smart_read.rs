use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput, require_resolved_path};
use crate::tool_defs::tool_def;

pub struct CtxSmartReadTool;

impl McpTool for CtxSmartReadTool {
    fn name(&self) -> &'static str {
        "ctx_smart_read"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_smart_read",
            "Auto-select optimal read mode (full|map|signatures|auto) based on file size,\n\
             type, and compression history. Use when you want smart defaults without\n\
             choosing a mode. For explicit control use ctx_read with mode= parameter directly.",
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

        if crate::core::binary_detect::is_binary_file(&path) {
            let msg = crate::core::binary_detect::binary_file_message(&path);
            return Err(ErrorData::invalid_params(msg, None));
        }
        {
            let cap = crate::core::limits::max_read_bytes() as u64;
            if let Ok(meta) = std::fs::metadata(&path)
                && meta.len() > cap
            {
                let msg = format!(
                    "File too large ({} bytes, limit {} bytes via LCTX_MAX_READ_BYTES). \
                         Use mode=\"lines:1-100\" for partial reads or increase the limit.",
                    meta.len(),
                    cap
                );
                return Err(ErrorData::invalid_params(msg, None));
            }
        }

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
                    "cache busy (ctx_smart_read) — retry in a moment",
                    None,
                ));
            };
            let output = crate::tools::ctx_smart_read::handle(&mut cache, &path, ctx.crp_mode);
            let original = cache.get(&path).map_or(0, |e| e.original_tokens);
            let tokens = crate::core::tokens::count_tokens(&output);
            drop(cache);

            let saved = original.saturating_sub(tokens);
            Ok(ToolOutput {
                text: output,
                original_tokens: original,
                saved_tokens: saved,
                mode: Some("auto".to_string()),
                path: Some(path),
                changed: false,
                shell_outcome: None,
            })
        })
    }
}
