use lsp_types::{Location, Position};
use serde_json::Value;

use crate::lsp::client::uri_to_file_path;

pub fn handle(args: &Value, project_root: &str, abs_path: &str) -> String {
    let action = args
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("references");

    if matches!(
        action,
        "replace_symbol_body" | "insert_before_symbol" | "insert_after_symbol"
    ) {
        return handle_symbol_edit(action, args, project_root);
    }

    if matches!(action, "rename_preview" | "rename_apply") {
        return handle_rename_refactor(action, args, project_root);
    }

    if matches!(action, "safe_delete_preview" | "safe_delete_apply") {
        return handle_safe_delete_refactor(action, args, project_root);
    }

    if matches!(action, "move_preview" | "move_apply") {
        return handle_move_refactor(action, args, project_root);
    }

    if matches!(action, "inline_preview" | "inline_apply") {
        return handle_inline_refactor(action, args, project_root);
    }

    if action == "reformat" {
        return handle_reformat_refactor(args, project_root);
    }

    let line = args.get("line").and_then(Value::as_u64).unwrap_or(1) as u32;
    let column = args.get("column").and_then(Value::as_u64).unwrap_or(0) as u32;
    let scope = args
        .get("scope")
        .and_then(Value::as_str)
        .unwrap_or("project");

    let uri = match crate::lsp::router::open_file(abs_path, project_root) {
        Ok(u) => u,
        Err(e) => return format!("ERROR: {e}"),
    };

    let position = Position::new(line.saturating_sub(1), column);

    // #475: the IDE symbol `rename` rewrites the target file in place; deny when
    // it sits inside a read-only root (the other read actions below never write).
    if action == "rename"
        && let Some(e) = deny_if_read_only(abs_path)
    {
        return e;
    }

    match action {
        "rename" => handle_rename(args, abs_path, project_root, &uri, position),
        "references" => handle_references(abs_path, project_root, &uri, position, scope),
        "definition" => handle_definition(abs_path, project_root, &uri, position),
        "implementations" => handle_implementations(abs_path, project_root, &uri, position, scope),
        "declaration" => handle_declaration(abs_path, project_root, &uri, position),
        "type_hierarchy" => handle_type_hierarchy(args, abs_path, project_root, &uri, position),
        "symbols_overview" => handle_symbols_overview(abs_path, project_root, &uri),
        "inspections" => handle_inspections(args, abs_path, project_root, &uri),
        _ => format!(
            "ERROR: Unknown action '{action}'. Available: rename, references, definition, \
             implementations, declaration, type_hierarchy, symbols_overview, inspections, \
             replace_symbol_body, insert_before_symbol, insert_after_symbol, \
             rename_preview, rename_apply, safe_delete_preview, safe_delete_apply, \
             move_preview, move_apply, inline_preview, inline_apply, reformat."
        ),
    }
}

/// #475 read-only-roots default-deny for refactor writes. Returns an early
/// `ERROR: …` string when `abs_path` resolves inside a configured read-only
/// root, `None` otherwise. Two-phase `*_preview` actions only read and are
/// never gated; every apply / symbol-edit / reformat / rename path routes its
/// resolved target(s) through this before any IDE or headless write.
fn deny_if_read_only(abs_path: &str) -> Option<String> {
    crate::core::pathjail::enforce_writable(std::path::Path::new(abs_path))
        .err()
        .map(|e| format!("ERROR: {e}"))
}

fn handle_rename(
    args: &Value,
    file_path: &str,
    project_root: &str,
    uri: &lsp_types::Uri,
    position: Position,
) -> String {
    let Some(new_name) = args.get("new_name").and_then(Value::as_str) else {
        return "ERROR: 'new_name' parameter is required for rename.".to_string();
    };

    let result = crate::lsp::router::with_backend(file_path, project_root, |backend, _| {
        backend.rename(uri, position, new_name)
    });

    match result {
        Ok(Some(edit)) => format_workspace_edit(&edit, project_root),
        Ok(None) => "No rename edits returned by language server.".to_string(),
        Err(e) => format!("ERROR: {e}"),
    }
}

fn handle_references(
    file_path: &str,
    project_root: &str,
    uri: &lsp_types::Uri,
    position: Position,
    scope: &str,
) -> String {
    let result = crate::lsp::router::with_backend(file_path, project_root, |backend, _| {
        let locs = backend.references(uri, position, scope)?;
        Ok((locs, backend.last_truncation()))
    });

    match result {
        Ok((locations, meta)) => {
            let mut out = format_locations(&locations, project_root);
            out.push_str(&truncation_note(locations.len(), meta));
            out
        }
        Err(e) => format!("ERROR: {e}"),
    }
}

fn handle_definition(
    file_path: &str,
    project_root: &str,
    uri: &lsp_types::Uri,
    position: Position,
) -> String {
    let result = crate::lsp::router::with_backend(file_path, project_root, |backend, _| {
        backend.definition(uri, position)
    });

    match result {
        Ok(resp) => {
            let locations = match resp {
                lsp_types::GotoDefinitionResponse::Scalar(loc) => vec![loc],
                lsp_types::GotoDefinitionResponse::Array(locs) => locs,
                lsp_types::GotoDefinitionResponse::Link(links) => links
                    .into_iter()
                    .map(|l| Location {
                        uri: l.target_uri,
                        range: l.target_selection_range,
                    })
                    .collect(),
            };
            format_locations(&locations, project_root)
        }
        Err(e) => format!("ERROR: {e}"),
    }
}

fn handle_implementations(
    file_path: &str,
    project_root: &str,
    uri: &lsp_types::Uri,
    position: Position,
    scope: &str,
) -> String {
    let result = crate::lsp::router::with_backend(file_path, project_root, |backend, _| {
        let locs = backend.implementations(uri, position, scope)?;
        Ok((locs, backend.last_truncation()))
    });

    match result {
        Ok((locations, meta)) => {
            let mut out = format_locations(&locations, project_root);
            out.push_str(&truncation_note(locations.len(), meta));
            out
        }
        Err(e) => format!("ERROR: {e}"),
    }
}

fn handle_declaration(
    file_path: &str,
    project_root: &str,
    uri: &lsp_types::Uri,
    position: Position,
) -> String {
    let result = crate::lsp::router::with_backend(file_path, project_root, |backend, _| {
        backend.declaration(uri, position)
    });

    match result {
        Ok(locations) => format_locations(&locations, project_root),
        Err(e) => format!("ERROR: {e}"),
    }
}

use crate::lsp::backend::{
    HierarchyDirection, InspectionDiag, InspectionInfo, SymbolOverviewItem, TypeHierarchyNode,
};

/// A resolved symbol location (project-relative path + 1-based inclusive line span).
#[derive(Debug)]
pub(crate) struct Resolved {
    pub rel_path: String,
    pub start_line: usize,
    pub end_line: usize,
}

/// Apply a resolved edit. IDE-first: a live JetBrains backend (port file +
/// liveness, mirroring router::select_backend) handles it via WriteCommandAction;
/// otherwise the headless local_range_write applies the identical bytes.
pub(crate) fn apply_symbol_edit(
    action: &str,
    project_root: &str,
    edit: &crate::lsp::backend::RangeEdit,
) -> Result<crate::lsp::backend::EditResult, String> {
    use crate::lsp::backend::LspBackend;
    use crate::lsp::port_discovery;

    let mut backend: Box<dyn LspBackend> =
        if let Some(pf) = port_discovery::read_port_file(project_root) {
            if port_discovery::pid_alive(pf.pid) && port_discovery::health_ok(&pf) {
                Box::new(crate::lsp::jetbrains_backend::JetBrainsHttpBackend::new(
                    pf.port,
                    pf.token,
                    project_root.to_string(),
                    pf.pid,
                ))
            } else {
                Box::new(crate::lsp::edit_apply::HeadlessBackend)
            }
        } else {
            Box::new(crate::lsp::edit_apply::HeadlessBackend)
        };

    match action {
        "replace_symbol_body" => backend.replace_symbol_body(edit),
        "insert_before_symbol" => backend.insert_before_symbol(edit),
        "insert_after_symbol" => backend.insert_after_symbol(edit),
        other => Err(format!("INTERNAL: not an edit action: {other}")),
    }
}

