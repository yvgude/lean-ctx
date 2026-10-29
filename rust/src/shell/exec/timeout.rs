use crate::core::config;

const DEFAULT_MAX_BYTES: usize = 8 * 1024 * 1024; // 8 MB
const DEFAULT_TIMEOUT: std::time::Duration = std::time::Duration::from_mins(2);
const HEAVY_MAX_BYTES: usize = 32 * 1024 * 1024; // 32 MB
const HEAVY_TIMEOUT: std::time::Duration = std::time::Duration::from_mins(10);

pub(in crate::shell) fn exec_limits(command: &str) -> (usize, std::time::Duration) {
    let max_bytes = if is_heavy_command(command) {
        HEAVY_MAX_BYTES
    } else {
        DEFAULT_MAX_BYTES
    };
    (max_bytes, shell_timeout(command))
}

/// Resolve the timeout `ctx_shell` / the shell hook grants a command.
///
/// Heavy builds/tests (cargo install/nextest/build, npm ci, git commit/push, …)
/// get the long ceiling instead of being killed at the 2-minute default, keeping
/// the MCP path and the interactive hook consistent. The constants are
/// overridable so operators can pin any value. Precedence (first match wins):
///
/// 1. `LEAN_CTX_SHELL_TIMEOUT_MS` — universal override, in milliseconds.
/// 2. heavy command → `LEAN_CTX_SHELL_HEAVY_TIMEOUT_SECS` / config
///    `shell_heavy_timeout_secs`, else [`HEAVY_TIMEOUT`].
/// 3. normal command → `LEAN_CTX_SHELL_TIMEOUT_SECS` / config
///    `shell_timeout_secs`, else [`DEFAULT_TIMEOUT`].
#[must_use]
pub(crate) fn shell_timeout(command: &str) -> std::time::Duration {
    shell_timeout_with_override(command, None)
}

/// Hard ceiling for a per-call `timeout_ms` override: generous enough for any
/// legitimate build/release job, low enough that a typo'd value cannot wedge
/// the executor for days.
pub(crate) const MAX_CALL_TIMEOUT_MS: u64 = 3_600_000; // 1 hour

/// [`shell_timeout`] with an optional per-call override (ctx_shell's
/// `timeout_ms` arg). Precedence: operator env pin (`LEAN_CTX_SHELL_TIMEOUT_MS`)
/// > per-call override (clamped to [`MAX_CALL_TIMEOUT_MS`], zero ignored)
/// > per-tier env/config > built-in heavy/normal ceilings.
#[must_use]
pub(crate) fn shell_timeout_with_override(
    command: &str,
    override_ms: Option<u64>,
) -> std::time::Duration {
    if let Some(ms) = env_u64("LEAN_CTX_SHELL_TIMEOUT_MS") {
        return std::time::Duration::from_millis(ms);
    }
    if let Some(ms) = override_ms.filter(|n| *n > 0) {
        return std::time::Duration::from_millis(ms.min(MAX_CALL_TIMEOUT_MS));
    }
    if is_heavy_command(command) {
        if let Some(secs) = env_u64("LEAN_CTX_SHELL_HEAVY_TIMEOUT_SECS")
            .or_else(|| config::Config::load().shell_heavy_timeout_secs)
        {
            return std::time::Duration::from_secs(secs);
        }
        HEAVY_TIMEOUT
    } else {
        if let Some(secs) = env_u64("LEAN_CTX_SHELL_TIMEOUT_SECS")
            .or_else(|| config::Config::load().shell_timeout_secs)
        {
            return std::time::Duration::from_secs(secs);
        }
        DEFAULT_TIMEOUT
    }
}

/// Parse a positive `u64` from an env var, ignoring absent/empty/zero/invalid
/// values so the caller falls through to the next precedence tier.
fn env_u64(var: &str) -> Option<u64> {
    std::env::var(var)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|n| *n > 0)
}

