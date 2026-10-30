use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{
    McpTool, ToolContext, ToolOutput, get_str, get_str_array, get_usize, require_resolved_path,
};
use crate::tool_defs::tool_def;

pub struct CtxMultiRepoTool;

impl McpTool for CtxMultiRepoTool {
    fn name(&self) -> &'static str {
        "ctx_multi_repo"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_multi_repo",
            "Multi-repository — add, remove, search project directories.\n\
             WORKFLOW: list_roots → add_root/remove_root → search.\n\
             ANTI-PATTERN: not for single-repo projects — use ctx_search.\n\
             Actions: add_root|remove_root|list_roots|search|status|save_config.\n\
             Cross-repo search runs hybrid retrieval per root (BM25+dense+SPLADE)\n\
             and merges with RRF; mode=\"bm25\" forces lexical-only.\n\
             ctx_search/ctx_glob/ctx_tree/ctx_read all accept a repo=<alias>\n\
             arg (not in their own schema) to target a registered root by\n\
             alias instead of the project root — list_roots shows the aliases.",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["add_root", "remove_root", "list_roots", "search", "status", "save_config"],
                        "description": "add_root|remove_root|list_roots|search|status|save_config"
                    },
                    "path": {
                        "type": "string",
                        "description": "Repo path"
                    },
                    "alias": {
                        "type": "string",
                        "description": "Short alias (auto-derived if omitted)"
                    },
                    "query": {
                        "type": "string",
                        "description": "Search query (for search action)"
                    },
                    "roots": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Filter to specific repos by alias/path"
                    },
                    "max_results": {
                        "type": "integer",
                        "description": "Max results"
                    },
                    "mode": {
                        "type": "string",
                        "enum": ["hybrid", "bm25"],
                        "description": "Per-root ranking signal (default: hybrid; bm25 = lexical only)"
                    }
                },
                "required": ["action"],
                "allOf": [
                    {
                        "if": { "properties": { "action": { "const": "add_root" } }, "required": ["action"] },
                        "then": { "required": ["action", "path"] }
                    },
                    {
                        "if": { "properties": { "action": { "const": "remove_root" } }, "required": ["action"] },
                        "then": { "required": ["action", "path"] }
                    },
                    {
                        "if": { "properties": { "action": { "const": "search" } }, "required": ["action"] },
                        "then": { "required": ["action", "query"] }
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

        // A registered root is searched and read through `repo=<alias>`, which
        // jails only against that root: the root itself must pass PathJail like
        // every other path argument, or `add_root /` would open the whole disk.
        // `remove_root` takes an alias or a registered path and opens nothing.
        let path = if action == "add_root" {
            Some(require_resolved_path(ctx, args, "path")?)
        } else {
            get_str(args, "path")
        };
        let alias = get_str(args, "alias");
        let query = get_str(args, "query");
        let roots_filter = get_str_array(args, "roots");
        let max_results = get_usize(args, "max_results").unwrap_or(20).min(1000);
        let mode = get_str(args, "mode");

        let (result, original_tokens) = crate::tools::ctx_multi_repo::handle(
            &action,
            path.as_deref(),
            alias.as_deref(),
            query.as_deref(),
            roots_filter.as_deref(),
            max_results,
            mode.as_deref(),
        );

        if result.starts_with("ERROR:") {
            return Err(ErrorData::invalid_params(result, None));
        }

        let sent = crate::core::tokens::count_tokens(&result);
        let saved = original_tokens.saturating_sub(sent);

        Ok(ToolOutput {
            text: result,
            original_tokens,
            saved_tokens: saved,
            mode: Some("multi_repo".to_string()),
            path,
            changed: action == "add_root" || action == "remove_root",
            shell_outcome: None,
            content_blocks: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(path: &str) -> Map<String, Value> {
        json!({ "action": "add_root", "path": path })
            .as_object()
            .cloned()
            .unwrap()
    }

    /// `add_root` registers only the jailed path the dispatcher resolved; a raw
    /// path the jail refused (or never resolved) is not registered.
    #[test]
    fn add_root_uses_the_jailed_path_only() {
        let tool = CtxMultiRepoTool;
        let root_count = || {
            crate::core::multi_repo::global_manager()
                .lock()
                .unwrap()
                .root_count()
        };
        let before = root_count();

        // No resolution at all (raw argument only): refused.
        let Err(err) = tool.handle(&args("/"), &ToolContext::default()) else {
            panic!("an unresolved root must be refused");
        };
        assert!(err.message.contains("path"), "{}", err.message);

        // The jail refused the argument: its refusal is returned.
        let mut ctx = ToolContext::default();
        ctx.path_errors
            .insert("path".into(), "path escapes project root".into());
        let Err(err) = tool.handle(&args("/"), &ctx) else {
            panic!("a jail-refused root must be refused");
        };
        assert!(
            err.message.contains("escapes project root"),
            "{}",
            err.message
        );
        assert_eq!(root_count(), before, "a refused root is never registered");

        // The resolved path is what gets registered, not the raw argument.
        let tmp = tempfile::tempdir().unwrap();
        let mut ctx = ToolContext::default();
        ctx.resolved_paths
            .insert("path".into(), tmp.path().to_string_lossy().into_owned());
        let alias = "jailed-root-test";
        let mut a = args("/");
        a.insert("alias".into(), json!(alias));
        let Ok(out) = tool.handle(&a, &ctx) else {
            panic!("the resolved root is added");
        };
        assert!(
            out.text.contains(&*tmp.path().to_string_lossy()),
            "{}",
            out.text
        );
        crate::core::multi_repo::global_manager()
            .lock()
            .unwrap()
            .remove_root(&tmp.path().to_string_lossy())
            .unwrap();
    }
}