/// Leading whitespace of the 1-based `line` in `content` (anchor indentation).
pub(crate) fn anchor_indent(content: &str, line: usize) -> String {
    content
        .lines()
        .nth(line.saturating_sub(1))
        .map(|l| l.chars().take_while(|c| *c == ' ' || *c == '\t').collect())
        .unwrap_or_default()
}

/// Prefix `indent` to the first line of `text` iff that line has no leading
/// whitespace of its own (deterministic; the same Rust computes it for both
/// apply paths, so the wire text is byte-identical).
pub(crate) fn reindent_first_line(text: &str, indent: &str) -> String {
    if text.starts_with(' ') || text.starts_with('\t') || indent.is_empty() {
        return text.to_string();
    }
    format!("{indent}{text}")
}

/// True if symbol `name` denotes a container for type `ancestor`: the bare type
/// itself (struct/enum/inherent `impl Type`) — exact match — or a trait impl,
/// whose indexed name is `<Trait> for <Type>` (see the round-trip note in
/// graph_provider.rs). Generic args on the impl target (`… for Type<T>`) are
/// stripped so `Type/method` still resolves. Language-agnostic: non-Rust
/// container names never contain `" for "`, so only the exact branch applies.
fn container_matches_ancestor(name: &str, ancestor: &str) -> bool {
    if name == ancestor {
        return true;
    }
    match name.rsplit_once(" for ") {
        Some((_, target)) => target.split('<').next().unwrap_or(target).trim() == ancestor,
        None => false,
    }
}

/// Resolve a `name_path` (`Class/method` or bare `name`) to a single symbol via
/// the tree-sitter index (spec v2a §3/§5.3). Disambiguates a qualified path by
/// enclosing-range containment (ancestor symbol's line span contains the leaf's).
pub(crate) fn resolve_name_path(name_path: &str, project_root: &str) -> Result<Resolved, String> {
    use crate::core::graph_provider;
    let open = graph_provider::open_or_build(project_root)
        .ok_or_else(|| "NO_SYMBOL: no symbol index available".to_string())?;
    let gp = &open.provider;

    let segments: Vec<&str> = name_path.split('/').filter(|s| !s.is_empty()).collect();
    let leaf = *segments
        .last()
        .ok_or_else(|| "NO_SYMBOL: empty name_path".to_string())?;

    // Exact-name leaf candidates (case-sensitive — the index may substring-match).
    let mut leaves: Vec<_> = gp
        .find_symbols(leaf, None, None)
        .into_iter()
        .filter(|s| s.name == leaf)
        .collect();

    if segments.len() >= 2 {
        let ancestor = segments[segments.len() - 2];
        let parents: Vec<_> = gp
            .find_symbols(ancestor, None, None)
            .into_iter()
            .filter(|s| container_matches_ancestor(&s.name, ancestor))
            .collect();
        leaves.retain(|leaf_sym| {
            parents.iter().any(|p| {
                p.file == leaf_sym.file
                    && p.start_line <= leaf_sym.start_line
                    && leaf_sym.end_line <= p.end_line
            })
        });
    }

    match leaves.len() {
        0 => Err(format!(
            "NO_SYMBOL: '{name_path}' did not resolve to any indexed symbol"
        )),
        1 => Ok(Resolved {
            rel_path: leaves[0].file.clone(),
            start_line: leaves[0].start_line,
            end_line: leaves[0].end_line,
        }),
        _ => {
            let mut msg = format!(
                "AMBIGUOUS_SYMBOL: '{name_path}' matches {} symbols; qualify it:\n",
                leaves.len()
            );
            for s in leaves.iter().take(10) {
                msg.push_str(&format!(
                    "  {}:{} (L{}-{})\n",
                    s.file, s.name, s.start_line, s.end_line
                ));
            }
            Err(msg)
        }
    }
}

/// Read the current on-disk text covered by a usage's range, jail-checking its
/// path first. Out-of-jail / unreadable / bad range → `Err` (spec §5.4 Multi-File
/// jail: every plugin-reported path is re-checked against `project_root`).
pub(crate) fn usage_range_text(
    project_root: &str,
    u: &crate::lsp::backend::UsageSite,
) -> Result<String, String> {
    let abs = crate::core::path_resolve::resolve_tool_path(Some(project_root), None, &u.path)
        .map_err(|e| format!("CONFLICT: usage path blocked by jail: {e}"))?;
    let content =
        std::fs::read_to_string(&abs).map_err(|e| format!("FILE_NOT_FOUND: {abs}: {e}"))?;
    let s = crate::lsp::edit_apply::offset_of(&content, u.range.start_line, u.range.start_char)?;
    let e = crate::lsp::edit_apply::offset_of(&content, u.range.end_line, u.range.end_char)?;
    if e < s {
        return Err("POSITION_OUT_OF_RANGE: end before start".to_string());
    }
    Ok(content[s..e].to_string())
}

/// Stateless Multi-File integrity guard (spec §5.2). BLAKE3 over the usages
/// canonicalized by sorted `(path, range)` plus each usage's *current* on-disk
/// text. `context` is display-only and intentionally excluded. Re-built in
/// `rename_apply` and compared → mismatch = `CONFLICT` (TOCTOU).
pub(crate) fn plan_hash(
    project_root: &str,
    usages: &[crate::lsp::backend::UsageSite],
) -> Result<String, String> {
    use crate::lsp::backend::TextRange0Based;
    let mut rows: Vec<(String, TextRange0Based, String)> = Vec::with_capacity(usages.len());
    for u in usages {
        let text = usage_range_text(project_root, u)?;
        rows.push((u.path.clone(), u.range, text));
    }
    rows.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then(a.1.start_line.cmp(&b.1.start_line))
            .then(a.1.start_char.cmp(&b.1.start_char))
            .then(a.1.end_line.cmp(&b.1.end_line))
            .then(a.1.end_char.cmp(&b.1.end_char))
    });
    let mut canon = String::new();
    for (path, r, text) in &rows {
        canon.push_str(&format!(
            "{path}|{}:{}-{}:{}|{text}\n",
            r.start_line, r.start_char, r.end_line, r.end_char
        ));
    }
    Ok(crate::core::hasher::hash_hex(canon.as_bytes()))
}

/// Resolve the rename target: `name_path` (primary, reuse v2a) or `path`+`line`
/// (+`end_line`) fallback. Returns `(rel_path, start_line, end_line)` 1-based incl.
fn resolve_rename_target(
    args: &Value,
    project_root: &str,
) -> Result<(String, usize, usize), String> {
    if let Some(np) = args.get("name_path").and_then(Value::as_str) {
        let r = resolve_name_path(np, project_root)?;
        Ok((r.rel_path, r.start_line, r.end_line))
    } else {
        let path = args
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| "provide 'name_path' or 'path'+'line' for rename.".to_string())?;
        let line = args.get("line").and_then(Value::as_u64).unwrap_or(0) as usize;
        let end = args
            .get("end_line")
            .and_then(Value::as_u64)
            .unwrap_or(line as u64) as usize;
        if line == 0 {
            return Err("'line' is required (1-based) when using the path fallback.".to_string());
        }
        Ok((path.to_string(), line, end))
    }
}

