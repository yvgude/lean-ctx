use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{
    McpTool, ToolContext, ToolOutput, get_bool, get_int, get_str, require_resolved_path,
};
use crate::tool_defs::tool_def;

pub struct CtxPatchTool;

impl McpTool for CtxPatchTool {
    fn name(&self) -> &'static str {
        "ctx_patch"
    }

    // Schema diet (#576 pattern): the advertised surface carries only the
    // functional teaching (anchor source, op routing, batch atomicity).
    // Handler-only params stay supported but unadvertised: expected_md5,
    // backup, backup_path, validate_syntax, evidence, diff_max_lines,
    // allow_lossy_utf8 — same hidden-params contract as ctx_edit.
    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_patch",
            "Hash-anchored edit — patch by (line,hash) anchor from ctx_read(anchored)/ctx_search(anchored=true).\n\
             Ops: set_line(line,hash,new_text) | replace_lines(start_line/hash,end_line/hash,new_text) |\n\
             insert_after(line,hash,new_text) | delete(line,hash or start/end range) |\n\
             replace_symbol(name,new_body) | create(new_text) | replace_all(find,replace,dry_run).\n\
             Batch: ops:[{…}]. Stale anchor → CONFLICT with fresh anchors.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "op": { "type": "string", "enum": ["set_line", "replace_lines", "insert_after", "delete", "replace_symbol", "create", "replace_all"] },
                    "line": { "type": "integer" },
                    "hash": { "type": "string" },
                    "start_line": { "type": "integer" },
                    "start_hash": { "type": "string" },
                    "end_line": { "type": "integer" },
                    "end_hash": { "type": "string" },
                    "new_text": { "type": "string" },
                    "name": { "type": "string" },
                    "new_body": { "type": "string" },
                    "find": { "type": "string", "description": "Literal text to find (replace_all)" },
                    "replace": { "type": "string", "description": "Replacement text (replace_all)" },
                    "dry_run": { "type": "boolean", "description": "Preview only, do not write (replace_all)" },
                    "ops": { "type": "array", "items": { "type": "object" } }
                },
                "required": ["path"]
            }),
        )
    }

    fn handle(
        &self,
        args: &Map<String, Value>,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ErrorData> {
        // replace_symbol is a whole-symbol rewrite — delegate to the LSP/IDE-aware
        // ctx_refactor so there is one symbol-edit implementation (epic #1008).
        if crate::tools::ctx_patch::is_replace_symbol(args) {
            return delegate_replace_symbol(args, ctx);
        }

        // #825: replace_all short-circuits before anchor parsing.
        if get_str(args, "op").as_deref() == Some("replace_all") {
            return handle_replace_all(args, ctx);
        }

        let path = require_resolved_path(ctx, args, "path")?;

        let ops = crate::tools::ctx_patch::parse_ops(args)
            .map_err(|e| ErrorData::invalid_params(e, None))?;

        let expected_md5 = get_str(args, "expected_md5");
        let backup = get_bool(args, "backup").unwrap_or(false);
        let backup_path = get_str(args, "backup_path")
            .map(|p| ctx.resolved_paths.get("backup_path").cloned().unwrap_or(p));
        let evidence = get_bool(args, "evidence").unwrap_or(true);
        let diff_max_lines = get_int(args, "diff_max_lines")
            .and_then(|v| usize::try_from(v.max(0)).ok())
            .unwrap_or(200);
        let allow_lossy_utf8 = get_bool(args, "allow_lossy_utf8").unwrap_or(false);
        let validate_syntax = get_bool(args, "validate_syntax").unwrap_or(true);

        let patch_params = crate::tools::ctx_patch::PatchParams {
            path: path.clone(),
            ops,
            expected_md5,
            backup,
            backup_path,
            evidence,
            diff_max_lines,
            allow_lossy_utf8,
            validate_syntax,
        };

        tokio::task::block_in_place(|| {
            let cache_lock = ctx
                .cache
                .as_ref()
                .ok_or_else(|| ErrorData::internal_error("cache not available", None))?;
            let rt = tokio::runtime::Handle::current();

            // Serialize edits to the SAME file via the shared per-file lock (the
            // same registry ctx_edit/ctx_read use), so anchored and str_replace
            // edits of one file never interleave (issue #320). Correctness across
            // processes still rests on the TOCTOU preimage guard + atomic rename.
            let file_lock = crate::core::path_locks::per_file_lock(&path);
            let _file_guard = {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
                loop {
                    if let Ok(guard) = file_lock.try_lock() {
                        break guard;
                    }
                    if std::time::Instant::now() >= deadline {
                        return Err(ErrorData::internal_error(
                            format!(
                                "per-file edit lock contention for {path} — another edit to the same file is in progress, retry in a moment"
                            ),
                            None,
                        ));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
            };

            let last_mode = match rt.block_on(tokio::time::timeout(
                std::time::Duration::from_secs(5),
                cache_lock.read(),
            )) {
                Ok(cache) => cache
                    .get(&path)
                    .map(|e| e.last_mode.clone())
                    .unwrap_or_default(),
                Err(_) => String::new(),
            };

            // Heavy disk I/O — no global cache lock held here.
            let (output, effect) = crate::tools::ctx_patch::run_io(&patch_params, &last_mode);

            crate::tools::ctx_patch::record_outcome(&patch_params, &last_mode, &output, &effect);

            if !matches!(effect, crate::tools::ctx_edit::CacheEffect::None) {
                match rt.block_on(tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    cache_lock.write(),
                )) {
                    Ok(mut cache) => {
                        crate::tools::ctx_edit::apply_cache_effect(&mut cache, &path, effect);
                    }
                    Err(_) => {
                        tracing::warn!(
                            "ctx_patch: cache write-lock timeout (5s) applying post-edit cache effect for {path}"
                        );
                    }
                }
            }

            if let Some(session_lock) = ctx.session.as_ref() {
                let guard = rt.block_on(tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    session_lock.write(),
                ));
                if let Ok(mut session) = guard {
                    session.mark_modified(&path);
                }
            }

            Ok(ToolOutput {
                text: output,
                original_tokens: 0,
                saved_tokens: 0,
                mode: None,
                path: Some(path),
                changed: false,
                shell_outcome: None,
            })
        })
    }
}

