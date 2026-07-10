use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput, get_str, require_resolved_path};
use crate::tool_defs::tool_def;

pub struct CtxCacheTool;

impl McpTool for CtxCacheTool {
    fn name(&self) -> &'static str {
        "ctx_cache"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_cache",
            "Cache operations — inspect, clear, or invalidate the read cache.\n\
            Actions: status lists cached files; clear empties all (recover token budget);\n\
            invalidate path=... refreshes a single entry.\n\
            Use to diagnose stale content or recover budget after large reads.\n\
            ANTIPATTERN: does NOT affect disk files — only cached read content.",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["status", "clear", "invalidate"],
                        "description": "status|clear|invalidate"
                    },
                    "path": {
                        "type": "string",
                        "description": "File path for invalidate action (only used with action=invalidate)"
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

        let invalidate_path = if action == "invalidate" {
            Some(require_resolved_path(ctx, args, "path")?)
        } else {
            None
        };

        let cache = ctx
            .cache
            .as_ref()
            .ok_or_else(|| ErrorData::internal_error("cache not available", None))?;
        let Some(mut guard) = crate::server::bounded_lock::write(cache, "ctx_cache") else {
            return Ok(ToolOutput::simple(
                "[cache lock temporarily unavailable — retry in a moment]".to_string(),
            ));
        };

        let result = match action.as_str() {
            "status" => {
                let entries = guard.get_all_entries();
                let mut lines = if entries.is_empty() {
                    vec!["Cache empty — no files tracked.".to_string()]
                } else {
                    let mut lines = vec![format!("Cache: {} file(s)", entries.len())];
                    for (path, entry) in &entries {
                        let fref = guard
                            .file_ref_map()
                            .get(*path)
                            .map_or("F?", std::string::String::as_str);
                        lines.push(format!(
                            "  {fref}={} [{}L, {}t, read {}x]",
                            crate::core::protocol::shorten_path(path),
                            entry.line_count,
                            entry.original_tokens,
                            entry.read_count()
                        ));
                    }
                    lines
                };
                // Re-delivery telemetry: forced full re-sends grouped by cause
                // (diagnostic only — never enters a cacheable body, #498).
                let t = crate::core::cache_telemetry::snapshot();
                if t.total() > 0 {
                    lines.push(format!(
                        "re-deliveries forced: compaction={} idle={} eviction={} conversation={} (total {})",
                        t.compaction, t.idle, t.eviction, t.conversation, t.total()
                    ));
                }
                if t.raw_cap_count > 0 {
                    lines.push(format!(
                        "raw-cap fallbacks: {} (prevented {} tokens inflation)",
                        t.raw_cap_count, t.raw_cap_prevented
                    ));
                }
                lines.join("\n")
            }
            "clear" => {
                let count = guard.clear();
                format!(
                    "Cache cleared — {count} file(s) removed. Next ctx_read will return full content."
                )
            }
            "invalidate" => {
                let Some(path) = invalidate_path else {
                    return Ok(ToolOutput::simple(
                        "Missing path for invalidate action.".to_string(),
                    ));
                };
                if guard.invalidate(&path) {
                    format!(
                        "Invalidated cache for {}. Next ctx_read will return full content.",
                        crate::core::protocol::shorten_path(&path)
                    )
                } else {
                    format!(
                        "{} was not in cache.",
                        crate::core::protocol::shorten_path(&path)
                    )
                }
            }
            _ => "Unknown action. Use: status, clear, invalidate".to_string(),
        };

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