/// Deterministic 3-stage Backing-B reachability gate (spec §3.1, v1-§8): live
/// port file + pid alive + `/health` ping. Any miss → `BACKEND_REQUIRED` BEFORE
/// any rename HTTP call. NO fallback to Backing A (no IDE-grade rename there).
fn live_jetbrains_backend(
    project_root: &str,
) -> Result<Box<dyn crate::lsp::backend::LspBackend>, String> {
    use crate::lsp::port_discovery;
    if let Some(pf) = port_discovery::read_port_file(project_root)
        && port_discovery::pid_alive(pf.pid)
        && port_discovery::health_ok(&pf)
    {
        return Ok(Box::new(
            crate::lsp::jetbrains_backend::JetBrainsHttpBackend::new(
                pf.port,
                pf.token,
                project_root.to_string(),
                pf.pid,
            ),
        ));
    }
    Err("BACKEND_REQUIRED: rename requires a running JetBrains IDE \
         (no live port file / health check failed)"
        .to_string())
}

/// Phase 1 renderer: ask Backing B for usages+conflicts, build the stateless
/// plan_hash, and present the blast radius (files, usage count, conflicts).
fn render_rename_preview(
    backend: &mut dyn crate::lsp::backend::LspBackend,
    project_root: &str,
    query: &crate::lsp::backend::RenameQuery,
    new_name: &str,
) -> String {
    let plan = match backend.rename_preview(query) {
        Ok(p) => p,
        Err(e) => return format!("ERROR: {e}"),
    };
    let hash = match plan_hash(project_root, &plan.usages) {
        Ok(h) => h,
        Err(e) => return format!("ERROR: {e}"),
    };
    let mut usage_files: Vec<&str> = plan.usages.iter().map(|u| u.path.as_str()).collect();
    usage_files.sort_unstable();
    usage_files.dedup();
    let mut all_files: Vec<&str> = usage_files.clone();
    all_files.push(query.rel_path.as_str());
    all_files.sort_unstable();
    all_files.dedup();
    let mut out = format!(
        "rename_preview: '{}' → '{new_name}'\n  usages: {}\n  files: {}\n  plan_hash: {hash}\n",
        query.rel_path,
        plan.usages.len(),
        all_files.len(),
    );
    if !plan.conflicts.is_empty() {
        out.push_str(&format!(
            "  conflicts: {} (rename_apply blocks unless force=true)\n",
            plan.conflicts.len()
        ));
        for c in &plan.conflicts {
            out.push_str(&format!("    {}: {}\n", c.path, c.message));
        }
    }
    for f in &usage_files {
        let n = plan.usages.iter().filter(|u| u.path == **f).count();
        out.push_str(&format!("  {f}: {n} usage(s)\n"));
    }
    out
}

/// Phase 2 renderer: re-fetch usages, enforce the plan_hash (TOCTOU) + conflict
/// gates in Rust, then run the IDE Multi-File transaction and evict changed files.
fn render_rename_apply(
    backend: &mut dyn crate::lsp::backend::LspBackend,
    project_root: &str,
    query: &crate::lsp::backend::RenameQuery,
    new_name: &str,
    expected_hash: &str,
    force: bool,
) -> String {
    let plan = match backend.rename_preview(query) {
        Ok(p) => p,
        Err(e) => return format!("ERROR: {e}"),
    };
    let mut pre: Vec<(String, u32, String)> = Vec::with_capacity(plan.usages.len());
    for u in &plan.usages {
        match usage_range_text(project_root, u) {
            Ok(t) => pre.push((u.path.clone(), u.range.start_line + 1, t)),
            Err(e) => return format!("ERROR: {e}"),
        }
    }
    let actual = match plan_hash(project_root, &plan.usages) {
        Ok(h) => h,
        Err(e) => return format!("ERROR: {e}"),
    };
    if actual != expected_hash {
        return format!(
            "ERROR: CONFLICT: plan_hash mismatch (source changed since preview; \
             expected={expected_hash}, actual={actual})"
        );
    }
    if !plan.conflicts.is_empty() && !force {
        return format!(
            "ERROR: CONFLICT: {} refactoring conflict(s); pass force=true to override",
            plan.conflicts.len()
        );
    }

    let apply = crate::lsp::backend::RenameApply {
        abs_path: query.abs_path.clone(),
        rel_path: query.rel_path.clone(),
        target_range: query.target_range,
        new_name: new_name.to_string(),
        force,
    };
    let res = match backend.rename_apply(&apply) {
        Ok(r) => r,
        Err(e) => return format!("ERROR: {e}"),
    };

    // Jail-check + cache-evict each changed file (Multi-File coherence, spec §9).
    for cp in &res.changed_paths {
        match crate::core::path_resolve::resolve_tool_path(Some(project_root), None, cp) {
            Ok(abs) => crate::core::cli_cache::invalidate(&abs),
            Err(e) => return format!("ERROR: CONFLICT: changed path blocked by jail: {e}"),
        }
    }

    let mut out = format!(
        "rename_apply: '{}' → '{new_name}' applied\n  changed files: {}\n  usages: {}\n",
        query.rel_path,
        res.changed_paths.len(),
        pre.len(),
    );
    for (path, line, old) in &pre {
        out.push_str(&format!("  {path}:{line}  \"{old}\" → \"{new_name}\"\n"));
    }
    out
}

/// Entry for the Two-Phase rename actions. Resolves the target (name_path / pos),
/// double-jails, requires a live IDE, then dispatches to the preview/apply renderer.
fn handle_rename_refactor(action: &str, args: &Value, project_root: &str) -> String {
    let Some(new_name) = args.get("new_name").and_then(Value::as_str) else {
        return "ERROR: 'new_name' is required for rename.".to_string();
    };
    if action == "rename_apply" && args.get("plan_hash").and_then(Value::as_str).is_none() {
        return "ERROR: 'plan_hash' is required for rename_apply (run rename_preview first)."
            .to_string();
    }

    let (rel_path, start_line, end_line) = match resolve_rename_target(args, project_root) {
        Ok(t) => t,
        Err(e) => return format!("ERROR: {e}"),
    };
    let abs_path =
        match crate::core::path_resolve::resolve_tool_path(Some(project_root), None, &rel_path) {
            Ok(p) => p,
            Err(e) => return format!("ERROR: path blocked by jail: {e}"),
        };
    // #475: rename_apply rewrites the file in place; rename_preview only reads.
    if action == "rename_apply"
        && let Some(e) = deny_if_read_only(&abs_path)
    {
        return e;
    }
    let content = match std::fs::read_to_string(&abs_path) {
        Ok(c) => c,
        Err(e) => return format!("ERROR: FILE_NOT_FOUND: {abs_path}: {e}"),
    };
    let end_col = content
        .lines()
        .nth(end_line.saturating_sub(1))
        .map_or(0, str::len) as u32;
    let target_range = crate::lsp::backend::TextRange0Based {
        start_line: (start_line - 1) as u32,
        start_char: 0,
        end_line: (end_line - 1) as u32,
        end_char: end_col,
    };
    let search_comments = args
        .get("search_comments")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let search_text_occurrences = args
        .get("search_text_occurrences")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let mut backend = match live_jetbrains_backend(project_root) {
        Ok(b) => b,
        Err(e) => return format!("ERROR: {e}"),
    };

    let query = crate::lsp::backend::RenameQuery {
        abs_path,
        rel_path,
        target_range,
        new_name: new_name.to_string(),
        search_comments,
        search_text_occurrences,
    };

    match action {
        "rename_preview" => render_rename_preview(backend.as_mut(), project_root, &query, new_name),
        "rename_apply" => {
            let expected = args
                .get("plan_hash")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let force = args.get("force").and_then(Value::as_bool).unwrap_or(false);
            render_rename_apply(
                backend.as_mut(),
                project_root,
                &query,
                new_name,
                expected,
                force,
            )
        }
        other => format!("ERROR: INTERNAL: not a rename action: {other}"),
    }
}

