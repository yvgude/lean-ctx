// SPDX-License-Identifier: Apache-2.0
//! An alias body runs wherever its name is used later on the line, so it is
//! gated like a command line of its own: with an allowlist every command in
//! it must be listed, without one it gets the unconditional rules. With an
//! allowlist an alias may also not take the name of a listed program or a
//! shell builtin; without one only a plain definition is accepted.

use super::*;

const ALLOWLIST: &str = "echo,ls,git,true,alias";

fn verdict(list: &str, cmd: &str) -> Result<(), ShellError> {
    lean_ctx::test_env::set_var("LEAN_CTX_SHELL_SECURITY", "enforce");
    lean_ctx::test_env::set_var("LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE", list);
    let verdict = enforce_shell_allowlist(cmd);
    lean_ctx::test_env::remove_var("LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE");
    lean_ctx::test_env::remove_var("LEAN_CTX_SHELL_SECURITY");
    verdict
}

fn refused(list: &str, cases: &[&str]) {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    for cmd in cases {
        let verdict = verdict(list, cmd);
        assert!(verdict.is_err(), "allowlist {list:?}: {cmd:?}: {verdict:?}");
    }
}

fn passes(list: &str, cases: &[&str]) {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    for cmd in cases {
        let verdict = verdict(list, cmd);
        assert!(verdict.is_ok(), "allowlist {list:?}: {cmd:?}: {verdict:?}");
    }
}

/// Definitions under unlisted names whose bodies are made only of listed
/// commands pass in both modes.
const LISTED_BODIES: &[&str] = &[
    "alias ll='ls -l'",
    "alias gs='git status # short'",
    "alias l='ls -l' g='git log'",
    "alias -p",
    "alias ll",
];

#[test]
fn alias_body_with_listed_commands_passes_in_allowlist_mode() {
    passes(ALLOWLIST, LISTED_BODIES);
}

#[test]
fn alias_use_under_unlisted_name_stays_refused_in_allowlist_mode() {
    refused(ALLOWLIST, &["alias ll='ls -l'; ll"]);
}

#[test]
fn alias_body_checked_in_allowlist_mode() {
    refused(
        ALLOWLIST,
        &[
            "alias c='marker_cmd notes.txt'",
            "alias ll='ls -l; marker_cmd notes.txt'",
            "alias ll='ls -l' c='marker_cmd notes.txt'",
            "alias c=\"echo x # it's\nmarker_cmd notes.txt\"",
            "alias c='echo $(marker_cmd notes.txt)'",
            "alias c='marker_cmd notes.txt'; c",
            "alias c='rm -rf ~/.config/lean-ctx'",
        ],
    );
}

#[test]
fn alias_named_like_listed_program_refused_in_allowlist_mode() {
    refused(
        ALLOWLIST,
        &[
            "alias ls='ls -l'",
            "alias echo='echo'",
            "alias ll='ls -l' echo='echo'",
            "alias git='git status'",
        ],
    );
}

#[test]
fn alias_named_like_builtin_refused_in_allowlist_mode() {
    refused(
        ALLOWLIST,
        &["alias cd='ls'", "alias command='ls'", "alias trap='ls'"],
    );
}

#[test]
fn alias_with_wrapper_body_refused_in_allowlist_mode() {
    refused(
        ALLOWLIST,
        &[
            "alias echo='command'; echo marker_cmd",
            "alias echo='command '; echo marker_cmd",
            "alias ls='command '; ls marker_cmd",
            "alias echo='trap'; echo 'marker_cmd' EXIT",
            "alias ls='git -c'; ls core.pager=marker_cmd log",
        ],
    );
}

#[test]
fn alias_with_body_ending_in_operator_refused_in_allowlist_mode() {
    refused(
        ALLOWLIST,
        &[
            "alias echo='ls;'; echo marker_cmd",
            "alias echo='ls |'; echo marker_cmd",
            "alias ls='true &&'; ls marker_cmd",
        ],
    );
}

#[test]
fn alias_with_open_body_refused_in_allowlist_mode() {
    refused(
        ALLOWLIST,
        &[
            "alias echo='echo $('; echo marker_cmd )",
            "alias ls='echo $('",
        ],
    );
}

