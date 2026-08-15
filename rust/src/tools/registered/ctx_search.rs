use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::core::ocla::cache_types::{CacheKeyBuilder, SearchQueryKey};
use crate::server::tool_trait::{
    McpTool, ToolContext, ToolOutput, get_bool, get_int, get_str, get_str_array, get_usize,
};
use crate::tool_defs::tool_def;

pub struct CtxSearchTool;

/// Which search engine a `ctx_search` call routes to (#509). One tool, many
/// engines — replacing the former `ctx_search`/`ctx_semantic_search`/`ctx_symbol`
/// trio with a single, less ambiguous entry point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SearchAction {
    Regex,
    Semantic,
    Symbol,
    Reindex,
    FindRelated,
}

impl SearchAction {
    /// An explicit `action` wins; otherwise the engine is inferred from which
    /// field the caller set, so pre-#509 call sites (`pattern`/`query`/`name`)
    /// keep working unchanged. Unknown `action` values fall through to inference.
    fn resolve(args: &Map<String, Value>) -> Self {
        if let Some(a) = get_str(args, "action") {
            match a.trim().to_ascii_lowercase().as_str() {
                "regex" | "grep" | "pattern" => return Self::Regex,
                "semantic" | "search" => return Self::Semantic,
                "symbol" => return Self::Symbol,
                "reindex" => return Self::Reindex,
                "find_related" | "related" => return Self::FindRelated,
                _ => {}
            }
        }
        if args.contains_key("handle") {
            Self::Symbol
        } else if args.contains_key("pattern") {
            Self::Regex
        } else if args.contains_key("name") {
            Self::Symbol
        } else if args.contains_key("file_path") && args.contains_key("line") {
            Self::FindRelated
        } else if args.contains_key("query") {
            Self::Semantic
        } else {
            Self::Regex
        }
    }
}

impl McpTool for CtxSearchTool {
    fn name(&self) -> &'static str {
        "ctx_search"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_search",
            "Search code: regex(pattern, default) | semantic(query) | symbol(name|handle) | \
             reindex | find_related(file_path,line). anchored=true enables ctx_patch refs; \
             queries batches regex searches. Run ctx_compose FIRST.",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["regex", "semantic", "symbol", "reindex", "find_related"]
                    },
                    "pattern": { "type": "string" },
                    "query": { "type": "string" },
                    "name": { "type": "string" },
                    "handle": { "type": "string" },
                    "path": { "type": "string" },
                    "paths": { "type": "array", "items": { "type": "string" } },
                    "include": { "type": "string", "description": "Glob, e.g. *.rs" },
                    "exclude": { "type": "string" },
                    "exclude_pattern": { "type": "string" },
                    "anchored": { "type": "boolean" },
                    "max_results": { "type": "integer" },
                    "top_k": { "type": "integer" },
                    "mode": { "type": "string", "enum": ["bm25", "dense", "hybrid"] },
                    "file": { "type": "string" },
                    "kind": { "type": "string" },
                    "file_path": { "type": "string" },
                    "line": { "type": "integer" },
                    "queries": {
                        "type": "array",
                        "items": { "type": "object" }
                    }
                },
                "allOf": [
                    {
                        "if": { "properties": { "action": { "const": "regex" } }, "required": ["action"] },
                        "then": { "anyOf": [{ "required": ["pattern"] }, { "required": ["queries"] }] }
                    },
                    {
                        "if": { "properties": { "action": { "const": "semantic" } }, "required": ["action"] },
                        "then": { "required": ["query"] }
                    },
                    {
                        "if": { "properties": { "action": { "const": "symbol" } }, "required": ["action"] },
                        "then": { "anyOf": [{ "required": ["name"] }, { "required": ["handle"] }] }
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
        let action = SearchAction::resolve(args);
        let result = match action {
            SearchAction::Regex => handle_regex(args, ctx),
            SearchAction::Semantic => handle_semantic(args, ctx),
            SearchAction::Symbol => handle_symbol(args, ctx),
            SearchAction::Reindex => handle_reindex(args, ctx),
            SearchAction::FindRelated => handle_find_related(args, ctx),
        };
        if let Ok(output) = &result {
            record_attribution_result(ctx, action, args, output);
        }
        result
    }
}