/// Phase 1 renderer for safe_delete: ask Backing B for the REMAINING references
/// (blocking usages/conflicts), build the stateless plan_hash, present them.
fn render_safe_delete_preview(
    backend: &mut dyn crate::lsp::backend::LspBackend,
    project_root: &str,
    query: &crate::lsp::backend::SafeDeleteQuery,
) -> String {
    let plan = match backend.safe_delete_preview(query) {
        Ok(p) => p,
        Err(e) => return format!("ERROR: {e}"),
    };
    let hash = match plan_hash(project_root, &plan.usages) {
        Ok(h) => h,
        Err(e) => return format!("ERROR: {e}"),
    };
    let mut files: Vec<&str> = plan.usages.iter().map(|u| u.path.as_str()).collect();
    files.sort_unstable();
    files.dedup();
    let mut out = format!(
        "safe_delete_preview: '{}'\n  blocking usages: {}\n  files: {}\n  plan_hash: {hash}\n",
        query.rel_path,
        plan.usages.len(),
        files.len(),
    );
    if !plan.conflicts.is_empty() {
        out.push_str(&format!(
            "  conflicts: {} (safe_delete_apply blocks unless force=true)\n",
            plan.conflicts.len()
        ));
        for c in &plan.conflicts {
            out.push_str(&format!("    {}: {}\n", c.path, c.message));
        }
    }
    for f in &files {
        let n = plan.usages.iter().filter(|u| u.path == **f).count();
        out.push_str(&format!("  {f}: {n} remaining ref(s)\n"));
    }
    out
}

/// Phase 2 renderer for safe_delete: re-fetch usages, enforce plan_hash (TOCTOU)
/// and a conflict gate (conflict = "reference still exists", spec §5.4) in Rust,
/// then run the IDE delete transaction and evict changed files.
fn render_safe_delete_apply(
    backend: &mut dyn crate::lsp::backend::LspBackend,
    project_root: &str,
    query: &crate::lsp::backend::SafeDeleteQuery,
    expected_hash: &str,
    force: bool,
    propagate: bool,
) -> String {
    let plan = match backend.safe_delete_preview(query) {
        Ok(p) => p,
        Err(e) => return format!("ERROR: {e}"),
    };
    // Gate (a): TOCTOU plan_hash (also jail-checks every usage path).
    let actual = match plan_hash(project_root, &plan.usages) {
        Ok(h) => h,
        Err(e) => return format!("ERROR: {e}"),
    };
    if actual != expected_hash {
        return format!(
            "ERROR: CONFLICT: plan_hash mismatch (source changed since preview; \
             expected={expected_hash}, actual={actual})"
        );
    }
    // Gate (b): remaining references block unless force.
    if !plan.conflicts.is_empty() && !force {
        return format!(
            "ERROR: CONFLICT: {} blocking reference(s) remain; pass force=true to delete anyway",
            plan.conflicts.len()
        );
    }

    let apply = crate::lsp::backend::SafeDeleteApply {
        query: query.clone(),
        force,
        propagate,
    };
    let res = match backend.safe_delete_apply(&apply) {
        Ok(r) => r,
        Err(e) => return format!("ERROR: {e}"),
    };

    // Jail-check + cache-evict each changed file (Multi-File coherence, spec §9).
    for cp in &res.changed_paths {
        match crate::core::path_resolve::resolve_tool_path(Some(project_root), None, cp) {
            Ok(abs) => crate::core::cli_cache::invalidate(&abs),
            Err(e) => return format!("ERROR: CONFLICT: changed path blocked by jail: {e}"),
        }
    }

    format!(
        "safe_delete_apply: '{}' deleted\n  changed files: {}\n",
        query.rel_path,
        res.changed_paths.len(),
    )
}

/// Entry for the Two-Phase safe_delete actions. Resolves the source (name_path /
/// position), jail-checks it, requires a live IDE, then dispatches to the renderer.
/// Two-stage jail only (source + changed_paths) — no new caller-supplied target.
fn handle_safe_delete_refactor(action: &str, args: &Value, project_root: &str) -> String {
    if action == "safe_delete_apply" && args.get("plan_hash").and_then(Value::as_str).is_none() {
        return "ERROR: 'plan_hash' is required for safe_delete_apply (run safe_delete_preview first)."
            .to_string();
    }
    // Resolve source symbol → 1-based inclusive span (reuse v2b resolver).
    let (rel_path, start_line, end_line) = match resolve_rename_target(args, project_root) {
        Ok(t) => t,
        Err(e) => return format!("ERROR: {e}"),
    };
    let abs_path =
        match crate::core::path_resolve::resolve_tool_path(Some(project_root), None, &rel_path) {
            Ok(p) => p,
            Err(e) => return format!("ERROR: path blocked by jail: {e}"),
        };
    // #475: safe_delete_apply removes code from the file; preview only reads.
    if action == "safe_delete_apply"
        && let Some(e) = deny_if_read_only(&abs_path)
    {
        return e;
    }
    let content = match std::fs::read_to_string(&abs_path) {
        Ok(c) => c,
        Err(e) => return format!("ERROR: FILE_NOT_FOUND: {abs_path}: {e}"),
    };
    let end_col = content
        .lines()
        .nth(end_line.saturating_sub(1))
        .map_or(0, str::len) as u32;
    let src_range = crate::lsp::backend::TextRange0Based {
        start_line: (start_line - 1) as u32,
        start_char: 0,
        end_line: (end_line - 1) as u32,
        end_char: end_col,
    };

    let mut backend = match live_jetbrains_backend(project_root) {
        Ok(b) => b,
        Err(e) => return format!("ERROR: {e}"),
    };

    let query = crate::lsp::backend::SafeDeleteQuery {
        abs_path,
        rel_path,
        src_range,
    };

    match action {
        "safe_delete_preview" => render_safe_delete_preview(backend.as_mut(), project_root, &query),
        "safe_delete_apply" => {
            let expected = args
                .get("plan_hash")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let force = args.get("force").and_then(Value::as_bool).unwrap_or(false);
            let propagate = args
                .get("propagate")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            render_safe_delete_apply(
                backend.as_mut(),
                project_root,
                &query,
                expected,
                force,
                propagate,
            )
        }
        other => format!("ERROR: INTERNAL: not a safe_delete action: {other}"),
    }
}

