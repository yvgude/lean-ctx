use rmcp::model::Tool;
use rmcp::ErrorData;
use serde_json::{json, Map, Value};

use crate::server::tool_trait::{
    get_bool, get_str, get_str_array, McpTool, ToolContext, ToolOutput,
};
use crate::tool_defs::tool_def;

pub struct CtxMultiReadTool;

impl McpTool for CtxMultiReadTool {
    fn name(&self) -> &'static str {
        "ctx_multi_read"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_multi_read",
            "Batch read files in one call. Same modes as ctx_read.",
            json!({
                "type": "object",
                "properties": {
                    "paths": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Absolute file paths to read, in order"
                    },
                    "mode": {
                        "type": "string",
                        "description": "Compression mode (default: full). Same modes as ctx_read (auto, full, map, signatures, diff, aggressive, entropy, task, reference, lines:N-M)."
                    },
                    "fresh": {
                        "type": "boolean",
                        "description": "Bypass cache and force a full re-read for all paths. Use when running as a subagent that may not have the parent's context."
                    }
                },
                "required": ["paths"]
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

        tokio::task::block_in_place(|| {
            let session_lock = ctx
                .session
                .as_ref()
                .ok_or_else(|| ErrorData::internal_error("session not available", None))?;
            let cache_lock = ctx
                .cache
                .as_ref()
                .ok_or_else(|| ErrorData::internal_error("cache not available", None))?;

            let cap = crate::core::limits::max_read_bytes() as u64;
            let mut paths = Vec::with_capacity(raw_paths.len());
            {
                let session = session_lock.blocking_read();
                for p in &raw_paths {
                    let resolved = super::resolve_path_sync(&session, p)
                        .map_err(|e| ErrorData::invalid_params(e, None))?;
                    if crate::core::binary_detect::is_binary_file(&resolved) {
                        continue;
                    }
                    if let Ok(meta) = std::fs::metadata(&resolved) {
                        if meta.len() > cap {
                            continue;
                        }
                    }
                    paths.push(resolved);
                }
            }
            if paths.is_empty() {
                return Err(ErrorData::invalid_params(
                    "all paths are binary or exceed the size limit",
                    None,
                ));
            }

            let mode = get_str(args, "mode").unwrap_or_else(|| {
                let p = crate::core::profiles::active_profile();
                let dm = p.read.default_mode_effective();
                if dm == "auto" {
                    "full".to_string()
                } else {
                    dm.to_string()
                }
            });
            let current_task = {
                let session = session_lock.blocking_read();
                session.task.as_ref().map(|t| t.description.clone())
            };

            let fresh = get_bool(args, "fresh").unwrap_or(false);
            let mut cache = cache_lock.blocking_write();
            let output = crate::tools::ctx_multi_read::handle_with_task_fresh(
                &mut cache,
                &paths,
                &mode,
                fresh,
                ctx.crp_mode,
                current_task.as_deref(),
            );
            let mut total_original: usize = 0;
            for path in &paths {
                total_original =
                    total_original.saturating_add(cache.get(path).map_or(0, |e| e.original_tokens));
            }
            let tokens = crate::core::tokens::count_tokens(&output);
            drop(cache);

            Ok(ToolOutput {
                text: output,
                original_tokens: total_original,
                saved_tokens: total_original.saturating_sub(tokens),
                mode: Some(mode),
                path: None,
                changed: false,
            })
        })
    }
}
