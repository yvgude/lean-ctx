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
            "Safe file edit. Anchored ops use line+hash from ctx_read(mode=\"anchored\"); \
             CONFLICT means re-read. replace_unique(path,old_text,new_text) is a no-read, \
             exact unique replacement. replace_symbol/create/replace_all and cross-file ops[] supported.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "op": { "type": "string", "enum": ["set_line", "replace_lines", "insert_after", "delete", "replace_unique", "replace_symbol", "create", "replace_all"] },
                    "line": { "type": "integer" },
                    "hash": { "type": "string" },
                    "start_line": { "type": "integer" },
                    "start_hash": { "type": "string" },
                    "end_line": { "type": "integer" },
                    "end_hash": { "type": "string" },
                    "new_text": { "type": "string" },
                    "old_text": { "type": "string" },
                    "name": { "type": "string" },
                    "find": { "type": "string" },
                    "replace": { "type": "string" },
                    "dry_run": { "type": "boolean" },
                    "ops": { "type": "array", "items": { "type": "object" } }
                }
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

        if get_str(args, "op").as_deref() == Some("replace_unique") {
            return delegate_replace_unique(args, ctx);
        }

        // #825: replace_all short-circuits before anchor parsing.
        if get_str(args, "op").as_deref() == Some("replace_all") {
            return handle_replace_all(args, ctx);
        }

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

        // #1005: a batch (`ops[]`) groups its ops by each op's own `path`, so a
        // batch may span files and needs no top-level `path`. A single op still
        // requires the top-level `path`.
        let groups = plan_groups(args, ctx)?;
        validate_cross_file_options(args, groups.len())?;
        let output_path = (groups.len() == 1).then(|| groups[0].0.clone());

        let mut texts = Vec::with_capacity(groups.len());
        for (path, ops) in groups {
            let patch_params = crate::tools::ctx_patch::PatchParams {
                path: path.clone(),
                ops,
                expected_md5: expected_md5.clone(),
                backup,
                backup_path: backup_path.clone(),
                evidence,
                diff_max_lines,
                allow_lossy_utf8,
                validate_syntax,
            };
            let output = apply_one(ctx, &patch_params)?;
            texts.push(format!("[{path}]\n{output}"));
        }

        Ok(ToolOutput {
            text: texts.join("\n\n"),
            original_tokens: 0,
            saved_tokens: 0,
            mode: None,
            path: output_path,
            changed: false,
            shell_outcome: None,
            content_blocks: None,
        })
    }
}

/// One-shot content-anchored edit (#1010). Delegate to ctx_edit's audited
/// unique-replacement path so ambiguity checks, TOCTOU guards, atomic writes,
/// cache invalidation, evidence and session tracking remain single-sourced.
fn delegate_replace_unique(
    args: &Map<String, Value>,
    ctx: &ToolContext,
) -> Result<ToolOutput, ErrorData> {
    let edit_args =
        build_unique_edit_args(args).map_err(|message| ErrorData::invalid_params(message, None))?;
    crate::tools::registered::ctx_edit::CtxEditTool.handle(&edit_args, ctx)
}

fn build_unique_edit_args(args: &Map<String, Value>) -> Result<Map<String, Value>, String> {
    let old_text = get_str(args, "old_text")
        .filter(|text| !text.is_empty())
        .ok_or_else(|| "replace_unique requires non-empty old_text".to_string())?;
    let new_text =
        get_str(args, "new_text").ok_or_else(|| "replace_unique requires new_text".to_string())?;

    let mut edit_args = args.clone();
    edit_args.insert("old_string".into(), Value::String(old_text));
    edit_args.insert("new_string".into(), Value::String(new_text));
    edit_args.insert("replace_all".into(), Value::Bool(false));
    edit_args.remove("old_text");
    edit_args.remove("op");
    Ok(edit_args)
}