/// Resolve the `move` target (spec §5.3 stage 2): EXACTLY ONE of `target_path` /
/// `target_parent` must be set. `target_path` → jail-checked dir/file →
/// MoveTarget::Path. `target_parent` → resolve_name_path → its file → MoveTarget::
/// Parent. None/both → INVALID_TARGET. Jail violation → INVALID_TARGET. This runs
/// BEFORE any backend call so an out-of-jail target can never reach the plugin.
fn resolve_move_target(
    args: &Value,
    project_root: &str,
) -> Result<crate::lsp::backend::MoveTarget, String> {
    let target_path = args.get("target_path").and_then(Value::as_str);
    let target_parent = args.get("target_parent").and_then(Value::as_str);
    match (target_path, target_parent) {
        (Some(_), Some(_)) | (None, None) => {
            Err("INVALID_TARGET: set exactly one of 'target_path' or 'target_parent'".to_string())
        }
        (Some(tp), None) => {
            let abs = crate::core::path_resolve::resolve_tool_path(Some(project_root), None, tp)
                .map_err(|e| format!("INVALID_TARGET: target_path blocked by jail: {e}"))?;
            Ok(crate::lsp::backend::MoveTarget::Path {
                abs_path: abs,
                rel_path: tp.to_string(),
            })
        }
        (None, Some(parent_np)) => {
            let r = resolve_name_path(parent_np, project_root)?; // NO_SYMBOL / AMBIGUOUS_SYMBOL
            let abs =
                crate::core::path_resolve::resolve_tool_path(Some(project_root), None, &r.rel_path)
                    .map_err(|e| {
                        format!("INVALID_TARGET: target_parent file blocked by jail: {e}")
                    })?;
            let content =
                std::fs::read_to_string(&abs).map_err(|e| format!("FILE_NOT_FOUND: {abs}: {e}"))?;
            let end_col = content
                .lines()
                .nth(r.end_line.saturating_sub(1))
                .map_or(0, str::len) as u32;
            Ok(crate::lsp::backend::MoveTarget::Parent {
                abs_path: abs,
                rel_path: r.rel_path,
                range: crate::lsp::backend::TextRange0Based {
                    start_line: (r.start_line - 1) as u32,
                    start_char: 0,
                    end_line: (r.end_line - 1) as u32,
                    end_char: end_col,
                },
            })
        }
    }
}

/// Phase 1 renderer for move: ask Backing B for usages+conflicts at the new
/// location, build the stateless plan_hash, present the blast radius.
fn render_move_preview(
    backend: &mut dyn crate::lsp::backend::LspBackend,
    project_root: &str,
    query: &crate::lsp::backend::MoveQuery,
) -> String {
    let plan = match backend.move_preview(query) {
        Ok(p) => p,
        Err(e) => return format!("ERROR: {e}"),
    };
    let hash = match plan_hash(project_root, &plan.usages) {
        Ok(h) => h,
        Err(e) => return format!("ERROR: {e}"),
    };
    let target_desc = match &query.target {
        crate::lsp::backend::MoveTarget::Path { rel_path, .. } => format!("→ {rel_path}"),
        crate::lsp::backend::MoveTarget::Parent { rel_path, .. } => {
            format!("→ member of {rel_path}")
        }
    };
    let mut files: Vec<&str> = plan.usages.iter().map(|u| u.path.as_str()).collect();
    files.push(query.rel_path.as_str());
    files.sort_unstable();
    files.dedup();
    let mut out = format!(
        "move_preview: '{}' {target_desc}\n  usages: {}\n  files: {}\n  plan_hash: {hash}\n",
        query.rel_path,
        plan.usages.len(),
        files.len(),
    );
    if !plan.conflicts.is_empty() {
        out.push_str(&format!(
            "  conflicts: {} (move_apply blocks unless force=true)\n",
            plan.conflicts.len()
        ));
        for c in &plan.conflicts {
            out.push_str(&format!("    {}: {}\n", c.path, c.message));
        }
    }
    out
}

/// Phase 2 renderer for move: re-fetch usages, enforce plan_hash (TOCTOU) +
/// conflict gate in Rust, run the IDE Multi-File move, then jail-check + evict
/// every changed path (spec §5.3 stage 3 — includes the NEW destination file).
fn render_move_apply(
    backend: &mut dyn crate::lsp::backend::LspBackend,
    project_root: &str,
    query: &crate::lsp::backend::MoveQuery,
    expected_hash: &str,
    force: bool,
) -> String {
    let plan = match backend.move_preview(query) {
        Ok(p) => p,
        Err(e) => return format!("ERROR: {e}"),
    };
    let actual = match plan_hash(project_root, &plan.usages) {
        Ok(h) => h,
        Err(e) => return format!("ERROR: {e}"),
    };
    if actual != expected_hash {
        return format!(
            "ERROR: CONFLICT: plan_hash mismatch (source changed since preview; \
             expected={expected_hash}, actual={actual})"
        );
    }
    if !plan.conflicts.is_empty() && !force {
        return format!(
            "ERROR: CONFLICT: {} refactoring conflict(s); pass force=true to override",
            plan.conflicts.len()
        );
    }

    let apply = crate::lsp::backend::MoveApply {
        query: query.clone(),
        force,
    };
    let res = match backend.move_apply(&apply) {
        Ok(r) => r,
        Err(e) => return format!("ERROR: {e}"),
    };

    // Stage-3 jail: every changed path (incl. the new destination file) re-checked
    // against project_root BEFORE eviction (spec §5.3).
    for cp in &res.changed_paths {
        match crate::core::path_resolve::resolve_tool_path(Some(project_root), None, cp) {
            Ok(abs) => crate::core::cli_cache::invalidate(&abs),
            Err(e) => return format!("ERROR: CONFLICT: changed path blocked by jail: {e}"),
        }
    }

    format!(
        "move_apply: '{}' applied\n  changed files: {}\n",
        query.rel_path,
        res.changed_paths.len(),
    )
}

/// Entry for the Two-Phase move actions. Resolves the source (stage-1 jail), the
/// target (stage-2 jail via resolve_move_target → INVALID_TARGET on miss/escape),
/// requires a live IDE, then dispatches. Stage-3 jail is inside render_move_apply.
fn handle_move_refactor(action: &str, args: &Value, project_root: &str) -> String {
    if action == "move_apply" && args.get("plan_hash").and_then(Value::as_str).is_none() {
        return "ERROR: 'plan_hash' is required for move_apply (run move_preview first)."
            .to_string();
    }
    // Stage 2 (target) BEFORE any read/backend work, so INVALID_TARGET fires first.
    let target = match resolve_move_target(args, project_root) {
        Ok(t) => t,
        Err(e) => return format!("ERROR: {e}"),
    };
    let (rel_path, start_line, end_line) = match resolve_rename_target(args, project_root) {
        Ok(t) => t,
        Err(e) => return format!("ERROR: {e}"),
    };
    let abs_path =
        match crate::core::path_resolve::resolve_tool_path(Some(project_root), None, &rel_path) {
            Ok(p) => p,
            Err(e) => return format!("ERROR: path blocked by jail: {e}"),
        };
    // #475: move_apply edits the source and writes the destination; deny if
    // EITHER end sits inside a read-only root (preview only reads).
    if action == "move_apply" {
        let dest_abs = match &target {
            crate::lsp::backend::MoveTarget::Path { abs_path, .. }
            | crate::lsp::backend::MoveTarget::Parent { abs_path, .. } => abs_path.as_str(),
        };
        if let Some(e) = deny_if_read_only(&abs_path).or_else(|| deny_if_read_only(dest_abs)) {
            return e;
        }
    }
    let content = match std::fs::read_to_string(&abs_path) {
        Ok(c) => c,
        Err(e) => return format!("ERROR: FILE_NOT_FOUND: {abs_path}: {e}"),
    };
    let end_col = content
        .lines()
        .nth(end_line.saturating_sub(1))
        .map_or(0, str::len) as u32;
    let src_range = crate::lsp::backend::TextRange0Based {
        start_line: (start_line - 1) as u32,
        start_char: 0,
        end_line: (end_line - 1) as u32,
        end_char: end_col,
    };

    let mut backend = match live_jetbrains_backend(project_root) {
        Ok(b) => b,
        Err(e) => return format!("ERROR: {e}"),
    };

    let query = crate::lsp::backend::MoveQuery {
        abs_path,
        rel_path,
        src_range,
        target,
    };

    match action {
        "move_preview" => render_move_preview(backend.as_mut(), project_root, &query),
        "move_apply" => {
            let expected = args
                .get("plan_hash")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let force = args.get("force").and_then(Value::as_bool).unwrap_or(false);
            render_move_apply(backend.as_mut(), project_root, &query, expected, force)
        }
        other => format!("ERROR: INTERNAL: not a move action: {other}"),
    }
}

