use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{
    McpTool, ToolContext, ToolOutput, get_bool, get_int, get_str, require_resolved_path,
    tool_execution_error,
};
use crate::tool_defs::tool_def;

pub struct CtxPatchTool;

/// Whether this host draws edit diffs from the archive: Claude Code with the
/// lean-ctx mod in its plugin cache (file-only probe, once per process). Any
/// other host gets the bare one-line receipt.
pub(crate) fn host_draws_diffs(ctx: &ToolContext) -> bool {
    static MOD_PRESENT: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let claude = ctx
        .client_name
        .as_ref()
        .and_then(|name| {
            name.try_read()
                .ok()
                .map(|n| n.to_ascii_lowercase().contains("claude"))
        })
        .unwrap_or(false);
    claude
        && *MOD_PRESENT.get_or_init(|| {
            std::env::var_os("CLAUDE_CONFIG_DIR")
                .map(std::path::PathBuf::from)
                .or_else(|| dirs::home_dir().map(|home| home.join(".claude")))
                .is_some_and(|dir| {
                    !crate::hooks::agents::claude_mod::cached_versions(&dir).is_empty()
                })
        })
}

impl McpTool for CtxPatchTool {
    fn name(&self) -> &'static str {
        "ctx_patch"
    }

    // Schema diet (#576 pattern): the advertised surface carries only the
    // functional teaching (anchor source, op routing, batch atomicity).
    // Handler-only params stay supported but unadvertised: expected_blake3 (alias
    // expected_md5),
    // backup, backup_path, validate_syntax, evidence, diff_max_lines,
    // allow_lossy_utf8 — same hidden-params contract as ctx_edit.
    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_patch",
            "Safe file edit, project-relative paths. replace_unique(path,old_text,new_text): \
             exact unique replacement, needs no anchors. Line ops take line+hash from \
             ctx_read(mode=\"anchored:N-M\"); CONFLICT means re-read. ops[] batch \
             (cross-file, incl. replace_unique/replace_symbol); create and replace_all only \
             as top-level calls.",
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
                },
                // Per-op required params encoded as the source of truth (#1020):
                // a client reading the schema knows which fields an op needs
                // BEFORE calling. Each `if` requires `op` so it stays dormant for
                // batch calls (which carry `op` inside ops[], not at top level).
                // Only ops with UNCONDITIONAL requirements are listed — insert_after
                // (hash optional when line=0) and delete (single vs range) keep their
                // requirements in the parser to avoid rejecting valid calls.
                "allOf": [
                    { "if": { "properties": { "op": { "const": "set_line" } }, "required": ["op"] },
                      "then": { "required": ["op", "line", "hash", "new_text"] } },
                    { "if": { "properties": { "op": { "const": "replace_lines" } }, "required": ["op"] },
                      "then": { "required": ["op", "start_line", "start_hash", "end_line", "end_hash", "new_text"] } },
                    { "if": { "properties": { "op": { "const": "replace_unique" } }, "required": ["op"] },
                      "then": { "required": ["op", "old_text", "new_text"] } },
                    { "if": { "properties": { "op": { "const": "replace_symbol" } }, "required": ["op"] },
                      "then": { "required": ["op", "new_text"] } },
                    { "if": { "properties": { "op": { "const": "create" } }, "required": ["op"] },
                      "then": { "required": ["op", "new_text"] } },
                    { "if": { "properties": { "op": { "const": "replace_all" } }, "required": ["op"] },
                      "then": { "required": ["op", "find", "replace"] } }
                ]
            }),
        )
    }

    fn handle(
        &self,
        args: &Map<String, Value>,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ErrorData> {
        let inferred = infer_replace_unique(args);
        let args = inferred.as_ref().unwrap_or(args);

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

        // #1088: replace_unique/replace_symbol are allowed inside ops[]. Split
        // the batch into runs: each delegated op reuses its single-op write
        // path; consecutive anchored ops keep shared-preimage batch semantics.
        if let Some(arr) = args.get("ops").and_then(Value::as_array) {
            // GH #1433: pre-validate before any ops are applied.
            for (i, v) in arr.iter().enumerate() {
                if let Some(kind) = is_unbatchable_op(v) {
                    return Err(ErrorData::invalid_params(
                        format!(
                            "ops[{i}]: {kind} cannot be batched in ops[] \
                             — send it as a separate top-level ctx_patch call"
                        ),
                        None,
                    ));
                }
            }
            if arr.iter().any(|v| delegated_op_kind(v).is_some()) {
                return handle_mixed_batch(args, arr, ctx);
            }
        }

        handle_anchored(args, ctx)
    }
}