/// Options with one top-level value cannot be applied safely to multiple files:
/// one digest cannot describe several preimages, and one explicit backup path
/// would be overwritten by the second file. Reject before any write occurs.
fn validate_cross_file_options(
    args: &Map<String, Value>,
    file_count: usize,
) -> Result<(), ErrorData> {
    if file_count <= 1 {
        return Ok(());
    }
    for key in ["expected_md5", "backup_path"] {
        if args.contains_key(key) {
            return Err(ErrorData::invalid_params(
                format!("cross-file ctx_patch batches do not support top-level '{key}'"),
                None,
            ));
        }
    }
    Ok(())
}

/// Build the per-file work list. A single op targets the top-level `path`
/// (still required). A batch (`ops[]`) groups its ops by each op's own `path`,
/// falling back to the top-level `path`, so a batch may span files without a
/// top-level `path` (#1005).
fn plan_groups(
    args: &Map<String, Value>,
    ctx: &ToolContext,
) -> Result<Vec<(String, Vec<crate::tools::ctx_patch::AnchorOp>)>, ErrorData> {
    let Some(ops_val) = args.get("ops") else {
        let path = require_resolved_path(ctx, args, "path")?;
        let ops = crate::tools::ctx_patch::parse_ops(args)
            .map_err(|e| ErrorData::invalid_params(e, None))?;
        return Ok(vec![(path, ops)]);
    };

    let arr = ops_val
        .as_array()
        .ok_or_else(|| ErrorData::invalid_params("ops must be an array of edit objects", None))?;
    let grouped = group_ops_by_path(arr, get_str(args, "path").as_deref())
        .map_err(|e| ErrorData::invalid_params(e, None))?;

    let mut groups = Vec::with_capacity(grouped.len());
    for (raw_path, op_objs) in grouped {
        let resolved = ctx
            .resolve_path_sync(&raw_path)
            .map_err(|e| ErrorData::invalid_params(format!("path: {e}"), None))?;
        ctx.ensure_writable(&resolved)
            .map_err(|e| ErrorData::invalid_params(e, None))?;
        let sub = Map::from_iter([("ops".to_string(), Value::Array(op_objs))]);
        let ops = crate::tools::ctx_patch::parse_ops(&sub)
            .map_err(|e| ErrorData::invalid_params(e, None))?;
        groups.push((resolved, ops));
    }
    Ok(groups)
}

/// Pure grouping: bucket batch op objects by their own `path` (or the batch's
/// top-level `path` fallback), preserving first-seen file order. Errors if an op
/// names no path and there is no top-level fallback.
fn group_ops_by_path(
    ops: &[Value],
    top_path: Option<&str>,
) -> Result<Vec<(String, Vec<Value>)>, String> {
    if ops.is_empty() {
        return Err("ops[] is empty — provide at least one edit".to_string());
    }
    let mut order: Vec<String> = Vec::new();
    let mut by_path: std::collections::HashMap<String, Vec<Value>> =
        std::collections::HashMap::new();
    for (i, op) in ops.iter().enumerate() {
        let obj = op
            .as_object()
            .ok_or_else(|| format!("ops[{i}] must be an object"))?;
        let raw = obj
            .get("path")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| top_path.map(str::to_string))
            .ok_or_else(|| {
                format!("ops[{i}] needs its own 'path' (no top-level 'path' to fall back to)")
            })?;
        if !by_path.contains_key(&raw) {
            order.push(raw.clone());
        }
        by_path.entry(raw).or_default().push(op.clone());
    }
    Ok(order
        .into_iter()
        .map(|p| {
            let ops = by_path.remove(&p).unwrap_or_default();
            (p, ops)
        })
        .collect())
}