/// Both modes: the allowlist above and none.
const MODES: &[&str] = &[ALLOWLIST, ""];

fn refused_in_both_modes(cases: &[&str]) {
    for list in MODES {
        refused(list, cases);
    }
}

/// Without an allowlist a plain definition passes: plain words under a name
/// that is no known program, also with one quoted segment.
#[test]
fn alias_plain_definition_passes_in_blocklist_mode() {
    passes(
        "",
        &[
            "alias ll='ls -l'",
            "alias ll='ls -l'; ll",
            "alias l='ls -l' c='marker_cmd notes.txt'",
            "alias c=clear",
            "alias k=kubectl",
            "alias today='date +%F'",
            "alias f=\"grep -n 'a b' notes.txt\"",
            "alias -p",
            "alias ll",
        ],
    );
}

#[test]
fn alias_body_with_state_write_refused_in_blocklist_mode() {
    refused(
        "",
        &[
            "alias c='rm -rf ~/.config/lean-ctx'",
            "alias c=\"echo x # it's\nrm -rf ~/.config/lean-ctx\"",
        ],
    );
}

#[test]
fn alias_named_like_program_refused_in_both_modes() {
    refused_in_both_modes(&[
        "alias ls=marker_cmd; ls notes.txt",
        "alias ls='ls -l'",
        "alias echo=marker_cmd",
        "alias a=marker_cmd; alias ls=a; ls notes.txt",
        "builtin alias ls=marker_cmd",
        "command alias ls=marker_cmd",
        "if true; then alias ls=marker_cmd; fi",
    ]);
    refused(
        "",
        &[
            "alias cat=marker_cmd",
            "alias grep='grep -n'",
            "alias rm=marker_cmd",
            "alias mv=marker_cmd",
            "alias sed=marker_cmd",
            "alias cp=marker_cmd",
            "alias LS=marker_cmd",
            "alias cd=ls",
            "alias trap=ls",
            "alias b=rm",
        ],
    );
}

/// A body that starts with `git` takes further git options at its use
/// (known limit: `gs='git status'` is refused too without an allowlist).
#[test]
fn alias_with_git_body_refused_in_both_modes() {
    refused_in_both_modes(&[
        "alias g=git; g -c core.pager=marker_cmd log",
        "alias g='git log'; g --exec-path=. log",
    ]);
    refused("", &["alias gs='git status'", "alias g=git"]);
}

#[test]
fn alias_with_ansi_c_body_refused_in_both_modes() {
    refused_in_both_modes(&[
        "alias e=$'marker_cmd'; e",
        "alias e=$'ls\\x20-l'; e",
        "alias e=$'ls\\nmarker_cmd'; e",
    ]);
}

#[test]
fn alias_with_wrapper_after_first_word_refused_in_both_modes() {
    refused_in_both_modes(&[
        "alias e='! command'; e marker_cmd",
        "alias e='ls\ncommand'; e marker_cmd",
        "alias e='ls; command'; e marker_cmd",
        "alias e='>notes.txt command'; e marker_cmd",
        "alias e='ls && env'; e marker_cmd",
    ]);
    refused(
        "",
        &[
            "alias e='command'",
            "alias e='ls '",
            "alias e='ls;'",
            "alias e=''",
            "alias e='FOO=1 ls'",
            "alias e=sh",
            "alias e=python3",
            "alias e='nice -n1 ls'",
        ],
    );
}

#[test]
fn alias_with_open_group_refused_in_both_modes() {
    refused_in_both_modes(&[
        "alias e='if true; then'; e marker_cmd; fi",
        "alias e='while false; do'; e marker_cmd; done",
        "alias e='{ ls'; e; }",
        "alias e='(ls'; e)",
    ]);
    refused("", &["alias e='echo \"'", "alias e=\"echo '\""]);
}

#[test]
fn alias_with_substitution_body_refused_in_both_modes() {
    refused_in_both_modes(&[
        "alias e='echo $(marker_cmd)'",
        "alias e='echo `marker_cmd`'",
        "alias e='$V'; e",
    ]);
    refused("", &["alias e='echo $('", "alias e='echo ${V}'"]);
}
