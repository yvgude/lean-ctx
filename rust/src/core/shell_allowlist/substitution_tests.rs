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
