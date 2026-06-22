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
mod tests {
    use serde_json::json;

    /// §4.5: inner handle MUST use the (already jailed) abs_path it is given,
    /// never re-derive a path from raw args. A raw "../escape.rs" must never
    /// reach the filesystem layer; only the provided abs_path does.
    #[test]
    fn inner_handle_uses_provided_abs_path_not_raw_args() {
        let args = json!({"action": "references", "path": "../escape.rs", "line": 1, "column": 0});
        let out = super::handle(&args, "/proj", "/proj/jailed.rs");
        // open_file fails reading the (nonexistent) jailed file → error names abs_path.
        assert!(out.contains("/proj/jailed.rs"), "abs_path not used: {out}");
        assert!(
            !out.contains("../escape.rs"),
            "raw path leaked to fs layer: {out}"
        );
    }

    /// `declaration` is a known action: the unknown-action arm must not fire for it,
    /// and its help text now advertises `declaration`.
    ///
    /// NOTE (adaptation): the real `handle` opens the file *before* the action
    /// match, so reaching the unknown-action help arm requires a backend. We seed
    /// a no-op stub backend for `rust` and point at a real temp `.rs` file so
    /// dispatch deterministically reaches the help text, offline, without
    /// starting rust-analyzer.
    #[test]
    fn unknown_action_help_lists_declaration() {
        struct StubBackend;
        impl crate::lsp::backend::LspBackend for StubBackend {
            fn open_file(
                &mut self,
                _uri: &lsp_types::Uri,
                _language_id: &str,
                _text: &str,
            ) -> Result<(), String> {
                Ok(())
            }
            fn references(
                &mut self,
                _uri: &lsp_types::Uri,
                _position: lsp_types::Position,
                _scope: &str,
            ) -> Result<Vec<lsp_types::Location>, String> {
                Ok(vec![])
            }
            fn definition(
                &mut self,
                _uri: &lsp_types::Uri,
                _position: lsp_types::Position,
            ) -> Result<lsp_types::GotoDefinitionResponse, String> {
                Ok(lsp_types::GotoDefinitionResponse::Array(vec![]))
            }
            fn implementations(
                &mut self,
                _uri: &lsp_types::Uri,
                _position: lsp_types::Position,
                _scope: &str,
            ) -> Result<Vec<lsp_types::Location>, String> {
                Ok(vec![])
            }
            fn rename(
                &mut self,
                _uri: &lsp_types::Uri,
                _position: lsp_types::Position,
                _new_name: &str,
            ) -> Result<Option<lsp_types::WorkspaceEdit>, String> {
                Ok(None)
            }
        }

        let dir = std::env::temp_dir().join(format!("leanctx_r1_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("x.rs");
        std::fs::write(&file, "fn x() {}\n").unwrap();
        let root = dir.to_string_lossy().to_string();
        let abs = file.to_string_lossy().to_string();

        crate::lsp::router::seed_stub_backend("rust", Box::new(StubBackend));

        let args = json!({"action": "definitely_bogus", "path": "x.rs", "line": 1});
        let out = super::handle(&args, &root, &abs);
        assert!(
            out.contains("declaration"),
            "help text missing declaration: {out}"
        );
        assert!(
            out.contains("inspections"),
            "help text missing inspections: {out}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn type_hierarchy_formats_indented_tree() {
        use crate::lsp::backend::{
            HierarchyDirection, LspBackend, SymbolOverviewItem, TypeHierarchyNode,
        };

        struct HierBackend;
        impl LspBackend for HierBackend {
            fn open_file(&mut self, _u: &lsp_types::Uri, _l: &str, _t: &str) -> Result<(), String> {
                Ok(())
            }
            fn references(
                &mut self,
                _u: &lsp_types::Uri,
                _p: lsp_types::Position,
                _s: &str,
            ) -> Result<Vec<lsp_types::Location>, String> {
                Ok(vec![])
            }
            fn definition(
                &mut self,
                _u: &lsp_types::Uri,
                _p: lsp_types::Position,
            ) -> Result<lsp_types::GotoDefinitionResponse, String> {
                Ok(lsp_types::GotoDefinitionResponse::Array(vec![]))
            }
            fn implementations(
                &mut self,
                _u: &lsp_types::Uri,
                _p: lsp_types::Position,
                _s: &str,
            ) -> Result<Vec<lsp_types::Location>, String> {
                Ok(vec![])
            }
            fn rename(
                &mut self,
                _u: &lsp_types::Uri,
                _p: lsp_types::Position,
                _n: &str,
            ) -> Result<Option<lsp_types::WorkspaceEdit>, String> {
                Ok(None)
            }
            fn type_hierarchy(
                &mut self,
                _u: &lsp_types::Uri,
                _p: lsp_types::Position,
                dir: HierarchyDirection,
            ) -> Result<TypeHierarchyNode, String> {
                assert_eq!(dir, HierarchyDirection::Subtypes);
                Ok(TypeHierarchyNode {
                    name: "Animal".into(),
                    path: "A.kt".into(),
                    line: 1,
                    children: vec![TypeHierarchyNode {
                        name: "Dog".into(),
                        path: "A.kt".into(),
                        line: 2,
                        children: vec![],
                    }],
                })
            }
            fn symbols_overview(
                &mut self,
                _u: &lsp_types::Uri,
            ) -> Result<Vec<SymbolOverviewItem>, String> {
                Ok(vec![SymbolOverviewItem {
                    name: "Animal".into(),
                    kind: "interface".into(),
                    line: 1,
                }])
            }
        }

        let tree = HierBackend
            .type_hierarchy(
                &crate::lsp::client::file_path_to_uri("/p/A.kt").unwrap(),
                lsp_types::Position::new(0, 0),
                HierarchyDirection::Subtypes,
            )
            .unwrap();
        let out = super::format_type_hierarchy(&tree);
        assert!(out.contains("Animal (A.kt:1)"), "{out}");
        assert!(out.contains("  Dog (A.kt:2)"), "{out}"); // child indented

        let items = HierBackend
            .symbols_overview(&crate::lsp::client::file_path_to_uri("/p/A.kt").unwrap())
            .unwrap();
        let out2 = super::format_symbols_overview(&items);
        assert!(out2.contains("interface Animal (line 1)"), "{out2}");
    }

    #[test]
    fn parse_direction_defaults_to_supertypes() {
        use crate::lsp::backend::HierarchyDirection;
        assert_eq!(
            super::parse_direction(&json!({})),
            HierarchyDirection::Supertypes
        );
        assert_eq!(
            super::parse_direction(&json!({"direction": "subtypes"})),
            HierarchyDirection::Subtypes
        );
        assert_eq!(
            super::parse_direction(&json!({"direction": "supertypes"})),
            HierarchyDirection::Supertypes
        );
    }

    #[test]
    fn resolve_name_path_unique_class() {
        let _lock = crate::core::data_dir::test_env_lock();
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("data");
        std::fs::create_dir_all(&data).unwrap();
        crate::test_env::set_var("LEAN_CTX_DATA_DIR", data.to_string_lossy().to_string());

        let proj = tmp.path().join("proj");
        std::fs::create_dir_all(proj.join("src")).unwrap();
        std::fs::write(
            proj.join("Cargo.toml"),
            "[package]\nname=\"x\"\nversion=\"0.0.0\"\n",
        )
        .unwrap();
        std::fs::write(
            proj.join("src/lib.rs"),
            "pub struct UniqueZqWidget { pub a: u8 }\n",
        )
        .unwrap();
        let root = proj.to_string_lossy().to_string();

        let r = super::resolve_name_path("UniqueZqWidget", &root).expect("unique resolution");
        assert!(r.rel_path.ends_with("lib.rs"), "got: {}", r.rel_path);
        assert!(r.end_line >= r.start_line && r.start_line > 0);

        crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
    }

    #[test]
    fn resolve_name_path_unknown_is_no_symbol() {
        let _lock = crate::core::data_dir::test_env_lock();
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("data");
        std::fs::create_dir_all(&data).unwrap();
        crate::test_env::set_var("LEAN_CTX_DATA_DIR", data.to_string_lossy().to_string());

        let proj = tmp.path().join("proj");
        std::fs::create_dir_all(proj.join("src")).unwrap();
        std::fs::write(
            proj.join("Cargo.toml"),
            "[package]\nname=\"x\"\nversion=\"0.0.0\"\n",
        )
        .unwrap();
        std::fs::write(
            proj.join("src/lib.rs"),
            "pub struct UniqueZqWidget { pub a: u8 }\n",
        )
        .unwrap();
        let root = proj.to_string_lossy().to_string();

        let err = super::resolve_name_path("ZzzNoSuchSymbol123", &root).unwrap_err();
        assert!(err.starts_with("NO_SYMBOL"), "got: {err}");

        crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
    }

    #[test]
    fn resolve_name_path_trait_impl_method() {
        let _lock = crate::core::data_dir::test_env_lock();
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("data");
        std::fs::create_dir_all(&data).unwrap();
        crate::test_env::set_var("LEAN_CTX_DATA_DIR", data.to_string_lossy().to_string());

        let proj = tmp.path().join("proj");
        std::fs::create_dir_all(proj.join("src")).unwrap();
        std::fs::write(
            proj.join("Cargo.toml"),
            "[package]\nname=\"x\"\nversion=\"0.0.0\"\n",
        )
        .unwrap();
        std::fs::write(
            proj.join("src/lib.rs"),
            "pub struct RenderBridge;\n\
             pub trait Exec { fn execute(&self); }\n\
             impl Exec for RenderBridge {\n\
             \x20   fn execute(&self) { let _ = 1; }\n\
             }\n",
        )
        .unwrap();
        let root = proj.to_string_lossy().to_string();

        let r = super::resolve_name_path("RenderBridge/execute", &root)
            .expect("trait-impl method should resolve");
        assert!(r.rel_path.ends_with("lib.rs"), "got: {}", r.rel_path);
        // Muss auf den Impl-Methoden-Body zeigen (Zeile >= 3), nicht auf das
        // struct (Z. 1) oder die Trait-Deklaration (Z. 2).
        assert!(
            r.start_line >= 3,
            "should point at impl method, got L{}",
            r.start_line
        );
        assert!(r.end_line >= r.start_line && r.start_line > 0);

        crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
    }

    #[test]
    fn container_matches_ancestor_cases() {
        use super::container_matches_ancestor as m;
        assert!(m("RenderBridge", "RenderBridge"));
        assert!(m("Exec for RenderBridge", "RenderBridge"));
        assert!(m("Exec for RenderBridge<Wasm>", "RenderBridge"));
        assert!(!m("OtherType", "RenderBridge"));
        assert!(!m("Exec for Other", "RenderBridge"));
    }

    #[test]
    fn resolve_name_path_inherent_impl_method() {
        let _lock = crate::core::data_dir::test_env_lock();
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("data");
        std::fs::create_dir_all(&data).unwrap();
        crate::test_env::set_var("LEAN_CTX_DATA_DIR", data.to_string_lossy().to_string());

        let proj = tmp.path().join("proj");
        std::fs::create_dir_all(proj.join("src")).unwrap();
        std::fs::write(
            proj.join("Cargo.toml"),
            "[package]\nname=\"x\"\nversion=\"0.0.0\"\n",
        )
        .unwrap();
        std::fs::write(
            proj.join("src/lib.rs"),
            "pub struct RenderBridge;\n\
             impl RenderBridge {\n\
             \x20   pub fn run(&self) { let _ = 1; }\n\
             }\n",
        )
        .unwrap();
        let root = proj.to_string_lossy().to_string();

        let r = super::resolve_name_path("RenderBridge/run", &root)
            .expect("inherent-impl method should still resolve");
        assert!(r.rel_path.ends_with("lib.rs"), "got: {}", r.rel_path);
        assert!(r.start_line >= 2 && r.end_line >= r.start_line);

        crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
    }

    #[test]
    fn resolve_name_path_ambiguous_trait_impls() {
        let _lock = crate::core::data_dir::test_env_lock();
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("data");
        std::fs::create_dir_all(&data).unwrap();
        crate::test_env::set_var("LEAN_CTX_DATA_DIR", data.to_string_lossy().to_string());

        let proj = tmp.path().join("proj");
        std::fs::create_dir_all(proj.join("src")).unwrap();
        std::fs::write(
            proj.join("Cargo.toml"),
            "[package]\nname=\"x\"\nversion=\"0.0.0\"\n",
        )
        .unwrap();
        std::fs::write(
            proj.join("src/lib.rs"),
            "pub struct RenderBridge;\n\
             pub trait A { fn execute(&self); }\n\
             pub trait B { fn execute(&self); }\n\
             pub mod a;\n\
             pub mod b;\n",
        )
        .unwrap();
        // a.rs: impl A for RenderBridge — plain targets, multi-line body so fn is indexed
        std::fs::write(
            proj.join("src/a.rs"),
            "impl A for RenderBridge {\n\
             \x20   fn execute(&self) { let _ = 1; }\n\
             }\n",
        )
        .unwrap();
        // b.rs: impl B for RenderBridge — plain targets, multi-line body so fn is indexed
        std::fs::write(
            proj.join("src/b.rs"),
            "impl B for RenderBridge {\n\
             \x20   fn execute(&self) { let _ = 1; }\n\
             }\n",
        )
        .unwrap();
        let root = proj.to_string_lossy().to_string();

        // "RenderBridge/execute": two segments → container_matches_ancestor runs for each hit.
        // "A for RenderBridge" and "B for RenderBridge" both match ancestor "RenderBridge",
        // producing two distinct hits (src/a.rs and src/b.rs) → AMBIGUOUS_SYMBOL.
        let err = super::resolve_name_path("RenderBridge/execute", &root)
            .expect_err("two trait impls (cross-file) with same method must be ambiguous");
        assert!(err.starts_with("AMBIGUOUS_SYMBOL"), "got: {err}");

        crate::test_env::remove_var("LEAN_CTX_DATA_DIR");
    }

    #[test]
    fn anchor_indent_reads_leading_whitespace() {
        let content = "class A {\n    fun b() {}\n}\n";
        assert_eq!(super::anchor_indent(content, 2), "    "); // line 2 (1-based) → 4 spaces
        assert_eq!(super::anchor_indent(content, 1), ""); // line 1 → none
    }

    #[test]
    fn reindent_prefixes_first_line_only() {
        assert_eq!(
            super::reindent_first_line("fun x() {}", "    "),
            "    fun x() {}"
        );
        // Already-indented text is left untouched.
        assert_eq!(
            super::reindent_first_line("    fun x()", "    "),
            "    fun x()"
        );
    }

    #[test]
    fn apply_symbol_edit_headless_replaces_range() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Foo.txt"), "aaa\nBODY\nccc\n").unwrap();
        let abs = dir.path().join("Foo.txt").to_string_lossy().to_string();
        let edit = crate::lsp::backend::RangeEdit {
            abs_path: abs.clone(),
            rel_path: "Foo.txt".into(),
            range: crate::lsp::backend::TextRange0Based {
                start_line: 1,
                start_char: 0,
                end_line: 1,
                end_char: 4,
            },
            text: "NEW".into(),
            expected_hash: None,
        };
        // No port file under this temp dir → headless apply.
        let res =
            super::apply_symbol_edit("replace_symbol_body", dir.path().to_str().unwrap(), &edit)
                .unwrap();
        assert!(res.applied);
        assert_eq!(std::fs::read_to_string(&abs).unwrap(), "aaa\nNEW\nccc\n");
    }

    #[test]
    fn handle_replace_symbol_body_via_position_fallback() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn old() {\n  1\n}\n").unwrap();
        let args = serde_json::json!({
            "action": "replace_symbol_body",
            "path": "a.rs",
            "line": 1,
            "end_line": 3,
            "new_body": "fn new() {\n  2\n}"
        });
        let out = super::handle(&args, dir.path().to_str().unwrap(), "");
        assert!(out.contains("replace_symbol_body applied"), "got: {out}");
        let after = std::fs::read_to_string(dir.path().join("a.rs")).unwrap();
        assert!(after.contains("fn new()"), "file: {after}");
    }

    #[test]
    fn handle_replace_symbol_body_conflict_on_stale_hash() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn old() {\n  1\n}\n").unwrap();
        // Range = full file lines 1..=3; old content = the whole file text.
        let stale = serde_json::json!({
            "action": "replace_symbol_body",
            "path": "a.rs", "line": 1, "end_line": 3,
            "new_body": "fn new() {\n  2\n}",
            "expected_hash": "deadbeefnotahash"
        });
        let out = super::handle(&stale, dir.path().to_str().unwrap(), "");
        assert!(out.contains("CONFLICT"), "got: {out}");
        // file unchanged
        assert!(
            std::fs::read_to_string(dir.path().join("a.rs"))
                .unwrap()
                .contains("fn old()")
        );
    }