/// Phase 1 renderer for inline: ask Backing B for substitution sites + conflicts,
/// build the stateless plan_hash, present the blast radius.
fn render_inline_preview(
    backend: &mut dyn crate::lsp::backend::LspBackend,
    project_root: &str,
    query: &crate::lsp::backend::InlineQuery,
) -> String {
    let plan = match backend.inline_preview(query) {
        Ok(p) => p,
        Err(e) => return format!("ERROR: {e}"),
    };
    let hash = match plan_hash(project_root, &plan.usages) {
        Ok(h) => h,
        Err(e) => return format!("ERROR: {e}"),
    };
    let mut files: Vec<&str> = plan.usages.iter().map(|u| u.path.as_str()).collect();
    files.push(query.rel_path.as_str());
    files.sort_unstable();
    files.dedup();
    let mut out = format!(
        "inline_preview: '{}'\n  usages: {}\n  files: {}\n  plan_hash: {hash}\n",
        query.rel_path,
        plan.usages.len(),
        files.len(),
    );
    if !plan.conflicts.is_empty() {
        out.push_str(&format!(
            "  conflicts: {} (inline_apply blocks — no force; hard refusal → UNSUPPORTED)\n",
            plan.conflicts.len()
        ));
        for c in &plan.conflicts {
            out.push_str(&format!("    {}: {}\n", c.path, c.message));
        }
    }
    out
}

/// Phase 2 renderer for inline: re-fetch sites, enforce plan_hash (TOCTOU) and a
/// FORCE-LESS conflict gate (spec §5.2, Entscheidung 4) in Rust, run the IDE
/// inline transaction, then jail-check + evict every changed path.
fn render_inline_apply(
    backend: &mut dyn crate::lsp::backend::LspBackend,
    project_root: &str,
    query: &crate::lsp::backend::InlineQuery,
    expected_hash: &str,
) -> String {
    let plan = match backend.inline_preview(query) {
        Ok(p) => p,
        Err(e) => return format!("ERROR: {e}"),
    };
    let actual = match plan_hash(project_root, &plan.usages) {
        Ok(h) => h,
        Err(e) => return format!("ERROR: {e}"),
    };
    if actual != expected_hash {
        return format!(
            "ERROR: CONFLICT: plan_hash mismatch (source changed since preview; \
             expected={expected_hash}, actual={actual})"
        );
    }
    // FORCE-LESS gate: any conflict is final (no bypass arg exists, spec §5.2).
    if !plan.conflicts.is_empty() {
        return format!(
            "ERROR: CONFLICT: {} inline conflict(s); inline cannot be forced",
            plan.conflicts.len()
        );
    }

    let apply = crate::lsp::backend::InlineApply {
        query: query.clone(),
    };
    let res = match backend.inline_apply(&apply) {
        Ok(r) => r,
        // Hard refusal from IntelliJ (recursive, multiple returns, override) → UNSUPPORTED.
        Err(e) => return format!("ERROR: {e}"),
    };

    for cp in &res.changed_paths {
        match crate::core::path_resolve::resolve_tool_path(Some(project_root), None, cp) {
            Ok(abs) => crate::core::cli_cache::invalidate(&abs),
            Err(e) => return format!("ERROR: CONFLICT: changed path blocked by jail: {e}"),
        }
    }

    format!(
        "inline_apply: '{}' applied\n  changed files: {}\n",
        query.rel_path,
        res.changed_paths.len(),
    )
}

/// Entry for the Two-Phase inline actions. Resolves the source (name_path /
/// position), jail-checks it, requires a live IDE, then dispatches. NO `force`.
fn handle_inline_refactor(action: &str, args: &Value, project_root: &str) -> String {
    if action == "inline_apply" && args.get("plan_hash").and_then(Value::as_str).is_none() {
        return "ERROR: 'plan_hash' is required for inline_apply (run inline_preview first)."
            .to_string();
    }
    let (rel_path, start_line, end_line) = match resolve_rename_target(args, project_root) {
        Ok(t) => t,
        Err(e) => return format!("ERROR: {e}"),
    };
    let abs_path =
        match crate::core::path_resolve::resolve_tool_path(Some(project_root), None, &rel_path) {
            Ok(p) => p,
            Err(e) => return format!("ERROR: path blocked by jail: {e}"),
        };
    // #475: inline_apply rewrites call sites in the file; preview only reads.
    if action == "inline_apply"
        && let Some(e) = deny_if_read_only(&abs_path)
    {
        return e;
    }
    let content = match std::fs::read_to_string(&abs_path) {
        Ok(c) => c,
        Err(e) => return format!("ERROR: FILE_NOT_FOUND: {abs_path}: {e}"),
    };
    let end_col = content
        .lines()
        .nth(end_line.saturating_sub(1))
        .map_or(0, str::len) as u32;
    let src_range = crate::lsp::backend::TextRange0Based {
        start_line: (start_line - 1) as u32,
        start_char: 0,
        end_line: (end_line - 1) as u32,
        end_char: end_col,
    };

    let mut backend = match live_jetbrains_backend(project_root) {
        Ok(b) => b,
        Err(e) => return format!("ERROR: {e}"),
    };

    let keep_definition = args
        .get("keep_definition")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let query = crate::lsp::backend::InlineQuery {
        abs_path,
        rel_path,
        src_range,
        keep_definition,
    };

    match action {
        "inline_preview" => render_inline_preview(backend.as_mut(), project_root, &query),
        "inline_apply" => {
            let expected = args
                .get("plan_hash")
                .and_then(Value::as_str)
                .unwrap_or_default();
            render_inline_apply(backend.as_mut(), project_root, &query, expected)
        }
        other => format!("ERROR: INTERNAL: not an inline action: {other}"),
    }
}

/// Resolve the reformat address (spec §5.3) to (abs_path, rel_path, scope).
/// EXACTLY one address form: name_path → Symbol; path alone → File; path+line
/// (+end_line) → Region. None / contradictory → INVALID_TARGET. Jail-checked here.
fn resolve_reformat_scope(
    args: &Value,
    project_root: &str,
) -> Result<(String, String, crate::lsp::backend::ReformatScope), String> {
    use crate::lsp::backend::{ReformatScope, TextRange0Based};
    let name_path = args.get("name_path").and_then(Value::as_str);
    let path = args.get("path").and_then(Value::as_str);
    let line = args.get("line").and_then(Value::as_u64);

    match (name_path, path) {
        (Some(_), Some(_)) | (None, None) => {
            Err("INVALID_TARGET: set exactly one of 'name_path' or 'path' for reformat".to_string())
        }
        (Some(np), None) => {
            let r = resolve_name_path(np, project_root)?; // NO_SYMBOL / AMBIGUOUS_SYMBOL
            let abs =
                crate::core::path_resolve::resolve_tool_path(Some(project_root), None, &r.rel_path)
                    .map_err(|e| format!("INVALID_TARGET: path blocked by jail: {e}"))?;
            let content =
                std::fs::read_to_string(&abs).map_err(|e| format!("FILE_NOT_FOUND: {abs}: {e}"))?;
            let end_col = content
                .lines()
                .nth(r.end_line.saturating_sub(1))
                .map_or(0, str::len) as u32;
            let range = TextRange0Based {
                start_line: (r.start_line - 1) as u32,
                start_char: 0,
                end_line: (r.end_line - 1) as u32,
                end_char: end_col,
            };
            Ok((abs, r.rel_path, ReformatScope::Symbol { range }))
        }
        (None, Some(p)) => {
            let abs = crate::core::path_resolve::resolve_tool_path(Some(project_root), None, p)
                .map_err(|e| format!("INVALID_TARGET: path blocked by jail: {e}"))?;
            match line {
                None => Ok((abs, p.to_string(), ReformatScope::File)),
                Some(l) => {
                    if l == 0 {
                        return Err(
                            "INVALID_TARGET: 'line' is 1-based (>=1) for a region reformat"
                                .to_string(),
                        );
                    }
                    let end = args.get("end_line").and_then(Value::as_u64).unwrap_or(l);
                    let content = std::fs::read_to_string(&abs)
                        .map_err(|e| format!("FILE_NOT_FOUND: {abs}: {e}"))?;
                    let end_col = content
                        .lines()
                        .nth((end as usize).saturating_sub(1))
                        .map_or(0, str::len) as u32;
                    let range = TextRange0Based {
                        start_line: (l - 1) as u32,
                        start_char: 0,
                        end_line: (end - 1) as u32,
                        end_char: end_col,
                    };
                    Ok((abs, p.to_string(), ReformatScope::Region { range }))
                }
            }
        }
    }
}