/// Handle `op="replace_symbol"` by translating to `ctx_refactor`'s
/// `replace_symbol_body` and dispatching through it. The symbol-resolution,
/// CONFLICT guard and atomic write all live in ctx_refactor — this is a thin,
/// pure-mapping adapter (mapping logic lives in `ctx_patch::symbol`).
fn delegate_replace_symbol(
    args: &Map<String, Value>,
    ctx: &ToolContext,
) -> Result<ToolOutput, ErrorData> {
    let refactor_args = crate::tools::ctx_patch::build_refactor_args(args)
        .map_err(|e| ErrorData::invalid_params(e, None))?;

    // Resolve `path` at the boundary when given (the name route resolves its own
    // path inside ctx_refactor). abs_path is unused by the symbol-edit branch but
    // we mirror ctx_refactor's wrapper to keep jail behaviour identical.
    let has_path = args.get("path").and_then(Value::as_str).is_some();
    let abs_path = if has_path {
        require_resolved_path(ctx, args, "path")?
    } else {
        String::new()
    };

    let args_value = Value::Object(refactor_args);
    let result = crate::tools::ctx_refactor::handle(&args_value, &ctx.project_root, &abs_path);
    let changed = !result.starts_with("ERROR") && !result.starts_with("CONFLICT");

    Ok(ToolOutput {
        text: result,
        original_tokens: 0,
        saved_tokens: 0,
        mode: Some("replace_symbol".to_string()),
        path: get_str(args, "path"),
        changed,
        shell_outcome: None,
    })
}

