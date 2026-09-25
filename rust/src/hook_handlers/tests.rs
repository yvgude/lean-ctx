//! Tests for hook handlers. Extracted from `hook_handlers/mod.rs`;
//! `super::*` resolves to the `hook_handlers` module.

use super::*;

fn expect_wrapped(cmd: &str, binary: &str) -> String {
    crate::shell::join_command_for(&[binary.to_owned(), "-c".to_owned(), cmd.to_owned()], "-c")
}

/// Pins a deterministic shell allowlist while `body` runs, so the `passes_enforced`
/// gate now consulted by [`build_rewrite_compound`] never depends on the
/// developer's `config.toml`. `git/cargo/npm/head/grep/wc/cat/rg/echo/cd/ls` are
/// allowed; `python3` and `kubectl` are deliberately absent so the tricky-sink
/// branch (left raw for the agent shell, #589) is exercised. Serialized via the
/// shared test lock; the env is removed before the caller asserts so a failed
/// assertion can never leak it into another test.
fn with_test_allowlist<T>(body: impl FnOnce() -> T) -> T {
    let _lock = crate::core::data_dir::test_env_lock();
    crate::test_env::set_var(
        "LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE",
        "git,cargo,npm,head,grep,wc,cat,rg,echo,cd,ls",
    );
    let out = body();
    crate::test_env::remove_var("LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE");
    out
}

#[cfg(test)]
#[path = "tests_rewrite_extras.rs"]
mod rewrite_extras;
#[cfg(test)]
#[path = "tests_command_rewrites.rs"]
mod tests_command_rewrites;
#[cfg(test)]
#[path = "tests_file_rewrites.rs"]
mod tests_file_rewrites;
#[cfg(test)]
#[path = "tests_parsing_platform.rs"]
mod tests_parsing_platform;
#[cfg(test)]
#[path = "tests_redirects.rs"]
mod tests_redirects;
#[cfg(test)]
#[path = "tests_search_rewrites.rs"]
mod tests_search_rewrites;