fn record_attribution_result(
    ctx: &ToolContext,
    action: SearchAction,
    args: &Map<String, Value>,
    output: &ToolOutput,
) {
    let session_id = crate::core::task_spine::TaskSpine::task_id()
        .or_else(|| {
            ctx.session
                .as_ref()
                .and_then(|session| session.try_read().ok().map(|state| state.id.clone()))
        })
        .unwrap_or_else(|| "mcp-session".to_string());
    let action = match action {
        SearchAction::Regex => "regex",
        SearchAction::Semantic => "semantic",
        SearchAction::Symbol => "symbol",
        SearchAction::Reindex => "reindex",
        SearchAction::FindRelated => "find_related",
    };
    let query = get_str(args, "pattern")
        .or_else(|| get_str(args, "query"))
        .or_else(|| get_str(args, "handle"))
        .or_else(|| get_str(args, "name"))
        .or_else(|| get_str(args, "file_path"))
        .unwrap_or_default();
    let source = if query.is_empty() {
        format!("ctx_search {action}")
    } else {
        format!("ctx_search {action} {query}")
    };
    let turn_provided = ctx.call_count.as_ref().map_or(0, |count| {
        count.load(std::sync::atomic::Ordering::Relaxed) as u64
    });
    let token_cost = crate::core::tokens::count_tokens(&output.text);
    let chunk = crate::core::causal_attribution::ContextChunkRecord::new(
        &output.text,
        source,
        token_cost,
        turn_provided,
    );
    if let Err(error) = crate::core::causal_attribution::record_chunk(&session_id, chunk) {
        tracing::debug!(%error, "causal attribution ctx_search recording failed");
    }
}

/// Known argument keys for ctx_search — used by the lenient fallback to detect
/// unrecognized keys that weaker models may use instead of `pattern`.
const KNOWN_KEYS: &[&str] = &[
    "action",
    "pattern",
    "query",
    "name",
    "handle",
    "path",
    "paths",
    "include",
    "exclude",
    "exclude_pattern",
    "ext",
    "anchored",
    "max_results",
    "top_k",
    "mode",
    "file",
    "kind",
    "file_path",
    "line",
    "languages",
    "path_glob",
    "workspace",
    "artifacts",
    "ignore_gitignore",
];

