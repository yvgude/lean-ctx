//! The five plan-hash gated refactor operations: rename, safe-delete,
//! move, inline and reformat (preview/apply pairs).

#[allow(clippy::wildcard_imports)]
use super::*;

/// Resolve the rename target: `name_path` (primary, reuse v2a) or `path`+`line`
/// (+`end_line`) fallback. Returns `(rel_path, start_line, end_line)` 1-based incl.
pub(super) fn resolve_rename_target(
    args: &Value,
    project_root: &str,
) -> Result<(String, usize, usize), String> {
    if let Some(np) = args.get("name_path").and_then(Value::as_str) {
        // #845: a caller-supplied `path` alongside `name_path` scopes
        // resolution to that file, so a common method name that's ambiguous
        // repo-wide can still resolve in one call. Basename only — see the
        // matching comment in mod.rs::handle_symbol_edit for why (worktree
        // roots differ from the index's).
        let file_filter = args
            .get("path")
            .and_then(Value::as_str)
            .and_then(|p| std::path::Path::new(p).file_name())
            .and_then(|f| f.to_str());
        let r = resolve_name_path(np, project_root, file_filter)?;
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
pub(super) fn live_jetbrains_backend(
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
pub(super) fn render_rename_preview(
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
pub(super) fn render_rename_apply(
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
pub(super) fn handle_rename_refactor(action: &str, args: &Value, project_root: &str) -> String {
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
pub(super) fn render_safe_delete_apply(
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
pub(super) fn handle_safe_delete_refactor(
    action: &str,
    args: &Value,
    project_root: &str,
) -> String {
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
pub(super) fn resolve_move_target(
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
            let r = resolve_name_path(parent_np, project_root, None)?; // NO_SYMBOL / AMBIGUOUS_SYMBOL
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
pub(super) fn render_move_apply(
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
pub(super) fn handle_move_refactor(action: &str, args: &Value, project_root: &str) -> String {
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
pub(super) fn render_inline_apply(
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
pub(super) fn handle_inline_refactor(action: &str, args: &Value, project_root: &str) -> String {
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
pub(super) fn resolve_reformat_scope(
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
            let r = resolve_name_path(np, project_root, None)?; // NO_SYMBOL / AMBIGUOUS_SYMBOL
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

/// Jetbrains-arm renderer: run the IDE reformat and Single-File/Multi-File evict
/// EVERY changed path (spec §5.3). No plan_hash, no preview. Keeping the full
/// `changed_paths` list — and invalidating all of them — is the B2 contract.
pub(super) fn render_reformat(
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

/// Command-arm renderer (e.g. rustfmt, single file): hash the resolved single
/// file before/after so the report is honest, and only invalidate when the bytes
/// actually changed. blake3 runs on the resolved file path — NEVER a directory —
/// so a no-op run reports "unchanged", not a false "changed" (B2).
fn render_reformat_command(
    template: &str,
    abs_path: &str,
    rel_path: &str,
    project_root: &str,
) -> String {
    let before = crate::lsp::format::blake3_of(abs_path).ok();
    if let Err(e) = crate::lsp::format::run_command_formatter(template, abs_path, project_root) {
        return format!("ERROR: {e}");
    }
    let after = crate::lsp::format::blake3_of(abs_path).ok();
    let changed = before.is_some() && before != after;
    if changed {
        crate::core::cli_cache::invalidate(abs_path);
    }
    let label = crate::lsp::format::command_label(template);
    format!(
        "reformat: '{rel_path}' via {label} — {}\n",
        if changed { "changed" } else { "unchanged" }
    )
}

pub(super) fn handle_reformat_refactor(args: &Value, project_root: &str) -> String {
    let (abs_path, rel_path, scope) = match resolve_reformat_scope(args, project_root) {
        Ok(t) => t,
        Err(e) => return format!("ERROR: {e}"),
    };
    // #475: reformat rewrites the file; deny inside a read-only root.
    if let Some(e) = deny_if_read_only(&abs_path) {
        return e;
    }
    // T4 formatter routing: an external command (e.g. rustfmt) formats the single
    // file directly (no IDE needed); otherwise defer to the live JetBrains backend.
    match crate::lsp::format::resolve_formatter(&abs_path) {
        crate::lsp::format::Formatter::Command(template) => {
            render_reformat_command(&template, &abs_path, &rel_path, project_root)
        }
        crate::lsp::format::Formatter::Jetbrains => {
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
    }
}