/// Apply one file's anchored patch: acquire the per-file lock, run the I/O off
/// the global cache lock, then fold the resulting cache effect and session mark.
/// Returns the rendered patch/CONFLICT text. Shared by the single-op and the
/// per-file batch paths (#1005).
fn apply_one(
    ctx: &ToolContext,
    params: &crate::tools::ctx_patch::PatchParams,
) -> Result<String, ErrorData> {
    let path = params.path.clone();
    tokio::task::block_in_place(|| {
        let cache_lock = ctx
            .cache
            .as_ref()
            .ok_or_else(|| ErrorData::internal_error("cache not available", None))?;
        let rt = tokio::runtime::Handle::current();

        // Serialize edits to the SAME file via the shared per-file lock (the same
        // registry ctx_edit/ctx_read use), so anchored and str_replace edits of
        // one file never interleave (issue #320). Correctness across processes
        // still rests on the TOCTOU preimage guard + atomic rename.
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
        let (output, effect) = crate::tools::ctx_patch::run_io(params, &last_mode);

        crate::tools::ctx_patch::record_outcome(params, &last_mode, &output, &effect);

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

        Ok(output)
    })
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
        content_blocks: None,
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

    for foreign in ["new_text", "new_string", "old_string"] {
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
        for key in ["new_string", "new_text", "old_string"] {
            let err = resolve_find_replace(&obj(json!({"find": "a", key: "b"}))).unwrap_err();
            assert!(
                err.contains(key),
                "must name the offending key {key}: {err}"
            );
        }
    }

    #[test]
    fn empty_find_is_rejected() {
        let err = resolve_find_replace(&obj(json!({"find": "", "replace": "b"}))).unwrap_err();
        assert!(err.contains("find"), "got: {err}");
    }
}

#[cfg(test)]
mod batch_grouping_tests {
    use super::*;
    use serde_json::json;

    /// #1005: a batch spanning two files groups ops per file, preserving the
    /// first-seen file order — no top-level `path` needed.
    #[test]
    fn groups_ops_across_files_preserving_order() {
        let ops = vec![
            json!({"op":"insert_after","path":"a.go","line":1,"hash":"aa","new_text":"x"}),
            json!({"op":"insert_after","path":"b.go","line":2,"hash":"bb","new_text":"y"}),
            json!({"op":"set_line","path":"a.go","line":3,"hash":"cc","new_text":"z"}),
        ];
        let g = group_ops_by_path(&ops, None).unwrap();
        assert_eq!(g.len(), 2);
        assert_eq!(g[0].0, "a.go");
        assert_eq!(g[0].1.len(), 2);
        assert_eq!(g[1].0, "b.go");
        assert_eq!(g[1].1.len(), 1);
    }

    #[test]
    fn ops_without_path_fall_back_to_top_level() {
        let ops = vec![json!({"op":"set_line","line":1,"hash":"aa","new_text":"x"})];
        let g = group_ops_by_path(&ops, Some("top.go")).unwrap();
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].0, "top.go");
    }

    #[test]
    fn op_without_path_and_no_top_level_is_rejected() {
        let ops = vec![json!({"op":"set_line","line":1,"hash":"aa","new_text":"x"})];
        let err = group_ops_by_path(&ops, None).unwrap_err();
        assert!(err.contains("path"), "got: {err}");
    }

    #[test]
    fn empty_ops_rejected() {
        let err = group_ops_by_path(&[], None).unwrap_err();
        assert!(err.contains("empty"), "got: {err}");
    }

    #[test]
    fn cross_file_rejects_single_value_preimage_and_backup_options() {
        for key in ["expected_md5", "backup_path"] {
            let args = Map::from_iter([(key.to_string(), json!("one-value"))]);
            assert!(validate_cross_file_options(&args, 2).is_err());
            assert!(validate_cross_file_options(&args, 1).is_ok());
        }
    }

    #[test]
    fn replace_unique_maps_to_a_single_safe_ctx_edit_replacement() {
        let args = Map::from_iter([
            ("path".into(), json!("a.rs")),
            ("op".into(), json!("replace_unique")),
            ("old_text".into(), json!("old")),
            ("new_text".into(), json!("new")),
        ]);
        let mapped = build_unique_edit_args(&args).expect("valid mapping");
        assert_eq!(mapped.get("old_string"), Some(&json!("old")));
        assert_eq!(mapped.get("new_string"), Some(&json!("new")));
        assert_eq!(mapped.get("replace_all"), Some(&json!(false)));
        assert!(!mapped.contains_key("op"));
        assert!(!mapped.contains_key("old_text"));
    }

    #[test]
    fn replace_unique_requires_explicit_old_and_new_text() {
        assert!(build_unique_edit_args(&Map::new()).is_err());
        let only_old = Map::from_iter([("old_text".into(), json!("old"))]);
        assert!(build_unique_edit_args(&only_old).is_err());
    }
}
