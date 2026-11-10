// SPDX-License-Identifier: Apache-2.0
//! Tests for comment stripping in the shell allowlist (#1109).

use super::strip_comments;

#[test]
fn strip_comments_drops_standalone_comment_line() {
    assert_eq!(
        strip_comments("cd /repo\n# drop the block\nsed s/a/b/ f"),
        "cd /repo\n\nsed s/a/b/ f"
    );
}

#[test]
fn strip_comments_drops_trailing_and_post_operator_comment() {
    assert_eq!(strip_comments("ls -la # list files"), "ls -la ");
    assert_eq!(strip_comments("ls;# c"), "ls;");
    assert_eq!(strip_comments("ls |# c"), "ls |");
}

#[test]
fn strip_comments_keeps_hash_inside_quotes() {
    assert_eq!(
        strip_comments("echo \"# not a comment\""),
        "echo \"# not a comment\""
    );
    assert_eq!(strip_comments("echo '#nope'"), "echo '#nope'");
}

#[test]
fn strip_comments_keeps_hash_in_words_and_expansions() {
    assert_eq!(strip_comments("echo ${#arr}"), "echo ${#arr}");
    assert_eq!(strip_comments("echo ${v#pre}"), "echo ${v#pre}");
    assert_eq!(strip_comments("echo $((16#ff))"), "echo $((16#ff))");
    assert_eq!(
        strip_comments("curl http://h/p#frag"),
        "curl http://h/p#frag"
    );
    assert_eq!(strip_comments("echo a\\#b"), "echo a\\#b");
}

#[test]
fn comment_line_between_commands_does_not_block() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    lean_ctx::test_env::set_var("LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE", "cd,sed,grep");
    let cmd = "cd /repo\n# drop the conflict block\nsed s/a/b/ f\ngrep -c x f";
    let result = super::super::enforce_shell_allowlist(cmd);
    lean_ctx::test_env::remove_var("LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE");
    assert!(
        result.is_ok(),
        "a comment line must not be treated as a command: {result:?}"
    );
}

#[test]
fn commented_out_command_is_not_executed_but_does_not_leak() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    lean_ctx::test_env::set_var("LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE", "ls");
    let result = super::super::enforce_shell_allowlist("ls # rm -rf /");
    lean_ctx::test_env::remove_var("LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE");
    assert!(
        result.is_ok(),
        "trailing comment must not block: {result:?}"
    );
}
