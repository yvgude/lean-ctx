use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput, get_bool, get_int, get_str};
use crate::tool_defs::tool_def;

pub struct CtxSearchTool;

impl McpTool for CtxSearchTool {
    fn name(&self) -> &'static str {
        "ctx_search"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_search",
            "Regex pattern search — use when you know the exact pattern. For understanding code or\n\
             finding answers, use ctx_compose FIRST (one call replaces search+read+symbol chains).\n\
             pattern required; include='*.rs'; path scopes; max_results=N (default 20).\n\
             paths=['dir1','dir2'] for multi-root. ignore_gitignore bypasses .gitignore (needs role).",
            json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "Regex pattern" },
                    "path": { "type": "string", "description": "Search dir" },
                    "paths": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Multi-root (alternative to path)"
                    },
                    "include": { "type": "string", "description": "Glob filter: *.ts, src/**/*.rs" },
                    "max_results": { "type": "integer", "description": "Default 20" },
                    "ignore_gitignore": { "type": "boolean", "description": "Scan gitignored (needs role)" }
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
        // `include` is the canonical glob filter; `ext` is the deprecated alias
        // (bare extension → `*.{ext}`). `include` wins when both are supplied.
        let include =
            get_str(args, "include").or_else(|| get_str(args, "ext").map(|e| ext_to_include(&e)));
        let max = (get_int(args, "max_results").unwrap_or(20) as usize).min(500);
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
            let pat = pattern.clone();
            let r = root.clone();
            let inc = include.clone();

            let search_result = tokio::task::block_in_place(|| {
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    crate::tools::ctx_search::handle(
                        &pat,
                        &r,
                        inc.as_deref(),
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

            if result.starts_with("ERROR:") || result.trim().is_empty() {
                if !result.trim().is_empty() {
                    combined.push_str(&format!("── {root} ──\n{result}\n\n"));
                }
                continue;
            }

            combined.push_str(&format!("── {root} ──\n{result}\n\n"));
            total_observed += outcome.observed_tokens;
            total_sent += crate::core::tokens::count_tokens(&result);
        }

        if combined.is_empty() {
            combined = "No matches found across any root.".to_string();
        }

        // Dashboard, footer and verified ledger all use *observed* tokens —
        // the modeled 2.5x native-grep baseline never inflates user-facing
        // numbers (GL #573). It only feeds the explicitly-estimated stats
        // series via `tool_lifecycle::record_search`.
        let final_out =
            crate::core::protocol::append_savings(&combined, total_observed, total_sent);
        let saved = total_observed.saturating_sub(total_sent);
        // #685: `actual_tokens` is the *sent* output, not the saving — passing
        // `saved` here recorded `actual=observed−sent` and `saved=sent` (both
        // wrong). Align with cli_grep/cli_shell, which pass the output count.
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
    let pattern_clone = pattern.to_string();
    let path_clone = path.to_string();

    let search_result = tokio::task::block_in_place(|| {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            crate::tools::ctx_search::handle(
                &pattern_clone,
                &path_clone,
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