/// Single-Phase reformat: resolve address → scope, jail, require live IDE, run the
/// IDE reformat, then Single-File evict (spec §5.3). No plan_hash, no preview.
fn render_reformat(
    backend: &mut dyn crate::lsp::backend::LspBackend,
    project_root: &str,
    query: &crate::lsp::backend::ReformatQuery,
) -> String {
    let res = match backend.reformat(query) {
        Ok(r) => r,
        Err(e) => return format!("ERROR: {e}"),
    };
    for cp in &res.changed_paths {
        match crate::core::path_resolve::resolve_tool_path(Some(project_root), None, cp) {
            Ok(abs) => crate::core::cli_cache::invalidate(&abs),
            Err(e) => return format!("ERROR: INVALID_TARGET: changed path blocked by jail: {e}"),
        }
    }
    format!(
        "reformat: '{}' applied\n  changed files: {}\n",
        query.rel_path,
        res.changed_paths.len(),
    )
}

fn handle_reformat_refactor(args: &Value, project_root: &str) -> String {
    let (abs_path, rel_path, scope) = match resolve_reformat_scope(args, project_root) {
        Ok(t) => t,
        Err(e) => return format!("ERROR: {e}"),
    };
    // #475: reformat rewrites the file; deny inside a read-only root.
    if let Some(e) = deny_if_read_only(&abs_path) {
        return e;
    }
    let mut backend = match live_jetbrains_backend(project_root) {
        Ok(b) => b,
        Err(e) => return format!("ERROR: {e}"),
    };
    let optimize_imports = args
        .get("optimize_imports")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let query = crate::lsp::backend::ReformatQuery {
        abs_path,
        rel_path,
        scope,
        optimize_imports,
    };
    render_reformat(backend.as_mut(), project_root, &query)
}

fn parse_direction(args: &Value) -> HierarchyDirection {
    match args.get("direction").and_then(Value::as_str) {
        Some("subtypes") => HierarchyDirection::Subtypes,
        _ => HierarchyDirection::Supertypes,
    }
}

fn handle_type_hierarchy(
    args: &Value,
    file_path: &str,
    project_root: &str,
    uri: &lsp_types::Uri,
    position: Position,
) -> String {
    let direction = parse_direction(args);
    let result = crate::lsp::router::with_backend(file_path, project_root, |backend, _| {
        let tree = backend.type_hierarchy(uri, position, direction)?;
        Ok((tree, backend.last_truncation()))
    });
    match result {
        Ok((tree, meta)) => {
            let mut out = format_type_hierarchy(&tree);
            if matches!(meta, Some(m) if m.truncated) {
                out.push_str("\n(truncated — depth/node cap reached)\n");
            }
            out
        }
        Err(e) => format!("ERROR: {e}"),
    }
}

fn handle_symbols_overview(file_path: &str, project_root: &str, uri: &lsp_types::Uri) -> String {
    let result = crate::lsp::router::with_backend(file_path, project_root, |backend, _| {
        let items = backend.symbols_overview(uri)?;
        Ok((items, backend.last_truncation()))
    });
    match result {
        Ok((items, meta)) => {
            let mut out = format_symbols_overview(&items);
            out.push_str(&truncation_note(items.len(), meta));
            out
        }
        Err(e) => format!("ERROR: {e}"),
    }
}

fn handle_symbol_edit(action: &str, args: &Value, project_root: &str) -> String {
    let (rel_path, start_line, end_line) = if let Some(np) =
        args.get("name_path").and_then(Value::as_str)
    {
        match resolve_name_path(np, project_root) {
            Ok(r) => (r.rel_path, r.start_line, r.end_line),
            Err(e) => return format!("ERROR: {e}"),
        }
    } else {
        let Some(path) = args.get("path").and_then(Value::as_str) else {
            return "ERROR: provide 'name_path' or 'path'+'line' for symbol edits.".to_string();
        };
        let line = args.get("line").and_then(Value::as_u64).unwrap_or(0) as usize;
        let end = args
            .get("end_line")
            .and_then(Value::as_u64)
            .unwrap_or(line as u64) as usize;
        if line == 0 {
            return "ERROR: 'line' is required (1-based) when using the path fallback.".to_string();
        }
        (path.to_string(), line, end)
    };

    // 2) PathJail on the resolved path (v1 §4.5 seam — critical before writes).
    let abs_path =
        match crate::core::path_resolve::resolve_tool_path(Some(project_root), None, &rel_path) {
            Ok(p) => p,
            Err(e) => return format!("ERROR: path blocked by jail: {e}"),
        };
    // #475: replace/insert symbol edits always write; deny inside a read-only root.
    if let Some(e) = deny_if_read_only(&abs_path) {
        return e;
    }

    let content = match std::fs::read_to_string(&abs_path) {
        Ok(c) => c,
        Err(e) => return format!("ERROR: FILE_NOT_FOUND: {abs_path}: {e}"),
    };

    // 3) Build the canonical range + final wire text per action.
    let expected_hash = args
        .get("expected_hash")
        .and_then(Value::as_str)
        .map(String::from);
    let (range, text) = match action {
        "replace_symbol_body" => {
            let Some(new_body) = args.get("new_body").and_then(Value::as_str) else {
                return "ERROR: 'new_body' is required for replace_symbol_body.".to_string();
            };
            let end_col = content
                .lines()
                .nth(end_line.saturating_sub(1))
                .map_or(0, str::len) as u32;
            (
                crate::lsp::backend::TextRange0Based {
                    start_line: (start_line - 1) as u32,
                    start_char: 0,
                    end_line: (end_line - 1) as u32,
                    end_char: end_col,
                },
                new_body.to_string(),
            )
        }
        "insert_before_symbol" | "insert_after_symbol" => {
            let Some(t) = args.get("text").and_then(Value::as_str) else {
                return format!("ERROR: 'text' is required for {action}.");
            };
            let indent = anchor_indent(&content, start_line);
            let final_text = format!("{}\n", reindent_first_line(t, &indent));
            let insert_line = if action == "insert_before_symbol" {
                (start_line - 1) as u32
            } else {
                end_line as u32
            };
            (
                crate::lsp::backend::TextRange0Based {
                    start_line: insert_line,
                    start_char: 0,
                    end_line: insert_line,
                    end_char: 0,
                },
                final_text,
            )
        }
        other => return format!("ERROR: INTERNAL: not an edit action: {other}"),
    };

    // CONFLICT guard (BLAKE3, same source as headless local_range_write): verify
    // expected_hash against the current on-disk range BEFORE dispatch. This makes
    // the IDE path enforce CONFLICT identically to the headless path (which also
    // re-checks atomically). hash_hex == blake3::hash(...).to_hex().
    if let Some(exp) = &expected_hash {
        let s =
            match crate::lsp::edit_apply::offset_of(&content, range.start_line, range.start_char) {
                Ok(o) => o,
                Err(e) => return format!("ERROR: {e}"),
            };
        let e = match crate::lsp::edit_apply::offset_of(&content, range.end_line, range.end_char) {
            Ok(o) => o,
            Err(e) => return format!("ERROR: {e}"),
        };
        if e < s {
            return "ERROR: POSITION_OUT_OF_RANGE: end before start".to_string();
        }
        let actual = crate::core::hasher::hash_hex(&content.as_bytes()[s..e]);
        if *exp != actual {
            return format!(
                "ERROR: CONFLICT: range hash mismatch (expected={exp}, actual={actual})"
            );
        }
    }

    let edit = crate::lsp::backend::RangeEdit {
        abs_path,
        rel_path,
        range,
        text,
        expected_hash,
    };

    // 4) Dispatch (IDE-first, headless fallback) + format.
    match apply_symbol_edit(action, project_root, &edit) {
        Ok(res) => format_edit_result(action, &res),
        Err(e) => format!("ERROR: {e}"),
    }
}

