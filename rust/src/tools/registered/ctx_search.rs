use rmcp::model::Tool;
use rmcp::ErrorData;
use serde_json::{json, Map, Value};

use crate::server::tool_trait::{get_bool, get_int, get_str, McpTool, ToolContext, ToolOutput};
use crate::tool_defs::tool_def;

pub struct CtxSearchTool;

impl McpTool for CtxSearchTool {
    fn name(&self) -> &'static str {
        "ctx_search"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_search",
            "Regex code search (.gitignore aware, compact results). Deterministic ordering. Secret-like files (e.g. .env, *.pem) are skipped unless role allows. ignore_gitignore requires explicit policy.",
            json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "Regex pattern" },
                    "path": { "type": "string", "description": "Directory to search" },
                    "ext": { "type": "string", "description": "File extension filter" },
                    "max_results": { "type": "integer", "description": "Max results (default: 20)" },
                    "ignore_gitignore": { "type": "boolean", "description": "Set true to scan ALL files including .gitignore'd paths (default: false). Requires role policy (e.g. admin)." }
                },
                "required": ["pattern"]
            }),
        )
    }

    fn handle(
        &self,
        args: &Map<String, Value>,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ErrorData> {
        let pattern = get_str(args, "pattern")
            .ok_or_else(|| ErrorData::invalid_params("pattern is required", None))?;
        let path = ctx.resolved_path("path").unwrap_or(".").to_string();
        let ext = get_str(args, "ext");
        let max = (get_int(args, "max_results").unwrap_or(20) as usize).min(500);
        let no_gitignore = get_bool(args, "ignore_gitignore").unwrap_or(false);

        if no_gitignore {
            if let Err(e) = crate::core::io_boundary::ensure_ignore_gitignore_allowed("ctx_search")
            {
                return Ok(ToolOutput::simple(e));
            }
        }

        let crp = ctx.crp_mode;
        let respect = !no_gitignore;
        let allow_secret_paths = crate::core::roles::active_role().io.allow_secret_paths;

        let pattern_clone = pattern.clone();
        let path_clone = path.clone();

        let search_result = tokio::task::block_in_place(|| {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                crate::tools::ctx_search::handle(
                    &pattern_clone,
                    &path_clone,
                    ext.as_deref(),
                    max,
                    crp,
                    respect,
                    allow_secret_paths,
                )
            }));
            match result {
                Ok(r) => Ok(r),
                Err(_) => Err("search task panicked"),
            }
        });

        let (result, original) = match search_result {
            Ok(r) => r,
            Err(e) => {
                return Err(ErrorData::internal_error(
                    format!("search task failed: {e}"),
                    None,
                ));
            }
        };

        let sent = crate::core::tokens::count_tokens(&result);
        let saved = original.saturating_sub(sent);

        let final_out = crate::core::protocol::append_savings(&result, original, sent);

        Ok(ToolOutput {
            text: final_out,
            original_tokens: original,
            saved_tokens: saved,
            mode: None,
            path: Some(path),
        })
    }
}
