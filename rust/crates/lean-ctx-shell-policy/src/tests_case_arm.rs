// SPDX-License-Identifier: Apache-2.0
//! A `case` arm ends at a `;;` or `esac` the shell reads as syntax. One inside
//! a `#` comment or a here-document body is text, so an arm holding either is
//! refused rather than split there. A `case` in a substitution is read both
//! as written and without its comments, and the bodies of both readings are
//! gated, also inside the scripts other commands run.

use super::*;

/// A write into lean-ctx's state: refused in both modes.
const WRITE: &str = "rm -rf ~/.config/lean-ctx";

fn with_mode(list: &str, f: impl FnOnce()) {
    lean_ctx::test_env::set_var("LEAN_CTX_SHELL_SECURITY", "enforce");
    lean_ctx::test_env::set_var("LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE", list);
    f();
    lean_ctx::test_env::remove_var("LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE");
    lean_ctx::test_env::remove_var("LEAN_CTX_SHELL_SECURITY");
}

const LISTS: [&str; 2] = ["", "echo,rm,trap,alias,true,cat,watch,git,ls"];

fn refused_in(lists: &[&str], templates: &[&str]) {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    for list in lists {
        with_mode(list, || {
            for template in templates {
                let cmd = template.replace("{W}", WRITE);
                let verdict = enforce_shell_allowlist(&cmd);
                assert!(verdict.is_err(), "allowlist {list:?}: {cmd:?}: {verdict:?}");
            }
        });
    }
}

#[test]
fn case_arm_with_comment_is_not_split_inside_it() {
    for input in [
        "case x in a) echo # ;;\nls ;; esac",
        "echo $(case x in a) echo # ;;\nls ;; esac)",
        "echo `case x in a) echo # ;;\nls ;; esac`",
        "case x in a) echo hi;# ;;\nls ;; esac",
    ] {
        assert!(rewrite_case_constructs(input).is_err(), "{input:?}");
    }
}

#[test]
fn case_arm_with_here_document_is_not_split_inside_it() {
    for input in [
        "case x in a) cat <<E\n;;\nE\nls ;; esac",
        "case x in a) cat <<-E\n;;\nE\nls ;; esac",
        "echo $(case x in a) cat <<E\n;;\nE\nls ;; esac)",
        "echo `case x in a) cat <<E\n;;\nE\nls ;; esac`",
    ] {
        assert!(rewrite_case_constructs(input).is_err(), "{input:?}");
    }
    assert_eq!(
        rewrite_case_constructs("case x in a) cat <<< hi ;; esac").unwrap(),
        "cat <<< hi"
    );
}

#[test]
fn case_arm_comment_does_not_hide_a_command() {
    refused_in(
        &LISTS,
        &[
            "case x in x) echo # ;;\n {W} ;; esac",
            "echo $(case x in x) # ;;\n ({W}) ;; esac)",
            "echo \"$(case x in x) # ;;\n {W} ;; esac)\"",
            "echo `case x in x) # ;;\n {W} ;; esac`",
        ],
    );
}

#[test]
fn case_arm_here_document_does_not_hide_a_command() {
    refused_in(
        &LISTS,
        &[
            "case x in x) cat <<E\n;;\nE\n{W}\n;; esac",
            "echo $(case x in x) cat <<E\n;;\nE\n{W}; case y in y) echo ok;; esac\nesac)",
            "echo `case x in x) cat <<E\n;;\nE\n{W}; case y in y) echo ok;; esac\nesac`",
        ],
    );
}

/// A `#` inside a string in a `case` in a quoted substitution is no comment:
/// the arm after it is still gated, at the top level and in the scripts
/// other commands run.
#[test]
fn quoted_hash_in_substituted_case_keeps_the_arm_gated() {
    let arm = "echo \"$(case a in a) echo \";; esac) #\"; {W};; esac)\"";
    let quoted = arm.replace('\'', "'\\''");
    refused_in(
        &LISTS,
        &[
            arm,
            &format!("trap '{quoted}' EXIT"),
            &format!("watch '{quoted}'"),
            &format!("git rebase --exec '{quoted}' main"),
        ],
    );
    refused_in(&[""], &[&format!("alias a='{quoted}'")]);
}

/// Without an allowlist, a `trap` action's here-document and substitutions
/// are read like the line's own.
#[test]
fn trap_action_here_document_and_substitution_are_gated() {
    refused_in(
        &LISTS,
        &[
            "trap \"cat <<E\n# it's\nE\n{W}\" EXIT",
            "trap 'echo $(echo x # say \"hi\n{W}\n)' EXIT",
            "trap 'echo $(echo; {W})' EXIT",
        ],
    );
}

#[test]
fn ordinary_case_constructs_still_pass() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    for list in LISTS {
        with_mode(list, || {
            for cmd in [
                "case x in x) echo hi ;; y) echo ok ;; esac",
                "case \"$1\" in -h|--help) echo help ;; *) echo other ;; esac",
                "echo $(case x in x) echo hi;; esac)",
                "echo \"$(case x in x) echo hi;; y) echo ok;; esac)\"",
                "case x in x) cat <<< hi ;; esac",
                "case x in x) echo hi # note\n;; esac",
                "trap 'echo done' EXIT",
                "echo $(echo a)#b",
            ] {
                let verdict = enforce_shell_allowlist(cmd);
                assert!(verdict.is_ok(), "allowlist {list:?}: {cmd:?}: {verdict:?}");
            }
        });
    }
}
