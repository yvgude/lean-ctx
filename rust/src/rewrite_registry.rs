/// Single source of truth for all commands that lean-ctx rewrites/compresses.
/// Used by: hook_handlers (PreToolUse), hooks.rs (bash scripts), cli.rs (shell aliases).
pub const REWRITE_COMMANDS: &[RewriteEntry] = &[
    // Version control
    re("git", Category::Vcs),
    re("gh", Category::Vcs),
    // Rust
    re("cargo", Category::Build),
    // JavaScript/Node
    re("npm", Category::PackageManager),
    re("pnpm", Category::PackageManager),
    re("yarn", Category::PackageManager),
    re("bun", Category::Build),
    re("bunx", Category::Build),
    re("deno", Category::Build),
    re("vite", Category::Build),
    // Python
    re("python", Category::Build),
    re("python3", Category::Build),
    re("pip", Category::PackageManager),
    re("pip3", Category::PackageManager),
    re("uv", Category::PackageManager),
    re("pytest", Category::Build),
    re("mypy", Category::Lint),
    re("ruff", Category::Lint),
    // Go
    re("go", Category::Build),
    re("golangci-lint", Category::Lint),
    // Containers / Infra
    re("docker", Category::Infra),
    re("docker-compose", Category::Infra),
    re("kubectl", Category::Infra),
    re("helm", Category::Infra),
    re("aws", Category::Infra),
    re("terraform", Category::Infra),
    re("tofu", Category::Infra),
    // Linters / Formatters
    re("eslint", Category::Lint),
    re("prettier", Category::Lint),
    re("tsc", Category::Lint),
    re("biome", Category::Lint),
    // HTTP
    re("curl", Category::Http),
    re("wget", Category::Http),
    // PHP
    re("php", Category::Build),
    re("composer", Category::PackageManager),
    // .NET
    re("dotnet", Category::Build),
    // Ruby
    re("bundle", Category::PackageManager),
    re("rake", Category::Build),
    // Elixir
    re("mix", Category::Build),
    // Swift / Zig / CMake
    re("swift", Category::Build),
    re("zig", Category::Build),
    re("cmake", Category::Build),
    re("make", Category::Build),
    // Search (rewritten in hooks to enforce hybrid)
    re("grep", Category::Search),
    re("egrep", Category::Search),
    re("fgrep", Category::Search),
    re("rg", Category::Search),
    // File read alternatives (rewritten to lean-ctx read, not lean-ctx -c)
    re("cat", Category::FileRead),
    re("head", Category::FileRead),
    re("tail", Category::FileRead),
    // Directory listing (rewritten in hooks to enforce hybrid; may fall back to `lean-ctx -c`)
    re("ls", Category::DirList),
    re("find", Category::DirList),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Category {
    Vcs,
    Build,
    PackageManager,
    Lint,
    Infra,
    Http,
    Search,
    FileRead,
    DirList,
}

#[derive(Debug, Clone, Copy)]
pub struct RewriteEntry {
    pub command: &'static str,
    pub category: Category,
}

const fn re(command: &'static str, category: Category) -> RewriteEntry {
    RewriteEntry { command, category }
}

/// Commands eligible for PreToolUse hook rewriting (IDE hooks).
/// Excludes `FileRead` (handled separately in hook_handlers).
pub fn hook_prefixes() -> Vec<String> {
    REWRITE_COMMANDS
        .iter()
        .filter(|e| !matches!(e.category, Category::FileRead))
        .map(|e| format!("{} ", e.command))
        .collect()
}

/// Commands eligible for PreToolUse hook (bare command match, no trailing space).
/// Used for commands like `eslint`, `prettier`, `tsc` that may run without args.
pub fn hook_bare_commands() -> Vec<&'static str> {
    REWRITE_COMMANDS
        .iter()
        .filter(|e| !matches!(e.category, Category::FileRead))
        .map(|e| e.command)
        .collect()
}

/// Check if a command is a file-read alternative (cat/head/tail) that should be
/// rewritten to `lean-ctx read` rather than `lean-ctx -c`.
pub fn is_file_read_command(cmd: &str) -> bool {
    REWRITE_COMMANDS
        .iter()
        .filter(|e| e.category == Category::FileRead)
        .any(|e| {
            let prefix = format!("{} ", e.command);
            cmd.starts_with(&prefix) || cmd == e.command
        })
}

/// All command names for shell alias generation.
pub fn shell_alias_commands() -> Vec<&'static str> {
    REWRITE_COMMANDS.iter().map(|e| e.command).collect()
}

/// Generates a bash `case` pattern for rewrite scripts.
/// e.g. `git\ *|gh\ *|cargo\ *|npm\ *|...`
pub fn bash_case_pattern() -> String {
    REWRITE_COMMANDS
        .iter()
        .filter(|e| !matches!(e.category, Category::FileRead))
        .map(|e| {
            if e.command.contains('-') {
                format!("{}*", e.command.replace('-', r"\-"))
            } else {
                format!(r"{}\ *", e.command)
            }
        })
        .collect::<Vec<_>>()
        .join("|")
}

