// SPDX-License-Identifier: Apache-2.0
//! A `#` comment is read the way the shell reads it: it starts only at the
//! start of a word, outside quotes, and runs to the end of its line. A quote,
//! backtick, `$(` or backslash inside it is comment text, so it must never
//! join the next line into the commented one. A `#` inside a word, an
//! expansion or quotes is no comment and keeps its old meaning.

use super::*;

fn list() -> Allowlist {
    allow(&["echo", "rm", "trap", "alias", "true"])
}

/// Every line below runs a write into lean-ctx's state on its own line,
/// after a comment.
const AFTER_COMMENT: &[(&str, &str)] = &[
    (
        "apostrophe",
        "d=~/.config/lean-ctx; echo x # it's\nrm -rf $d",
    ),
    (
        "double quote",
        "d=~/.config/lean-ctx; echo x # say \"hi\nrm -rf $d",
    ),
    (
        "backtick",
        "d=~/.config/lean-ctx; echo x # see `x\nrm -rf $d",
    ),
    (
        "command substitution",
        "d=~/.config/lean-ctx; echo x # $(\nrm -rf $d",
    ),
    (
        "backslash",
        "d=~/.config/lean-ctx; echo x # dir\\\nrm -rf $d",
    ),
    (
        "after a semicolon",
        "d=~/.config/lean-ctx; echo x;#'\nrm -rf $d",
    ),
    (
        "crlf",
        "d=~/.config/lean-ctx; echo x # it's\r\nrm -rf $d\r\n",
    ),
    ("literal path", "echo x # it's\nrm -rf ~/.config/lean-ctx"),
];

fn refuses_both_modes(cases: &[(&str, &str)]) {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    for (name, cmd) in cases {
        assert!(
            check_unconditional_blocked_only(cmd).is_err(),
            "{name}: must refuse without an allowlist: {cmd:?}"
        );
        assert!(
            check_all_segments(cmd, &list()).is_err(),
            "{name}: must refuse with an allowlist: {cmd:?}"
        );
    }
}

#[test]
fn comment_with_apostrophe_does_not_hide_the_next_line() {
    refuses_both_modes(&AFTER_COMMENT[..1]);
}

#[test]
fn comment_with_double_quote_does_not_hide_the_next_line() {
    refuses_both_modes(&AFTER_COMMENT[1..2]);
}

#[test]
fn comment_with_backtick_does_not_hide_the_next_line() {
    refuses_both_modes(&AFTER_COMMENT[2..3]);
}

#[test]
fn comment_with_command_substitution_does_not_hide_the_next_line() {
    refuses_both_modes(&AFTER_COMMENT[3..4]);
}

#[test]
fn comment_ending_in_backslash_does_not_join_the_next_line() {
    refuses_both_modes(&AFTER_COMMENT[4..5]);
}

#[test]
fn comment_right_after_a_semicolon_does_not_hide_the_next_line() {
    refuses_both_modes(&AFTER_COMMENT[5..6]);
}

#[test]
fn comment_before_crlf_does_not_hide_the_next_line() {
    refuses_both_modes(&AFTER_COMMENT[6..7]);
}

#[test]
fn comment_does_not_hide_a_literal_state_path_on_the_next_line() {
    refuses_both_modes(&AFTER_COMMENT[7..8]);
}

/// A comment that ends the input, with no newline after it: the command
/// before it is still judged, and a lone comment stays harmless.
#[test]
fn comment_at_the_end_of_input_without_newline() {
    refuses_both_modes(&[
        ("trailing", "d=~/.config/lean-ctx; rm -rf $d # it's"),
        (
            "trailing after a line",
            "echo x # it's\nrm -rf ~/.config/lean-ctx # it's",
        ),
    ]);
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    for cmd in ["echo x # it's", "echo x # \"`$(\\", "true;#'"] {
        assert!(check_unconditional_blocked_only(cmd).is_ok(), "{cmd:?}");
        assert!(check_all_segments(cmd, &list()).is_ok(), "{cmd:?}");
    }
}

/// The real lines that reach the per-segment checks without the line's own
/// comment strip: a `trap` action and an alias body in both modes, and a
/// `case` inside a backtick substitution.
#[test]
fn comment_inside_a_nested_script_does_not_hide_a_command() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    lean_ctx::test_env::set_var("LEAN_CTX_SHELL_SECURITY", "enforce");
    for list in ["", "echo,rm,trap,alias"] {
        lean_ctx::test_env::set_var("LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE", list);
        let cases = [
            "trap \"echo x # it's\nrm -rf ~/.config/lean-ctx\" EXIT",
            "echo \"`case x in a) echo # it's\nrm -rf ~/.config/lean-ctx;; esac`\"",
            "alias a=\"echo x # it's\nrm -rf ~/.config/lean-ctx\"",
        ];
        for cmd in cases {
            let verdict = enforce_shell_allowlist(cmd);
            assert!(verdict.is_err(), "allowlist {list:?}: {cmd:?}: {verdict:?}");
        }
    }
    lean_ctx::test_env::remove_var("LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE");
    lean_ctx::test_env::remove_var("LEAN_CTX_SHELL_SECURITY");
}

/// Fail closed: a script with a comment whose quoting the shells read
/// differently is refused, not split by a guess.
#[test]
fn comment_in_a_script_the_lexer_cannot_read_is_refused() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    let cmd = "echo x # it's\necho $'a\\'b' # c";
    assert!(check_unconditional_blocked_only(cmd).is_err(), "{cmd:?}");
    assert!(check_all_segments(cmd, &list()).is_err(), "{cmd:?}");
}

/// A `#` that starts no comment keeps its meaning: it hides nothing after
/// it, and a line with only such a `#` still passes.
#[test]
fn hash_that_starts_no_comment_keeps_its_meaning() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    for word in [
        "a#b",
        "${#x}",
        "$#",
        "${x#'y'}",
        "${x#y}",
        "'#'",
        "'# it'\"'\"'s'",
        "\"# it's\"",
        "\"a#b\"",
    ] {
        let harmless = format!("echo {word}");
        assert!(
            check_unconditional_blocked_only(&harmless).is_ok(),
            "{harmless:?}"
        );
        assert!(
            check_all_segments(&harmless, &list()).is_ok(),
            "{harmless:?}"
        );
        for sep in ["; ", "\n"] {
            let write = format!("echo {word}{sep}rm -rf ~/.config/lean-ctx");
            assert!(
                check_unconditional_blocked_only(&write).is_err(),
                "{write:?}"
            );
            assert!(check_all_segments(&write, &list()).is_err(), "{write:?}");
        }
    }
}
