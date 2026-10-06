// SPDX-License-Identifier: Apache-2.0
//! MCP schema and dispatch for the single ctx_read implementation.

use super::{
    CtxReadTool, ErrorData, Map, McpTool, Tool, ToolContext, ToolOutput, Value, engine, get_str,
    record_attribution_result, require_resolved_path,
};

impl McpTool for CtxReadTool {
    fn name(&self) -> &'static str {
        "ctx_read"
    }

    fn produces_machine_readable(&self, args: Option<&Map<String, Value>>) -> bool {
        // The explicit Engine view is a byte-exact payload, even when it is text.
        // Security and final turn-budget enforcement still run in the pipeline.
        crate::server::native_receipts::requested("ctx_read", args)
    }

    fn tool_def(&self) -> Tool {
        super::schema::ctx_read_tool_def()
    }

    fn handle(
        &self,
        args: &Map<String, Value>,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ErrorData> {
        let engine_interface_v1 = engine::interface_v1_requested(args)?;
        engine::validate_v1_request_shape(args, engine_interface_v1)?;
        // #509: ctx_read absorbs multi-file batch reads (supersedes ctx_multi_read).
        // A non-empty `paths` array routes to the one shared batch implementation.
        if args
            .get("paths")
            .and_then(|v| v.as_array())
            .is_some_and(|a| !a.is_empty())
        {
            let result = super::super::ctx_multi_read::batch_read(args, ctx);
            if let Ok(output) = &result {
                record_attribution_result(ctx, "ctx_read batch".to_string(), output);
            }
            return result;
        }

        let path = if let Some(repo) = get_str(args, "repo") {
            let root = crate::core::multi_repo::resolve_repo_root(&repo).ok_or_else(|| {
                let known = crate::core::multi_repo::known_aliases().join(", ");
                let known = if known.is_empty() {
                    "none registered — use ctx_multi_repo add_root".to_string()
                } else {
                    known
                };
                ErrorData::invalid_params(
                    format!("unknown repo alias: {repo} (known: {known})"),
                    None,
                )
            })?;
            let rel = get_str(args, "path").unwrap_or_else(|| ".".to_string());
            crate::core::path_resolve::resolve_tool_path(Some(&root), None, &rel)
                .map_err(|e| ErrorData::invalid_params(e, None))?
        } else {
            require_resolved_path(ctx, args, "path")?
        };

        let result = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.handle_inner(args, ctx, &path, engine_interface_v1)
        })) {
            Ok(result) => result,
            Err(_) => Err(ErrorData::internal_error(
                format!(
                    "ctx_read panicked while processing '{path}'. This is a bug — please report it."
                ),
                None,
            )),
        };
        if let Ok(output) = &result {
            record_attribution_result(ctx, format!("ctx_read {path}"), output);
        }
        result
    }
}
