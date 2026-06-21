use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput, get_bool, get_int, get_str};
use crate::tool_defs::tool_def;

pub struct CtxGlobTool;

impl McpTool for CtxGlobTool {
    fn name(&self) -> &'static str {
        "ctx_glob"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_glob",
            "Find files by glob pattern. Respects .gitignore;\n\
             supports multi-root via `paths` array. max_results=N sets limit.\n\
             For file content search, use ctx_search (pattern) or ctx_semantic_search (meaning).",
            json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "Glob pattern (e.g. **/*.ts, *.rs)" },
                    "path": { "type": "string", "description": "Directory to search (default: .)" },
                    "paths": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Multiple directories to search (alternative to path)"
                    },
                    "max_results": { "type": "integer", "description": "Max results (default: 200)" },
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
        let resolved = crate::server::multi_path::resolve_tool_paths(args, ctx)
            .map_err(|e| ErrorData::invalid_params(format!("ERROR: {e}"), None))?;
        let max = (get_int(args, "max_results").unwrap_or(200) as usize).min(500);
        let no_gitignore = get_bool(args, "ignore_gitignore").unwrap_or(false);

        if no_gitignore
            && let Err(e) = crate::core::io_boundary::ensure_ignore_gitignore_allowed("ctx_glob")
        {
            return Ok(ToolOutput::simple(e));
        }

        let respect = !no_gitignore;
        let allow_secret_paths = crate::core::roles::active_role().io.allow_secret_paths;

        if !resolved.is_multi {
            return handle_single(
                &pattern,
                &resolved.roots[0],
                respect,
                allow_secret_paths,
                max,
            );
        }

        let _mode_guard = crate::core::savings_footer::ModeGuard::new("glob");
        let per_root_max = (max / resolved.roots.len()).max(5);
        let mut combined = String::new();
        let mut total_original: usize = 0;
        let mut total_sent: usize = 0;

        for root in &resolved.roots {
            // The dispatch layer already runs `handle()` inside `block_in_place`
            // (server/dispatch/mod.rs); the per-root walk is synchronous, so we
            // call it directly and only guard against panics — nesting another
            // `block_in_place` here would needlessly consume blocking-pool
            // threads (the lesson from the ctx_multi_read crash, #271).
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                crate::tools::ctx_glob::handle(
                    &pattern,
                    root,
                    respect,
                    allow_secret_paths,
                    per_root_max,
                )
            }));

            let Ok((result, original)) = result else {
                combined.push_str(&format!("── {root} ──\nERROR: internal panic\n\n"));
                continue;
            };

            combined.push_str(&format!("── {root} ──\n{result}\n\n"));
            if !result.starts_with("ERROR:") {
                total_original += original;
                total_sent += crate::core::tokens::count_tokens(&result);
            }
        }

        let final_out =
            crate::core::protocol::append_savings(&combined, total_original, total_sent);
        let saved = total_original.saturating_sub(total_sent);

        Ok(ToolOutput {
            text: final_out,
            original_tokens: total_original,
            saved_tokens: saved,
            mode: None,
            path: None,
            changed: false,
            shell_outcome: None,
        })
    }
}

fn handle_single(
    pattern: &str,
    path: &str,
    respect_gitignore: bool,
    allow_secret_paths: bool,
    max_results: usize,
) -> Result<ToolOutput, ErrorData> {
    let pattern = pattern.to_string();
    let path_clone = path.to_string();
    let Ok((result, original)) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        crate::tools::ctx_glob::handle(
            &pattern,
            &path_clone,
            respect_gitignore,
            allow_secret_paths,
            max_results,
        )
    })) else {
        return Err(ErrorData::internal_error(
            format!(
                "ctx_glob panicked while processing '{path}'. This is a bug — please report it."
            ),
            None,
        ));
    };

    if result.starts_with("ERROR:") {
        return Err(ErrorData::invalid_params(result, None));
    }

    let sent = crate::core::tokens::count_tokens(&result);
    let saved = original.saturating_sub(sent);
    let final_out = crate::core::protocol::append_savings(&result, original, sent);

    Ok(ToolOutput {
        text: final_out,
        original_tokens: original,
        saved_tokens: saved,
        mode: None,
        path: Some(path.to_string()),
        changed: false,
        shell_outcome: None,
    })
}
