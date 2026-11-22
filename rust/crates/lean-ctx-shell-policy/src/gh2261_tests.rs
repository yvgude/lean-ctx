// SPDX-License-Identifier: Apache-2.0
//! #2261: a `gh api` pagination loop was refused as a program run on
//! lean-ctx's own state. The loop reassigns `c=$(…)`, so `$c` stands for a
//! run-time value, and the state gate also substituted the `$c` inside the
//! single-quoted `'($c)'`. There the value sat inside a word, which made the
//! whole command count as a literal path into the state. Single-quoted text
//! never expands, so it no longer stands for the variable.

use super::*;

fn list() -> Allowlist {
    allow(&["gh", "echo", "jq", "rm", "cat", "trap", "timeout"])
}

/// The issue's repro table and its real-world form: every row runs.
const ALLOWED: &[&str] = &[
    // Row 1: was refused as `gh ~/.config/lean-ctx`.
    "c=\"\"; for p in 1; do gh api -X GET -f c=\"$c\" -f q='($c)' rate_limit --jq .rate.limit; c=$(echo q); done",
    // Row 2: `c=q` instead of `c=$(echo q)`.
    "c=\"\"; for p in 1; do gh api -X GET -f c=\"$c\" -f q='($c)' rate_limit --jq .rate.limit; c=q; done",
    // Row 3: without `-f c=\"$c\"`.
    "c=\"\"; for p in 1; do gh api -X GET -f q='($c)' rate_limit --jq .rate.limit; c=$(echo q); done",
    // Row 4: without `-f q='($c)'`.
    "c=\"\"; for p in 1; do gh api -X GET -f c=\"$c\" rate_limit --jq .rate.limit; c=$(echo q); done",
    // Row 5: no loop.
    "c=\"\"; gh api -X GET -f c=\"$c\" -f q='($c)' rate_limit",
    // The GraphQL pagination loop the issue came from.
    "cursor=\"\"; for page in 1 2; do out=$(gh api graphql -f cursor=\"$cursor\" -f query='query($cursor:String){viewer{repositories(first:1,after:$cursor){pageInfo{endCursor}}}}'); cursor=$(echo \"$out\" | jq -r .data.viewer.repositories.pageInfo.endCursor); done",
    // A literal value: single-quoted `$d` is text, not the state directory.
    "d=~/.config/lean-ctx; gh api -X GET -f q='$d' rate_limit",
    "d=~/.config/lean-ctx; echo '$d' '${d}' '$(cat $d)'",
];

/// A variable that really expands to lean-ctx's state still counts, whether
/// or not the same name also appears inside single quotes.
const REFUSED: &[&str] = &[
    "c=\"\"; for p in 1; do rm -rf \"$c\" '$c'; c=$(echo q); done",
    "d=~/.config/lean-ctx; gh api -X GET -f c=\"$d\" -f q='($d)' rate_limit",
    "for d in ~/.config/lean-ctx; do rm -rf $d '$d'; done",
    "d=~/.config/lean-ctx; echo \"it's $d\" > \"$d/x\"",
    // A trap action is shell code run later: its single-quoted `$d` expands.
    "d=~/.config/lean-ctx; trap 'rm -rf $d' EXIT",
];

#[test]
fn gh2261_issue_rows_pass_without_an_allowlist() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    for cmd in ALLOWED {
        let verdict = check_unconditional_blocked_only(cmd);
        assert!(verdict.is_ok(), "must pass: {cmd}: {verdict:?}");
    }
}

#[test]
fn gh2261_issue_rows_pass_with_an_allowlist() {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    for cmd in ALLOWED {
        let verdict = check_all_segments(cmd, &list());
        assert!(verdict.is_ok(), "must pass: {cmd}: {verdict:?}");
    }
}

#[test]
fn gh2261_real_expansions_into_the_state_stay_refused() {
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
