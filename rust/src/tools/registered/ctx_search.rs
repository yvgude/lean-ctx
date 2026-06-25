use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{
    McpTool, ToolContext, ToolOutput, get_bool, get_int, get_str, get_usize,
};
use crate::tool_defs::tool_def;

pub struct CtxSearchTool;

impl McpTool for CtxSearchTool {
    fn name(&self) -> &'static str {
        "ctx_search"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_search",
            "Search code with regex (action=grep) or semantic search (action=search).\n\
             action=grep (default when pattern is present): regex file search.\n\
             action=search: semantic/vector search (uses ctx_semantic_search internally).\n\
             action=reindex: rebuild search index.\n\
             For action=grep: pattern required; include='*.rs'; path scopes;\n\
             limit=N or max_results=N (default 20); paths=['dir1','dir2'] for multi-root;\n\
             ignore_gitignore bypasses .gitignore (needs role).",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["grep", "search", "reindex"],
                        "description": "grep|search|reindex"
                    },
                    "pattern": { "type": "string", "description": "Regex (grep)" },
                    "path": { "type": "string", "description": "Dir" },
                    "paths": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Multi-root dirs"
                    },
                    "include": { "type": "string", "description": "Glob (grep)" },
                    "max_results": { "type": "integer", "description": "Max (deprecated, use limit)" },
                    "limit": { "type": "integer", "description": "Max results" },
                    "offset": { "type": "integer", "description": "Result offset (pagination)" },
                    "context": { "type": "boolean", "description": "Show source context around matches" },
                    "ignore_gitignore": { "type": "boolean", "description": "Scan gitignored (grep)" },
                    "query": { "type": "string", "description": "Query (search)" },
                    "method": { "type": "string", "enum": ["bm25", "dense", "hybrid"], "description": "bm25|dense|hybrid" },
                    "top_k": { "type": "integer", "description": "Results (search)" },
                    "languages": { "type": "array", "items": { "type": "string" }, "description": "Lang filter" },
                    "path_glob": { "type": "string", "description": "Path glob (search)" },
                    "related_to": { "type": "string", "description": "Related symbol (search)" },
                    "mode": { "type": "string", "description": "full|incremental (reindex)" },
                    "artifacts": { "type": "boolean", "description": "Artifacts only (reindex)" },
                    "workspace": { "type": "boolean", "description": "Workspace (reindex)" }
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
        // Parse action; default to "grep" when pattern is present (backward compat)
        let action = get_str(args, "action").unwrap_or_default();

        match action.as_str() {
            "" | "grep" => {
                let pattern = get_str(args, "pattern")
                    .ok_or_else(|| ErrorData::invalid_params("pattern is required", None))?;
                let resolved = crate::server::multi_path::resolve_tool_paths(args, ctx)
                    .map_err(|e| ErrorData::invalid_params(format!("ERROR: {e}"), None))?;
                // `include` is the canonical glob filter; `ext` is the deprecated alias
                // (bare extension → `*.{ext}`). `include` wins when both are supplied.
                let include = get_str(args, "include")
                    .or_else(|| get_str(args, "ext").map(|e| ext_to_include(&e)));
                // Backward compat: limit replaces max_results
                let max = get_usize(args, "limit")
                    .or_else(|| {
                        get_int(args, "max_results")
                            .and_then(|n| usize::try_from(n).ok())
                    })
                    .unwrap_or(20)
                    .min(500);
                let no_gitignore = get_bool(args, "ignore_gitignore").unwrap_or(false);

                if no_gitignore
                    && let Err(e) = crate::core::io_boundary::ensure_ignore_gitignore_allowed("ctx_search")
                {
                    return Ok(ToolOutput::simple(e));
                }

                let crp = ctx.crp_mode;
                let respect = !no_gitignore;
                let allow_secret_paths = crate::core::roles::active_role().io.allow_secret_paths;

                if !resolved.is_multi {
                    return search_single(
                        &pattern,
                        &resolved.roots[0],
                        include.as_deref(),
                        max,
                        crp,
                        respect,
                        allow_secret_paths,
                    );
                }

                let _mode_guard = crate::core::savings_footer::ModeGuard::new("search");
                let per_root_max = (max / resolved.roots.len()).max(5);
                let mut combined = String::new();
                let mut total_observed: usize = 0;
                let mut total_sent: usize = 0;

                for root in &resolved.roots {
                    let search_result = tokio::task::block_in_place(|| {
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            crate::tools::ctx_search::handle(
                                &pattern,
                                root,
                                include.as_deref(),
                                per_root_max,
                                crp,
                                respect,
                                allow_secret_paths,
                            )
                        }))
                        .ok()
                    });

                    let Some(outcome) = search_result else {
                        combined.push_str(&format!("── {root} ──\nERROR: search panicked\n\n"));
                        continue;
                    };
                    let result = outcome.text;

                    if result.trim().is_empty() {
                        continue;
                    }

                    combined.push_str(&format!("── {root} ──\n{result}\n\n"));

                    if result.starts_with("ERROR:") {
                        continue;
                    }

                    total_observed += outcome.observed_tokens;
                    total_sent += crate::core::tokens::count_tokens(&result);
                }

                if combined.is_empty() {
                    combined = "No matches found across any root.".to_string();
                }

                let final_out =
                    crate::core::protocol::append_savings(&combined, total_observed, total_sent);
                let saved = total_observed.saturating_sub(total_sent);
                crate::core::savings_ledger::record_tool_event("ctx_search", total_observed, total_sent);

                Ok(ToolOutput {
                    text: final_out,
                    original_tokens: total_observed,
                    saved_tokens: saved,
                    mode: None,
                    path: None,
                    changed: false,
                    shell_outcome: None,
                })
            }
            "search" | "reindex" => {
                let search = crate::tools::ctx_search::CtxSearch::try_from(args)
                    .map_err(|e| ErrorData::invalid_params(e, None))?;
                let outcome = crate::tools::ctx_search::handle_enum(
                    search,
                    ctx.crp_mode,
                    &ctx.project_root,
                );
                let out: ToolOutput = outcome.into();
                crate::core::savings_ledger::record_tool_event(
                    "ctx_search",
                    out.original_tokens,
                    out.original_tokens.saturating_sub(out.saved_tokens),
                );
                Ok(out)
            }
            other => Err(ErrorData::invalid_params(
                format!("Unknown action '{other}'. Must be one of: grep, search, reindex"),
                None,
            )),
        }
    }
}