    #[test]
    fn references_output_surfaces_truncation_note() {
        use lsp_types::Position;
        struct TruncBackend;
        impl crate::lsp::backend::LspBackend for TruncBackend {
            fn open_file(&mut self, _u: &lsp_types::Uri, _l: &str, _t: &str) -> Result<(), String> {
                Ok(())
            }
            fn references(
                &mut self,
                _u: &lsp_types::Uri,
                _p: lsp_types::Position,
                _s: &str,
            ) -> Result<Vec<lsp_types::Location>, String> {
                let uri = crate::lsp::client::file_path_to_uri("/proj/a.rs").unwrap();
                Ok(vec![lsp_types::Location {
                    uri,
                    range: lsp_types::Range::default(),
                }])
            }
            fn definition(
                &mut self,
                _u: &lsp_types::Uri,
                _p: lsp_types::Position,
            ) -> Result<lsp_types::GotoDefinitionResponse, String> {
                Ok(lsp_types::GotoDefinitionResponse::Array(vec![]))
            }
            fn implementations(
                &mut self,
                _u: &lsp_types::Uri,
                _p: lsp_types::Position,
                _s: &str,
            ) -> Result<Vec<lsp_types::Location>, String> {
                Ok(vec![])
            }
            fn rename(
                &mut self,
                _u: &lsp_types::Uri,
                _p: lsp_types::Position,
                _n: &str,
            ) -> Result<Option<lsp_types::WorkspaceEdit>, String> {
                Ok(None)
            }
            fn last_truncation(&self) -> Option<crate::lsp::backend::Truncation> {
                Some(crate::lsp::backend::Truncation {
                    truncated: true,
                    total: 742,
                })
            }
        }
        crate::lsp::router::seed_stub_backend("rust", Box::new(TruncBackend));
        let uri = crate::lsp::client::file_path_to_uri("/proj/a.rs").unwrap();
        let out = super::handle_references(
            "/proj/a.rs",
            "/proj",
            &uri,
            Position {
                line: 0,
                character: 0,
            },
            "project",
        );
        assert!(
            out.contains("truncated"),
            "expected truncation note, got: {out}"
        );
        assert!(out.contains("742"), "expected total in note, got: {out}");
    }

