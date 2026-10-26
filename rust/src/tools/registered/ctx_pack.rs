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
            "WORKFLOW: create -> export -> import -> install for sharing context state.\n\
            ANTIPATTERN: NOT for ephemeral session save (use ctx_session).\n\
            Context Package Manager — create, install, manage portable context packages\n\
            with knowledge, graph, session patterns, and gotchas.\n\
            Actions: pr, create, list, info, remove, install, export, import, auto_load, summary, bundle.\n\
            bundle: one budgeted XML document (task-ranked files, signatures, tree) that fits a chat input box.\n\
            Saves tokens: pre-built context state (avoids re-building).",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["pr", "create", "list", "info", "remove", "install", "export", "import", "auto_load", "summary", "bundle"],
                        "description": "Pack action to perform"
                    },
                    "path": {
                        "type": "string",
                        "description": "Directory or file to bundle (bundle; default: project root)"
                    },
                    "limit": {
                        "type": "string",
                        "description": "Hard size cap, e.g. 128k, 2M, 32000 (bundle; default 128k)"
                    },
                    "unit": {
                        "type": "string",
                        "enum": ["chars", "tokens"],
                        "description": "What limit counts (bundle; default chars)"
                    },
                    "intent": {
                        "type": "string",
                        "description": "Task the bundle is for; ranks files (bundle; default: session task)"
                    },
                    "emit": {
                        "type": "string",
                        "enum": ["xml", "plain", "both"],
                        "description": "xml bundle, plain allocation report, or both (bundle)"
                    },
                    "include": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Globs to include (bundle)"
                    },
                    "ignore": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Globs to exclude (bundle)"
                    },
                    "with_knowledge": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Knowledge categories to append, e.g. decision, architecture, or all (bundle)"
                    },
                    "knowledge_limit": {
                        "type": "integer",
                        "description": "Max knowledge facts (bundle; default 10)"
                    },
                    "with_auto": {
                        "type": "boolean",
                        "description": "Also include machine-derived auto:* facts (bundle)"
                    },
                    "project_root": {
                        "type": "string",
                        "description": "Project root directory"
                    },
                    "name": {
                        "type": "string",
                        "description": "Package name"
                    },
                    "version": {
                        "type": "string",
                        "description": "Package version (semver)"
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
                        "description": "Tags for categorization (for create)"
                    },
                    "layers": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Layers to include: knowledge|graph|session|patterns|gotchas"
                    },
                    "level": {
                        "type": "integer",
                        "description": "Detail level 1-3 (higher = more detail)"
                    },
                    "scope": {
                        "type": "string",
                        "description": "Package scope (e.g. @org/name)"
                    },
                    "base": {
                        "type": "string",
                        "description": "Git base ref for PR diff"
                    },
                    "format": {
                        "type": "string",
                        "enum": ["markdown", "json"],
                        "description": "Output format: markdown|json"
                    },
                    "depth": {
                        "type": "integer",
                        "description": "Impact depth for pr action (default: 3)"
                    },
                    "diff": {
                        "type": "string",
                        "description": "Git diff --name-status text input"
                    },
                    "file": {
                        "type": "string",
                        "description": "File path for import/export; bundle output file"
                    },
                    "apply": {
                        "type": "boolean",
                        "description": "Apply after import (default: false)"
                    },
                    "enable": {
                        "type": "boolean",
                        "description": "Enable auto-load (default: true)"
                    }
                },
                "required": ["action"],
                "allOf": [
                    {
                        "if": { "properties": { "action": { "const": "create" } }, "required": ["action"] },
                        "then": { "required": ["action", "name"] }
                    },
                    {
                        "if": { "properties": { "action": { "const": "info" } }, "required": ["action"] },
                        "then": { "required": ["action", "name"] }
                    },
                    {
                        "if": { "properties": { "action": { "const": "remove" } }, "required": ["action"] },
                        "then": { "required": ["action", "name"] }
                    },
                    {
                        "if": { "properties": { "action": { "const": "install" } }, "required": ["action"] },
                        "then": { "required": ["action", "name"] }
                    },
                    {
                        "if": { "properties": { "action": { "const": "export" } }, "required": ["action"] },
                        "then": { "required": ["action", "name"] }
                    },
                    {
                        "if": { "properties": { "action": { "const": "import" } }, "required": ["action"] },
                        "then": { "required": ["action", "file"] }
                    }
                ]
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
            "bundle" => handle_bundle(args, ctx, &project_root)?,
            _ => "Unknown action. Use: pr, create, list, info, remove, install, export, import, auto_load, summary, bundle".to_string(),
        };

        Ok(ToolOutput::simple(result))
    }
}

