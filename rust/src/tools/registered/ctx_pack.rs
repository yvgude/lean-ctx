use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{
    McpTool, ToolContext, ToolOutput, get_bool, get_int, get_str, get_str_array, get_usize,
};
use crate::tool_defs::tool_def;

pub struct CtxPackTool;

impl McpTool for CtxPackTool {
    fn name(&self) -> &'static str {
        "ctx_pack"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_pack",
            "Context Package Manager. Actions: pr (PR context), create (build package from project), list, info, remove, install, export, import, auto_load, summary.",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["pr", "create", "list", "info", "remove", "install", "export", "import", "auto_load", "summary"],
                        "description": "Pack action to perform"
                    },
                    "project_root": {
                        "type": "string",
                        "description": "Project root (default: session project root)"
                    },
                    "name": {
                        "type": "string",
                        "description": "Package name (required for create, info, remove, install, export, auto_load)"
                    },
                    "version": {
                        "type": "string",
                        "description": "Package version (default: latest or '1.0.0' for create)"
                    },
                    "description": {
                        "type": "string",
                        "description": "Package description (for create)"
                    },
                    "author": {
                        "type": "string",
                        "description": "Package author (for create)"
                    },
                    "tags": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Tags (for create)"
                    },
                    "layers": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Layers to include: knowledge, graph, session, patterns, gotchas (for create)"
                    },
                    "level": {
                        "type": "integer",
                        "description": "Conformance level 1-3 (for create, default: 1)"
                    },
                    "scope": {
                        "type": "string",
                        "description": "Package scope like @org (for create)"
                    },
                    "base": {
                        "type": "string",
                        "description": "Git base ref (for pr action, default: auto-detect)"
                    },
                    "format": {
                        "type": "string",
                        "enum": ["markdown", "json"],
                        "description": "Output format (for pr action, default: markdown)"
                    },
                    "depth": {
                        "type": "integer",
                        "description": "Impact depth (for pr action, default: 3)"
                    },
                    "diff": {
                        "type": "string",
                        "description": "Git diff --name-status text (for pr action; if omitted, computed via git)"
                    },
                    "file": {
                        "type": "string",
                        "description": "File path (for import/export)"
                    },
                    "apply": {
                        "type": "boolean",
                        "description": "Apply package after import (for import action, default: false)"
                    },
                    "enable": {
                        "type": "boolean",
                        "description": "Enable or disable auto-load (for auto_load action, default: true)"
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

        let project_root = if let Some(p) = ctx
            .resolved_path("project_root")
            .or(ctx.resolved_path("root"))
        {
            p.to_string()
        } else if let Some(err) = ctx.path_error("project_root").or(ctx.path_error("root")) {
            return Err(ErrorData::invalid_params(
                format!("project_root: {err}"),
                None,
            ));
        } else {
            ctx.project_root.clone()
        };

        let result = match action.as_str() {
            "pr" => {
                let base = get_str(args, "base");
                let format = get_str(args, "format");
                let depth = get_usize(args, "depth").map(|d| d.min(64));
                let diff = get_str(args, "diff");
                crate::tools::ctx_pack::handle(
                    "pr",
                    &project_root,
                    base.as_deref(),
                    format.as_deref(),
                    depth,
                    diff.as_deref(),
                )
            }
            "create" => {
                let name = get_str(args, "name")
                    .ok_or_else(|| ErrorData::invalid_params("name is required for create", None))?;
                let version = get_str(args, "version");
                let description = get_str(args, "description");
                let author = get_str(args, "author");
                let tags = get_str_array(args, "tags");
                let layers = get_str_array(args, "layers");
                let level = get_int(args, "level").and_then(|l| u32::try_from(l).ok());
                let scope = get_str(args, "scope");
                crate::tools::ctx_pack::handle_create(
                    &project_root,
                    &name,
                    version.as_deref(),
                    description.as_deref(),
                    author.as_deref(),
                    tags.as_deref(),
                    layers.as_deref(),
                    level,
                    scope.as_deref(),
                )
            }
            "list" => crate::tools::ctx_pack::handle_list(),
            "info" => {
                let name = get_str(args, "name")
                    .ok_or_else(|| ErrorData::invalid_params("name is required for info", None))?;
                let version = get_str(args, "version");
                crate::tools::ctx_pack::handle_info(&name, version.as_deref())
            }
            "remove" => {
                let name = get_str(args, "name")
                    .ok_or_else(|| ErrorData::invalid_params("name is required for remove", None))?;
                let version = get_str(args, "version");
                crate::tools::ctx_pack::handle_remove(&name, version.as_deref())
            }
            "install" => {
                let name = get_str(args, "name").ok_or_else(|| {
                    ErrorData::invalid_params("name is required for install", None)
                })?;
                let version = get_str(args, "version");
                crate::tools::ctx_pack::handle_install(&name, version.as_deref(), &project_root)
            }
            "export" => {
                let name = get_str(args, "name").ok_or_else(|| {
                    ErrorData::invalid_params("name is required for export", None)
                })?;
                let version = get_str(args, "version");
                let file = get_str(args, "file");
                crate::tools::ctx_pack::handle_export(&name, version.as_deref(), file.as_deref())
            }
            "import" => {
                let file = get_str(args, "file")
                    .ok_or_else(|| ErrorData::invalid_params("file is required for import", None))?;
                let apply = get_bool(args, "apply").unwrap_or(false);
                crate::tools::ctx_pack::handle_import(&file, apply, &project_root)
            }
            "auto_load" => {
                let name = get_str(args, "name");
                let version = get_str(args, "version");
                let enable = get_bool(args, "enable").unwrap_or(true);
                crate::tools::ctx_pack::handle_auto_load(
                    name.as_deref(),
                    version.as_deref(),
                    enable,
                )
            }
            "summary" => crate::tools::ctx_pack::handle_summary(&project_root),
            _ => "Unknown action. Use: pr, create, list, info, remove, install, export, import, auto_load, summary".to_string(),
        };

        Ok(ToolOutput::simple(result))
    }
}