    #[test]
    fn inspections_run_and_list_dispatch_and_truncation() {
        struct InspBackend;
        impl crate::lsp::backend::LspBackend for InspBackend {
            fn open_file(&mut self, _u: &lsp_types::Uri, _l: &str, _t: &str) -> Result<(), String> {
                Ok(())
            }
            fn references(
                &mut self,
                _u: &lsp_types::Uri,
                _p: lsp_types::Position,
                _s: &str,
            ) -> Result<Vec<lsp_types::Location>, String> {
                Ok(vec![])
            }
            fn definition(
                &mut self,
                _u: &lsp_types::Uri,
                _p: lsp_types::Position,
            ) -> Result<lsp_types::GotoDefinitionResponse, String> {
                Ok(lsp_types::GotoDefinitionResponse::Array(vec![]))
            }
            fn implementations(
                &mut self,
                _u: &lsp_types::Uri,
                _p: lsp_types::Position,
                _s: &str,
            ) -> Result<Vec<lsp_types::Location>, String> {
                Ok(vec![])
            }
            fn rename(
                &mut self,
                _u: &lsp_types::Uri,
                _p: lsp_types::Position,
                _n: &str,
            ) -> Result<Option<lsp_types::WorkspaceEdit>, String> {
                Ok(None)
            }
            fn inspections(
                &mut self,
                _u: &lsp_types::Uri,
            ) -> Result<Vec<crate::lsp::backend::InspectionDiag>, String> {
                Ok(vec![crate::lsp::backend::InspectionDiag {
                    path: "A.kt".into(),
                    line: 7,
                    severity: "WARNING".into(),
                    message: "unused".into(),
                }])
            }
            fn list_inspections(
                &mut self,
            ) -> Result<Vec<crate::lsp::backend::InspectionInfo>, String> {
                Ok(vec![crate::lsp::backend::InspectionInfo {
                    id: "UnusedSymbol".into(),
                    name: "Unused declaration".into(),
                    severity: "WARNING".into(),
                }])
            }
            fn last_truncation(&self) -> Option<crate::lsp::backend::Truncation> {
                Some(crate::lsp::backend::Truncation {
                    truncated: true,
                    total: 99,
                })
            }
        }
        crate::lsp::router::seed_stub_backend("rust", Box::new(InspBackend));
        let uri = crate::lsp::client::file_path_to_uri("/proj/a.rs").unwrap();

        // run mode (default): formats path:line SEVERITY message + truncation note
        let run_out = super::handle_inspections(
            &json!({"action": "inspections"}),
            "/proj/a.rs",
            "/proj",
            &uri,
        );
        assert!(run_out.contains("A.kt:7"), "run diag missing: {run_out}");
        assert!(
            run_out.contains("WARNING"),
            "run severity missing: {run_out}"
        );
        assert!(run_out.contains("unused"), "run message missing: {run_out}");
        assert!(
            run_out.contains("truncated"),
            "run truncation missing: {run_out}"
        );
        assert!(run_out.contains("99"), "run total missing: {run_out}");

        // list mode: formats id name severity
        let list_out = super::handle_inspections(
            &json!({"action": "inspections", "mode": "list"}),
            "/proj/a.rs",
            "/proj",
            &uri,
        );
        assert!(
            list_out.contains("UnusedSymbol"),
            "list id missing: {list_out}"
        );
        assert!(
            list_out.contains("Unused declaration"),
            "list name missing: {list_out}"
        );

        // unknown mode → defined ERROR
        let bad_out = super::handle_inspections(
            &json!({"action": "inspections", "mode": "bogus"}),
            "/proj/a.rs",
            "/proj",
            &uri,
        );
        assert!(
            bad_out.contains("ERROR"),
            "unknown mode not rejected: {bad_out}"
        );
    }