/// #879: resolve `find`/`replace` for replace_all, failing *closed* on the
/// destructive path. Historically a missing `replace` defaulted to "" — so a
/// typo'd replacement key (`new_string=`/`new_text=` carried over from the other
/// ops) meant "delete every match" and still reported success. Now: reject
/// replacement keys that belong to other ops, and require `replace` to be
/// present. An empty deletion must be opted into explicitly with `replace=""`.
fn resolve_find_replace(args: &Map<String, Value>) -> Result<(String, String), String> {
    let find = get_str(args, "find")
        .filter(|s| !s.is_empty())
        .ok_or("replace_all requires non-empty 'find'")?;

    for foreign in ["new_text", "new_string", "old_string", "new_body"] {
        if args.contains_key(foreign) {
            return Err(format!(
                "replace_all names its replacement 'replace', not '{foreign}' — rename it \
                 (an unrecognized replacement key would silently delete every match)"
            ));
        }
    }

    let replace = args
        .get("replace")
        .and_then(Value::as_str)
        .map(String::from)
        .ok_or(
            "replace_all requires 'replace' (the replacement text); pass replace=\"\" \
             explicitly to delete every match",
        )?;

    Ok((find, replace))
}

#[cfg(test)]
mod replace_all_tests {
    use super::*;
    use serde_json::json;

    fn obj(v: Value) -> Map<String, Value> {
        match v {
            Value::Object(m) => m,
            _ => panic!("expected object"),
        }
    }

    #[test]
    fn resolves_find_and_replace() {
        let (f, r) = resolve_find_replace(&obj(json!({"find": "a", "replace": "b"}))).unwrap();
        assert_eq!((f.as_str(), r.as_str()), ("a", "b"));
    }

    #[test]
    fn explicit_empty_replace_is_a_deletion() {
        let (_f, r) = resolve_find_replace(&obj(json!({"find": "a", "replace": ""}))).unwrap();
        assert_eq!(r, "");
    }

    #[test]
    fn missing_replace_is_rejected_not_silent_delete() {
        let err = resolve_find_replace(&obj(json!({"find": "a"}))).unwrap_err();
        assert!(err.contains("requires 'replace'"), "got: {err}");
    }

    #[test]
    fn foreign_replacement_key_is_rejected() {
        for key in ["new_string", "new_text", "old_string", "new_body"] {
            let err = resolve_find_replace(&obj(json!({"find": "a", key: "b"}))).unwrap_err();
            assert!(err.contains(key), "must name the offending key {key}: {err}");
        }
    }

    #[test]
    fn empty_find_is_rejected() {
        let err = resolve_find_replace(&obj(json!({"find": "", "replace": "b"}))).unwrap_err();
        assert!(err.contains("find"), "got: {err}");
    }
}
/// #825: Bulk literal find-and-replace — no anchors needed.
fn handle_replace_all(
    args: &Map<String, Value>,
    ctx: &ToolContext,
) -> Result<ToolOutput, ErrorData> {
    let path = require_resolved_path(ctx, args, "path")?;
    let (find, replace) =
        resolve_find_replace(args).map_err(|e| ErrorData::invalid_params(e, None))?;
    let dry_run = get_bool(args, "dry_run").unwrap_or(false);

    let content = std::fs::read_to_string(&path)
        .map_err(|e| ErrorData::internal_error(format!("cannot read {path}: {e}"), None))?;

    let count = content.matches(find.as_str()).count();
    if count == 0 {
        return Ok(ToolOutput::simple(format!(
            "No matches for {find:?} in {path}"
        )));
    }

    if dry_run {
        return Ok(ToolOutput::simple(format!(
            "DRY RUN: {count} occurrence(s) of {find:?} would be replaced with {replace:?} in {path}"
        )));
    }

    let file_lock = crate::core::path_locks::per_file_lock(&path);
    let _guard = file_lock
        .lock()
        .map_err(|_| ErrorData::internal_error(format!("lock contention for {path}"), None))?;

    let new_content = content.replace(find.as_str(), &replace);
    crate::config_io::write_atomic(std::path::Path::new(&path), &new_content)
        .map_err(|e| ErrorData::internal_error(format!("write failed: {e}"), None))?;

    if let Some(cache) = ctx.cache.as_ref() {
        let rt = tokio::runtime::Handle::current();
        if let Ok(mut c) = rt.block_on(tokio::time::timeout(
            std::time::Duration::from_secs(2),
            cache.write(),
        )) {
            c.invalidate(&path);
        }
    }

    Ok(ToolOutput::simple(format!(
        "Replaced {count} occurrence(s) of {find:?} with {replace:?} in {path}"
    )))
}
