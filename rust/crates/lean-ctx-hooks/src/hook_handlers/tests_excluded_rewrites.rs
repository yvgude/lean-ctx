// SPDX-License-Identifier: Apache-2.0
//! #2194: `hook rewrite` honours the user's `excluded_commands` (and the
//! `redirect_exclude` paths) exactly like the `lean-ctx -c` shell path.
use super::*;

const SECURITY_ENV: [&str; 3] = [
    "LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE",
    "LEAN_CTX_SHELL_SECURITY",
    "LEAN_CTX_HOOK_EXCLUDE",
];

/// Runs `body` with an isolated config dir (never the real HOME's config), a
/// fixed allowlist, and `write_config` as the means to change `config.toml`
/// between calls. The env is removed before the caller asserts.
pub(crate) fn with_isolated_config<T>(body: impl FnOnce(&dyn Fn(&str)) -> T) -> T {
    let _iso = lean_ctx::core::data_dir::isolated_data_dir();
    lean_ctx::test_env::remove_var("LEAN_CTX_HOOK_EXCLUDE");
    lean_ctx::test_env::set_var(
        "LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE",
        "git,cargo,npm,head,tail,grep,wc,cat,rg,echo,cd,ls",
    );
    lean_ctx::test_env::set_var("LEAN_CTX_SHELL_SECURITY", "enforce");
    let path = lean_ctx::core::config::Config::path().expect("isolated config path");
    std::fs::create_dir_all(path.parent().expect("config dir")).expect("create config dir");
    let write_config = |toml: &str| std::fs::write(&path, toml).expect("write config");
    write_config("");
    let out = body(&write_config);
    for var in SECURITY_ENV {
        lean_ctx::test_env::remove_var(var);
    }
    out
}

/// The hook's stdout for a Cursor-shaped shell call (no Claude/Gemini
/// permission mirroring, so a read-only rewrite is always delivered).
fn rewrite_hook(cmd: &str) -> String {
    let payload = serde_json::json!({
        "tool_name": "Shell",
        "tool_input": { "command": cmd },
    });
    file_rewrite::compute_rewrite_for(&payload.to_string(), "lean-ctx")
}

fn is_rewritten(out: &str) -> bool {
    out.contains("updated_input")
}

#[test]
fn excluded_command_passes_through_untouched_and_others_still_rewrite() {
    let (excluded, chained, other) = with_isolated_config(|write_config| {
        write_config("excluded_commands = [\"git\"]\n");
        (
            rewrite_hook("git status"),
            rewrite_hook("cd src && git log --oneline -3"),
            rewrite_hook("cat src/main.rs"),
        )
    });
    assert_eq!(excluded, "{}", "an excluded command stays untouched");
    assert_eq!(chained, "{}", "an excluded segment keeps the chain native");
    assert!(
        is_rewritten(&other),
        "a command not excluded is rewritten as before: {other}"
    );
}

#[test]
fn config_change_takes_effect_on_the_next_hook_call() {
    let (before, excluded, after) = with_isolated_config(|write_config| {
        let before = rewrite_hook("git status");
        write_config("excluded_commands = [\"git\"]\n");
        let excluded = rewrite_hook("git status");
        write_config("excluded_commands = []\n");
        (before, excluded, rewrite_hook("git status"))
    });
    assert!(is_rewritten(&before), "no exclusion: rewritten: {before}");
    assert_eq!(excluded, "{}", "the new exclusion applies at once");
    assert!(
        is_rewritten(&after),
        "the exclusion removed: rewritten again: {after}"
    );
}

#[test]
fn excluded_command_still_meets_the_allowlist() {
    let out = with_isolated_config(|write_config| {
        write_config("excluded_commands = [\"kubectl\"]\n");
        rewrite_hook("kubectl get pods")
    });
    assert!(
        out.contains("\"deny\"") && !out.contains("updated_input"),
        "an exclusion skips the rewrite, never the allowlist: {out}"
    );
}

#[test]
fn redirect_exclude_paths_are_not_rewritten_as_shell_reads() {
    let (excluded, other) = with_isolated_config(|write_config| {
        write_config("redirect_exclude = [\"docs/**\"]\n");
        (
            rewrite_hook("cat docs/notes.md"),
            rewrite_hook("cat src/main.rs"),
        )
    });
    assert_eq!(excluded, "{}", "a redirect_exclude path stays native");
    assert!(is_rewritten(&other), "other paths still rewrite: {other}");
}

/// r1: a line break separates commands, so an excluded command on a later
/// line keeps the whole call native, and a `redirect_exclude` read on a later
/// line does too.
#[test]
fn an_excluded_command_on_a_later_line_is_not_rewritten() {
    let candidate = |cmd: &str| file_rewrite::rewrite_candidate(cmd, "lean-ctx");
    let (before, command, hook, read) = with_isolated_config(|write_config| {
        let before = (
            candidate("cargo test\ngit status"),
            candidate("cargo test\ncat docs/notes.md"),
        );
        write_config("excluded_commands = [\"git\"]\nredirect_exclude = [\"docs/**\"]\n");
        (
            before,
            candidate("cargo test\ngit status"),
            rewrite_hook("ls\ngit status"),
            candidate("cargo test\ncat docs/notes.md"),
        )
    });
    assert!(
        before.0.is_some(),
        "no exclusion: the line is wrapped: {before:?}"
    );
    assert!(
        before.1.is_some(),
        "no exclusion: the read is wrapped: {before:?}"
    );
    assert_eq!(command, None, "git on line 2 keeps the line native");
    assert!(!is_rewritten(&hook), "the hook leaves it untouched: {hook}");
    assert_eq!(read, None, "an excluded read on line 2 stays native");
}

#[test]
fn excluded_commands_match_like_the_shell_path() {
    let excluded = vec!["git".to_string(), "Docker Compose".to_string()];
    for cmd in [
        "git",
        "git status",
        "GIT status",
        "GIT_DIR=.git git log",
        "cd wt && git diff",
        "echo x | git hash-object --stdin",
        "docker compose up",
        "cargo test\ngit status",
        "cargo test\r\n  GIT_DIR=.git git log",
        "cd wt\n\ncat x | git hash-object --stdin",
    ] {
        assert!(
            exclusions::names_excluded_command(cmd, &excluded),
            "{cmd} is excluded"
        );
    }
    // The whole-command cases are the shell path's own verdict.
    for cmd in ["git", "git status", "GIT status", "docker compose up"] {
        assert_eq!(
            lean_ctx::shell::output_policy::classify(cmd, &excluded),
            lean_ctx::shell::output_policy::OutputPolicy::Passthrough,
            "{cmd}: the shell path excludes it too"
        );
    }
    for cmd in [
        "gitk",
        "cargo test",
        "echo git",
        "docker ps",
        "legit status",
        "cargo test\nls",
    ] {
        assert!(
            !exclusions::names_excluded_command(cmd, &excluded),
            "{cmd} is not excluded"
        );
    }
    assert!(!exclusions::names_excluded_command("git status", &[]));
}