    #[test]
    fn usage_range_text_reads_jailed_slice() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "let foo = 1;\nfoo + foo;\n").unwrap();
        let root = dir.path().to_str().unwrap();
        let u = crate::lsp::backend::UsageSite {
            path: "a.rs".into(),
            range: crate::lsp::backend::TextRange0Based {
                start_line: 0,
                start_char: 4,
                end_line: 0,
                end_char: 7,
            },
            context: None,
        };
        assert_eq!(super::usage_range_text(root, &u).unwrap(), "foo");
    }

    // Jail rejection only happens when the jail is compiled in. `--all-features`
    // pulls in `no-jail` (jail disabled), so skip there like the move/resolve jail
    // assertions below.
    #[cfg(not(feature = "no-jail"))]
    #[test]
    fn usage_range_text_rejects_jail_escape() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_str().unwrap();
        let u = crate::lsp::backend::UsageSite {
            path: "../../etc/passwd".into(),
            range: crate::lsp::backend::TextRange0Based {
                start_line: 0,
                start_char: 0,
                end_line: 0,
                end_char: 1,
            },
            context: None,
        };
        assert!(super::usage_range_text(root, &u).is_err());
    }

    #[test]
    fn plan_hash_is_deterministic_and_order_independent() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "let foo = 1;\nfoo + foo;\n").unwrap();
        let root = dir.path().to_str().unwrap();
        let u1 = crate::lsp::backend::UsageSite {
            path: "a.rs".into(),
            range: crate::lsp::backend::TextRange0Based {
                start_line: 0,
                start_char: 4,
                end_line: 0,
                end_char: 7,
            },
            context: Some("ignored-in-hash".into()),
        };
        let u2 = crate::lsp::backend::UsageSite {
            path: "a.rs".into(),
            range: crate::lsp::backend::TextRange0Based {
                start_line: 1,
                start_char: 0,
                end_line: 1,
                end_char: 3,
            },
            context: None,
        };
        let h1 = super::plan_hash(root, &[u1.clone(), u2.clone()]).unwrap();
        let h2 = super::plan_hash(root, std::slice::from_ref(&u2)).unwrap(); // subset → differs
        let h3 = super::plan_hash(root, &[u2, u1]).unwrap(); // reversed → SAME (sorted canonical)
        assert_eq!(h1.len(), 64);
        assert_eq!(h1, h3, "hash must be order-independent");
        assert_ne!(h1, h2, "different usage set must differ");
    }

    #[test]
    fn resolve_rename_target_position_fallback() {
        let (rel, sl, el) = super::resolve_rename_target(
            &serde_json::json!({"path": "a.rs", "line": 3, "end_line": 5}),
            "/proj",
        )
        .unwrap();
        assert_eq!(rel, "a.rs");
        assert_eq!((sl, el), (3, 5));
    }

    #[test]
    fn resolve_rename_target_requires_line_in_fallback() {
        let err = super::resolve_rename_target(&serde_json::json!({"path": "a.rs"}), "/proj")
            .unwrap_err();
        assert!(err.contains("line"), "got: {err}");
    }

    #[test]
    fn live_backend_absent_is_backend_required() {
        // No port file under an unlikely root → deterministic BACKEND_REQUIRED, no HTTP.
        let err = super::live_jetbrains_backend("/nonexistent/leanctx/proj/zzz")
            .err()
            .expect("expected Err from live_jetbrains_backend");
        assert!(err.starts_with("BACKEND_REQUIRED"), "got: {err}");
    }

    /// Minimal backend that returns canned rename plans + records apply calls.
    struct RenameStub {
        plan: crate::lsp::backend::RenamePlan,
        applied_with_force: std::cell::Cell<Option<bool>>,
    }
    impl crate::lsp::backend::LspBackend for RenameStub {
        fn open_file(&mut self, _u: &lsp_types::Uri, _l: &str, _t: &str) -> Result<(), String> {
            Ok(())
        }
        fn references(
            &mut self,
            _u: &lsp_types::Uri,
            _p: lsp_types::Position,
            _s: &str,
        ) -> Result<Vec<lsp_types::Location>, String> {
            Ok(vec![])
        }
        fn definition(
            &mut self,
            _u: &lsp_types::Uri,
            _p: lsp_types::Position,
        ) -> Result<lsp_types::GotoDefinitionResponse, String> {
            Ok(lsp_types::GotoDefinitionResponse::Array(vec![]))
        }
        fn implementations(
            &mut self,
            _u: &lsp_types::Uri,
            _p: lsp_types::Position,
            _s: &str,
        ) -> Result<Vec<lsp_types::Location>, String> {
            Ok(vec![])
        }
        fn rename(
            &mut self,
            _u: &lsp_types::Uri,
            _p: lsp_types::Position,
            _n: &str,
        ) -> Result<Option<lsp_types::WorkspaceEdit>, String> {
            Ok(None)
        }
        fn rename_preview(
            &mut self,
            _q: &crate::lsp::backend::RenameQuery,
        ) -> Result<crate::lsp::backend::RenamePlan, String> {
            Ok(self.plan.clone())
        }
        fn rename_apply(
            &mut self,
            req: &crate::lsp::backend::RenameApply,
        ) -> Result<crate::lsp::backend::RenameResult, String> {
            self.applied_with_force.set(Some(req.force));
            Ok(crate::lsp::backend::RenameResult {
                applied: true,
                changed_paths: vec!["a.rs".into()],
            })
        }
    }

    fn stub_query(abs: &str) -> crate::lsp::backend::RenameQuery {
        crate::lsp::backend::RenameQuery {
            abs_path: abs.into(),
            rel_path: "a.rs".into(),
            target_range: crate::lsp::backend::TextRange0Based {
                start_line: 0,
                start_char: 4,
                end_line: 0,
                end_char: 7,
            },
            new_name: "bar".into(),
            search_comments: false,
            search_text_occurrences: false,
        }
    }

    #[test]
    fn apply_blocks_on_plan_hash_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "let foo = 1;\nfoo + foo;\n").unwrap();
        let root = dir.path().to_str().unwrap();
        let usage = crate::lsp::backend::UsageSite {
            path: "a.rs".into(),
            range: crate::lsp::backend::TextRange0Based {
                start_line: 0,
                start_char: 4,
                end_line: 0,
                end_char: 7,
            },
            context: None,
        };
        let mut be = RenameStub {
            plan: crate::lsp::backend::RenamePlan {
                usages: vec![usage],
                conflicts: vec![],
            },
            applied_with_force: std::cell::Cell::new(None),
        };
        let q = stub_query(&dir.path().join("a.rs").to_string_lossy());
        let out = super::render_rename_apply(&mut be, root, &q, "bar", "stalehash", false);
        assert!(out.contains("CONFLICT"), "got: {out}");
        assert_eq!(
            be.applied_with_force.get(),
            None,
            "apply must not run on hash mismatch"
        );
    }

    #[test]
    fn apply_blocks_on_conflicts_without_force_and_passes_with_force() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "let foo = 1;\nfoo + foo;\n").unwrap();
        let root = dir.path().to_str().unwrap();
        let usage = crate::lsp::backend::UsageSite {
            path: "a.rs".into(),
            range: crate::lsp::backend::TextRange0Based {
                start_line: 0,
                start_char: 4,
                end_line: 0,
                end_char: 7,
            },
            context: None,
        };
        let plan = crate::lsp::backend::RenamePlan {
            usages: vec![usage.clone()],
            conflicts: vec![crate::lsp::backend::Conflict {
                path: "a.rs".into(),
                range: None,
                message: "clash".into(),
            }],
        };
        let hash = super::plan_hash(root, &plan.usages).unwrap();
        let q = stub_query(&dir.path().join("a.rs").to_string_lossy());

        // force=false → CONFLICT, apply not called.
        let mut be = RenameStub {
            plan: plan.clone(),
            applied_with_force: std::cell::Cell::new(None),
        };
        let out = super::render_rename_apply(&mut be, root, &q, "bar", &hash, false);
        assert!(out.contains("CONFLICT"), "got: {out}");
        assert_eq!(be.applied_with_force.get(), None);

        // force=true → applies, force passed through.
        let mut be2 = RenameStub {
            plan,
            applied_with_force: std::cell::Cell::new(None),
        };
        let out2 = super::render_rename_apply(&mut be2, root, &q, "bar", &hash, true);
        assert!(out2.contains("applied"), "got: {out2}");
        assert_eq!(be2.applied_with_force.get(), Some(true));
    }

    #[test]
    fn apply_success_emits_diff_and_evicts() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "let foo = 1;\nfoo + foo;\n").unwrap();
        let root = dir.path().to_str().unwrap();
        let usage = crate::lsp::backend::UsageSite {
            path: "a.rs".into(),
            range: crate::lsp::backend::TextRange0Based {
                start_line: 0,
                start_char: 4,
                end_line: 0,
                end_char: 7,
            },
            context: None,
        };
        let plan = crate::lsp::backend::RenamePlan {
            usages: vec![usage],
            conflicts: vec![],
        };
        let hash = super::plan_hash(root, &plan.usages).unwrap();
        let mut be = RenameStub {
            plan,
            applied_with_force: std::cell::Cell::new(None),
        };
        let q = stub_query(&dir.path().join("a.rs").to_string_lossy());
        let out = super::render_rename_apply(&mut be, root, &q, "bar", &hash, false);
        assert!(out.contains("applied"), "got: {out}");
        assert!(out.contains("\"foo\" → \"bar\""), "diff missing: {out}");
    }

    #[test]
    fn preview_renders_plan_hash_and_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("usage.rs"), "let foo = 1;\nfoo + foo;\n").unwrap();
        let root = dir.path().to_str().unwrap();
        let usage = crate::lsp::backend::UsageSite {
            path: "usage.rs".into(),
            range: crate::lsp::backend::TextRange0Based {
                start_line: 0,
                start_char: 4,
                end_line: 0,
                end_char: 7,
            },
            context: None,
        };
        let plan = crate::lsp::backend::RenamePlan {
            usages: vec![usage],
            conflicts: vec![],
        };
        let mut be = RenameStub {
            plan,
            applied_with_force: std::cell::Cell::new(None),
        };
        let mut q = stub_query(&dir.path().join("usage.rs").to_string_lossy());
        q.rel_path = "decl.rs".into();
        let out = super::render_rename_preview(&mut be, root, &q, "bar");
        assert!(out.contains("plan_hash:"), "got: {out}");
        assert!(out.contains("usages: 1"), "got: {out}");
        assert!(out.contains("files: 2"), "got: {out}");
        assert!(out.contains("usage.rs: 1 usage"), "got: {out}");
    }

    #[test]
    fn handle_rename_preview_without_ide_is_backend_required() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn foo() {}\n").unwrap();
        let root = dir.path().to_str().unwrap();
        // No port file under this temp root → BACKEND_REQUIRED before any HTTP.
        let args = serde_json::json!({
            "action": "rename_preview", "path": "a.rs", "line": 1, "new_name": "bar"
        });
        let out = super::handle(&args, root, "");
        assert!(out.contains("BACKEND_REQUIRED"), "got: {out}");
    }

    #[test]
    fn handle_rename_apply_requires_plan_hash() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn foo() {}\n").unwrap();
        let root = dir.path().to_str().unwrap();
        let args = serde_json::json!({
            "action": "rename_apply", "path": "a.rs", "line": 1, "new_name": "bar"
        });
        let out = super::handle(&args, root, "");
        assert!(out.contains("plan_hash"), "got: {out}");
    }

    #[test]
    fn handle_safe_delete_preview_without_ide_is_backend_required() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn foo() {}\n").unwrap();
        let root = dir.path().to_str().unwrap();
        let args = serde_json::json!({"action": "safe_delete_preview", "path": "a.rs", "line": 1});
        let out = super::handle(&args, root, "");
        assert!(out.contains("BACKEND_REQUIRED"), "got: {out}");
    }

    #[test]
    fn handle_safe_delete_apply_requires_plan_hash() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn foo() {}\n").unwrap();
        let root = dir.path().to_str().unwrap();
        let args = serde_json::json!({"action": "safe_delete_apply", "path": "a.rs", "line": 1});
        let out = super::handle(&args, root, "");
        assert!(out.contains("plan_hash"), "got: {out}");
    }

    #[test]
    fn resolve_move_target_requires_exactly_one_field() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("app/moved")).unwrap();
        let root = dir.path().to_str().unwrap();

        // Neither set → INVALID_TARGET.
        let err = super::resolve_move_target(&serde_json::json!({}), root).unwrap_err();
        assert!(err.starts_with("INVALID_TARGET"), "got: {err}");

        // Both set → INVALID_TARGET.
        let err2 = super::resolve_move_target(
            &serde_json::json!({"target_path": "app/moved", "target_parent": "Other"}),
            root,
        )
        .unwrap_err();
        assert!(err2.starts_with("INVALID_TARGET"), "got: {err2}");
    }

    // Jail rejection only happens when the jail is compiled in. `--all-features`
    // pulls in `no-jail` (jail disabled), so skip there like every other jail
    // assertion (see e.g. server::multi_path tests).
    #[cfg(not(feature = "no-jail"))]
    #[test]
    fn resolve_move_target_path_is_jailed() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("app/moved")).unwrap();
        let root = dir.path().to_str().unwrap();

        // In-jail path resolves to a MoveTarget::Path.
        let t = super::resolve_move_target(&serde_json::json!({"target_path": "app/moved"}), root)
            .unwrap();
        match t {
            crate::lsp::backend::MoveTarget::Path { rel_path, .. } => {
                assert_eq!(rel_path, "app/moved");
            }
            other @ crate::lsp::backend::MoveTarget::Parent { .. } => {
                panic!("expected Path, got {other:?}")
            }
        }

        // Escape attempt → INVALID_TARGET (jail violation, before any backend call).
        let err =
            super::resolve_move_target(&serde_json::json!({"target_path": "../../etc/skel"}), root)
                .unwrap_err();
        assert!(err.starts_with("INVALID_TARGET"), "got: {err}");
    }

    /// Minimal backend for the move renderers: canned plan + recorded apply flags + changed paths.
    struct MoveStub {
        plan: crate::lsp::backend::RenamePlan,
        applied_with_force: std::cell::Cell<Option<bool>>,
    }
    impl crate::lsp::backend::LspBackend for MoveStub {
        fn open_file(&mut self, _u: &lsp_types::Uri, _l: &str, _t: &str) -> Result<(), String> {
            Ok(())
        }
        fn references(
            &mut self,
            _u: &lsp_types::Uri,
            _p: lsp_types::Position,
            _s: &str,
        ) -> Result<Vec<lsp_types::Location>, String> {
            Ok(vec![])
        }
        fn definition(
            &mut self,
            _u: &lsp_types::Uri,
            _p: lsp_types::Position,
        ) -> Result<lsp_types::GotoDefinitionResponse, String> {
            Ok(lsp_types::GotoDefinitionResponse::Array(vec![]))
        }
        fn implementations(
            &mut self,
            _u: &lsp_types::Uri,
            _p: lsp_types::Position,
            _s: &str,
        ) -> Result<Vec<lsp_types::Location>, String> {
            Ok(vec![])
        }
        fn rename(
            &mut self,
            _u: &lsp_types::Uri,
            _p: lsp_types::Position,
            _n: &str,
        ) -> Result<Option<lsp_types::WorkspaceEdit>, String> {
            Ok(None)
        }
        fn move_preview(
            &mut self,
            _q: &crate::lsp::backend::MoveQuery,
        ) -> Result<crate::lsp::backend::RenamePlan, String> {
            Ok(self.plan.clone())
        }
        fn move_apply(
            &mut self,
            req: &crate::lsp::backend::MoveApply,
        ) -> Result<crate::lsp::backend::RenameResult, String> {
            self.applied_with_force.set(Some(req.force));
            Ok(crate::lsp::backend::RenameResult {
                applied: true,
                changed_paths: vec!["app/moved/Widget.kt".into()],
            })
        }
    }

    fn move_query(abs: &str) -> crate::lsp::backend::MoveQuery {
        crate::lsp::backend::MoveQuery {
            abs_path: abs.into(),
            rel_path: "a.rs".into(),
            src_range: crate::lsp::backend::TextRange0Based {
                start_line: 0,
                start_char: 4,
                end_line: 0,
                end_char: 7,
            },
            target: crate::lsp::backend::MoveTarget::Path {
                abs_path: "/p/app/moved".into(),
                rel_path: "app/moved".into(),
            },
        }
    }

    #[test]
    fn move_apply_gates_then_evicts() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("app/moved")).unwrap();
        std::fs::write(dir.path().join("a.rs"), "let foo = 1;\nfoo + foo;\n").unwrap();
        std::fs::write(dir.path().join("app/moved/Widget.kt"), "// moved\n").unwrap();
        let root = dir.path().to_str().unwrap();
        let usage = crate::lsp::backend::UsageSite {
            path: "a.rs".into(),
            range: crate::lsp::backend::TextRange0Based {
                start_line: 0,
                start_char: 4,
                end_line: 0,
                end_char: 7,
            },
            context: None,
        };
        let plan = crate::lsp::backend::RenamePlan {
            usages: vec![usage],
            conflicts: vec![],
        };
        let hash = super::plan_hash(root, &plan.usages).unwrap();
        let q = move_query(&dir.path().join("a.rs").to_string_lossy());

        // hash mismatch → CONFLICT, apply not called.
        let mut be = MoveStub {
            plan: plan.clone(),
            applied_with_force: std::cell::Cell::new(None),
        };
        let out = super::render_move_apply(&mut be, root, &q, "stalehash", false);
        assert!(out.contains("CONFLICT"), "got: {out}");
        assert_eq!(be.applied_with_force.get(), None);

        // matching hash + force → applies, force passed through, changed path jailed+evicted.
        let mut be2 = MoveStub {
            plan,
            applied_with_force: std::cell::Cell::new(None),
        };
        let out2 = super::render_move_apply(&mut be2, root, &q, &hash, true);
        assert!(out2.contains("applied"), "got: {out2}");
        assert_eq!(be2.applied_with_force.get(), Some(true));
    }

    // See above: jail rejection requires the jail compiled in (skipped under no-jail).
    #[cfg(not(feature = "no-jail"))]
    #[test]
    fn move_apply_rejects_out_of_jail_changed_path() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "let foo = 1;\nfoo + foo;\n").unwrap();
        let root = dir.path().to_str().unwrap();
        let usage = crate::lsp::backend::UsageSite {
            path: "a.rs".into(),
            range: crate::lsp::backend::TextRange0Based {
                start_line: 0,
                start_char: 4,
                end_line: 0,
                end_char: 7,
            },
            context: None,
        };
        // Stub returns an out-of-jail changed path (stage-3 jail must reject it post-apply).
        struct EscapeStub {
            plan: crate::lsp::backend::RenamePlan,
        }
        impl crate::lsp::backend::LspBackend for EscapeStub {
            fn open_file(&mut self, _u: &lsp_types::Uri, _l: &str, _t: &str) -> Result<(), String> {
                Ok(())
            }
            fn references(
                &mut self,
                _u: &lsp_types::Uri,
                _p: lsp_types::Position,
                _s: &str,
            ) -> Result<Vec<lsp_types::Location>, String> {
                Ok(vec![])
            }
            fn definition(
                &mut self,
                _u: &lsp_types::Uri,
                _p: lsp_types::Position,
            ) -> Result<lsp_types::GotoDefinitionResponse, String> {
                Ok(lsp_types::GotoDefinitionResponse::Array(vec![]))
            }
            fn implementations(
                &mut self,
                _u: &lsp_types::Uri,
                _p: lsp_types::Position,
                _s: &str,
            ) -> Result<Vec<lsp_types::Location>, String> {
                Ok(vec![])
            }
            fn rename(
                &mut self,
                _u: &lsp_types::Uri,
                _p: lsp_types::Position,
                _n: &str,
            ) -> Result<Option<lsp_types::WorkspaceEdit>, String> {
                Ok(None)
            }
            fn move_preview(
                &mut self,
                _q: &crate::lsp::backend::MoveQuery,
            ) -> Result<crate::lsp::backend::RenamePlan, String> {
                Ok(self.plan.clone())
            }
            fn move_apply(
                &mut self,
                _r: &crate::lsp::backend::MoveApply,
            ) -> Result<crate::lsp::backend::RenameResult, String> {
                Ok(crate::lsp::backend::RenameResult {
                    applied: true,
                    changed_paths: vec!["../../etc/passwd".into()],
                })
            }
        }
        let plan = crate::lsp::backend::RenamePlan {
            usages: vec![usage],
            conflicts: vec![],
        };
        let hash = super::plan_hash(root, &plan.usages).unwrap();
        let mut be = EscapeStub { plan };
        let q = move_query(&dir.path().join("a.rs").to_string_lossy());
        let out = super::render_move_apply(&mut be, root, &q, &hash, false);
        assert!(out.contains("jail"), "expected jail rejection, got: {out}");
    }

    #[test]
    fn handle_move_preview_invalid_target_before_backend() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn foo() {}\n").unwrap();
        let root = dir.path().to_str().unwrap();
        // No target → INVALID_TARGET, and crucially BEFORE BACKEND_REQUIRED (no live IDE here).
        let args = serde_json::json!({"action": "move_preview", "path": "a.rs", "line": 1});
        let out = super::handle(&args, root, "");
        assert!(out.contains("INVALID_TARGET"), "got: {out}");
        assert!(
            !out.contains("BACKEND_REQUIRED"),
            "target gate must precede backend gate: {out}"
        );
    }

    #[test]
    fn handle_move_apply_requires_plan_hash() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("x")).unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn foo() {}\n").unwrap();
        let root = dir.path().to_str().unwrap();
        let args = serde_json::json!({"action": "move_apply", "path": "a.rs", "line": 1, "target_path": "x"});
        let out = super::handle(&args, root, "");
        assert!(out.contains("plan_hash"), "got: {out}");
    }

    #[test]
    fn unknown_action_help_lists_rename_actions() {
        // Resolution happens before backend selection for rename actions, so an
        // empty new_name short-circuits with a clear ERROR mentioning new_name.
        let args = serde_json::json!({"action": "rename_preview", "path": "a.rs", "line": 1});
        let out = super::handle(&args, "/proj", "");
        assert!(out.contains("new_name"), "got: {out}");
    }

    /// Minimal backend for the safe_delete renderers: canned plan + recorded apply flags.
    struct SafeDeleteStub {
        plan: crate::lsp::backend::RenamePlan,
        applied: std::cell::Cell<Option<(bool, bool)>>, // (force, propagate)
    }
    impl crate::lsp::backend::LspBackend for SafeDeleteStub {
        fn open_file(&mut self, _u: &lsp_types::Uri, _l: &str, _t: &str) -> Result<(), String> {
            Ok(())
        }
        fn references(
            &mut self,
            _u: &lsp_types::Uri,
            _p: lsp_types::Position,
            _s: &str,
        ) -> Result<Vec<lsp_types::Location>, String> {
            Ok(vec![])
        }
        fn definition(
            &mut self,
            _u: &lsp_types::Uri,
            _p: lsp_types::Position,
        ) -> Result<lsp_types::GotoDefinitionResponse, String> {
            Ok(lsp_types::GotoDefinitionResponse::Array(vec![]))
        }
        fn implementations(
            &mut self,
            _u: &lsp_types::Uri,
            _p: lsp_types::Position,
            _s: &str,
        ) -> Result<Vec<lsp_types::Location>, String> {
            Ok(vec![])
        }
        fn rename(
            &mut self,
            _u: &lsp_types::Uri,
            _p: lsp_types::Position,
            _n: &str,
        ) -> Result<Option<lsp_types::WorkspaceEdit>, String> {
            Ok(None)
        }
        fn safe_delete_preview(
            &mut self,
            _q: &crate::lsp::backend::SafeDeleteQuery,
        ) -> Result<crate::lsp::backend::RenamePlan, String> {
            Ok(self.plan.clone())
        }
        fn safe_delete_apply(
            &mut self,
            req: &crate::lsp::backend::SafeDeleteApply,
        ) -> Result<crate::lsp::backend::RenameResult, String> {
            self.applied.set(Some((req.force, req.propagate)));
            Ok(crate::lsp::backend::RenameResult {
                applied: true,
                changed_paths: vec!["Widget.kt".into()],
            })
        }
    }

    fn safe_delete_query(abs: &str) -> crate::lsp::backend::SafeDeleteQuery {
        crate::lsp::backend::SafeDeleteQuery {
            abs_path: abs.into(),
            rel_path: "a.rs".into(),
            src_range: crate::lsp::backend::TextRange0Based {
                start_line: 0,
                start_char: 4,
                end_line: 0,
                end_char: 7,
            },
        }
    }

    #[test]
    fn safe_delete_apply_blocks_on_remaining_refs_without_force() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "let foo = 1;\nfoo + foo;\n").unwrap();
        let root = dir.path().to_str().unwrap();
        let usage = crate::lsp::backend::UsageSite {
            path: "a.rs".into(),
            range: crate::lsp::backend::TextRange0Based {
                start_line: 0,
                start_char: 4,
                end_line: 0,
                end_char: 7,
            },
            context: None,
        };
        // A remaining reference = a blocking conflict (spec §5.4).
        let plan = crate::lsp::backend::RenamePlan {
            usages: vec![usage.clone()],
            conflicts: vec![crate::lsp::backend::Conflict {
                path: "a.rs".into(),
                range: None,
                message: "still referenced".into(),
            }],
        };
        let hash = super::plan_hash(root, &plan.usages).unwrap();
        let q = safe_delete_query(&dir.path().join("a.rs").to_string_lossy());

        // force=false → CONFLICT, apply not called.
        let mut be = SafeDeleteStub {
            plan: plan.clone(),
            applied: std::cell::Cell::new(None),
        };
        let out = super::render_safe_delete_apply(&mut be, root, &q, &hash, false, false);
        assert!(out.contains("CONFLICT"), "got: {out}");
        assert_eq!(be.applied.get(), None);

        // force=true → applies, force+propagate passed through.
        let mut be2 = SafeDeleteStub {
            plan,
            applied: std::cell::Cell::new(None),
        };
        let out2 = super::render_safe_delete_apply(&mut be2, root, &q, &hash, true, true);
        assert!(
            out2.contains("deleted") || out2.contains("applied"),
            "got: {out2}"
        );
        assert_eq!(be2.applied.get(), Some((true, true)));
    }

    #[test]
    fn safe_delete_apply_blocks_on_plan_hash_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "let foo = 1;\nfoo + foo;\n").unwrap();
        let root = dir.path().to_str().unwrap();
        let usage = crate::lsp::backend::UsageSite {
            path: "a.rs".into(),
            range: crate::lsp::backend::TextRange0Based {
                start_line: 0,
                start_char: 4,
                end_line: 0,
                end_char: 7,
            },
            context: None,
        };
        let mut be = SafeDeleteStub {
            plan: crate::lsp::backend::RenamePlan {
                usages: vec![usage],
                conflicts: vec![],
            },
            applied: std::cell::Cell::new(None),
        };
        let q = safe_delete_query(&dir.path().join("a.rs").to_string_lossy());
        let out = super::render_safe_delete_apply(&mut be, root, &q, "stalehash", false, false);
        assert!(out.contains("CONFLICT"), "got: {out}");
        assert_eq!(be.applied.get(), None);
    }

    /// Minimal backend for the inline renderers: canned preview plan (with
    /// optional conflicts) + a no-op apply. Mirrors SafeDeleteStub above, but the
    /// inline path has NO force flag, so the stub records nothing.
    struct InlineStub {
        conflicts: Vec<crate::lsp::backend::Conflict>,
    }
    impl crate::lsp::backend::LspBackend for InlineStub {
        fn open_file(&mut self, _u: &lsp_types::Uri, _l: &str, _t: &str) -> Result<(), String> {
            Ok(())
        }
        fn references(
            &mut self,
            _u: &lsp_types::Uri,
            _p: lsp_types::Position,
            _s: &str,
        ) -> Result<Vec<lsp_types::Location>, String> {
            Ok(vec![])
        }
        fn definition(
            &mut self,
            _u: &lsp_types::Uri,
            _p: lsp_types::Position,
        ) -> Result<lsp_types::GotoDefinitionResponse, String> {
            Ok(lsp_types::GotoDefinitionResponse::Array(vec![]))
        }
        fn implementations(
            &mut self,
            _u: &lsp_types::Uri,
            _p: lsp_types::Position,
            _s: &str,
        ) -> Result<Vec<lsp_types::Location>, String> {
            Ok(vec![])
        }
        fn rename(
            &mut self,
            _u: &lsp_types::Uri,
            _p: lsp_types::Position,
            _n: &str,
        ) -> Result<Option<lsp_types::WorkspaceEdit>, String> {
            Ok(None)
        }
        fn inline_preview(
            &mut self,
            _q: &crate::lsp::backend::InlineQuery,
        ) -> Result<crate::lsp::backend::RenamePlan, String> {
            Ok(crate::lsp::backend::RenamePlan {
                usages: vec![],
                conflicts: self.conflicts.clone(),
            })
        }
        fn inline_apply(
            &mut self,
            _r: &crate::lsp::backend::InlineApply,
        ) -> Result<crate::lsp::backend::RenameResult, String> {
            Ok(crate::lsp::backend::RenameResult {
                applied: true,
                changed_paths: vec![],
            })
        }
    }

    fn inline_query(abs: &str) -> crate::lsp::backend::InlineQuery {
        crate::lsp::backend::InlineQuery {
            abs_path: abs.to_string(),
            rel_path: "Calc.kt".to_string(),
            src_range: crate::lsp::backend::TextRange0Based {
                start_line: 0,
                start_char: 0,
                end_line: 0,
                end_char: 0,
            },
            keep_definition: false,
        }
    }

    #[test]
    fn handle_inline_apply_requires_plan_hash() {
        let args = serde_json::json!({ "action": "inline_apply", "name_path": "Calc/tmp" });
        let out = super::handle_inline_refactor("inline_apply", &args, "/nonexistent-root");
        assert!(out.contains("plan_hash"), "got: {out}");
    }

    #[test]
    fn handle_inline_preview_without_ide_is_backend_required() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Calc.kt"), "val tmp = 1\n").unwrap();
        let root = dir.path().to_str().unwrap();
        // File exists → flow reaches the live-IDE gate; no port file → BACKEND_REQUIRED.
        let args = serde_json::json!({ "action": "inline_preview", "path": "Calc.kt", "line": 1 });
        let out = super::handle_inline_refactor("inline_preview", &args, root);
        assert!(out.contains("BACKEND_REQUIRED"), "got: {out}");
    }

    #[test]
    fn inline_apply_blocks_on_conflicts_with_no_force_path() {
        // A conflicting plan must ALWAYS produce CONFLICT — there is no force arg to pass.
        let mut be = InlineStub {
            conflicts: vec![crate::lsp::backend::Conflict {
                path: "Calc.kt".into(),
                range: None,
                message: "recursive".into(),
            }],
        };
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("Calc.kt");
        std::fs::write(&f, "val tmp = 1\n").unwrap();
        let q = inline_query(f.to_str().unwrap());
        // expected_hash is irrelevant: the conflict gate fires regardless.
        let out = super::render_inline_apply(&mut be, dir.path().to_str().unwrap(), &q, "deadbeef");
        assert!(out.contains("CONFLICT"), "got: {out}");
    }

    #[test]
    fn reformat_invalid_target_when_no_address() {
        let args = serde_json::json!({ "action": "reformat" });
        let out = super::handle_reformat_refactor(&args, env!("CARGO_MANIFEST_DIR"));
        assert!(out.contains("INVALID_TARGET"), "got: {out}");
    }

    #[test]
    fn reformat_address_dispatch_resolves_scope() {
        // path alone → File; path+line → Region; name_path → Symbol.
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("M.kt");
        std::fs::write(&f, "fun a(){}\nfun b(){}\n").unwrap();
        let root = dir.path().to_str().unwrap();

        let file_args = serde_json::json!({ "action": "reformat", "path": "M.kt" });
        let (_abs, _rel, scope) = super::resolve_reformat_scope(&file_args, root).unwrap();
        assert!(matches!(scope, crate::lsp::backend::ReformatScope::File));

        let region_args =
            serde_json::json!({ "action": "reformat", "path": "M.kt", "line": 1, "end_line": 2 });
        let (_a, _r, scope) = super::resolve_reformat_scope(&region_args, root).unwrap();
        assert!(matches!(
            scope,
            crate::lsp::backend::ReformatScope::Region { .. }
        ));
    }

    #[test]
    fn reformat_without_ide_is_backend_required() {
        let args = serde_json::json!({ "action": "reformat", "path": "M.kt" });
        let out = super::handle_reformat_refactor(&args, env!("CARGO_MANIFEST_DIR"));
        // Either resolved scope then BACKEND_REQUIRED, or FILE_NOT_FOUND if M.kt absent in manifest.
        assert!(
            out.contains("BACKEND_REQUIRED") || out.contains("FILE_NOT_FOUND"),
            "got: {out}"
        );
    }
}
