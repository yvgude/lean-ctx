// SPDX-License-Identifier: Apache-2.0
//! #2231: the state gate judged the first word of a command as its program.
//! `timeout 120 find ~ -name x`, `nice ls ~` and a `for` loop over `~` were
//! refused as programs that "may write" above lean-ctx's state, and the
//! interpreter flag scan ran past the script operand (`timeout 300 node x.js
//! "a rm -rf b"` read `-rf` as a node flag).

use super::*;

fn list() -> Allowlist {
    allow(&[
        "find", "timeout", "nice", "ls", "node", "rm", "cat", "true", "env", "sed",
    ])
}

const ALLOWED: &[&str] = &[
    "find ~ -name x",
    "timeout 120 find ~ -name x",
    "nice ls ~",
    "for d in ~ /tmp; do ls $d; done",
    "node x.js \"a rm -rf b\"",
    "timeout 300 node x.js \"a rm -rf b\"",
];

const REFUSED: &[&str] = &[
    "timeout 5 cat x > ~/.config/lean-ctx/config.toml",
    ">~/.local/share/lean-ctx/x timeout 5 true",
    "timeout 5 rm -rf ~/.config/lean-ctx",
    "nice rm -rf ~/.config",
    "env -C ~ ls",
    "env -S 'rm -rf ~/.config/lean-ctx'",
    "env PATH=/tmp/bin ls ~",
    "env LD_PRELOAD=/tmp/x.so ls ~",
    "for d in ~; do rm -rf \"$d/.config/lean-ctx\"; done",
    "for d in /tmp ~; do x=$d/.config; rm -rf \"$x/lean-ctx\"; done",
    "timeout 300 node -e x",
    "timeout 300 node x.js -e y && node -e z",
    "timeout 5 python3 -W ignore -c 'print(1)'",
    "timeout 5 bash -x -c 'id'",
];

#[test]
fn gh2231_issue_rows_pass_without_an_allowlist() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    for cmd in ALLOWED {
        let verdict = check_unconditional_blocked_only(cmd);
        assert!(verdict.is_ok(), "must pass: {cmd}: {verdict:?}");
    }
}

#[test]
fn gh2231_issue_rows_pass_with_an_allowlist() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    for cmd in ALLOWED {
        let verdict = check_all_segments(cmd, &list());
        assert!(verdict.is_ok(), "must pass: {cmd}: {verdict:?}");
    }
}

#[test]
fn gh2231_real_state_writes_stay_refused() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    for cmd in REFUSED {
        assert!(
            check_unconditional_blocked_only(cmd).is_err(),
            "must refuse without an allowlist: {cmd}"
        );
        assert!(
            check_all_segments(cmd, &list()).is_err(),
            "must refuse with an allowlist: {cmd}"
        );
    }
}

/// A loop over the state directory itself: the header alone is refused
/// without an allowlist; with one, the gate judges the loop body exactly
/// like the same command written with the bound value, and a write through
/// the loop variable stays refused in both modes.
#[test]
fn gh2231_loop_over_the_state_matches_the_unrolled_command() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    let looped = "for d in ~/.config/lean-ctx; do ls $d; done";
    assert!(check_unconditional_blocked_only(looped).is_err());
    assert_eq!(
        check_all_segments(looped, &list()).is_ok(),
        check_all_segments("ls ~/.config/lean-ctx", &list()).is_ok(),
        "{looped}"
    );
    let writes = "for d in ~/.config/lean-ctx; do rm -rf $d; done";
    assert!(check_unconditional_blocked_only(writes).is_err());
    assert!(check_all_segments(writes, &list()).is_err());
}

/// The interpreter flag scan stops at the script: the script's own
/// arguments are not interpreter flags, while flags before it still are.
#[test]
fn gh2231_interpreter_scan_stops_at_the_script_operand() {
    use crate::enforcement::script_operand;
    let words = |s: &str| shell_tokenize(s);
    assert_eq!(script_operand("node", &words("node x.js -rf")), Some(1));
    assert_eq!(
        script_operand("python3", &words("python3 -u -B x.py -c y")),
        Some(3)
    );
    assert_eq!(script_operand("bash", &words("bash -- x.sh -c y")), Some(2));
    assert_eq!(script_operand("node", &words("node - -e y")), Some(1));
    // An option this check does not know to take no value: scan everything.
    for cmd in [
        "python3 -W ignore -c y",
        "node --inspect x.js",
        "bash -xc id",
    ] {
        let base = cmd.split(' ').next().unwrap_or_default();
        assert_eq!(script_operand(base, &words(cmd)), None, "{cmd}");
    }
}