/// Space-separated list for shell alias arrays.
pub fn shell_alias_list() -> String {
    shell_alias_commands().join(" ")
}

/// Check if a command string matches a rewritable prefix (for hook handlers).
/// Excludes FileRead (handled separately in hook_handlers).
///
/// Handles common shell patterns:
/// - Bare command: `git status`
/// - Env-var prefixed: `PYTHONPATH=src python script.py`
/// - Path-qualified: `./.venv/bin/python`, `/usr/bin/python3`
pub fn is_rewritable_command(cmd: &str) -> bool {
    let effective = strip_env_prefix(cmd);
    let basename = extract_command_basename(effective);

    for entry in REWRITE_COMMANDS {
        if matches!(entry.category, Category::FileRead) {
            continue;
        }
        let prefix = format!("{} ", entry.command);
        if effective.starts_with(&prefix)
            || effective == entry.command
            || basename == entry.command
            || basename.starts_with(&format!("{} ", entry.command))
        {
            return true;
        }
    }
    false
}

/// Strip leading `KEY=value` env-var assignments from a command string.
/// e.g. `PYTHONPATH=src FOO=bar python x.py` → `python x.py`
pub fn strip_env_prefix(cmd: &str) -> &str {
    let mut rest = cmd;
    loop {
        let trimmed = rest.trim_start();
        if let Some(eq_pos) = trimmed.find('=') {
            let before_eq = &trimmed[..eq_pos];
            if !before_eq.is_empty()
                && before_eq
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_')
                && let Some(space_pos) = trimmed[eq_pos..].find(' ')
            {
                rest = &trimmed[eq_pos + space_pos..];
                continue;
            }
        }
        return trimmed;
    }
}