/// #1113: detect shell loop constructs that are intentionally long-running.
/// Matches `while true`, `while :`, `while sleep`, `for … in`, and similar
/// patterns used for poll loops and background monitors.
fn is_loop_command(lower: &str) -> bool {
    let trimmed = lower.trim_start();
    trimmed.starts_with("while ")
        || trimmed.starts_with("until ")
        || (trimmed.starts_with("for ") && trimmed.contains(" in "))
}

/// Strip leading `KEY=value` env-var assignments from a command string.
/// Local copy of `rewrite_registry::strip_env_prefix` for decoupling.
fn strip_env_prefix(cmd: &str) -> &str {
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

fn is_heavy_command(command: &str) -> bool {
    let cmd = command.trim();
    let lower = cmd.to_lowercase();
    // #1113: shell loops (`while true; do ... done`, `for i in ...`) are
    // intentionally long-running poll/monitor scripts. Killing them at 120s
    // breaks CI watchers and background monitors.
    if is_loop_command(&lower) {
        return true;
    }
    static HEAVY_PREFIXES: &[&str] = &[
        "cargo build",
        "cargo test",
        "cargo nextest",
        "cargo clippy",
        "cargo check",
        "cargo install",
        "cargo bench",
        "npm run build",
        "npm install",
        "npm ci",
        "pnpm install",
        "pnpm build",
        "yarn install",
        "yarn build",
        "bun install",
        "make",
        "cmake",
        "bazel build",
        "bazel test",
        "gradle build",
        "gradle test",
        "mvn package",
        "mvn install",
        "mvn test",
        "go build",
        "go test",
        "dotnet build",
        "dotnet test",
        "swift build",
        "swift test",
        "flutter build",
        "docker build",
        "docker compose build",
        "pip install",
        "poetry install",
        "uv sync",
        "bundle install",
        "mix compile",
        // Git commands that fire build/test hooks: a `pre-commit` running
        // `cargo clippy` or a `pre-push` running a full preflight can take
        // minutes, far past the 2-minute default. Killing git mid-hook leaves
        // the working tree staged-but-uncommitted and the push half-done, so
        // these get the heavy ceiling. `git status`/`log`/`diff` stay default
        // because the prefix is the full `git <verb>`.
        "git commit",
        "git push",
        // Scripts and test runners that scan repos, run audits, or invoke
        // subprocesses with their own timeouts. Killing them mid-run wastes
        // agent tokens on retries.
        "python3 ",
        "python ",
        "pytest",
        "bash scripts/",
        "sh scripts/",
        "./scripts/",
        // `timeout N cmd` wraps an intentionally long command — respect it.
        "timeout ",
        // Task runners wrap builds/test gates; the underlying job is what's
        // heavy, so the wrapper gets the same ceiling. A fast subcommand
        // (`mise ls`) merely inherits a longer kill deadline — harmless.
        "mise ",
        "just ",
    ];

    let cfg_prefixes = config::Config::load().shell_heavy_prefixes;
    let matches_heavy = |s: &str| {
        HEAVY_PREFIXES.iter().any(|p| s.starts_with(p))
            || cfg_prefixes.iter().any(|p| s.starts_with(p.as_str()))
    };

    if matches_heavy(&lower) {
        return true;
    }

    // Agents prefix commands with `LEAN_CTX_DISABLED=1`, `RUST_LOG=debug`, etc.
    // Strip the assignments so the underlying command can match a heavy prefix.
    let stripped = strip_env_prefix(&lower);
    if stripped != lower && matches_heavy(stripped) {
        return true;
    }

    // Agents often prefix commands with `cd /path && ...` or `cd /path;`.
    // Extract the final segment after the last `&&` or `;` and check that too.
    let final_cmd = lower
        .rsplit_once("&&")
        .or_else(|| lower.rsplit_once(';'))
        .map_or("", |(_, rhs)| rhs.trim());

    if final_cmd.is_empty() {
        return false;
    }
    let final_stripped = strip_env_prefix(final_cmd);
    matches_heavy(final_cmd) || matches_heavy(final_stripped)
}

#[cfg(test)]
mod tests {
    #[test]
    fn heavy_commands_get_higher_byte_limits() {
        // exec_limits owns the byte ceiling; timeout resolution is covered by
        // `shell_timeout_resolves_heavy_normal_and_env_overrides` (which is
        // env/config-isolated, so these stay deterministic regardless of the
        // operator's config.toml).
        for cmd in [
            "cargo build --release",
            "cargo test --lib",
            "cargo nextest run",
            "npm run build",
            "docker build -t myapp .",
            // Git verbs that fire build/test hooks (pre-commit clippy, pre-push
            // preflight) must not be killed at the default ceiling (#854).
            "git commit --amend --no-edit",
            "git push -u origin HEAD",
            // Agents prefix with `cd /path && ...` — heavy detection must
            // look through it to avoid 120s timeout on builds.
            "cd /some/path && cargo test --lib",
            "cd /foo/bar && cargo build --release",
            "cd /workspace; npm ci",
        ] {
            let (bytes, _) = super::exec_limits(cmd);
            assert_eq!(bytes, super::HEAVY_MAX_BYTES, "heavy byte limit for {cmd}");
        }
    }

    #[test]
    fn normal_commands_get_default_byte_limits() {
        // Read-only git verbs stay on the default ceiling — only `commit`/`push`
        // (which fire the cargo-heavy hooks) are promoted.
        for cmd in ["echo hello", "git status", "git log --oneline -5"] {
            let (bytes, _) = super::exec_limits(cmd);
            assert_eq!(
                bytes,
                super::DEFAULT_MAX_BYTES,
                "default byte limit for {cmd}"
            );
        }
    }

    #[test]
    fn env_prefixed_commands_get_correct_heavy_classification() {
        for cmd in [
            "LEAN_CTX_DISABLED=1 cargo test --lib",
            "FOO=bar RUST_LOG=debug cargo test --lib",
            "LEAN_CTX_DISABLED=1 cargo build --release",
            "NODE_ENV=production npm run build",
            "cd /path && LEAN_CTX_DISABLED=1 cargo test --lib",
            "cd /path; FOO=bar npm run build",
            "cargo test --lib",
            "npm run build",
            "go test ./...",
            "cargo clippy --all-features -- -D warnings",
        ] {
            assert!(
                super::is_heavy_command(cmd),
                "expected heavy command: {cmd}"
            );
        }
    }

    #[test]
    fn ordinary_commands_remain_non_heavy_with_or_without_env_prefixes() {
        for cmd in [
            "FOO=bar ls -la",
            "LEAN_CTX_DISABLED=1 echo hello",
            "ls -la",
            "echo hello",
            "cat file.txt",
        ] {
            assert!(
                !super::is_heavy_command(cmd),
                "expected non-heavy command: {cmd}"
            );
        }
    }

    #[test]
    fn shell_timeout_resolves_heavy_normal_and_env_overrides() {
        // Serialize env mutation so this never races other env-reading tests.
        let _lock = crate::core::data_dir::test_env_lock();
        let saved_ms = std::env::var("LEAN_CTX_SHELL_TIMEOUT_MS").ok();
        let saved_secs = std::env::var("LEAN_CTX_SHELL_TIMEOUT_SECS").ok();
        let saved_heavy = std::env::var("LEAN_CTX_SHELL_HEAVY_TIMEOUT_SECS").ok();
        for v in [
            "LEAN_CTX_SHELL_TIMEOUT_MS",
            "LEAN_CTX_SHELL_TIMEOUT_SECS",
            "LEAN_CTX_SHELL_HEAVY_TIMEOUT_SECS",
        ] {
            crate::test_env::remove_var(v);
        }

        // Heavy builds/tests and hook-firing git verbs get the heavy ceiling;
        // read-only verbs stay on the default. Preserves the #854 promotion.
        assert_eq!(
            super::shell_timeout("cargo install --path ."),
            super::HEAVY_TIMEOUT
        );
        assert_eq!(
            super::shell_timeout("cargo nextest run"),
            super::HEAVY_TIMEOUT
        );
        assert_eq!(
            super::shell_timeout("git commit -m 'wip'"),
            super::HEAVY_TIMEOUT
        );
        assert_eq!(
            super::shell_timeout("git push origin main"),
            super::HEAVY_TIMEOUT
        );
        assert_eq!(super::shell_timeout("git status"), super::DEFAULT_TIMEOUT);
        assert_eq!(super::shell_timeout("ls -la"), super::DEFAULT_TIMEOUT);
        // `cd ... && heavy` must resolve to HEAVY so agents don't get killed at 120s.
        assert_eq!(
            super::shell_timeout("cd /some/project && cargo test --lib"),
            super::HEAVY_TIMEOUT
        );
        assert_eq!(
            super::shell_timeout("cd /workspace && cargo build --release"),
            super::HEAVY_TIMEOUT
        );
        assert_eq!(
            super::shell_timeout("cd /app; npm ci"),
            super::HEAVY_TIMEOUT
        );

        // Per-tier env overrides win over the built-in constants. (Non-round
        // second values keep the literals clippy-clean and unambiguous.)
        crate::test_env::set_var("LEAN_CTX_SHELL_HEAVY_TIMEOUT_SECS", "90");
        assert_eq!(
            super::shell_timeout("cargo build"),
            std::time::Duration::from_secs(90)
        );
        crate::test_env::remove_var("LEAN_CTX_SHELL_HEAVY_TIMEOUT_SECS");

        crate::test_env::set_var("LEAN_CTX_SHELL_TIMEOUT_SECS", "30");
        assert_eq!(
            super::shell_timeout("git status"),
            std::time::Duration::from_secs(30)
        );
        crate::test_env::remove_var("LEAN_CTX_SHELL_TIMEOUT_SECS");

        // The universal millisecond override wins over everything.
        crate::test_env::set_var("LEAN_CTX_SHELL_TIMEOUT_MS", "5000");
        assert_eq!(
            super::shell_timeout("cargo build"),
            std::time::Duration::from_secs(5)
        );
        assert_eq!(
            super::shell_timeout("git status"),
            std::time::Duration::from_secs(5)
        );
        crate::test_env::remove_var("LEAN_CTX_SHELL_TIMEOUT_MS");

        for (var, saved) in [
            ("LEAN_CTX_SHELL_TIMEOUT_MS", saved_ms),
            ("LEAN_CTX_SHELL_TIMEOUT_SECS", saved_secs),
            ("LEAN_CTX_SHELL_HEAVY_TIMEOUT_SECS", saved_heavy),
        ] {
            if let Some(v) = saved {
                crate::test_env::set_var(var, v);
            }
        }
    }

    // Task runners (mise/just) wrap builds and test gates that routinely run
    // past the 2-minute default; killing them mid-run loses the whole job.
    // They get the heavy ceiling like the underlying build tools they invoke.
    #[test]
    fn task_runners_get_heavy_ceiling() {
        let _lock = crate::core::data_dir::test_env_lock();
        let saved_ms = std::env::var("LEAN_CTX_SHELL_TIMEOUT_MS").ok();
        let saved_heavy = std::env::var("LEAN_CTX_SHELL_HEAVY_TIMEOUT_SECS").ok();
        crate::test_env::remove_var("LEAN_CTX_SHELL_TIMEOUT_MS");
        crate::test_env::remove_var("LEAN_CTX_SHELL_HEAVY_TIMEOUT_SECS");

        assert_eq!(super::shell_timeout("mise gate"), super::HEAVY_TIMEOUT);
        assert_eq!(super::shell_timeout("mise run gate"), super::HEAVY_TIMEOUT);
        assert_eq!(super::shell_timeout("just build"), super::HEAVY_TIMEOUT);

        if let Some(v) = saved_ms {
            crate::test_env::set_var("LEAN_CTX_SHELL_TIMEOUT_MS", v);
        }
        if let Some(v) = saved_heavy {
            crate::test_env::set_var("LEAN_CTX_SHELL_HEAVY_TIMEOUT_SECS", v);
        }
    }

    #[test]
    fn scripts_and_timeout_get_heavy_ceiling() {
        let _lock = crate::core::data_dir::test_env_lock();
        let saved_ms = std::env::var("LEAN_CTX_SHELL_TIMEOUT_MS").ok();
        let saved_heavy = std::env::var("LEAN_CTX_SHELL_HEAVY_TIMEOUT_SECS").ok();
        crate::test_env::remove_var("LEAN_CTX_SHELL_TIMEOUT_MS");
        crate::test_env::remove_var("LEAN_CTX_SHELL_HEAVY_TIMEOUT_SECS");

        assert_eq!(
            super::shell_timeout("python3 scripts/audit.py"),
            super::HEAVY_TIMEOUT
        );
        assert_eq!(
            super::shell_timeout("python scripts/gate.py --root ."),
            super::HEAVY_TIMEOUT
        );
        assert_eq!(super::shell_timeout("pytest tests/"), super::HEAVY_TIMEOUT);
        assert_eq!(
            super::shell_timeout("bash scripts/loc-gate.sh"),
            super::HEAVY_TIMEOUT
        );
        assert_eq!(
            super::shell_timeout("sh scripts/run.sh"),
            super::HEAVY_TIMEOUT
        );
        assert_eq!(
            super::shell_timeout("./scripts/deploy.sh"),
            super::HEAVY_TIMEOUT
        );
        assert_eq!(
            super::shell_timeout("timeout 300 python3 audit.py"),
            super::HEAVY_TIMEOUT
        );

        assert_eq!(
            super::shell_timeout("cd /repo && python3 gate.py"),
            super::HEAVY_TIMEOUT
        );
        assert_eq!(
            super::shell_timeout("cd /repo && timeout 600 make"),
            super::HEAVY_TIMEOUT
        );

        if let Some(v) = saved_ms {
            crate::test_env::set_var("LEAN_CTX_SHELL_TIMEOUT_MS", v);
        }
        if let Some(v) = saved_heavy {
            crate::test_env::set_var("LEAN_CTX_SHELL_HEAVY_TIMEOUT_SECS", v);
        }
    }

    // Per-call `timeout_ms` (ctx_shell tool arg): explicit caller intent beats
    // the built-in tiers in both directions, absurd values clamp to the 1h
    // ceiling, zero is ignored, and the operator's universal env pin stays top.
    #[test]
    fn per_call_timeout_override_resolves_and_clamps() {
        let _lock = crate::core::data_dir::test_env_lock();
        let saved_ms = std::env::var("LEAN_CTX_SHELL_TIMEOUT_MS").ok();
        crate::test_env::remove_var("LEAN_CTX_SHELL_TIMEOUT_MS");

        assert_eq!(
            super::shell_timeout_with_override("git status", Some(300_000)),
            std::time::Duration::from_mins(5)
        );
        assert_eq!(
            super::shell_timeout_with_override("cargo build", Some(30_000)),
            std::time::Duration::from_secs(30)
        );
        assert_eq!(
            super::shell_timeout_with_override("git status", Some(999_000_000)),
            std::time::Duration::from_millis(super::MAX_CALL_TIMEOUT_MS)
        );
        assert_eq!(
            super::shell_timeout_with_override("git status", Some(0)),
            super::DEFAULT_TIMEOUT
        );
        assert_eq!(
            super::shell_timeout_with_override("git status", None),
            super::DEFAULT_TIMEOUT
        );

        crate::test_env::set_var("LEAN_CTX_SHELL_TIMEOUT_MS", "5000");
        assert_eq!(
            super::shell_timeout_with_override("git status", Some(300_000)),
            std::time::Duration::from_secs(5)
        );
        crate::test_env::remove_var("LEAN_CTX_SHELL_TIMEOUT_MS");
        if let Some(v) = saved_ms {
            crate::test_env::set_var("LEAN_CTX_SHELL_TIMEOUT_MS", v);
        }
    }
}