/// `action=regex` (default) — exact-pattern search over one or more roots.
fn handle_regex(args: &Map<String, Value>, ctx: &ToolContext) -> Result<ToolOutput, ErrorData> {
    // #871: batch mode — `queries: [{pattern, include?, exclude?}]` runs multiple
    // searches in one round-trip with grouped output.
    if let Some(Value::Array(queries)) = args.get("queries") {
        return handle_batch_queries(queries, args, ctx);
    }
    // Lenient fallback: if `pattern` is missing, accept the first unrecognized
    // string value as the pattern. Handles weak models that use keys like
    // "search_term", "text", "regex", etc. instead of the documented "pattern".
    let pattern = get_str(args, "pattern")
        .or_else(|| {
            args.iter()
                .find(|(k, v)| !KNOWN_KEYS.contains(&k.as_str()) && v.is_string())
                .and_then(|(_, v)| v.as_str().map(String::from))
        })
        .ok_or_else(|| {
            ErrorData::invalid_params(
                "pattern is required. Example: ctx_search(pattern=\"fn main\", path=\"/src\")",
                None,
            )
        })?;
    let resolved = crate::server::multi_path::resolve_tool_paths(args, ctx)
        .map_err(|e| ErrorData::invalid_params(format!("ERROR: {e}"), None))?;
    // `include` is the canonical glob filter; `ext` is the deprecated alias
    // (bare extension → `*.{ext}`). `include` wins when both are supplied.
    let include =
        get_str(args, "include").or_else(|| get_str(args, "ext").map(|e| ext_to_include(&e)));
    let max = (get_int(args, "max_results").unwrap_or(20) as usize).min(500);
    let no_gitignore = get_bool(args, "ignore_gitignore").unwrap_or(false);
    // #1008: opt-in N:hh line anchors on each hit for direct ctx_patch edits.
    let anchored = get_bool(args, "anchored").unwrap_or(false);
    // #870: negative filters — `exclude` (path glob, complement of `include`)
    // and `exclude_pattern` (regex dropping matching result lines, grep -v).
    let exclude = get_str(args, "exclude");
    let exclude_pattern = get_str(args, "exclude_pattern");

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
            anchored,
            exclude.as_deref(),
            exclude_pattern.as_deref(),
        );
    }

    let _mode_guard = crate::core::savings_footer::ModeGuard::new("search");
    let per_root_max = (max / resolved.roots.len()).max(5);
    let mut combined = String::new();
    let mut total_observed: usize = 0;
    let mut total_sent: usize = 0;
    let mut remaining_budget = max;

    for root in &resolved.roots {
        if remaining_budget == 0 {
            break;
        }
        let this_root_max = per_root_max.min(remaining_budget);
        let search_result = tokio::task::block_in_place(|| {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                cached_or_search(
                    &pattern,
                    root,
                    include.as_deref(),
                    this_root_max,
                    crp,
                    respect,
                    allow_secret_paths,
                    anchored,
                    exclude.as_deref(),
                    exclude_pattern.as_deref(),
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

        let result_matches = result.lines().filter(|l| !l.is_empty()).count();
        remaining_budget = remaining_budget.saturating_sub(result_matches);
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
    let final_out = crate::core::protocol::append_savings(&combined, total_observed, total_sent);
    let saved = total_observed.saturating_sub(total_sent);
    // #685: `actual_tokens` is the *sent* output, not the saving — passing
    // `saved` here recorded `actual=observed−sent` and `saved=sent` (both
    // wrong). Align with cli_grep/cli_shell, which pass the output count.
    crate::core::savings_ledger::record_tool_event(
        "ctx_search",
        total_observed,
        total_sent,
        None,
        None,
    );

    // R30: Search evidence for batch queries.
    crate::tools::search_hook::on_search("batch_query", "regex", total_observed, total_sent);

    Ok(ToolOutput {
        text: final_out,
        original_tokens: total_observed,
        saved_tokens: saved,
        mode: None,
        path: None,
        changed: false,
        shell_outcome: None,
        content_blocks: None,
    })
}

/// Resolve the `path` arg to a jailed path, falling back to the project root —
/// the same precedence the former standalone semantic-search tool used.
fn resolve_path_or_root(ctx: &ToolContext) -> Result<String, ErrorData> {
    if let Some(p) = ctx.resolved_path("path") {
        Ok(p.to_string())
    } else if let Some(err) = ctx.path_error("path") {
        Err(ErrorData::invalid_params(format!("path: {err}"), None))
    } else {
        Ok(ctx.project_root.clone())
    }
}

/// Prime the per-call BM25 cache so semantic engines reuse the warmed index
/// instead of reloading it from disk (perf parity with the former tool).
fn prime_bm25_cache(ctx: &ToolContext) {
    if let Some(ref cache) = ctx.bm25_cache {
        crate::tools::ctx_semantic_search::set_thread_cache(cache.clone());
    }
}

/// `action=semantic` — meaning-based search, routed to the shared core fn.
fn handle_semantic(args: &Map<String, Value>, ctx: &ToolContext) -> Result<ToolOutput, ErrorData> {
    let query = get_str(args, "query")
        .ok_or_else(|| ErrorData::invalid_params("query is required for action=semantic", None))?;
    let path = resolve_path_or_root(ctx)?;
    let top_k = get_usize(args, "top_k").unwrap_or(10).min(1000);
    let mode = get_str(args, "mode");
    let languages = get_str_array(args, "languages");
    let path_glob = get_str(args, "path_glob");
    let workspace = get_bool(args, "workspace").unwrap_or(false);
    let artifacts = get_bool(args, "artifacts").unwrap_or(false);
    prime_bm25_cache(ctx);

    let mut result = tokio::task::block_in_place(|| {
        crate::tools::ctx_semantic_search::handle(
            &query,
            &path,
            top_k,
            ctx.crp_mode,
            languages.as_deref(),
            path_glob.as_deref(),
            mode.as_deref(),
            Some(workspace),
            Some(artifacts),
        )
    });

    // Context Kernel: enrich semantic search with cross-store context
    {
        let kernel_budget = 100;
        if let Some(enrichment) =
            crate::core::context_kernel::bridge::kernel_enrich(&query, &path, kernel_budget)
            && !enrichment.blocks.is_empty()
        {
            result.push_str("\n--- kernel context ---\n");
            result.push_str(&enrichment.blocks);
        }
    }

    // R30: Search evidence for semantic searches.
    let search_tokens = crate::core::tokens::count_tokens(&result);
    crate::tools::search_hook::on_search(&query, "semantic", search_tokens, search_tokens);
    Ok(semantic_output(result))
}

/// #1108: when `path` or `file` is an absolute path under a different project,
/// resolve that project's root for the graph lookup. Falls back to the session
/// project_root when no cross-project path is given.
fn resolve_symbol_root(args: &Map<String, Value>, session_root: &str) -> String {
    let candidate = get_str(args, "path")
        .or_else(|| get_str(args, "file"))
        .filter(|p| std::path::Path::new(p.as_str()).is_absolute());

    if let Some(abs_path) = candidate
        && let Some(detected) = crate::core::protocol::detect_project_root(&abs_path)
        && detected != session_root
    {
        return detected;
    }
    session_root.to_string()
}

/// `action=symbol` — one symbol's body. A `handle` (`path#name@Lline`) resolves
/// deterministically (exact, no fuzzy/disambiguation); otherwise `name` runs the
/// fuzzy lookup. Both route to the shared `ctx_symbol` core.
fn handle_symbol(args: &Map<String, Value>, ctx: &ToolContext) -> Result<ToolOutput, ErrorData> {
    // #1108: resolve graph root from `path` when given, instead of always
    // using the sticky session project_root. This enables cross-repo symbol
    // lookup in multi-project MCP sessions.
    let effective_root = resolve_symbol_root(args, &ctx.project_root);

    if let Some(handle) = get_str(args, "handle") {
        let (result, original) =
            crate::tools::ctx_symbol::render_by_handle(&handle, &effective_root);
        let sent = crate::core::tokens::count_tokens(&result);
        return Ok(ToolOutput {
            text: result,
            original_tokens: original,
            saved_tokens: original.saturating_sub(sent),
            mode: Some("handle".to_string()),
            path: None,
            changed: false,
            shell_outcome: None,
            content_blocks: None,
        });
    }

    let name = get_str(args, "name").ok_or_else(|| {
        ErrorData::invalid_params("name or handle is required for action=symbol", None)
    })?;
    let file = get_str(args, "file");
    let kind = get_str(args, "kind");

    let (result, original) =
        crate::tools::ctx_symbol::handle(&name, file.as_deref(), kind.as_deref(), &effective_root);
    let sent = crate::core::tokens::count_tokens(&result);
    // R30: Search evidence for symbol lookups.
    crate::tools::search_hook::on_search(&name, "symbol", original, sent);
    Ok(ToolOutput {
        text: result,
        original_tokens: original,
        saved_tokens: original.saturating_sub(sent),
        mode: kind,
        path: file,
        changed: false,
        shell_outcome: None,
        content_blocks: None,
    })
}

/// `action=reindex` — rebuild the BM25 (or artifacts) index, routed to core.
fn handle_reindex(args: &Map<String, Value>, ctx: &ToolContext) -> Result<ToolOutput, ErrorData> {
    let path = resolve_path_or_root(ctx)?;
    let workspace = get_bool(args, "workspace").unwrap_or(false);
    let artifacts = get_bool(args, "artifacts").unwrap_or(false);
    prime_bm25_cache(ctx);

    let result = tokio::task::block_in_place(|| {
        if artifacts {
            crate::tools::ctx_semantic_search::handle_reindex_artifacts(&path, workspace)
        } else {
            crate::tools::ctx_semantic_search::handle_reindex(&path)
        }
    });

    Ok(semantic_output(result))
}

/// `action=find_related` — context neighbors for a source location, via core.
fn handle_find_related(
    args: &Map<String, Value>,
    ctx: &ToolContext,
) -> Result<ToolOutput, ErrorData> {
    let path = resolve_path_or_root(ctx)?;
    let top_k = get_usize(args, "top_k").unwrap_or(10).min(1000);
    let fp = get_str(args, "file_path").unwrap_or_default();
    let line = get_int(args, "line").unwrap_or(1) as usize;
    if fp.is_empty() {
        return Err(ErrorData::invalid_params(
            "find_related requires file_path and line",
            None,
        ));
    }
    prime_bm25_cache(ctx);

    let result = tokio::task::block_in_place(|| {
        crate::tools::ctx_semantic_search::handle_find_related(
            &fp,
            line,
            &path,
            top_k,
            ctx.crp_mode,
        )
    });

    Ok(semantic_output(result))
}

/// Shared `ToolOutput` shape for the semantic-engine branches (token accounting
/// is handled inside the core fns, mirroring the former standalone tool).
fn semantic_output(text: String) -> ToolOutput {
    let tokens = crate::core::tokens::count_tokens(&text);
    ToolOutput {
        text,
        original_tokens: tokens,
        saved_tokens: 0,
        mode: Some("semantic".to_string()),
        path: None,
        changed: false,
        shell_outcome: None,
        content_blocks: None,
    }
}

#[allow(clippy::too_many_arguments)]
fn cached_or_search(
    pattern: &str,
    path: &str,
    include: Option<&str>,
    max: usize,
    crp: crate::tools::CrpMode,
    respect_gitignore: bool,
    allow_secret_paths: bool,
    anchored: bool,
    exclude: Option<&str>,
    exclude_pattern: Option<&str>,
) -> crate::tools::ctx_search::SearchOutcome {
    let builder = regex_cache_builder(
        pattern,
        path,
        include,
        exclude,
        exclude_pattern,
        max,
        respect_gitignore,
        allow_secret_paths,
        anchored,
    );
    let key = builder.cache_key();
    if let Some(entry) =
        crate::core::ocla::cache_delivery::check(&key, &builder.validator(), "ctx_search")
    {
        let text = crate::core::ocla::cache_delivery::stub(&entry, "regex search");
        return crate::tools::ctx_search::SearchOutcome {
            text,
            modeled_baseline: entry.token_count as usize,
            observed_tokens: entry.token_count as usize,
        };
    }

    let outcome = crate::tools::ctx_search::handle_filtered(
        pattern,
        path,
        include,
        max,
        crp,
        respect_gitignore,
        allow_secret_paths,
        anchored,
        exclude,
        exclude_pattern,
    );
    if !outcome.text.starts_with("ERROR:") {
        crate::core::ocla::cache_delivery::record(
            key,
            crate::core::ocla::cache_types::DeliveryKind::SearchQuery,
            builder.validator(),
            Some(builder.path),
            &outcome.text,
            "ctx_search",
        );
    }
    outcome
}

#[allow(clippy::too_many_arguments)]
fn regex_cache_builder(
    pattern: &str,
    path: &str,
    include: Option<&str>,
    exclude: Option<&str>,
    exclude_pattern: Option<&str>,
    max: usize,
    respect_gitignore: bool,
    allow_secret_paths: bool,
    anchored: bool,
) -> SearchQueryKey {
    let canonical = crate::core::pathutil::safe_canonicalize_or_self(std::path::Path::new(path));
    SearchQueryKey {
        pattern: pattern.into(),
        include: format!(
            "{}\\x1fmax:{max}\\x1fgitignore:{respect_gitignore}\\x1fsecret:{allow_secret_paths}\\x1fanchored:{anchored}",
            include.unwrap_or_default()
        ),
        exclude: format!(
            "{}\\x1fline:{}",
            exclude.unwrap_or_default(),
            exclude_pattern.unwrap_or_default()
        ),
        path: canonical.to_string_lossy().into_owned(),
        // Regex searches do not rely on embedding state. The root mtime gives
        // their immutable query key a cheap revision when the file universe changes.
        index_rev: directory_mtime_ns(&canonical)
            .unwrap_or_default()
            .to_string(),
    }
}

fn directory_mtime_ns(path: &std::path::Path) -> Option<u128> {
    std::fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_nanos())
}

#[cfg(test)]
mod cache_delivery_tests {
    use super::*;

    #[test]
    fn regex_adapter_records_then_serves_a_cross_agent_reference() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join("cached.rs"),
            "fn cache_delivery_probe() {}\n",
        )
        .unwrap();
        let path = directory.path().to_string_lossy();

        let first = search_single(
            "cache_delivery_probe",
            &path,
            Some("*.rs"),
            20,
            crate::tools::CrpMode::Off,
            true,
            true,
            false,
            None,
            None,
        )
        .unwrap();
        assert!(first.text.contains("cache_delivery_probe"));
        let second = search_single(
            "cache_delivery_probe",
            &path,
            Some("*.rs"),
            20,
            crate::tools::CrpMode::Off,
            true,
            true,
            false,
            None,
            None,
        )
        .unwrap();
        assert!(
            second.text.contains("[cross-agent cache"),
            "{}",
            second.text
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn search_single(
    pattern: &str,
    path: &str,
    include: Option<&str>,
    max: usize,
    crp: crate::tools::CrpMode,
    respect_gitignore: bool,
    allow_secret_paths: bool,
    anchored: bool,
    exclude: Option<&str>,
    exclude_pattern: Option<&str>,
) -> Result<ToolOutput, ErrorData> {
    let _mode_guard = crate::core::savings_footer::ModeGuard::new("search");

    let search_result = tokio::task::block_in_place(|| {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            cached_or_search(
                pattern,
                path,
                include,
                max,
                crp,
                respect_gitignore,
                allow_secret_paths,
                anchored,
                exclude,
                exclude_pattern,
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
    crate::core::savings_ledger::record_tool_event("ctx_search", observed, sent, None, None);

    // R30: Search evidence + dedup detection via kernel.
    crate::tools::search_hook::on_search(pattern, "regex", observed, sent);

    Ok(ToolOutput {
        text: final_out,
        original_tokens: observed,
        saved_tokens: saved,
        mode: None,
        path: Some(path.to_string()),
        changed: false,
        shell_outcome: None,
        content_blocks: None,
    })
}

/// Translate the deprecated `ext` parameter into an `include` glob.
///
/// The historical `ext` accepted a bare extension (`rs` or `.rs`) and matched it
/// exactly; the equivalent glob is `*.{ext}` (the `glob` crate's `*` spans path
/// separators, so it still matches at any depth, preserving the old behaviour).
/// A value that already looks like a glob/path (`*`, `{`, `?`, `/`) is passed
/// through untouched so any power user who put a pattern in `ext` keeps working.
/// #871: batch multi-query — runs each query independently and groups output.
fn handle_batch_queries(
    queries: &[Value],
    args: &Map<String, Value>,
    ctx: &ToolContext,
) -> Result<ToolOutput, ErrorData> {
    if queries.is_empty() {
        return Err(ErrorData::invalid_params(
            "queries array must not be empty",
            None,
        ));
    }
    if queries.len() > 10 {
        return Err(ErrorData::invalid_params(
            "queries array limited to 10 entries",
            None,
        ));
    }

    let resolved = crate::server::multi_path::resolve_tool_paths(args, ctx)
        .map_err(|e| ErrorData::invalid_params(format!("ERROR: {e}"), None))?;
    let no_gitignore = get_bool(args, "ignore_gitignore").unwrap_or(false);
    let anchored = get_bool(args, "anchored").unwrap_or(false);
    let crp = ctx.crp_mode;
    let respect = !no_gitignore;
    let allow_secret_paths = crate::core::roles::active_role().io.allow_secret_paths;
    let root = &resolved.roots[0];
    let global_max = (get_int(args, "max_results").unwrap_or(20) as usize).min(500);
    let per_query_max = (global_max / queries.len()).max(5);

    let _mode_guard = crate::core::savings_footer::ModeGuard::new("search");
    let mut combined = String::new();
    let mut total_observed: usize = 0;
    let mut total_sent: usize = 0;

    for (idx, q) in queries.iter().enumerate() {
        let Some(obj) = q.as_object() else {
            combined.push_str(&format!(
                "── query {} ──\nERROR: expected object\n\n",
                idx + 1
            ));
            continue;
        };
        let Some(pattern) = get_str(obj, "pattern") else {
            combined.push_str(&format!(
                "── query {} ──\nERROR: pattern required\n\n",
                idx + 1
            ));
            continue;
        };
        let include =
            get_str(obj, "include").or_else(|| get_str(obj, "ext").map(|e| ext_to_include(&e)));
        let exclude = get_str(obj, "exclude");
        let exclude_pattern = get_str(obj, "exclude_pattern");

        let search_result = tokio::task::block_in_place(|| {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                cached_or_search(
                    &pattern,
                    root,
                    include.as_deref(),
                    per_query_max,
                    crp,
                    respect,
                    allow_secret_paths,
                    anchored,
                    exclude.as_deref(),
                    exclude_pattern.as_deref(),
                )
            }))
            .ok()
        });

        let label = if queries.len() > 1 {
            format!(
                "── query {}: '{}' ──\n",
                idx + 1,
                truncate_query(&pattern, 40)
            )
        } else {
            String::new()
        };

        let Some(outcome) = search_result else {
            combined.push_str(&format!("{label}ERROR: search panicked\n\n"));
            continue;
        };

        if !outcome.text.trim().is_empty() {
            combined.push_str(&format!("{label}{}\n\n", outcome.text));
            total_observed += outcome.observed_tokens;
            total_sent += crate::core::tokens::count_tokens(&outcome.text);
        }
    }

    if combined.is_empty() {
        combined = "No matches found for any query.".to_string();
    }

    let final_out = crate::core::protocol::append_savings(&combined, total_observed, total_sent);
    let saved = total_observed.saturating_sub(total_sent);
    crate::core::savings_ledger::record_tool_event(
        "ctx_search",
        total_observed,
        total_sent,
        None,
        None,
    );

    // R30: Search evidence for batch queries.
    crate::tools::search_hook::on_search("batch_query", "regex", total_observed, total_sent);

    Ok(ToolOutput {
        text: final_out,
        original_tokens: total_observed,
        saved_tokens: saved,
        mode: None,
        path: None,
        changed: false,
        shell_outcome: None,
        content_blocks: None,
    })
}

/// Truncate a query string for display (used in batch labels).
fn truncate_query(q: &str, max: usize) -> String {
    if q.len() <= max {
        q.to_string()
    } else {
        format!("{}...", &q[..q.floor_char_boundary(max)])
    }
}

fn ext_to_include(ext: &str) -> String {
    if ext.contains(['*', '{', '?', '/']) {
        return ext.to_string();
    }
    let bare = ext.strip_prefix('.').unwrap_or(ext);
    format!("*.{bare}")
}

#[cfg(test)]
mod tests {
    use super::{SearchAction, ext_to_include};
    use serde_json::{Map, Value, json};

    fn args(pairs: &[(&str, Value)]) -> Map<String, Value> {
        pairs
            .iter()
            .cloned()
            .map(|(k, v)| (k.to_string(), v))
            .collect()
    }

    #[test]
    fn explicit_action_selects_engine() {
        // #509: an explicit action always wins, including synonyms.
        assert_eq!(
            SearchAction::resolve(&args(&[("action", json!("semantic"))])),
            SearchAction::Semantic
        );
        assert_eq!(
            SearchAction::resolve(&args(&[("action", json!("symbol"))])),
            SearchAction::Symbol
        );
        assert_eq!(
            SearchAction::resolve(&args(&[("action", json!("grep"))])),
            SearchAction::Regex
        );
        assert_eq!(
            SearchAction::resolve(&args(&[("action", json!("related"))])),
            SearchAction::FindRelated
        );
        assert_eq!(
            SearchAction::resolve(&args(&[("action", json!("reindex"))])),
            SearchAction::Reindex
        );
    }

    #[test]
    fn action_inferred_from_fields_for_backward_compat() {
        // Pre-#509 call sites set only one of these fields and no action.
        assert_eq!(
            SearchAction::resolve(&args(&[("pattern", json!("fn .*"))])),
            SearchAction::Regex
        );
        assert_eq!(
            SearchAction::resolve(&args(&[("query", json!("user auth"))])),
            SearchAction::Semantic
        );
        assert_eq!(
            SearchAction::resolve(&args(&[("name", json!("handle"))])),
            SearchAction::Symbol
        );
        assert_eq!(
            SearchAction::resolve(&args(&[("file_path", json!("a.rs")), ("line", json!(10))])),
            SearchAction::FindRelated
        );
    }

    #[test]
    fn handle_infers_symbol_action() {
        // A bare `handle` (no action) must route to the symbol engine.
        assert_eq!(
            SearchAction::resolve(&args(&[("handle", json!("src/lib.rs#Config::load@L22"))])),
            SearchAction::Symbol
        );
    }

    #[test]
    fn pattern_wins_over_query_and_unknown_action_falls_back_to_inference() {
        // A regex caller that also carries a stray query must stay regex.
        assert_eq!(
            SearchAction::resolve(&args(&[("pattern", json!("x")), ("query", json!("y"))])),
            SearchAction::Regex
        );
        // Unknown action value → infer from fields (here: symbol).
        assert_eq!(
            SearchAction::resolve(&args(&[("action", json!("bogus")), ("name", json!("f"))])),
            SearchAction::Symbol
        );
        // Nothing recognizable → default regex (the empty-call default).
        assert_eq!(SearchAction::resolve(&args(&[])), SearchAction::Regex);
    }

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

    #[test]
    fn lenient_fallback_uses_unknown_string_key_as_pattern() {
        use super::{KNOWN_KEYS, get_str};

        // Simulate Gemma sending {"search_term": "fn main"} — an unknown key
        // with a string value should be picked up by the lenient fallback.
        let a = args(&[("search_term", json!("fn main"))]);
        let pattern = get_str(&a, "pattern").or_else(|| {
            a.iter()
                .find(|(k, v)| !KNOWN_KEYS.contains(&k.as_str()) && v.is_string())
                .and_then(|(_, v)| v.as_str().map(String::from))
        });
        assert_eq!(pattern, Some("fn main".to_string()));
    }

    #[test]
    fn lenient_fallback_does_not_grab_known_keys() {
        use super::{KNOWN_KEYS, get_str};

        // If only known keys are present (but pattern is missing), fallback
        // should NOT pick them up — it returns None.
        let a = args(&[("path", json!("/src")), ("max_results", json!(10))]);
        let pattern = get_str(&a, "pattern").or_else(|| {
            a.iter()
                .find(|(k, v)| !KNOWN_KEYS.contains(&k.as_str()) && v.is_string())
                .and_then(|(_, v)| v.as_str().map(String::from))
        });
        assert_eq!(pattern, None);
    }
}
