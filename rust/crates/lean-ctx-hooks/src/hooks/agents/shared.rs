// SPDX-License-Identifier: Apache-2.0
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
    lean_ctx::core::jsonc::parse_jsonc(&content).map_err(|error| {
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
    let scope = lean_ctx::core::config::Config::load().rules_scope_effective();
    if global || scope == lean_ctx::core::config::RulesScope::Global {
        eprintln!(
            "Global mode: skipping project-local {file_name} (use without --global in a project)."
        );
        return None;
    }

    let cwd = std::env::current_dir().unwrap_or_default();
    if !is_inside_git_repo(&cwd)
        || cwd == lean_ctx::core::home::resolve_home_dir().unwrap_or_default()
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

#[cfg(test)]
pub(super) use lean_ctx::core::claude_md::count_marker_lines;
pub(super) use lean_ctx::core::claude_md::{
    contains_managed_md_block, managed_md_is_current, remove_managed_md_blocks,
};
