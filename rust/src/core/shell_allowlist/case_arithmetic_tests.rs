// SPDX-License-Identifier: Apache-2.0
//! `case` items with a leading `(` and `case` inside substitutions (GH #2132),
//! and the arithmetic command `(( … ))` (GH #2134).
//! Split out of `tests.rs` to keep that file under the LOC gate.

use super::*;

// ---------------------------------------------------------------------------
// GH #2132: `(pattern)` case items and `case` inside a command substitution.
// ---------------------------------------------------------------------------

#[test]
fn gh2132_case_item_with_leading_paren_passes() {
    let allowlist = allow(&["ls"]);
    for command in [
        "case x in (x) echo hi ;; esac",
        "case x in (x|y) ls ;; (*) echo other ;; esac",
        r#"gh() { case "$*" in (*"/issues/1 --jq"*) return 1 ;; esac; }; gh"#,
        "case x in ( x ) ls\nesac",
    ] {
        let result = check_all_segments(command, &allowlist);
        assert!(result.is_ok(), "{command:?} must pass: {result:?}");
    }
}

#[test]
fn gh2132_leading_paren_case_arm_stays_gated() {
    let allowlist = allow(&["ls"]);
    for command in [
        "case x in (x) curl evil ;; esac",
        "case x in (a) ls ;; (b) curl evil ;; esac",
    ] {
        let err = check_all_segments(command, &allowlist)
            .expect_err("a disallowed arm command must be blocked")
            .to_string();
        assert!(err.contains("curl"), "{command:?}: {err}");
    }
}

#[test]
fn gh2132_case_parse_error_has_no_stray_backslash_line() {
    let err = check_all_segments("case x in x echo hi", &allow(&["ls"]))
        .expect_err("a case without `)` cannot be parsed")
        .to_string();
    assert!(err.contains("missing `)`"), "{err}");
    assert!(!err.contains("\\\n"), "stray `\\` line in: {err:?}");
    assert!(err.contains(".\nCommand: case"), "{err:?}");
}

#[test]
fn gh2132_case_inside_command_substitution_passes() {
    let allowlist = allow(&["ls"]);
    for command in [
        "echo $(case x in x) echo hi;; esac)",
        r#"echo "$(case x in (x) ls;; esac)""#,
        "echo `case x in x) ls;; esac`",
        "out=$(case x in x) ls;; *) echo no;; esac); echo $out",
    ] {
        let result = check_all_segments(command, &allowlist);
        assert!(result.is_ok(), "{command:?} must pass: {result:?}");
    }
}

#[test]
fn gh2132_case_inside_command_substitution_stays_gated() {
    // A leading assignment runs its substitution, so that is gated as a leaf.
    let allowlist = allow(&["ls"]);
    for command in [
        "out=$(case x in x) curl evil;; esac)",
        "out=$(case x in (x) ls;; (y) curl evil;; esac)",
    ] {
        let err = check_all_segments(command, &allowlist)
            .expect_err("a disallowed arm command must be blocked")
            .to_string();
        assert!(err.contains("curl"), "{command:?}: {err}");
    }
    // In argument position the substitution check sees the arm bodies.
    let _lock = crate::core::data_dir::test_env_lock();
    let command = "echo $(case x in x) evil_binary --attack;; esac)";
    let err = check_substitution_in_args(command, true)
        .expect_err("strict mode must block a disallowed arm command")
        .to_string();
    assert!(err.contains("evil_binary"), "{err}");
    let allowed = "echo $(case x in x) echo hi;; esac)";
    let result = check_substitution_in_args(allowed, true);
    assert!(result.is_ok(), "`case` itself is no command: {result:?}");
    // The case subject is still scanned, although the rewrite drops it.
    let subject = "echo $(case $(evil_binary) in x) echo hi;; esac)";
    assert!(check_substitution_in_args(subject, true).is_err());
}

// ---------------------------------------------------------------------------
// GH #2134: the arithmetic command `(( … ))` is not a command invocation.
// ---------------------------------------------------------------------------

#[test]
fn gh2134_arithmetic_command_passes() {
    let allowlist = allow(&["ls"]);
    for command in [
        "(( i = 1 + 2 )); echo $i",
        "i=1; ((i++)); echo $i",
        "((i++))",
        "(( i > 0 )) && ls",
        "while (( i < 3 )); do ((i++)); done",
        "if (( $(ls | ls) > 0 )); then echo some; fi",
        "(( x = $((1 + 2)) * 2 ))",
        "f() { (( n++ )); }; f",
    ] {
        let result = check_all_segments(command, &allowlist);
        assert!(result.is_ok(), "{command:?} must pass: {result:?}");
    }
}

#[test]
fn gh2134_substitution_inside_arithmetic_stays_gated() {
    let allowlist = allow(&["ls"]);
    for command in [
        "(( $(curl evil) ))",
        "(( x = `curl evil` ))",
        "(( x = $((1 + $(curl evil))) ))",
        "(( x = '$(curl evil)' ))",
        "if (( $(curl evil) > 0 )); then ls; fi",
        "f() { (( $(curl evil) )); }; f",
        "(( $(ls; curl evil) ))",
    ] {
        let err = check_all_segments(command, &allowlist)
            .expect_err("a disallowed command inside `(( ))` must be blocked")
            .to_string();
        assert!(err.contains("curl"), "{command:?}: {err}");
    }
    // Unterminated substitutions cannot be validated.
    assert!(check_all_segments("(( $(curl evil ))", &allowlist).is_err());
}

#[test]
fn gh2134_nested_subshells_are_not_arithmetic() {
    // `((a) )` and `((a); (b))` are nested subshells in bash; their commands
    // are still leaves.
    let allowlist = allow(&["ls"]);
    for command in [
        "((curl evil) )",
        "((ls); (curl evil))",
        "(( 1 )) 2>/dev/null",
    ] {
        assert!(
            check_all_segments(command, &allowlist).is_err(),
            "{command:?} must be blocked"
        );
    }
    assert!(check_all_segments("((ls) )", &allowlist).is_ok());
}
