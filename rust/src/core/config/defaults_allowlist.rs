//! Default shell command allowlist.
//!
//! The curated set of executables that lean-ctx permits by default in
//! restricted shell mode. Kept in its own module so the (long, frequently
//! reviewed) data table does not bloat `config/mod.rs`. Users extend this
//! additively via `shell_allowlist_extra` / `lean-ctx allow`.

pub(crate) fn default_shell_allowlist() -> Vec<String> {
    [
        // VCS
        "git",
        "gh",
        "svn",
        "hg",
        // Build tools
        "cargo",
        "npm",
        "npx",
        "yarn",
        "pnpm",
        "bun",
        "bunx",
        "make",
        "cmake",
        "pip",
        "pip3",
        "poetry",
        "uv",
        "go",
        "mvn",
        "gradle",
        "mix",
        "dotnet",
        "swift",
        "zig",
        "rustup",
        "rustc",
        "deno",
        "bazel",
        // C/C++ compilers (compile-only; running the produced binary stays gated,
        // exactly like rustc/go above). A coding agent that compiles an ad-hoc
        // reproducer with `gcc repro.c` should not need an explicit opt-in (#361).
        "gcc",
        "cc",
        "clang",
        "g++",
        "c++",
        "clang++",
        // Package managers
        "pipenv",
        "conda",
        "mamba",
        "brew",
        "apt",
        "apt-get",
        "apk",
        "nix",
        // Common CLI
        "ls",
        "cat",
        "head",
        "tail",
        "wc",
        "sort",
        "uniq",
        "tr",
        "cut",
        "grep",
        "rg",
        "find",
        "fd",
        "ag",
        "ack",
        "sed",
        "awk",
        "echo",
        "printf",
        "true",
        "false",
        "test",
        // #855: `[` is `test`'s bracket-form alias — same builtin, same zero
        // external-execution surface, but was missing here even though `test`
        // itself was already allowlisted. `break`/`continue`/`return` are pure
        // loop/function control-flow builtins (no external process, no I/O) —
        // an `if …; then break; fi` inside an otherwise-allowlisted loop body
        // shouldn't need its own `lean-ctx allow` round-trip.
        "[",
        "break",
        "continue",
        "return",
        "expr",
        // #855: `seq` is the standard way to drive a bounded numeric `for`
        // loop (`for i in $(seq 1 10)`) — a common, safe idiom.
        "seq",
        "cd",
        "pwd",
        "basename",
        "dirname",
        "realpath",
        "readlink",
        "cp",
        "mv",
        "mkdir",
        "rm",
        "rmdir",
        "touch",
        "ln",
        "chmod",
        "chown",
        "diff",
        "patch",
        "tar",
        "zip",
        "unzip",
        "gzip",
        "gunzip",
        "zstd",
        "curl",
        "wget",
        "tree",
        "du",
        "df",
        "ps",
        "lsof",
        "watch",
        "tee",
        "less",
        "more",
        "id",
        "whoami",
        "uname",
        "hostname",
        // Dev tools
        // docker/podman removed from default: mount-based PathJail bypass risk
        // Add explicitly if needed: shell_allowlist = [..., "docker"]
        "node",
        "python",
        "python3",
        "ruby",
        "perl",
        "java",
        "javac",
        "tsc",
        "eslint",
        "prettier",
        "black",
        "ruff",
        "clippy",
        "jq",
        "yq",
        "which",
        "type",
        "file",
        "stat",
        "date",
        "sleep",
        "timeout",
        "nice",
        "ionice",
        "xargs",
        "env",
        "nohup",
        // Testing frameworks
        "pytest",
        "py.test",
        "jest",
        "vitest",
        "mocha",
        "cypress",
        "playwright",
        "puppeteer",
        // Pre-commit & git hooks
        "pre-commit",
        "husky",
        "lint-staged",
        "lefthook",
        "overcommit",
        "commitlint",
        // Linters & formatters
        "mypy",
        "pyright",
        "pylint",
        "flake8",
        "bandit",
        "isort",
        "autopep8",
        "yapf",
        "golangci-lint",
        "shellcheck",
        "markdownlint",
        "stylelint",
        // Bundlers & dev servers
        "webpack",
        "vite",
        "esbuild",
        "rollup",
        "turbo",
        "nx",
        "lerna",
        "next",
        "nuxt",
        // Ruby ecosystem
        "bundle",
        "bundler",
        "rake",
        "rails",
        "rspec",
        "rubocop",
        // PHP ecosystem
        "php",
        "composer",
        "phpunit",
        "artisan",
        // Mobile
        "flutter",
        "dart",
        "xcodebuild",
        "xcrun",
        "pod",
        "fastlane",
        // Cloud & infra tools are NOT in the defaults — see `cloud_infra_commands()`.
        // They mutate production infrastructure with ambient credentials; an agent
        // gets them only by explicit opt-in (`lean-ctx allow <cmd>`).
        // Database
        "psql",
        "mysql",
        "sqlite3",
        "mongosh",
        "redis-cli",
        "pg_dump",
        "pg_restore",
        "mysqldump",
        // JVM ecosystem
        "scala",
        "sbt",
        "kotlin",
        "kotlinc",
        // Elixir
        "elixir",
        "iex",
        // lean-ctx itself
        "lean-ctx",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect()
}

/// Cloud & infrastructure CLIs that mutate remote/production state using
/// ambient credentials (kubeconfig, AWS profiles, service principals, …).
///
/// Deliberately excluded from [`default_shell_allowlist`]: a coding agent that
/// can run `terraform apply` or `kubectl delete` by default is an incident
/// waiting to happen. Users opt in per tool via `lean-ctx allow <cmd>` or
/// `shell_allowlist_extra`. The block message points there
/// (see `shell_allowlist::allowlist_block_message`).
pub(crate) fn cloud_infra_commands() -> &'static [&'static str] {
    &[
        "terraform",
        "ansible",
        "kubectl",
        "helm",
        "az",
        "aws",
        "gcloud",
        "firebase",
        "heroku",
        "vercel",
        "netlify",
        "fly",
        "wrangler",
        "pulumi",
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    // P0-9 (#421): cloud/infra mutation tools must be opt-in, never default.
    #[test]
    fn cloud_infra_tools_are_not_in_the_default_allowlist() {
        let defaults = default_shell_allowlist();
        for tool in cloud_infra_commands() {
            assert!(
                !defaults.contains(&(*tool).to_string()),
                "{tool} must not be in the default allowlist"
            );
        }
    }

    #[test]
    fn dev_essentials_remain_in_the_default_allowlist() {
        let defaults = default_shell_allowlist();
        for tool in ["git", "cargo", "npm", "rm", "psql", "lean-ctx"] {
            assert!(
                defaults.contains(&tool.to_string()),
                "{tool} must stay in the default allowlist"
            );
        }
    }

    // #361: a coding agent must be able to compile an ad-hoc C/C++ reproducer
    // (`gcc repro.c`) without an explicit opt-in, like the other compilers.
    #[test]
    fn c_and_cpp_compilers_are_in_the_default_allowlist() {
        let defaults = default_shell_allowlist();
        for tool in ["gcc", "cc", "clang", "g++", "c++", "clang++"] {
            assert!(
                defaults.contains(&tool.to_string()),
                "{tool} must be in the default allowlist"
            );
        }
    }

    #[test]
    fn no_duplicates_in_default_allowlist() {
        let defaults = default_shell_allowlist();
        let mut sorted = defaults.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), defaults.len());
    }
}