fn format_edit_result(action: &str, res: &crate::lsp::backend::EditResult) -> String {
    if !res.applied {
        return format!("{action}: not applied.");
    }
    let r = res.new_range;
    let body = if res.diff.is_empty() {
        res.edited_text.clone()
    } else {
        res.diff.clone()
    };
    format!(
        "{action} applied (L{}:{}-L{}:{}):\n{}",
        r.start_line + 1,
        r.start_char,
        r.end_line + 1,
        r.end_char,
        body
    )
}

fn handle_inspections(
    args: &Value,
    file_path: &str,
    project_root: &str,
    uri: &lsp_types::Uri,
) -> String {
    let mode = args.get("mode").and_then(Value::as_str).unwrap_or("run");
    match mode {
        "run" => {
            let result = crate::lsp::router::with_backend(file_path, project_root, |backend, _| {
                let diags = backend.inspections(uri)?;
                Ok((diags, backend.last_truncation()))
            });
            match result {
                Ok((diags, meta)) => {
                    let mut out = format_inspections(&diags);
                    out.push_str(&truncation_note(diags.len(), meta));
                    out
                }
                Err(e) => format!("ERROR: {e}"),
            }
        }
        "list" => {
            let result = crate::lsp::router::with_backend(file_path, project_root, |backend, _| {
                let items = backend.list_inspections()?;
                Ok((items, backend.last_truncation()))
            });
            match result {
                Ok((items, meta)) => {
                    let mut out = format_inspection_list(&items);
                    out.push_str(&truncation_note(items.len(), meta));
                    out
                }
                Err(e) => format!("ERROR: {e}"),
            }
        }
        other => format!("ERROR: Unknown mode '{other}' for inspections. Available: run, list."),
    }
}

fn format_inspections(diags: &[InspectionDiag]) -> String {
    if diags.is_empty() {
        return "No inspection findings.".to_string();
    }
    let mut out = format!("{} finding(s):\n", diags.len());
    for d in diags {
        out.push_str(&format!(
            "  {}:{}  {}  {}\n",
            d.path, d.line, d.severity, d.message
        ));
    }
    out
}

fn format_inspection_list(items: &[InspectionInfo]) -> String {
    if items.is_empty() {
        return "No inspections enabled.".to_string();
    }
    let mut out = format!("{} inspection(s):\n", items.len());
    for i in items {
        out.push_str(&format!("  {}  {}  {}\n", i.id, i.name, i.severity));
    }
    out
}

fn truncation_note(shown: usize, meta: Option<crate::lsp::backend::Truncation>) -> String {
    match meta {
        Some(m) if m.truncated => {
            format!("\n(truncated — showing {shown} of {})\n", m.total)
        }
        _ => String::new(),
    }
}

fn format_type_hierarchy(root: &TypeHierarchyNode) -> String {
    fn walk(node: &TypeHierarchyNode, depth: usize, out: &mut String) {
        let indent = "  ".repeat(depth);
        out.push_str(&format!(
            "{indent}{} ({}:{})\n",
            node.name, node.path, node.line
        ));
        for child in &node.children {
            walk(child, depth + 1, out);
        }
    }
    let mut out = String::new();
    walk(root, 0, &mut out);
    out
}

fn format_symbols_overview(items: &[SymbolOverviewItem]) -> String {
    if items.is_empty() {
        return "No symbols found.".to_string();
    }
    let mut out = format!("{} symbol(s):\n", items.len());
    for item in items {
        out.push_str(&format!(
            "  {} {} (line {})\n",
            item.kind, item.name, item.line
        ));
    }
    out
}

fn format_locations(locations: &[Location], project_root: &str) -> String {
    if locations.is_empty() {
        return "No results found.".to_string();
    }

    let mut out = format!("{} location(s):\n", locations.len());
    for loc in locations {
        let path = uri_to_file_path(&loc.uri).map_or_else(
            || loc.uri.as_str().to_string(),
            |p| {
                p.strip_prefix(project_root)
                    .map(|s| s.strip_prefix('/').unwrap_or(s).to_string())
                    .unwrap_or(p)
            },
        );

        let line = loc.range.start.line + 1;
        let col = loc.range.start.character;
        out.push_str(&format!("  {path}:{line}:{col}\n"));
    }
    out
}

fn format_workspace_edit(edit: &lsp_types::WorkspaceEdit, project_root: &str) -> String {
    let mut out = String::from("Rename edits:\n");
    let mut file_count = 0;
    let mut edit_count = 0;

    if let Some(ref changes) = edit.changes {
        for (uri, edits) in changes {
            let path = uri_to_file_path(uri).map_or_else(
                || uri.as_str().to_string(),
                |p| {
                    p.strip_prefix(project_root)
                        .map(|s| s.strip_prefix('/').unwrap_or(s).to_string())
                        .unwrap_or(p)
                },
            );

            file_count += 1;
            out.push_str(&format!("  {path}: {} edit(s)\n", edits.len()));
            for e in edits {
                edit_count += 1;
                let line = e.range.start.line + 1;
                out.push_str(&format!("    L{line}: -> \"{}\"\n", e.new_text));
            }
        }
    }

    if let Some(ref doc_changes) = edit.document_changes {
        match doc_changes {
            lsp_types::DocumentChanges::Edits(edits) => {
                for text_edit in edits {
                    let path = uri_to_file_path(&text_edit.text_document.uri)
                        .unwrap_or_else(|| text_edit.text_document.uri.as_str().to_string());
                    file_count += 1;
                    let edits_len = text_edit.edits.len();
                    edit_count += edits_len;
                    out.push_str(&format!("  {path}: {edits_len} edit(s)\n"));
                }
            }
            lsp_types::DocumentChanges::Operations(ops) => {
                for op in ops {
                    if let lsp_types::DocumentChangeOperation::Edit(text_edit) = op {
                        let path = uri_to_file_path(&text_edit.text_document.uri)
                            .unwrap_or_else(|| text_edit.text_document.uri.as_str().to_string());
                        file_count += 1;
                        let edits_len = text_edit.edits.len();
                        edit_count += edits_len;
                        out.push_str(&format!("  {path}: {edits_len} edit(s)\n"));
                    }
                }
            }
        }
    }

    out.push_str(&format!(
        "\nTotal: {edit_count} edit(s) across {file_count} file(s)."
    ));
    out
}

#[cfg(test)]
#[path = "ctx_refactor_tests.rs"]
mod tests;
