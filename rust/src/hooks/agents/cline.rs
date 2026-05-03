use super::super::{resolve_binary_path, write_file};
use super::shared::prepare_project_rules_path;

pub(crate) fn install_cline_rules(global: bool) {
    let Some(rules_path) = prepare_project_rules_path(global, ".clinerules") else {
        return;
    };

    let binary = resolve_binary_path();
    let rules = format!(
        r"# lean-ctx Shell Optimization
# Prefix all shell commands with `{binary} -c` for 60-90% token savings.

When running shell commands, always prefix them with `{binary} -c` to compress output. For example:
- `{binary} -c git status` instead of `git status`
- `{binary} -c cargo test` instead of `cargo test`
- `{binary} -c ls src/` instead of `ls src/`

Supported commands: git, cargo, npm, pnpm, docker, kubectl, pip, ruff, go, curl, grep, find, ls, aws, helm, and 90+ more.
"
    );

    write_file(&rules_path, &rules);
    println!("Installed .clinerules in current project.");
}
