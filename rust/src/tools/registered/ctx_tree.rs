use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput, get_bool, get_int};
use crate::tool_defs::tool_def;

pub struct CtxTreeTool;

impl McpTool for CtxTreeTool {
    fn name(&self) -> &'static str {
        "ctx_tree"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_tree",
            "Directory tree with file counts per directory. depth=N (default 3);\n\
             show_hidden for dotfiles; paths for multi-root.\n\
             respect_gitignore filters ignored files (default true).",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Dir (default .)" },
                    "paths": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Multi-root (alternative to path)"
                    },
                    "depth": { "type": "integer", "description": "Max depth (default 3)" },
                    "show_hidden": { "type": "boolean", "description": "Include dotfiles" },
                    "respect_gitignore": { "type": "boolean", "description": "Filter ignored (default true)" }
                }
            }),
        )
    }

    fn handle(
        &self,
        args: &Map<String, Value>,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ErrorData> {
        let resolved = crate::server::multi_path::resolve_tool_paths(args, ctx)
            .map_err(|e| ErrorData::invalid_params(format!("ERROR: {e}"), None))?;
        let depth = (get_int(args, "depth").unwrap_or(3) as usize).min(10);
        let show_hidden = get_bool(args, "show_hidden").unwrap_or(false);
        let respect_gitignore = get_bool(args, "respect_gitignore").unwrap_or(true);

        if !resolved.is_multi {
            return handle_single(&resolved.roots[0], depth, show_hidden, respect_gitignore);
        }

        let mut combined = String::new();
        let mut total_original: usize = 0;
        let mut total_sent: usize = 0;

        for root in &resolved.roots {
            let root_clone = root.clone();
            let Ok((result, original)) =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    crate::tools::ctx_tree::handle(
                        &root_clone,
                        depth,
                        show_hidden,
                        respect_gitignore,
                    )
                }))
            else {
                combined.push_str(&format!("── {root} ──\nERROR: internal panic\n\n"));
                continue;
            };

            if result.starts_with("ERROR:") {
                combined.push_str(&format!("── {root} ──\n{result}\n\n"));
                continue;
            }

            combined.push_str(&format!("── {root} ──\n{result}\n\n"));
            total_original += original;
            total_sent += crate::core::tokens::count_tokens(&result);
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
    path: &str,
    depth: usize,
    show_hidden: bool,
    respect_gitignore: bool,
) -> Result<ToolOutput, ErrorData> {
    let path_clone = path.to_string();
    let Ok((result, original)) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        crate::tools::ctx_tree::handle(&path_clone, depth, show_hidden, respect_gitignore)
    })) else {
        return Err(ErrorData::internal_error(
            format!(
                "ctx_tree panicked while processing '{path}'. This is a bug — please report it."
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
