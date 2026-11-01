use std::path::PathBuf;

use super::super::{
    REDIRECT_SCRIPT_GENERIC, generate_compact_rewrite_script, is_inside_git_repo, make_executable,
    resolve_binary_path_for_bash, write_file, write_wrapper_file,
};

pub(super) fn install_standard_hook_scripts(
    hooks_dir: &std::path::Path,
    home: &std::path::Path,
    rewrite_name: &str,
    redirect_name: &str,
) {
    let _ = std::fs::create_dir_all(hooks_dir);

    // #719: never re-stamp a working portable wrapper with an absolute path.
    let binary = resolve_binary_path_for_bash();
    let rewrite_path = hooks_dir.join(rewrite_name);
    let rewrite_script = generate_compact_rewrite_script(&binary);
    write_wrapper_file(&rewrite_path, &rewrite_script, home);
    make_executable(&rewrite_path);

    let redirect_path = hooks_dir.join(redirect_name);
    write_file(&redirect_path, REDIRECT_SCRIPT_GENERIC);
    make_executable(&redirect_path);
}

/// An agent's JSON settings file as JSON for a merge, or `{}` when it is
/// missing or blank. A file that cannot be read or parsed is an error and is
/// left byte-identical: replacing it with `{}` dropped every other setting of
/// the user. `agent_id` names the `lean-ctx init --agent` to rerun.
pub(super) fn read_agent_json_config(
    path: &std::path::Path,
    agent_id: &str,
) -> Result<serde_json::Value, String> {
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(serde_json::json!({}));
        }
        Err(error) => return Err(format!("cannot read {}: {error}", path.display())),
    };
    if content.trim().is_empty() {
        return Ok(serde_json::json!({}));
    }
    crate::core::jsonc::parse_jsonc(&content).map_err(|error| {
        format!(
            "{} is not valid JSON ({error}); it was left unchanged. Fix it (or move it aside) \
             and run `lean-ctx init --agent {agent_id}` again.",
            path.display()
        )
    })
}

/// A refused settings file is reported even in quiet mode: the hooks or
/// permissions it should carry are missing until the file is fixed.
pub(super) fn report_unreadable_agent_config(agent: &str, error: &str) {
    tracing::error!("{agent} settings not updated: {error}");
    eprintln!("  \x1b[31m✗\x1b[0m {agent} settings not updated: {error}");
}

pub(super) fn prepare_project_rules_path(global: bool, file_name: &str) -> Option<PathBuf> {
    let scope = crate::core::config::Config::load().rules_scope_effective();
    if global || scope == crate::core::config::RulesScope::Global {
        eprintln!(
            "Global mode: skipping project-local {file_name} (use without --global in a project)."
        );
        return None;
    }

    let cwd = std::env::current_dir().unwrap_or_default();
    if !is_inside_git_repo(&cwd) || cwd == crate::core::home::resolve_home_dir().unwrap_or_default()
    {
        eprintln!("  Skipping {file_name}: not inside a git repository or in home directory.");
        return None;
    }

    let rules_path = PathBuf::from(file_name);
    if rules_path.exists() {
        let content = std::fs::read_to_string(&rules_path).unwrap_or_default();
        if content.contains("lean-ctx") {
            eprintln!("{file_name} already configured.");
            return None;
        }
    }

    Some(rules_path)
}

/// Remove the first lean-ctx block delimited by `start`..`end` from `content`.
/// Shared by the Claude/CodeBuddy CLAUDE.md/CODEBUDDY.md installers and `doctor`.
/// Markers match as whole (trimmed) lines only (GL #1158) — a prose mention of
/// a marker must never trigger block surgery.
pub(super) fn remove_block(content: &str, start: &str, end: &str) -> String {
    let s = crate::marked_block::marker_line_span(content, start);
    let e = s.and_then(|(si, _)| {
        crate::marked_block::marker_line_span(&content[si..], end)
            .map(|(es, ee)| (si + es, si + ee))
    });
    match (s, e) {
        (Some((si, _)), Some((_, end_after))) => {
            let before = content[..si].trim_end_matches('\n');
            let after = &content[end_after..];
            let mut out = before.to_string();
            out.push('\n');
            if !after.trim().is_empty() {
                out.push('\n');
                out.push_str(after.trim_start_matches('\n'));
            }
            out
        }
        _ => content.to_string(),
    }
}

/// Remove *every* lean-ctx block delimited by `start`..`end`. Heals files that
/// accumulated duplicate blocks from the pre-#549 marker mismatch (the detector
/// constant pointed at `<!-- lean-ctx-rules -->` while the written block used
/// `<!-- lean-ctx -->`, so every `setup`/`doctor --fix` appended a fresh copy).
/// Callers then write exactly one canonical block back.
pub(super) fn remove_all_blocks(content: &str, start: &str, end: &str) -> String {
    let mut out = content.to_string();
    while crate::marked_block::contains_marker_line(&out, start) {
        let next = remove_block(&out, start, end);
        if next == out {
            break; // malformed (start without end) — avoid an infinite loop
        }
        out = next;
    }
    out
}

/// Whole-line occurrences of `marker` — the only form the writers emit, so a
/// prose mention of a marker is never counted as a block (GL #1158, #1901).
pub(super) fn count_marker_lines(content: &str, marker: &str) -> usize {
    content.lines().filter(|l| l.trim() == marker).count()
}

/// Remove every lean-ctx block *and* every solution-rules block from a global
/// instructions file (CLAUDE.md / CODEBUDDY.md). The solution block is written
/// right after the lean-ctx block, outside its markers, so removing only the
/// lean-ctx block left it behind and each rewrite stacked another copy (#1901).
pub(super) fn remove_managed_md_blocks(content: &str, start: &str, end: &str) -> String {
    let out = remove_all_blocks(content, start, end);
    remove_all_blocks(
        &out,
        crate::core::rules_canonical::SOLUTION_BLOCK_START,
        crate::core::rules_canonical::SOLUTION_BLOCK_END,
    )
}

/// True when a global instructions file carries a lean-ctx or solution-rules
/// block (as whole marker lines) that a strip must remove.
pub(super) fn contains_managed_md_block(content: &str, start: &str) -> bool {
    crate::marked_block::contains_marker_line(content, start)
        || crate::marked_block::contains_marker_line(
            content,
            crate::core::rules_canonical::SOLUTION_BLOCK_START,
        )
}

/// True when a global instructions file already holds exactly the managed
/// content: one lean-ctx block, at most the one solution block `block`
/// carries, and `block` itself verbatim.
pub(super) fn managed_md_is_current(existing: &str, start: &str, block: &str) -> bool {
    let solution_start = crate::core::rules_canonical::SOLUTION_BLOCK_START;
    count_marker_lines(existing, start) == 1
        && count_marker_lines(existing, solution_start) == count_marker_lines(block, solution_start)
        && existing.contains(block)
}
