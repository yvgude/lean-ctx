// SPDX-License-Identifier: Apache-2.0
//! MCP schema and dispatch for the single ctx_read implementation.

use super::{
    CtxReadTool, ErrorData, Map, McpTool, Tool, ToolContext, ToolOutput, Value, engine, get_str,
    record_attribution_result, require_resolved_path,
};

impl McpTool for CtxReadTool {
    fn name(&self) -> &'static str {
        "ctx_read"
    }

    fn produces_machine_readable(&self, args: Option<&Map<String, Value>>) -> bool {
        // The explicit Engine view is a byte-exact payload, even when it is text.
        // Security and final turn-budget enforcement still run in the pipeline.
        crate::server::native_receipts::requested("ctx_read", args)
    }

    fn tool_def(&self) -> Tool {
        super::schema::ctx_read_tool_def()
    }

    fn handle(
        &self,
        args: &Map<String, Value>,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ErrorData> {
        let engine_interface_v1 = engine::interface_v1_requested(args)?;
        engine::validate_v1_request_shape(args, engine_interface_v1)?;
        // #509: ctx_read absorbs multi-file batch reads (supersedes ctx_multi_read).
        // A non-empty `paths` array routes to the one shared batch implementation.
        if args
            .get("paths")
            .and_then(|v| v.as_array())
            .is_some_and(|a| !a.is_empty())
        {
            let result = super::super::ctx_read_batch::batch_read(args, ctx);
            if let Ok(output) = &result {
                record_attribution_result(ctx, "ctx_read batch".to_string(), output);
            }
            return result;
        }

        let path = if let Some(repo) = get_str(args, "repo") {
            let root = crate::core::multi_repo::resolve_repo_root(&repo).ok_or_else(|| {
                let known = crate::core::multi_repo::known_aliases().join(", ");
                let known = if known.is_empty() {
                    "none registered — use ctx_multi_repo add_root".to_string()
                } else {
                    known
                };
                ErrorData::invalid_params(
                    format!("unknown repo alias: {repo} (known: {known})"),
                    None,
                )
            })?;
            let rel = get_str(args, "path").unwrap_or_else(|| ".".to_string());
            crate::core::path_resolve::resolve_tool_path(Some(&root), None, &rel)
                .map_err(|e| ErrorData::invalid_params(e, None))?
        } else {
            require_resolved_path(ctx, args, "path")?
        };

        // "File not found" is the most frequent read failure, and often the
        // path is one mechanical slip from a real file (other case, wrong
        // prefix). When exactly one file fits and the jail admits it, read it
        // and say so instead of failing. Byte-exact reads (the engine view,
        // `raw`) never substitute: their output must be the file and nothing else.
        let byte_exact = engine_interface_v1
            || get_str(args, "mode").as_deref() == Some("raw")
            || args.get("raw").and_then(Value::as_bool) == Some(true);
        let (path, substituted) = match (!byte_exact)
            .then(|| crate::core::path_suggest::unambiguous_match(std::path::Path::new(&path)))
            .flatten()
            .and_then(|found| ctx.resolve_path_sync(&found.to_string_lossy()).ok())
        {
            Some(found) => (found, Some(path)),
            None => (path, None),
        };

        // `symbol=` narrows the read to one definition's lines; the content then
        // flows through the ordinary `lines:`/`anchored:` path.
        let symbol_args;
        let mut symbol_note = None;
        let args = match get_str(args, "symbol").filter(|s| !s.trim().is_empty()) {
            None => args,
            Some(symbol) => {
                if byte_exact {
                    return Err(ErrorData::invalid_params(
                        "symbol= cannot combine with raw or engine_interface",
                        None,
                    ));
                }
                let (rewritten, note) = symbol_request(args, &path, &symbol)?;
                symbol_args = rewritten;
                symbol_note = note;
                &symbol_args
            }
        };

        let result = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.handle_inner(args, ctx, &path, engine_interface_v1)
        })) {
            Ok(result) => result,
            Err(_) => Err(ErrorData::internal_error(
                format!(
                    "ctx_read panicked while processing '{path}'. This is a bug — please report it."
                ),
                None,
            )),
        };
        // A trailing service line: the header stays the first line, as every
        // reader of ctx_read output (and the Claude Code plugin) expects.
        let result = result.map(|mut output| {
            if let Some(requested) = &substituted {
                output.text = format!(
                    "{}\n[lean-ctx: {requested} does not exist — read {path} instead]",
                    output.text.trim_end_matches('\n')
                );
            }
            if let Some(note) = &symbol_note {
                output.text = format!("{}\n{note}", output.text.trim_end_matches('\n'));
            }
            output
        });
        if let Ok(output) = &result {
            record_attribution_result(ctx, format!("ctx_read {path}"), output);
        }
        result
    }

    /// Single reads and batches share one implementation per file,
    /// `handle_inner_prepared`, and that is where the lineage is settled:
    ///
    /// - the requested file is published on every path — fresh reads through
    ///   `tools::ctx_read::read_file_*` (handler thread or
    ///   `TaskSpine::spawn_thread` workers), and cache hits, `[unchanged]`
    ///   stubs and deltas through `publish_requested_source`, bound to what
    ///   is on disk now;
    /// - whenever the reply carries content from anywhere else — graph
    ///   neighbours, cross-source hints, rule files, proactively expanded
    ///   content, the kernel supplement of a batch — the capture is marked
    ///   incomplete there, so that reply's lineage is unknown;
    /// - a source that cannot be bound (another repository via `repo`, an
    ///   image, a path outside the root) marks it incomplete as well.
    ///
    /// So a captured set that is still complete at the end IS the whole set,
    /// for every mode (the modes differ in rendering, not in what they read).
    fn publishes_every_source(&self, _args: Option<&Map<String, Value>>) -> bool {
        true
    }
}

/// Rewrite a `symbol=` request into the window read that serves it. Under an
/// active context policy the lookup is refused: resolving names before
/// admission would answer "does this symbol exist" for a withheld file.
fn symbol_request(
    args: &Map<String, Value>,
    path: &str,
    symbol: &str,
) -> Result<(Map<String, Value>, Option<String>), ErrorData> {
    if crate::core::policy::runtime::is_active() {
        return Err(ErrorData::invalid_params(
            "symbol= is unavailable under an active context policy; use mode=\"lines:N-M\"",
            None,
        ));
    }
    let content = std::fs::read_to_string(path)
        .map_err(|e| ErrorData::invalid_params(format!("cannot read {path}: {e}"), None))?;
    let anchored = get_str(args, "mode").is_some_and(|m| m.starts_with("anchored"));
    let (mode, note) = match super::symbol::resolve(&content, path, symbol, anchored)
        .map_err(|e| ErrorData::invalid_params(e, None))?
    {
        super::symbol::SymbolRead::Window { mode, note } => (mode, note),
        super::symbol::SymbolRead::NotFound(note) => ("signatures".to_string(), Some(note)),
    };
    let mut rewritten = args.clone();
    for key in ["symbol", "start_line", "offset", "limit"] {
        rewritten.remove(key);
    }
    rewritten.insert("mode".into(), Value::String(mode));
    Ok((rewritten, note))
}