/// Anchored pipeline: a single anchored op or an ops[] batch of anchored edits
/// (grouped per file, batch-atomic per file). Split out of `handle` so the
/// mixed-batch path (#1088) can reuse it per anchored run.
fn handle_anchored(args: &Map<String, Value>, ctx: &ToolContext) -> Result<ToolOutput, ErrorData> {
    if get_bool(args, "dry_run").unwrap_or(false) {
        let path = get_str(args, "path").unwrap_or_default();
        return Ok(ToolOutput::simple(format!(
            "DRY RUN: ctx_patch would apply anchor-based ops to {path}"
        )));
    }

    // #1592: the guard is a BLAKE3 digest; `expected_md5` was a misnomer and
    // stays accepted so existing callers keep working.
    let expected_blake3 =
        get_str(args, "expected_blake3").or_else(|| get_str(args, "expected_md5"));
    let backup = get_bool(args, "backup").unwrap_or(false);
    let backup_path = get_str(args, "backup_path")
        .map(|p| ctx.resolved_paths.get("backup_path").cloned().unwrap_or(p))
        .map(|p| crate::core::workspace_authority::confine(&p, &ctx.project_root, true))
        .transpose()
        .map_err(|error| ErrorData::invalid_params(error, None))?;
    // The diff and digests are opt-in: the default receipt is one line.
    let evidence = get_bool(args, "evidence").unwrap_or(false);
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
    // A one-file receipt already names its file; only a cross-file batch
    // labels each part with its full path.
    let label_parts = groups.len() > 1;
    let diff_artifact = host_draws_diffs(ctx);

    let mut texts = Vec::with_capacity(groups.len());
    for (path, ops) in groups {
        let patch_params = crate::tools::ctx_patch::PatchParams {
            path: path.clone(),
            ops,
            expected_blake3: expected_blake3.clone(),
            backup,
            backup_path: backup_path.clone(),
            evidence,
            diff_artifact,
            diff_max_lines,
            allow_lossy_utf8,
            validate_syntax,
        };
        let output =
            apply_one(ctx, &patch_params).map_err(|error| with_prior_edits(error, &texts))?;
        texts.push(if label_parts {
            format!("[{}]\n{output}", receipt_label(&path, &ctx.project_root))
        } else {
            output
        });
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

/// Repairs the argument shapes models send most often instead of rejecting
/// the call (the pilot's ctx_patch errors were all shape errors, not stale
/// anchors): `ops` sent as a JSON-encoded string, the text keys of the other
/// edit tools (`new_string`/`content` for `create` and anchored ops,
/// `find`/`replace` for `replace_unique`), and an edit that carries old and
/// new text but no `op`, which can only be `replace_unique` (every other op
/// needs line numbers or a symbol). Only key names and the array encoding
/// change; paths are still resolved per op downstream. `None` when nothing
/// needed repairing.
fn infer_replace_unique(args: &Map<String, Value>) -> Option<Map<String, Value>> {
    fn is_text_replacement(obj: &Map<String, Value>) -> bool {
        let text = |keys: [&str; 2]| keys.iter().find_map(|key| obj.get(*key)?.as_str());
        !obj.contains_key("op")
            && text(["old_text", "old_string"]).is_some_and(|old| !old.is_empty())
            && text(["new_text", "new_string"]).is_some()
    }
    /// Copies the first present alias into `key` when `key` is absent.
    fn alias(obj: &mut Map<String, Value>, key: &str, aliases: &[&str]) -> bool {
        if obj.contains_key(key) {
            return false;
        }
        let Some(value) = aliases
            .iter()
            .find_map(|alias| obj.get(*alias).filter(|v| v.is_string()).cloned())
        else {
            return false;
        };
        obj.insert(key.to_string(), value);
        true
    }
    fn repair(obj: &mut Map<String, Value>) -> bool {
        let mut changed = false;
        if is_text_replacement(obj) {
            obj.insert("op".into(), json!("replace_unique"));
            changed = true;
        }
        match obj.get("op").and_then(Value::as_str) {
            Some("replace_unique") => {
                changed |= alias(obj, "old_text", &["old_string", "find"]);
                changed |= alias(obj, "new_text", &["new_string", "replace"]);
            }
            Some("create") => changed |= alias(obj, "new_text", &["new_string", "content"]),
            Some("set_line" | "replace_lines" | "insert_after") => {
                changed |= alias(obj, "new_text", &["new_string"]);
            }
            _ => {}
        }
        changed
    }
    let mut out = args.clone();
    let mut repaired = false;
    if let Some(Value::String(encoded)) = out.get("ops")
        && let Ok(Value::Array(ops)) = serde_json::from_str::<Value>(encoded)
    {
        out.insert("ops".into(), Value::Array(ops));
        repaired = true;
    }
    if !out.contains_key("ops") {
        repaired |= repair(&mut out);
    }
    if let Some(Value::Array(ops)) = out.get_mut("ops") {
        for op in ops.iter_mut().filter_map(Value::as_object_mut) {
            repaired |= repair(op);
        }
    }
    repaired.then_some(out)
}

/// `Some(op)` when a batch op must be dispatched through its own single-op
/// write path rather than the anchored pipeline (#1088).
fn delegated_op_kind(v: &Value) -> Option<&str> {
    v.get("op")
        .and_then(Value::as_str)
        .filter(|k| matches!(*k, "replace_unique" | "replace_symbol"))
}

/// #1088: an ops[] batch containing replace_unique/replace_symbol. Ops apply
/// strictly in the order given, as if issued as separate calls: consecutive
/// anchored ops flush as one shared-preimage batch; each delegated op is
/// dispatched through the same delegate as its top-level form, evaluated
/// against the file state left by the preceding ops.
/// Ops that require their own top-level ctx_patch call and cannot be
/// batched inside ops[].
fn is_unbatchable_op(v: &Value) -> Option<&str> {
    v.get("op")
        .and_then(Value::as_str)
        .filter(|k| matches!(*k, "replace_all" | "create"))
}

fn handle_mixed_batch(
    args: &Map<String, Value>,
    arr: &[Value],
    ctx: &ToolContext,
) -> Result<ToolOutput, ErrorData> {
    // GH #1433: pre-validate the entire batch before applying any op.
    for (i, v) in arr.iter().enumerate() {
        if !v.is_object() {
            return Err(ErrorData::invalid_params(
                format!("ops[{i}] must be an object"),
                None,
            ));
        }
        if let Some(kind) = is_unbatchable_op(v) {
            return Err(ErrorData::invalid_params(
                format!(
                    "ops[{i}]: {kind} cannot be batched in ops[] \
                     — send it as a separate top-level ctx_patch call"
                ),
                None,
            ));
        }
    }

    // Every receipt already names its file ("patched cli.py: …"); a label is
    // only needed to tell files apart, so a one-file batch carries none.
    let top_path = get_str(args, "path");
    let mut targets = std::collections::BTreeSet::new();
    let mut unknown_target = false;
    for v in arr {
        match v
            .get("path")
            .and_then(Value::as_str)
            .or(top_path.as_deref())
        {
            Some(p) => {
                targets.insert(p);
            }
            None => unknown_target = true,
        }
    }
    let label_parts = unknown_target || targets.len() > 1;

    let mut texts: Vec<String> = Vec::new();
    let mut run: Vec<Value> = Vec::new();
    for (i, v) in arr.iter().enumerate() {
        let obj = v.as_object().ok_or_else(|| {
            ErrorData::invalid_params(format!("ops[{i}] must be an object"), None)
        })?;
        let Some(kind) = delegated_op_kind(v) else {
            run.push(v.clone());
            continue;
        };
        flush_anchored_run(args, ctx, &mut run, &mut texts)?;

        let (sub_args, sub_ctx) = delegated_op_call(args, obj, ctx, i, kind)
            .map_err(|error| with_prior_edits(error, &texts))?;
        let out = if kind == "replace_unique" {
            delegate_replace_unique(&sub_args, &sub_ctx)
        } else {
            delegate_replace_symbol(&sub_args, &sub_ctx)
        }
        .map_err(|mut e| {
            e.message = format!("ops[{i}] ({kind}): {}", e.message).into();
            with_prior_edits(e, &texts)
        })?;
        if label_parts {
            let label = sub_ctx.resolved_path("path").map_or_else(
                || kind.to_string(),
                |resolved| receipt_label(resolved, &ctx.project_root),
            );
            texts.push(format!("[{label}]\n{}", out.text));
        } else {
            texts.push(out.text);
        }
    }
    flush_anchored_run(args, ctx, &mut run, &mut texts)?;

    Ok(ToolOutput::simple(texts.join("\n\n")))
}

/// The label of one file's receipt in a cross-file batch: the path relative to
/// the project root (the spelling the model can send back), or the full path
/// for a file outside it. The root is tried as given and canonicalized, since
/// the resolved path may carry the canonical spelling (`/private/var` on macOS,
/// the long name without `\\?\` on Windows, where paths compare case-insensitively).
fn receipt_label(resolved: &str, project_root: &str) -> String {
    let display = crate::core::protocol::display_path(resolved);
    let canonical_root = std::fs::canonicalize(project_root).ok().map(|p| {
        crate::core::pathutil::strip_verbatim(p)
            .to_string_lossy()
            .into_owned()
    });
    for root in std::iter::once(project_root.to_string()).chain(canonical_root) {
        let root = crate::core::protocol::display_path(&root);
        let root = root.trim_end_matches('/');
        let (Some(head), Some(rest)) = (display.get(..root.len()), display.get(root.len()..))
        else {
            continue;
        };
        if root.is_empty() || rest.len() < 2 {
            continue;
        }
        let same_root = if cfg!(windows) {
            head.eq_ignore_ascii_case(root)
        } else {
            head == root
        };
        if same_root && let Some(rel) = rest.strip_prefix('/') {
            return rel.to_string();
        }
    }
    display
}

/// Flush buffered anchored ops through the anchored pipeline as one batch.
fn flush_anchored_run(
    args: &Map<String, Value>,
    ctx: &ToolContext,
    run: &mut Vec<Value>,
    texts: &mut Vec<String>,
) -> Result<(), ErrorData> {
    if run.is_empty() {
        return Ok(());
    }
    let mut sub = args.clone();
    sub.insert("ops".into(), Value::Array(std::mem::take(run)));
    texts.push(
        handle_anchored(&sub, ctx)
            .map_err(|error| with_prior_edits(error, texts))?
            .text,
    );
    Ok(())
}

/// A cross-file or mixed batch is not a transaction across completed runs.
/// Preserve their receipts when a later edit fails, and stop before more writes.
fn with_prior_edits(mut error: ErrorData, texts: &[String]) -> ErrorData {
    if !texts.is_empty() {
        error.code = crate::server::tool_trait::TOOL_EXECUTION_ERROR;
        error.message = format!(
            "{}\nEarlier ops in this batch were already applied:\n{}",
            error.message,
            texts.join("\n\n")
        )
        .into();
    }
    error
}

/// Build the (args, ctx) for one delegated batch op: the op object inherits the
/// batch's top-level `path`/`dry_run` when it doesn't set its own, and the op's
/// path is resolved into a per-op ctx — the dispatch layer only pre-resolves
/// the top-level `path`, which may differ or be absent (#1088).
fn delegated_op_call(
    args: &Map<String, Value>,
    op: &Map<String, Value>,
    ctx: &ToolContext,
    i: usize,
    kind: &str,
) -> Result<(Map<String, Value>, ToolContext), ErrorData> {
    let mut sub = op.clone();
    for key in ["path", "dry_run"] {
        if !sub.contains_key(key)
            && let Some(v) = args.get(key)
        {
            sub.insert(key.to_string(), v.clone());
        }
    }

    let mut sub_ctx = ctx.clone();
    sub_ctx.resolved_paths.remove("path");
    sub_ctx.path_errors.remove("path");
    match get_str(&sub, "path") {
        Some(raw) => {
            let resolved = sub_ctx
                .resolve_path_sync(&raw)
                .map_err(|e| ErrorData::invalid_params(format!("ops[{i}]: path: {e}"), None))?;
            sub_ctx
                .ensure_writable(&resolved)
                .map_err(|e| ErrorData::invalid_params(format!("ops[{i}]: {e}"), None))?;
            sub_ctx.resolved_paths.insert("path".to_string(), resolved);
        }
        // replace_symbol's `name` route resolves its own path inside
        // ctx_refactor; replace_unique always needs one.
        None if kind == "replace_unique" => {
            return Err(ErrorData::invalid_params(
                format!("ops[{i}] needs its own 'path' (no top-level 'path' to fall back to)"),
                None,
            ));
        }
        None => {}
    }
    Ok((sub, sub_ctx))
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

    if get_bool(args, "dry_run").unwrap_or(false) {
        let old_text = get_str(args, "old_text")
            .or_else(|| get_str(args, "old_string"))
            .unwrap_or_default();
        let new_text = get_str(args, "new_text")
            .or_else(|| get_str(args, "new_string"))
            .unwrap_or_default();
        let path = get_str(args, "path").unwrap_or_default();
        return Ok(ToolOutput::simple(format!(
            "DRY RUN: replace_unique would replace {old_text:?} with {new_text:?} in {path}"
        )));
    }

    crate::tools::registered::ctx_edit::handle_edit(&edit_args, ctx, host_draws_diffs(ctx))
}

fn build_unique_edit_args(args: &Map<String, Value>) -> Result<Map<String, Value>, String> {
    let old_text = get_str(args, "old_text")
        .or_else(|| get_str(args, "old_string"))
        .filter(|text| !text.is_empty())
        .ok_or_else(|| {
            "replace_unique requires non-empty old_text (old_string also accepted)".to_string()
        })?;
    let new_text = get_str(args, "new_text")
        .or_else(|| get_str(args, "new_string"))
        .ok_or_else(|| "replace_unique requires new_text (new_string also accepted)".to_string())?;

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
    for key in ["expected_blake3", "expected_md5", "backup_path"] {
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
    {
        let cache_lock = ctx
            .cache
            .as_ref()
            .ok_or_else(|| ErrorData::internal_error("cache not available", None))?;

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

        let last_mode = match crate::server::bounded_lock::read(cache_lock, "ctx_patch cache read")
        {
            Some(cache) => cache
                .get(&path)
                .map(|e| e.last_mode.clone())
                .unwrap_or_default(),
            None => String::new(),
        };

        // Heavy disk I/O — no global cache lock held here.
        let before = std::fs::read(&path).unwrap_or_default();
        let (output, effect) = crate::tools::ctx_patch::run_io(params, &last_mode);
        let written = matches!(effect, crate::tools::ctx_edit::CacheEffect::Invalidate);

        if written {
            let after = std::fs::read(&path).unwrap_or_default();
            super::ctx_edit::observe_mcp_edit(ctx, &path, "ctx_patch", &before, &after);
        }

        crate::tools::ctx_patch::record_outcome(params, &last_mode, &output, &effect);

        if !matches!(effect, crate::tools::ctx_edit::CacheEffect::None) {
            crate::tools::ctx_read::dedup_hook::on_write(&path);
            match crate::server::bounded_lock::write(cache_lock, "ctx_patch cache write") {
                Some(mut cache) => {
                    crate::tools::ctx_edit::apply_cache_effect(&mut cache, &path, effect);
                }
                None => {
                    tracing::warn!(
                        "ctx_patch: cache write-lock timeout applying effect for {path}"
                    );
                }
            }
        }

        if !written {
            return Err(tool_execution_error(output));
        }

        if let Some(session_lock) = ctx.session.as_ref() {
            if let Some(mut session) =
                crate::server::bounded_lock::write(session_lock, "ctx_patch session write")
            {
                session.mark_modified(&path);
            }
        }

        Ok(output)
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

    if get_bool(args, "dry_run").unwrap_or(false) {
        let name = get_str(args, "name").unwrap_or_default();
        let path = get_str(args, "path").unwrap_or_default();
        return Ok(ToolOutput::simple(format!(
            "DRY RUN: replace_symbol would rewrite symbol {name:?} in {path}"
        )));
    }

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
    if !changed {
        return Err(tool_execution_error(result));
    }

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
///
/// The other ops' names are accepted as aliases — `old_string`/`old_text` for
/// `find`, `new_string`/`new_text` for `replace` — because agents carry them
/// over and the mapping is unambiguous. What stays closed is the #879 hole: a
/// replacement must still be *named*, so a missing one is never read as "".
fn resolve_find_replace(args: &Map<String, Value>) -> Result<(String, String), String> {
    let find = aliased(args, "find", &["old_string", "old_text"])?
        .filter(|s| !s.is_empty())
        .ok_or("replace_all requires non-empty 'find' (old_string also accepted)")?;
    let replace = aliased(args, "replace", &["new_string", "new_text"])?.ok_or(
        "replace_all requires 'replace' (the replacement text, new_string also accepted); \
         pass replace=\"\" explicitly to delete every match",
    )?;
    Ok((find, replace))
}

/// The string under `key` or one of its `aliases`. Two spellings that
/// disagree are an error rather than a silent pick.
fn aliased(
    args: &Map<String, Value>,
    key: &str,
    aliases: &[&str],
) -> Result<Option<String>, String> {
    let mut found: Option<(&str, &str)> = None;
    for name in std::iter::once(key).chain(aliases.iter().copied()) {
        let Some(value) = args.get(name).and_then(Value::as_str) else {
            continue;
        };
        match found {
            Some((first, seen)) if seen != value => {
                return Err(format!(
                    "'{first}' and '{name}' name the same field with different values — pass only one"
                ));
            }
            Some(_) => {}
            None => found = Some((name, value)),
        }
    }
    Ok(found.map(|(_, value)| value.to_string()))
}

/// What a bulk replace works from: the ORIGINAL text (the only thing that is
/// ever written back, with the replacement applied) and the match count the
/// caller is told.
///
/// The count is model-visible and is a statement about the file's content, so
/// it is taken from the ADMITTED text, read through the common rooted reader
/// (G01). A source that is withheld is neither counted nor edited. When the
/// admitted text and the original disagree about the matches, the pattern
/// touches content the read path masks: answering with the original's count
/// would confirm a guess of a masked value, and replacing would edit content
/// the caller cannot see — the operation is refused and nothing is written.
/// Admitted or redacted text is never written to disk.
///
/// The original is read through the shared edit boundary (`edit_io`: symlink
/// rejection, size cap, strict UTF-8), and its fingerprint is what the write
/// is later checked against.
fn replace_all_plan(
    path: &str,
    root: &str,
    find: &str,
) -> Result<(crate::tools::edit_io::FilePreimage, usize), String> {
    use crate::tools::admitted_sources::SourceRead;
    let cap = crate::core::limits::max_read_bytes();
    let original = crate::tools::edit_io::read_preimage(std::path::Path::new(path), cap, false)?;
    let mut remaining = cap;
    let admitted =
        match crate::tools::admitted_sources::read(path, root, "ctx_patch", &mut remaining) {
            SourceRead::Admitted(read) => read.content,
            SourceRead::Withheld(reason) | SourceRead::Unavailable(reason) => {
                return Err(format!("cannot read {path}: {reason}"));
            }
        };
    let count = admitted.matches(find).count();
    if count != original.text.matches(find).count() {
        return Err(format!(
            "replace_all refused for {path}: the pattern overlaps content the context gateway \
             masks; nothing was changed"
        ));
    }
    Ok((original, count))
}

/// Writes a bulk replace through the shared edit boundary: the file must still
/// match the fingerprint it was planned from (TOCTOU guard), and the write
/// rejects symlinks and read-only roots and keeps the file's permissions.
fn replace_all_write(
    path: &str,
    pre: &crate::tools::edit_io::FilePreimage,
    new_content: &str,
) -> Result<(), String> {
    let path = std::path::Path::new(path);
    crate::tools::edit_io::ensure_preimage_still_matches(
        path,
        &pre.fp,
        crate::core::limits::max_read_bytes(),
    )?;
    crate::tools::edit_io::write_atomic_bytes_with_permissions(
        path,
        new_content.as_bytes(),
        Some(&pre.permissions),
    )
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

    // The per-file lock is held from the read through the write, so another
    // edit of this file in this process cannot land between them.
    let file_lock = crate::core::path_locks::per_file_lock(&path);
    let _guard = file_lock
        .lock()
        .map_err(|_| ErrorData::internal_error(format!("lock contention for {path}"), None))?;

    let root = crate::tools::admitted_sources::root_in_context(&path, ctx);
    let (pre, count) = replace_all_plan(&path, &root, &find).map_err(tool_execution_error)?;
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

    let new_content = pre.text.replace(find.as_str(), &replace);
    replace_all_write(&path, &pre, &new_content).map_err(tool_execution_error)?;
    super::ctx_edit::observe_mcp_edit(ctx, &path, "ctx_patch", &pre.bytes, new_content.as_bytes());

    crate::tools::ctx_read::dedup_hook::on_write(&path);
    if let Some(cache) = ctx.cache.as_ref() {
        if let Some(mut c) =
            crate::server::bounded_lock::write(cache, "ctx_patch replace_all cache invalidate")
        {
            c.invalidate(&path);
        }
    }
    if let Some(session_lock) = ctx.session.as_ref() {
        if let Some(mut session) =
            crate::server::bounded_lock::write(session_lock, "ctx_patch replace_all session")
        {
            session.mark_modified(&path);
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
    fn other_ops_names_are_aliases() {
        for (old, new) in [("old_string", "new_string"), ("old_text", "new_text")] {
            let (f, r) = resolve_find_replace(&obj(json!({old: "a", new: "b"}))).unwrap();
            assert_eq!((f.as_str(), r.as_str()), ("a", "b"), "{old}/{new}");
        }
    }

    #[test]
    fn alias_never_turns_a_missing_replacement_into_a_delete() {
        let err = resolve_find_replace(&obj(json!({"old_string": "a"}))).unwrap_err();
        assert!(err.contains("requires 'replace'"), "got: {err}");
    }

    #[test]
    fn conflicting_spellings_are_rejected() {
        let err = resolve_find_replace(&obj(
            json!({"find": "a", "old_string": "x", "replace": "b"}),
        ))
        .unwrap_err();
        assert!(
            err.contains("find") && err.contains("old_string"),
            "got: {err}"
        );
        let same = resolve_find_replace(&obj(
            json!({"find": "a", "old_string": "a", "replace": "b"}),
        ));
        assert!(same.is_ok(), "identical spellings agree");
    }

    #[test]
    fn empty_find_is_rejected() {
        let err = resolve_find_replace(&obj(json!({"find": "", "replace": "b"}))).unwrap_err();
        assert!(err.contains("find"), "got: {err}");
    }

    /// G01: `replace_all` read the file with `fs::read_to_string` and reported
    /// how often the pattern occurs — for any file, also one the request may
    /// not read, and also for a value the read path masks. The count now
    /// comes from the admitted text; the text that is written stays the
    /// original.
    #[test]
    fn the_reported_count_comes_from_the_admitted_text_and_the_original_is_what_is_written() {
        use crate::core::policy::runtime::TestPolicyOverride;
        let _data = crate::core::data_dir::isolated_data_dir();
        let policy = crate::core::policy::load(
            "name = \"ctx-patch-test\"\nversion = \"1.0.0\"\ndescription = \"test\"\n\
             [redaction]\ncustomer = \"CUS-[0-9]{4}\"\n\
             [filters]\nclassification = \"block\"\nblocked_labels = [\"CONFIDENTIAL\"]\n",
        )
        .unwrap();
        let project = tempfile::tempdir().unwrap();
        let root = project.path().to_string_lossy().into_owned();
        let file = |name: &str, content: &str| {
            let path = project.path().join(name);
            std::fs::write(&path, content).unwrap();
            path.to_string_lossy().into_owned()
        };
        let original = "let owner = \"CUS-1234\";\nlet mode = old_mode();\nold_mode();\n";
        let orders = file("orders.rs", original);
        let plan = file("plan.rs", "CONFIDENTIAL\nold_mode();\n");
        let planned = |path: &str, find: &str| {
            replace_all_plan(path, &root, find).map(|(pre, count)| (pre.text, count))
        };
        {
            // Control without a pack: original and count as before.
            let _community = TestPolicyOverride::set(None);
            assert_eq!(planned(&orders, "old_mode"), Ok((original.to_string(), 2)));
        }
        let _policy = TestPolicyOverride::set(Some(policy));
        // Unmasked content: the ORIGINAL is returned for the write.
        assert_eq!(planned(&orders, "old_mode"), Ok((original.to_string(), 2)));
        // A guess of the masked value is neither confirmed nor replaced.
        let refused = replace_all_plan(&orders, &root, "CUS-1234").unwrap_err();
        assert!(refused.contains("refused"), "{refused}");
        assert!(!refused.contains("1 occurrence"), "{refused}");
        // A withheld source is not counted at all.
        let withheld = replace_all_plan(&plan, &root, "old_mode").unwrap_err();
        assert!(withheld.contains("withheld"), "{withheld}");
        // No project root: nothing is read.
        assert!(replace_all_plan(&orders, "", "old_mode").is_err());
    }

    /// F4: replace_all reads and writes through the shared edit boundary, so a
    /// symlink is refused on the read and a file that changed after planning
    /// is not overwritten (TOCTOU guard).
    #[test]
    fn replace_all_uses_the_edit_boundary() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let project = tempfile::tempdir().unwrap();
        let root = project.path().to_string_lossy().into_owned();
        let target = project.path().join("a.rs");
        std::fs::write(&target, "old();\nold();\n").unwrap();
        let target_str = target.to_string_lossy().into_owned();

        #[cfg(unix)]
        {
            let link = project.path().join("link.rs");
            std::os::unix::fs::symlink(&target, &link).unwrap();
            let err = replace_all_plan(&link.to_string_lossy(), &root, "old").unwrap_err();
            assert!(err.contains("symlink"), "{err}");
        }

        let (pre, count) = replace_all_plan(&target_str, &root, "old").unwrap();
        assert_eq!(count, 2);
        // A concurrent writer changes the file after the plan was read.
        std::fs::write(&target, "old();\nnewer();\n").unwrap();
        let new_content = pre.text.replace("old", "new");
        let err = replace_all_write(&target_str, &pre, &new_content).unwrap_err();
        assert!(err.contains("TOCTOU"), "{err}");
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "old();\nnewer();\n"
        );

        // Unchanged since the plan: the write lands.
        let (pre, _) = replace_all_plan(&target_str, &root, "old").unwrap();
        let new_content = pre.text.replace("old", "new");
        replace_all_write(&target_str, &pre, &new_content).unwrap();
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "new();\nnewer();\n"
        );
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
        for key in ["expected_blake3", "expected_md5", "backup_path"] {
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

    #[test]
    fn dry_run_replace_unique_does_not_apply() {
        let args = Map::from_iter([
            ("path".into(), json!("a.rs")),
            ("op".into(), json!("replace_unique")),
            ("old_text".into(), json!("old")),
            ("new_text".into(), json!("new")),
            ("dry_run".into(), json!(true)),
        ]);
        let edit_args = build_unique_edit_args(&args).expect("validation passes");
        assert!(
            edit_args.contains_key("old_string"),
            "args mapped correctly"
        );
        assert!(
            args.get("dry_run").and_then(Value::as_bool) == Some(true),
            "dry_run flag preserved in original args"
        );
    }

    #[test]
    fn text_replacement_without_op_is_inferred_as_replace_unique() {
        let top = Map::from_iter([
            ("path".into(), json!("a.rs")),
            ("old_string".into(), json!("old")),
            ("new_string".into(), json!("new")),
        ]);
        let inferred = infer_replace_unique(&top).expect("op inferred");
        assert_eq!(inferred.get("op"), Some(&json!("replace_unique")));

        let batch = Map::from_iter([(
            "ops".into(),
            json!([
                {"path": "a.rs", "old_text": "a", "new_text": "b"},
                {"op": "set_line", "line": 1, "hash": "aa", "new_text": "x"}
            ]),
        )]);
        let inferred = infer_replace_unique(&batch).expect("op inferred in ops[]");
        assert_eq!(inferred["ops"][0]["op"], json!("replace_unique"));
        assert_eq!(inferred["ops"][1]["op"], json!("set_line"));

        // An explicit op, an empty old text or a missing new text stay as sent.
        for unchanged in [
            json!({"op": "set_line", "old_text": "a", "new_text": "b"}),
            json!({"old_text": "", "new_text": "b"}),
            json!({"old_text": "a"}),
        ] {
            assert!(infer_replace_unique(unchanged.as_object().unwrap()).is_none());
        }

        // The shapes behind every ctx_patch error of the pilot benchmark.
        let encoded = json!({"ops": "[{\"op\":\"replace_unique\",\"path\":\"a.rs\",\"old_text\":\"a\",\"new_text\":\"b\"}]"});
        let repaired = infer_replace_unique(encoded.as_object().unwrap()).expect("ops decoded");
        assert_eq!(repaired["ops"][0]["old_text"], json!("a"));
        let create = json!({"op": "create", "path": "n.py", "new_string": "x = 1\n"});
        let repaired = infer_replace_unique(create.as_object().unwrap()).expect("create aliased");
        assert_eq!(repaired["new_text"], json!("x = 1\n"));
        let find_replace =
            json!({"op": "replace_unique", "path": "a.rs", "find": "a", "replace": "b"});
        let repaired = infer_replace_unique(find_replace.as_object().unwrap()).expect("aliased");
        assert_eq!(
            (&repaired["old_text"], &repaired["new_text"]),
            (&json!("a"), &json!("b"))
        );
        // replace_all keeps its own find/replace keys.
        let replace_all = json!({"op": "replace_all", "path": "a.rs", "find": "a", "replace": "b"});
        assert!(infer_replace_unique(replace_all.as_object().unwrap()).is_none());
    }
}