/// Extract the basename of a potentially path-qualified command.
/// e.g. `./.venv/bin/python script.py` → `python script.py`
///      `/usr/local/bin/cargo test` → `cargo test`
fn extract_command_basename(cmd: &str) -> &str {
    let first_space = cmd.find(' ').unwrap_or(cmd.len());
    let cmd_part = &cmd[..first_space];
    if let Some(slash_pos) = cmd_part.rfind('/') {
        &cmd[slash_pos + 1..]
    } else {
        cmd
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_duplicates() {
        let mut seen = std::collections::HashSet::new();
        for entry in REWRITE_COMMANDS {
            assert!(
                seen.insert(entry.command),
                "duplicate command: {}",
                entry.command
            );
        }
    }

    #[test]
    fn hook_prefixes_exclude_search_fileread_dirlist() {
        let prefixes = hook_prefixes();
        assert!(!prefixes.contains(&"cat ".to_string()));
        assert!(!prefixes.contains(&"head ".to_string()));
        assert!(!prefixes.contains(&"tail ".to_string()));
        assert!(prefixes.contains(&"rg ".to_string()));
        assert!(prefixes.contains(&"grep ".to_string()));
        assert!(prefixes.contains(&"egrep ".to_string()));
        assert!(prefixes.contains(&"fgrep ".to_string()));
        assert!(prefixes.contains(&"ls ".to_string()));
        assert!(prefixes.contains(&"find ".to_string()));
        assert!(prefixes.contains(&"git ".to_string()));
        assert!(prefixes.contains(&"cargo ".to_string()));
    }

    #[test]
    fn is_rewritable_matches() {
        assert!(is_rewritable_command("git status"));
        assert!(is_rewritable_command("cargo test --lib"));
        assert!(is_rewritable_command("npm run build"));
        assert!(is_rewritable_command("eslint"));
        assert!(is_rewritable_command("docker-compose up"));
        assert!(is_rewritable_command("bun install"));
        assert!(is_rewritable_command("bunx vitest"));
        assert!(is_rewritable_command("deno test"));
        assert!(is_rewritable_command("vite build"));
        assert!(is_rewritable_command("terraform plan"));
        assert!(is_rewritable_command("make build"));
        assert!(is_rewritable_command("dotnet build"));
        assert!(is_rewritable_command("python script.py"));
        assert!(is_rewritable_command("python3 -m pytest"));
        assert!(is_rewritable_command("uv run test"));
    }

    #[test]
    fn is_rewritable_env_prefix() {
        assert!(is_rewritable_command("PYTHONPATH=src python script.py"));
        assert!(is_rewritable_command("FOO=bar BAZ=1 cargo test"));
        assert!(is_rewritable_command("NODE_ENV=test npm run test"));
        assert!(!is_rewritable_command("FOO=bar echo hello"));
    }

    #[test]
    fn is_rewritable_path_qualified() {
        assert!(is_rewritable_command("./.venv/bin/python script.py"));
        assert!(is_rewritable_command("/usr/bin/python3 test.py"));
        assert!(is_rewritable_command("/usr/local/bin/cargo build"));
        assert!(is_rewritable_command(
            "PYTHONPATH=src ./.venv/bin/python script.py"
        ));
        assert!(!is_rewritable_command("/usr/bin/some-unknown-tool arg"));
    }

    #[test]
    fn is_rewritable_excludes() {
        assert!(!is_rewritable_command("echo hello"));
        assert!(!is_rewritable_command("cd src"));
        assert!(!is_rewritable_command("cat file.rs"));
        assert!(!is_rewritable_command("head -20 file.rs"));
        assert!(is_rewritable_command("rg pattern"));
        assert!(is_rewritable_command("grep -rn pattern src/"));
        assert!(is_rewritable_command("egrep 'foo|bar' file.rs"));
        assert!(is_rewritable_command("fgrep literal file.rs"));
        assert!(is_rewritable_command("ls /tmp"));
        assert!(is_rewritable_command("find . -name '*.rs'"));
    }

    #[test]
    fn strip_env_prefix_works() {
        assert_eq!(strip_env_prefix("python x.py"), "python x.py");
        assert_eq!(strip_env_prefix("FOO=bar python x.py"), "python x.py");
        assert_eq!(strip_env_prefix("A=1 B=2 cargo test"), "cargo test");
        assert_eq!(strip_env_prefix("  FOO=bar cmd"), "cmd");
        assert_eq!(strip_env_prefix("no_equals here"), "no_equals here");
    }

    #[test]
    fn extract_command_basename_works() {
        assert_eq!(extract_command_basename("python x.py"), "python x.py");
        assert_eq!(
            extract_command_basename("./.venv/bin/python x.py"),
            "python x.py"
        );
        assert_eq!(
            extract_command_basename("/usr/bin/python3 -m pytest"),
            "python3 -m pytest"
        );
        assert_eq!(extract_command_basename("cargo test"), "cargo test");
    }

    #[test]
    fn file_read_commands_detected() {
        assert!(is_file_read_command("cat file.rs"));
        assert!(is_file_read_command("head -20 file.rs"));
        assert!(is_file_read_command("tail -n 10 file.rs"));
        assert!(!is_file_read_command("git status"));
        assert!(!is_file_read_command("echo hello"));
    }

    #[test]
    fn shell_alias_list_includes_all() {
        let list = shell_alias_list();
        assert!(list.contains("git"));
        assert!(list.contains("cargo"));
        assert!(list.contains("docker-compose"));
        assert!(list.contains("rg"));
        assert!(list.contains(" ls ") || list.ends_with(" ls"));
        assert!(list.contains("find"));
    }

    #[test]
    fn bash_case_pattern_valid() {
        let pattern = bash_case_pattern();
        assert!(pattern.contains(r"git\ *"));
        assert!(pattern.contains(r"cargo\ *"));
        assert!(pattern.contains(r"rg\ *"));
        assert!(pattern.contains(r"ls\ *"));
    }

    #[test]
    fn hook_prefixes_superset_of_bare_commands() {
        let prefixes = hook_prefixes();
        let bare = hook_bare_commands();
        for cmd in &bare {
            let with_space = format!("{cmd} ");
            assert!(
                prefixes.contains(&with_space),
                "bare command '{cmd}' missing from hook_prefixes"
            );
        }
        assert!(
            !bare.contains(&"cat"),
            "FileRead commands must not be in hook_bare_commands"
        );
    }

    #[test]
    fn shell_aliases_superset_of_hook_commands() {
        let aliases = shell_alias_commands();
        let hook = hook_bare_commands();
        for cmd in &hook {
            assert!(
                aliases.contains(cmd),
                "hook command '{cmd}' missing from shell_alias_commands"
            );
        }
    }

    #[test]
    fn all_categories_represented() {
        let categories: std::collections::HashSet<_> =
            REWRITE_COMMANDS.iter().map(|e| e.category).collect();
        assert!(categories.contains(&Category::Vcs));
        assert!(categories.contains(&Category::Build));
        assert!(categories.contains(&Category::PackageManager));
        assert!(categories.contains(&Category::Lint));
        assert!(categories.contains(&Category::Infra));
        assert!(categories.contains(&Category::Http));
        assert!(categories.contains(&Category::Search));
        assert!(categories.contains(&Category::DirList));
    }

    #[test]
    fn every_command_rewritable_except_fileread() {
        for entry in REWRITE_COMMANDS {
            let cmd = format!("{} --version", entry.command);
            if matches!(entry.category, Category::FileRead) {
                assert!(
                    !is_rewritable_command(&cmd),
                    "{:?} command '{}' should NOT be rewritable via -c wrap",
                    entry.category,
                    entry.command
                );
            } else {
                assert!(
                    is_rewritable_command(&cmd),
                    "command '{}' should be rewritable",
                    entry.command
                );
            }
        }
    }

    #[test]
    fn bash_pattern_has_entry_for_every_hookable_command() {
        let pattern = bash_case_pattern();
        for entry in REWRITE_COMMANDS {
            if matches!(entry.category, Category::FileRead) {
                continue;
            }
            let escaped = if entry.command.contains('-') {
                format!("{}*", entry.command.replace('-', r"\-"))
            } else {
                format!(r"{}\ *", entry.command)
            };
            assert!(
                pattern.contains(&escaped),
                "bash case pattern missing '{}'",
                entry.command
            );
        }
    }
}