fn search_single(
    pattern: &str,
    path: &str,
    include: Option<&str>,
    max: usize,
    crp: crate::tools::CrpMode,
    respect_gitignore: bool,
    allow_secret_paths: bool,
) -> Result<ToolOutput, ErrorData> {
    let _mode_guard = crate::core::savings_footer::ModeGuard::new("search");

    let search_result = tokio::task::block_in_place(|| {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            crate::tools::ctx_search::handle(
                pattern,
                path,
                include,
                max,
                crp,
                respect_gitignore,
                allow_secret_paths,
            )
        }));
        match result {
            Ok(r) => Ok(r),
            Err(_) => Err("search task panicked"),
        }
    });

    let outcome = match search_result {
        Ok(r) => r,
        Err(e) => {
            return Err(ErrorData::internal_error(
                format!("search task failed: {e}"),
                None,
            ));
        }
    };
    let result = outcome.text;
    // Observed tokens only — the modeled native-grep baseline stays out of
    // dashboard/footer/ledger (GL #573); see the multi-root branch above.
    let observed = outcome.observed_tokens;

    if result.starts_with("ERROR:") {
        return Err(ErrorData::invalid_params(result, None));
    }

    let sent = crate::core::tokens::count_tokens(&result);
    let saved = observed.saturating_sub(sent);
    let final_out = crate::core::protocol::append_savings(&result, observed, sent);
    // #685: pass the *sent* output as `actual_tokens` (not `saved`); see the
    // multi-root branch above for why the previous arg was a double bug.
    crate::core::savings_ledger::record_tool_event("ctx_search", observed, sent);

    Ok(ToolOutput {
        text: final_out,
        original_tokens: observed,
        saved_tokens: saved,
        mode: None,
        path: Some(path.to_string()),
        changed: false,
        shell_outcome: None,
    })
}

/// Translate the deprecated `ext` parameter into an `include` glob.
///
/// The historical `ext` accepted a bare extension (`rs` or `.rs`) and matched it
/// exactly; the equivalent glob is `*.{ext}` (the `glob` crate's `*` spans path
/// separators, so it still matches at any depth, preserving the old behaviour).
/// A value that already looks like a glob/path (`*`, `{`, `?`, `/`) is passed
/// through untouched so any power user who put a pattern in `ext` keeps working.
fn ext_to_include(ext: &str) -> String {
    if ext.contains(['*', '{', '?', '/']) {
        return ext.to_string();
    }
    let bare = ext.strip_prefix('.').unwrap_or(ext);
    format!("*.{bare}")
}

#[cfg(test)]
mod tests {
    use super::ext_to_include;

    #[test]
    fn ext_alias_bare_extension_becomes_glob() {
        assert_eq!(ext_to_include("rs"), "*.rs");
        assert_eq!(ext_to_include("ts"), "*.ts");
    }

    #[test]
    fn ext_alias_strips_leading_dot() {
        assert_eq!(ext_to_include(".rs"), "*.rs");
        assert_eq!(ext_to_include(".tsx"), "*.tsx");
    }

    #[test]
    fn ext_alias_passes_through_glob_like_values() {
        // Already a glob/path → keep verbatim, don't double-wrap.
        assert_eq!(ext_to_include("*.rs"), "*.rs");
        assert_eq!(ext_to_include("*.{rs,ts}"), "*.{rs,ts}");
        assert_eq!(ext_to_include("src/**/*.tsx"), "src/**/*.tsx");
    }
}