/// `action=bundle` (#1885): one budgeted XML document for a chat product.
/// Returns the XML (or only the report for `emit=plain`); with `file` the XML
/// is written there and only the report comes back. The secret scan is
/// always on over MCP.
fn handle_bundle(
    args: &Map<String, Value>,
    ctx: &ToolContext,
    project_root: &str,
) -> Result<String, ErrorData> {
    use crate::core::context_bundle::{self, BundleOptions, Unit, parse_limit};
    let invalid = |msg: String| ErrorData::invalid_params(msg, None);

    let mut opts = BundleOptions::new(project_root.into());
    if let Some(path) = ctx.resolved_path("path") {
        opts.scope = Some(path.into());
    } else if let Some(err) = ctx.path_error("path") {
        return Err(invalid(format!("path: {err}")));
    }
    match args.get("limit") {
        Some(Value::String(raw)) => opts.limit = parse_limit(raw).map_err(invalid)?,
        Some(Value::Number(n)) => {
            opts.limit = n
                .as_u64()
                .and_then(|n| usize::try_from(n).ok())
                .filter(|n| *n > 0)
                .ok_or_else(|| invalid("limit must be a positive integer".into()))?;
        }
        Some(_) => {
            return Err(invalid(
                "limit must be a number or a string like 128k".into(),
            ));
        }
        None => {}
    }
    if let Some(unit) = get_str(args, "unit") {
        opts.unit = Unit::parse(&unit).map_err(invalid)?;
    }
    opts.intent = get_str(args, "intent");
    opts.include = get_str_array(args, "include").unwrap_or_default();
    opts.exclude = get_str_array(args, "ignore").unwrap_or_default();
    opts.knowledge = get_str_array(args, "with_knowledge").filter(|c| !c.is_empty());
    if let Some(n) = get_usize(args, "knowledge_limit") {
        opts.knowledge_limit = n;
    }
    opts.knowledge_auto = get_bool(args, "with_auto").unwrap_or(false);

    let emit = get_str(args, "emit").unwrap_or_else(|| "xml".into());
    if !matches!(emit.as_str(), "xml" | "plain" | "both") {
        return Err(invalid(format!(
            "emit must be xml, plain or both (got '{emit}')"
        )));
    }
    let bundle = context_bundle::build(&opts).map_err(invalid)?;
    let report = bundle.report();

    if let Some(file) = ctx.resolved_path("file") {
        if emit == "plain" {
            return Err(invalid("emit=plain writes no bundle; drop file".into()));
        }
        ctx.ensure_writable(file).map_err(invalid)?;
        std::fs::write(file, &bundle.xml)
            .map_err(|e| ErrorData::internal_error(format!("cannot write {file}: {e}"), None))?;
        return Ok(format!("{report}written: {file}"));
    } else if let Some(err) = ctx.path_error("file") {
        return Err(invalid(format!("file: {err}")));
    }
    Ok(match emit.as_str() {
        "plain" => report,
        "both" => format!("{report}\n{}", bundle.xml),
        _ => bundle.xml,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
        dir
    }

    fn ctx_for(root: &std::path::Path, file: Option<&std::path::Path>) -> ToolContext {
        let mut ctx = ToolContext {
            project_root: root.to_string_lossy().into_owned(),
            ..Default::default()
        };
        if let Some(file) = file {
            ctx.resolved_paths
                .insert("file".into(), file.to_string_lossy().into_owned());
        }
        ctx
    }

    fn bundle(ctx: &ToolContext, extra: Value) -> Result<String, ErrorData> {
        let mut args = json!({ "action": "bundle", "limit": "8k" });
        if let (Some(args), Value::Object(extra)) = (args.as_object_mut(), extra) {
            args.extend(extra);
        }
        CtxPackTool
            .handle(args.as_object().unwrap(), ctx)
            .map(|out| out.text)
    }

    #[test]
    fn bundle_returns_xml_within_the_limit() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        let dir = project();
        let xml = bundle(&ctx_for(dir.path(), None), json!({})).unwrap();
        assert!(xml.starts_with("<bundle "), "{xml}");
        assert!(xml.contains("fn main()"), "{xml}");
        assert!(xml.chars().count() <= 8000);
    }

    #[test]
    fn bundle_rejects_bad_emit_and_limit() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        let dir = project();
        let ctx = ctx_for(dir.path(), None);
        assert!(bundle(&ctx, json!({ "emit": "html" })).is_err());
        assert!(bundle(&ctx, json!({ "limit": 0 })).is_err());
        assert!(bundle(&ctx, json!({ "limit": true })).is_err());
    }

    #[test]
    fn bundle_writes_the_file_and_returns_the_report() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        let dir = project();
        let out = dir.path().join("bundle.xml");
        let report = bundle(&ctx_for(dir.path(), Some(&out)), json!({})).unwrap();
        assert!(report.contains("written:"), "{report}");
        let xml = std::fs::read_to_string(&out).unwrap();
        assert!(xml.starts_with("<bundle "), "{xml}");
    }

    /// #475: `file` must not write into a read-only root.
    #[test]
    fn bundle_refuses_to_write_into_a_read_only_root() {
        let _iso = crate::core::data_dir::isolated_data_dir();
        let dir = project();
        let ro = dir.path().join("refrepo");
        std::fs::create_dir_all(&ro).unwrap();
        let out = ro.join("bundle.xml");

        let ro_canon = crate::core::pathjail::canonicalize_or_self(&ro);
        crate::test_env::set_var(
            "LEAN_CTX_READ_ONLY_ROOTS",
            ro_canon.to_string_lossy().as_ref(),
        );
        let result = bundle(&ctx_for(dir.path(), Some(&out)), json!({}));
        crate::test_env::remove_var("LEAN_CTX_READ_ONLY_ROOTS");

        let err = result.expect_err("write into a read-only root must be refused");
        assert!(err.message.contains("read-only"), "{err:?}");
        assert!(
            !out.exists(),
            "no bundle may be written into a read-only root"
        );
    }
}
