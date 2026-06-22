use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{
    McpTool, ToolContext, ToolOutput, get_int, get_str, require_resolved_path,
};
use crate::tool_defs::tool_def;

pub struct CtxExecuteTool;

impl McpTool for CtxExecuteTool {
    fn name(&self) -> &'static str {
        "ctx_execute"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_execute",
            "Run code in sandbox (11 languages) — use when compute beats shell glue.\n\
             action=code (default) for one-shot scripts; action=batch for parallel multi-language;\n\
             action=file to process a project file (extension auto-detects language).\n\
             Pass intent to focus large output. Prefer over ctx_shell for conditionals,\n\
             multi-line scripts, or cross-language data munging. Languages: javascript,\n\
             typescript, python, shell, ruby, go, rust, php, perl, r, elixir.",
            json!({
                "type": "object",
                "properties": {
                    "language": {
                        "type": "string",
                        "description": "javascript|typescript|python|shell|ruby|go|rust|php|perl|r|elixir (for action=code)"
                    },
                    "code": {
                        "type": "string",
                        "description": "Source code for action=code. Set intent to filter large output."
                    },
                    "intent": {
                        "type": "string",
                        "description": "Focus intent; triggers filtering when output is large."
                    },
                    "timeout": {
                        "type": "integer",
                        "description": "Timeout in seconds (default: 30)"
                    },
                    "action": {
                        "type": "string",
                        "description": "code (run script) | batch (parallel) | file (project file)"
                    },
                    "items": {
                        "type": "string",
                        "description": "JSON array of [{language, code}] for batch action."
                    },
                    "path": {
                        "type": "string",
                        "description": "File path for action=file (language auto-detected)."
                    }
                }
            }),
        )
    }

    fn handle(
        &self,
        args: &Map<String, Value>,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ErrorData> {
        let action = get_str(args, "action").unwrap_or_default();

        let (result, outcome) = if action == "batch" {
            let items_str = get_str(args, "items")
                .ok_or_else(|| ErrorData::invalid_params("items is required for batch", None))?;
            let items: Vec<serde_json::Value> = serde_json::from_str(&items_str)
                .map_err(|e| ErrorData::invalid_params(format!("Invalid items JSON: {e}"), None))?;
            let batch: Vec<(String, String)> = items
                .iter()
                .filter_map(|item| {
                    let lang = item.get("language")?.as_str()?.to_string();
                    let code = item.get("code")?.as_str()?.to_string();
                    Some((lang, code))
                })
                .collect();
            crate::tools::ctx_execute::handle_batch(&batch)
        } else if action == "file" {
            let path = require_resolved_path(ctx, args, "path")?;
            let project_root = if ctx.project_root.is_empty() {
                None
            } else {
                Some(ctx.project_root.as_str())
            };
            let intent = get_str(args, "intent");
            crate::tools::ctx_execute::handle_file(&path, intent.as_deref(), project_root)
        } else {
            let language = get_str(args, "language")
                .ok_or_else(|| ErrorData::invalid_params("language is required", None))?;
            let code = get_str(args, "code")
                .ok_or_else(|| ErrorData::invalid_params("code is required", None))?;
            let intent = get_str(args, "intent");
            let timeout = get_int(args, "timeout").and_then(|t| u64::try_from(t).ok());
            crate::tools::ctx_execute::handle(&language, &code, intent.as_deref(), timeout)
        };

        let result = crate::core::redaction::redact_text_if_enabled(&result);
        Ok(ToolOutput {
            text: result,
            original_tokens: 0,
            saved_tokens: 0,
            mode: Some(action),
            path: None,
            changed: false,
            shell_outcome: Some(outcome),
        })
    }
}
