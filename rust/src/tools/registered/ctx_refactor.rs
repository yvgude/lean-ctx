use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput, get_str, require_resolved_path};
use crate::tool_defs::tool_def;

pub struct CtxRefactorTool;

impl McpTool for CtxRefactorTool {
    fn name(&self) -> &'static str {
        "ctx_refactor"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_refactor",
            "Rename, move, safe_delete, inline, read-only analyses via LSP/IDE.\n\
             WORKFLOW: use action=references first to find usages before refactoring.\n\
             ANTIPATTERN: not for symbol discovery — use ctx_symbol/ctx_compose.\n\
             Single-phase edits (replace_symbol_body, reformat) work headless via name_path.\n\
             Two-phase ops (_preview+_apply) need JetBrains IDE (else BACKEND_REQUIRED).\n\
             Conflicts blocked unless force=true. See `action` parameter for full list.",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "description": "rename|references|definition|implementations|declaration|type_hierarchy|symbols_overview|inspections|replace_symbol_body|insert_before_symbol|insert_after_symbol|rename_preview|rename_apply|move_preview|move_apply|safe_delete_preview|safe_delete_apply|inline_preview|inline_apply|reformat"
                    },
                    "path": { "type": "string", "description": "Path" },
                    "line": { "type": "integer", "description": "1-indexed line" },
                    "column": { "type": "integer", "description": "0-indexed column" },
                    "new_name": { "type": "string", "description": "New symbol name" },
                    "scope": {
                        "type": "string",
                        "enum": ["project", "all"],
                        "description": "project|all"
                    },
                    "direction": {
                        "type": "string",
                        "enum": ["supertypes", "subtypes"],
                        "description": "supertypes|subtypes"
                    },
                    "mode": {
                        "type": "string",
                        "enum": ["run", "list"],
                        "description": "run|list"
                    },
                    "name_path": { "type": "string", "description": "Symbol path for body edits (qualified or bare)" },
                    "new_body": { "type": "string", "description": "Full replacement declaration text" },
                    "text": { "type": "string", "description": "Sibling text to insert (auto-indented)" },
                    "end_line": { "type": "integer", "description": "1-based last line (path+line fallback)" },
                    "expected_hash": { "type": "string", "description": "BLAKE3 hex of current range (TOCTOU guard)" },
                    "plan_hash": { "type": "string", "description": "BLAKE3 plan hash from rename_preview" },
                    "force": { "type": "boolean", "description": "Override refactoring conflicts" },
                    "search_comments": { "type": "boolean", "description": "Rename in comments/strings" },
                    "search_text_occurrences": { "type": "boolean", "description": "Rename in non-code text" },
                    "target_path": { "type": "string", "description": "Destination directory/file (project-relative)" },
                    "target_parent": { "type": "string", "description": "Destination parent symbol for member move" },
                    "propagate": { "type": "boolean", "description": "Delete unreferenced dependencies" },
                    "keep_definition": { "type": "boolean", "description": "Keep declaration after inline" },
                    "optimize_imports": { "type": "boolean", "description": "Remove unused imports" }
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
        // name_path edits resolve their own path; only require/resolve `path`
        // when actually provided (read actions + position-fallback edits).
        let has_path = args.get("path").and_then(Value::as_str).is_some();
        let abs_path = if has_path {
            require_resolved_path(ctx, args, "path")?
        } else {
            String::new()
        };

        let args_value = Value::Object(args.clone());
        let result = crate::tools::ctx_refactor::handle(&args_value, &ctx.project_root, &abs_path);

        let action = get_str(args, "action").unwrap_or_default();
        Ok(ToolOutput {
            text: result,
            original_tokens: 0,
            saved_tokens: 0,
            mode: Some(action.clone()),
            path: get_str(args, "path"),
            changed: matches!(
                action.as_str(),
                "replace_symbol_body"
                    | "insert_before_symbol"
                    | "insert_after_symbol"
                    | "rename_apply"
                    | "move_apply"
                    | "safe_delete_apply"
                    | "inline_apply"
                    | "reformat"
            ),
            shell_outcome: None,
        })
    }
}

#[cfg(test)]
mod schema_tests {
    use super::*;
    use crate::server::tool_trait::McpTool;

    #[test]
    fn schema_advertises_declaration_and_scope() {
        let tool = CtxRefactorTool;
        let def = tool.tool_def();
        let schema = serde_json::to_string(&def).unwrap();
        for needle in [
            "declaration",
            "\"scope\"",
            "type_hierarchy",
            "symbols_overview",
            "\"direction\"",
            "supertypes",
            "subtypes",
            "inspections",
            "\"mode\"",
            "replace_symbol_body",
            "insert_before_symbol",
            "insert_after_symbol",
            "name_path",
            "new_body",
            "expected_hash",
            "rename_preview",
            "rename_apply",
            "plan_hash",
            "force",
            "search_comments",
            "search_text_occurrences",
            "move_preview",
            "move_apply",
            "safe_delete_preview",
            "safe_delete_apply",
            "target_path",
            "target_parent",
            "propagate",
            "inline_preview",
            "inline_apply",
            "reformat",
            "keep_definition",
            "optimize_imports",
        ] {
            assert!(schema.contains(needle), "schema missing {needle}: {schema}");
        }
    }
}
