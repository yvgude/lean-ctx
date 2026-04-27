use std::path::PathBuf;

use super::super::{
    generate_compact_rewrite_script, is_inside_git_repo, make_executable,
    resolve_binary_path_for_bash, write_file, REDIRECT_SCRIPT_GENERIC,
};

pub(super) fn install_standard_hook_scripts(
    hooks_dir: &std::path::Path,
    rewrite_name: &str,
    redirect_name: &str,
) {
    let _ = std::fs::create_dir_all(hooks_dir);

    let binary = resolve_binary_path_for_bash();
    let rewrite_path = hooks_dir.join(rewrite_name);
    let rewrite_script = generate_compact_rewrite_script(&binary);
    write_file(&rewrite_path, &rewrite_script);
    make_executable(&rewrite_path);

    let redirect_path = hooks_dir.join(redirect_name);
    write_file(&redirect_path, REDIRECT_SCRIPT_GENERIC);
    make_executable(&redirect_path);
}

pub(super) fn prepare_project_rules_path(global: bool, file_name: &str) -> Option<PathBuf> {
    let scope = crate::core::config::Config::load().rules_scope_effective();
    if global || scope == crate::core::config::RulesScope::Global {
        println!(
            "Global mode: skipping project-local {file_name} (use without --global in a project)."
        );
        return None;
    }

    let cwd = std::env::current_dir().unwrap_or_default();
    if !is_inside_git_repo(&cwd) || cwd == dirs::home_dir().unwrap_or_default() {
        eprintln!("  Skipping {file_name}: not inside a git repository or in home directory.");
        return None;
    }

    let rules_path = PathBuf::from(file_name);
    if rules_path.exists() {
        let content = std::fs::read_to_string(&rules_path).unwrap_or_default();
        if content.contains("lean-ctx") {
            println!("{file_name} already configured.");
            return None;
        }
    }

    Some(rules_path)
}
