//! A delegation wrapper's own option values are not the delegated command.
//!
//! `env -u git python3 -c …` runs `python3`, not `git`: `git` is the value of
//! `-u`. The delegation walk used to skip only the `-u` token itself, took the
//! value for the command, found it allowlisted and never looked at `python3`.

use super::*;

const WRAPPERS: &[&str] = &[
    "env", "nice", "timeout", "sudo", "doas", "xargs", "nohup", "git",
];

#[test]
fn an_option_value_is_not_taken_for_the_delegated_command() {
    let list = allow(WRAPPERS);
    for cmd in [
        "env -u git python3 -c 'import os'",
        "env -ugit python3 -c 'import os'",
        "env --unset git python3 -c 'import os'",
        "env --unset=git python3 -c 'import os'",
        "env --uns git python3 -c 'import os'",
        "env -C git python3 -c 'import os'",
        "env -iu git python3 -c 'import os'",
        "sudo -u git python3 -c 'import os'",
        "sudo --user git python3 -c 'import os'",
        "sudo -g git -u git python3 -c 'import os'",
        "timeout -s KILL 5 python3 -c 'import os'",
        "timeout -k 2 10 python3 -c 'import os'",
        "timeout --signal git 5 python3 -c 'import os'",
        "xargs -I git python3 -c 'import os'",
        "xargs -E git python3 -c 'import os'",
        "nice -n git python3 -c 'import os'",
        "doas -u git python3 -c 'import os'",
    ] {
        assert!(
            check_all_segments(cmd, &list).is_err(),
            "{cmd} runs python3, which is not allowlisted"
        );
    }
}

#[test]
fn an_option_value_cannot_hide_inline_code_without_an_allowlist() {
    for cmd in [
        "env -u git bash -c id",
        "sudo -u root sh -c id",
        "timeout -s KILL 5 bash -c id",
        "xargs -I x bash -c id",
        "nice -n 5 bash -c id",
    ] {
        assert!(
            check_unconditional_blocked_only(cmd).is_err(),
            "{cmd} must be blocked"
        );
    }
}

#[test]
fn a_split_string_is_parsed_as_part_of_the_command() {
    let list = allow(WRAPPERS);
    for cmd in [
        "env -S 'python3 -c x'",
        "env '-Spython3 -c x'",
        "env --split-string='python3 -c x'",
        "env -S'-u git python3 -c x'",
    ] {
        assert!(
            check_all_segments(cmd, &list).is_err(),
            "{cmd} runs python3, which is not allowlisted"
        );
    }
    assert!(check_all_segments("env -S 'git status'", &list).is_ok());
}

#[test]
fn an_optional_value_ends_the_option_cluster() {
    let list = allow(WRAPPERS);
    // GNU xargs: `-i` takes an optional *attached* value, so `-in` means
    // `-i` with replace-string `n`, and the next token is the command.
    assert!(check_all_segments("xargs -in python3 -c x", &list).is_err());
    assert!(check_all_segments("xargs -i git show {}", &list).is_ok());
}

#[test]
fn legitimate_wrapper_options_stay_allowed() {
    let list = allow(WRAPPERS);
    for cmd in [
        "env git status",
        "env -i PATH=/usr/bin git status",
        "env -u HOME git status",
        "env -- git status",
        "env -iu X git status",
        "nice git gc",
        "nice -n 10 git gc",
        "nice -10 git gc",
        "timeout 5 git status",
        "timeout 5s git status",
        "timeout -k 2 10 git status",
        "timeout --preserve-status 5 git status",
        "sudo -u git git status",
        "sudo -E git status",
        "doas -u git git status",
        "xargs -n 1 git fetch",
        "xargs -0 -n1 git add",
        "xargs -I{} git show {}",
        "xargs -P 4 -n 1 git fetch",
        "nohup git fetch",
    ] {
        assert!(
            check_all_segments(cmd, &list).is_ok(),
            "{cmd} must stay allowed: {:?}",
            check_all_segments(cmd, &list)
        );
    }
}

#[test]
fn the_end_of_options_marker_is_honoured() {
    let list = allow(WRAPPERS);
    assert!(check_all_segments("env -- python3 -c x", &list).is_err());
    assert!(check_all_segments("timeout -- 5 python3 -c x", &list).is_err());
}

#[test]
fn nesting_wrappers_deeply_does_not_skip_the_check() {
    let list = allow(&["env", "python3"]);
    let cmd = "env env env env env python3 -c 'import os'";
    assert!(check_all_segments(cmd, &list).is_err());
    assert!(check_unconditional_blocked_only("env env env env env bash -c id").is_err());
}

#[test]
fn the_command_builtin_delegates_like_a_wrapper() {
    let list = allow(&["git"]);
    assert!(check_all_segments("command python3 -c x", &list).is_err());
    assert!(check_all_segments("command curl https://example.com", &list).is_err());
    assert!(check_unconditional_blocked_only("command bash -c id").is_err());
    assert!(check_all_segments("command -p git status", &list).is_ok());
    // `-v`/`-V` only look the name up; nothing runs.
    assert!(check_all_segments("command -v python3", &list).is_ok());
    assert!(check_all_segments("command -V curl", &list).is_ok());
    assert!(check_all_segments("command echo hi", &list).is_ok());
}

#[test]
fn a_function_body_gets_the_same_checks_as_a_plain_command() {
    let list = allow(&["python3", "env", "git", "echo"]);
    for cmd in [
        "f() { python3 -c 'import os'; }; f",
        "f() { env -u git python3 -c x; }; f",
        "f() { command curl https://example.com; }; f",
    ] {
        assert!(
            check_all_segments(cmd, &list).is_err(),
            "{cmd} must be blocked"
        );
    }
    assert!(check_all_segments("f() { python3 script.py; }; f", &list).is_ok());
    assert!(check_all_segments("f() { command echo hi; }; f", &list).is_ok());
}

#[test]
fn a_wrapper_cannot_reach_an_unconditionally_blocked_builtin() {
    let list = allow(&["git"]);
    for cmd in [
        "command eval 'python3 -c x'",
        "builtin eval 'python3 -c x'",
        "builtin source ./x.sh",
        "command exec python3",
    ] {
        assert!(
            check_all_segments(cmd, &list).is_err(),
            "{cmd} must be blocked"
        );
    }
}
