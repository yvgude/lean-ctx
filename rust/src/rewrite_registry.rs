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
    re("pip", Category::PackageManager),
    re("pip3", Category::PackageManager),
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
    // Search (only in shell aliases, NOT in hook rewrite to avoid overriding native tools)
    re("rg", Category::Search),
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
/// Excludes `rg` (search tools should use native Read/Grep in IDEs).
pub fn hook_prefixes() -> Vec<String> {
    REWRITE_COMMANDS
        .iter()
        .filter(|e| e.category != Category::Search)
        .map(|e| format!("{} ", e.command))
        .collect()
}

/// Commands eligible for PreToolUse hook (bare command match, no trailing space).
/// Used for commands like `eslint`, `prettier`, `tsc` that may run without args.
pub fn hook_bare_commands() -> Vec<&'static str> {
    REWRITE_COMMANDS
        .iter()
        .filter(|e| e.category != Category::Search)
        .map(|e| e.command)
        .collect()
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
        .filter(|e| e.category != Category::Search)
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
pub fn is_rewritable_command(cmd: &str) -> bool {
    for entry in REWRITE_COMMANDS {
        if entry.category == Category::Search {
            continue;
        }
        let prefix = format!("{} ", entry.command);
        if cmd.starts_with(&prefix) || cmd == entry.command {
            return true;
        }
    }
    false
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
    fn hook_prefixes_exclude_search() {
        let prefixes = hook_prefixes();
        assert!(!prefixes.contains(&"rg ".to_string()));
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
    }

    #[test]
    fn is_rewritable_excludes() {
        assert!(!is_rewritable_command("echo hello"));
        assert!(!is_rewritable_command("cd src"));
        assert!(!is_rewritable_command("rg pattern"));
    }

    #[test]
    fn shell_alias_list_includes_all() {
        let list = shell_alias_list();
        assert!(list.contains("git"));
        assert!(list.contains("cargo"));
        assert!(list.contains("docker-compose"));
        assert!(list.contains("rg"));
    }

    #[test]
    fn bash_case_pattern_valid() {
        let pattern = bash_case_pattern();
        assert!(pattern.contains(r"git\ *"));
        assert!(pattern.contains(r"cargo\ *"));
        assert!(
            !pattern.contains(r"rg\ *"),
            "rg should not be in hook case pattern"
        );
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
    }

    #[test]
    fn every_command_rewritable_except_search() {
        for entry in REWRITE_COMMANDS {
            let cmd = format!("{} --version", entry.command);
            if entry.category == Category::Search {
                assert!(
                    !is_rewritable_command(&cmd),
                    "search command '{}' should NOT be rewritable",
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
    fn bash_pattern_has_entry_for_every_non_search_command() {
        let pattern = bash_case_pattern();
        for entry in REWRITE_COMMANDS {
            if entry.category == Category::Search {
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
