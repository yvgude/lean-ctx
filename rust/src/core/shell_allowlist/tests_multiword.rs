//! Multi-word allowlist entry tests (GH #1419) and the subcommand-scoping
//! follow-up.
//!
//! Extracted from `tests.rs` to stay within the 1500-line LOC gate.

use super::enforcement::{Allowlist, check_all_segments, matches_allowlist_entry};
use super::tests::allow;

fn tokens(cmd: &str) -> Vec<String> {
    cmd.split_whitespace().map(str::to_string).collect()
}

#[test]
fn allowlist_matches_first_word_of_multi_word_entry() {
    let allowlist = allow(&["terraform plan *"]);
    assert!(
        matches_allowlist_entry(&tokens("terraform plan"), &allowlist),
        "'terraform plan *' must match the bare subcommand form"
    );
}

#[test]
fn allowlist_exact_match_still_works() {
    let allowlist = allow(&["git", "cargo"]);
    assert!(matches_allowlist_entry(&tokens("git"), &allowlist));
    assert!(matches_allowlist_entry(&tokens("cargo"), &allowlist));
    assert!(!matches_allowlist_entry(&tokens("terraform"), &allowlist));
}

#[test]
fn allowlist_multi_word_does_not_false_positive() {
    let allowlist = allow(&["terraform plan *"]);
    assert!(
        !matches_allowlist_entry(&tokens("git"), &allowlist),
        "unrelated base must not match 'terraform plan *'"
    );
}

// GH #1419: end-to-end integration tests via enforce_shell_allowlist.
// These exercise the full allowlist pipeline (not just matches_allowlist_entry)
// so a regression in check_all_segments or check_interpreter_inner is caught.

#[test]
fn issue_1419_multiword_extra_allows_direct_command() {
    let list = allow(&["git", "ls", "terraform plan *"]);
    assert!(
        check_all_segments("terraform plan -no-color -lock=false", &list).is_ok(),
        "terraform plan with multi-word allowlist entry must be allowed"
    );
}

#[test]
fn issue_1419_multiword_extra_blocks_unlisted_binary() {
    let list = allow(&["terraform plan *"]);
    let result = check_all_segments("kubectl get pods", &list);
    assert!(result.is_err(), "kubectl must still be blocked");
    assert!(result.unwrap_err().contains("kubectl"));
}

#[test]
fn issue_1419_multiword_entry_via_delegation_wrapper() {
    let list = allow(&["timeout", "terraform plan *"]);
    assert!(
        check_all_segments("timeout 120 terraform plan -no-color -lock=false", &list).is_ok(),
        "timeout wrapping terraform must be allowed with multi-word entry"
    );
}

#[test]
fn issue_1419_multiword_entry_scopes_to_subcommand() {
    let list = allow(&["terraform plan *"]);
    let result = check_all_segments("terraform notplan", &list);
    assert!(
        result.is_err(),
        "'terraform notplan' must be denied when only 'terraform plan *' is allowed"
    );
}

// ---------------------------------------------------------------------------
// §5.A: the full grammar matrix, `subcommand_scoping = true` (the default).
// ---------------------------------------------------------------------------

#[test]
fn grammar_matrix_subcommand_scoping_on() {
    let rows: &[(&str, &[&str], bool, &str)] = &[
        ("git status", &["git status"], true, "exact match"),
        (
            "git   status",
            &["git status"],
            true,
            "whitespace-insensitive",
        ),
        (
            "git status -s",
            &["git status"],
            false,
            "exact-unless-*: extra token now blocked",
        ),
        (
            "git log",
            &["git status"],
            false,
            "original bug: different subcommand, same binary",
        ),
        (
            "terraform plan",
            &["terraform plan *"],
            true,
            "zero-or-more: bare form",
        ),
        (
            "terraform plan -no-color -lock=false",
            &["terraform plan *"],
            true,
            "zero-or-more: with args",
        ),
        (
            "terraform notplan",
            &["terraform plan *"],
            false,
            "word-boundary: different subcommand",
        ),
        (
            "terraform planning",
            &["terraform plan *"],
            false,
            "word-boundary: not a substring match",
        ),
        (
            "cargo build --release",
            &["cargo"],
            true,
            "single-word entry = implicit wildcard",
        ),
        (
            "LANG=C git status",
            &["git status"],
            true,
            "normalizer: env assignment skipped",
        ),
        (
            "/usr/bin/git status",
            &["git status"],
            true,
            "normalizer: basename of first token",
        ),
        (
            "{ git status",
            &["git status"],
            true,
            "normalizer: leading brace group skipped (#939 wrapper form)",
        ),
        (
            "git -c alias.status='!sh' status",
            &["git status"],
            false,
            "security: global options are never skipped",
        ),
        (
            "cd /repo && git status",
            &["git status"],
            true,
            "documented workaround for global options",
        ),
    ];
    for (cmd, entries, expect_ok, label) in rows {
        let list = allow(entries);
        let result = check_all_segments(cmd, &list);
        assert_eq!(
            result.is_ok(),
            *expect_ok,
            "{label}: '{cmd}' vs {entries:?} — got {result:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// §5.B: dual-mode requirement — the compat flag genuinely restores the old,
// base-binary-only behavior for every row that differs between modes.
// ---------------------------------------------------------------------------

#[test]
fn grammar_matrix_subcommand_scoping_off_restores_old_behavior() {
    let rows: &[(&str, &[&str])] = &[
        ("git status -s", &["git status"]),
        ("git log", &["git status"]),
        ("terraform notplan", &["terraform plan *"]),
        ("terraform planning", &["terraform plan *"]),
        ("git -c alias.status='!sh' status", &["git status"]),
    ];
    for (cmd, entries) in rows {
        let list = Allowlist::new(entries.iter().map(|s| (*s).to_string()).collect(), false);
        assert!(
            check_all_segments(cmd, &list).is_ok(),
            "subcommand_scoping=false must restore base-only matching for '{cmd}' vs {entries:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// §5.C: thin propagation tests — one pass + one previously-bypassable-now-
// blocked case per call site, proving each one plumbs tokens through to the
// shared, already-fully-tested matching primitive.
// ---------------------------------------------------------------------------

#[test]
fn propagation_function_body() {
    let list = allow(&["git status"]);
    assert!(check_all_segments("f() { git status; }; f", &list).is_ok());
    assert!(check_all_segments("f() { git stash; }; f", &list).is_err());
}

#[test]
fn propagation_delegation() {
    let list = allow(&["timeout", "git status"]);
    assert!(check_all_segments("timeout 5 git status", &list).is_ok());
    assert!(check_all_segments("timeout 5 git stash", &list).is_err());
}

#[test]
fn propagation_powershell_override() {
    let list = allow(&["iex script.exs"]);
    assert!(check_all_segments("iex script.exs", &list).is_ok());
    assert!(check_all_segments("iex other.exs", &list).is_err());
}

/// §5.C project-root fall-through: uses `./Cargo.toml` as a stable existing
/// project-root file (same convention as `project_root_binary_accepts_existing_project_file`
/// in tests.rs) standing in for a scoped binary.
#[test]
fn propagation_project_root_fallthrough_bypass_closed() {
    // An entry names "Cargo.toml" explicitly (scoped to "lint") — a scoping
    // denial for a different subcommand must NOT fall through into the
    // project-root auto-allow, even though ./Cargo.toml genuinely exists
    // under the project root.
    let scoped = allow(&["Cargo.toml lint"]);
    assert!(check_all_segments("./Cargo.toml lint", &scoped).is_ok());
    assert!(
        check_all_segments("./Cargo.toml build", &scoped).is_err(),
        "a scoping denial must not fall through to the project-root auto-allow"
    );

    // No entry names "Cargo.toml" at all — the project-root auto-allow must
    // still work exactly as before (#813 unchanged).
    let unrelated = allow(&["git"]);
    assert!(
        check_all_segments("./Cargo.toml build", &unrelated).is_ok(),
        "project-root auto-allow must stay intact when the binary isn't scoped by any entry"
    );

    // subcommand_scoping = false: the compat path never even reaches the
    // project-root logic, because the old base-only match already passes.
    let compat = Allowlist::new(vec!["Cargo.toml lint".to_string()], false);
    assert!(check_all_segments("./Cargo.toml build", &compat).is_ok());
}

// ---------------------------------------------------------------------------
// §5.D: closest-entry block-message content.
// ---------------------------------------------------------------------------

#[test]
fn closest_entry_names_the_diverging_word() {
    let list = allow(&["terraform plan *"]);
    let err = check_all_segments("terraform notplan", &list)
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("terraform plan *") && err.contains("'plan'") && err.contains("'notplan'"),
        "must cite the closest entry and the diverging word: {err}"
    );
}

#[test]
fn closest_entry_tie_breaks_to_first_in_list() {
    let list = allow(&["terraform apply *", "terraform plan *"]);
    let err = check_all_segments("terraform destroy", &list)
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("terraform apply *"),
        "tied entries must cite the first one in list order: {err}"
    );
    assert!(!err.contains("terraform plan *"), "got: {err}");
}

#[test]
fn closest_entry_absent_when_no_entry_shares_the_base_binary() {
    let list = allow(&["git status"]);
    let err = check_all_segments("kubectl get pods", &list)
        .unwrap_err()
        .to_string();
    assert!(
        !err.contains("closest entry"),
        "no entry names kubectl at all — must not print closest-entry text: {err}"
    );
}

#[test]
fn closest_entry_hints_global_options_workaround() {
    let list = allow(&["git status"]);
    let err = check_all_segments("git -C /repo status", &list)
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("cd <dir> && <cmd>"),
        "diverging token starts with '-' — must hint the workaround: {err}"
    );

    let err = check_all_segments("git log", &list)
        .unwrap_err()
        .to_string();
    assert!(
        !err.contains("cd <dir> && <cmd>"),
        "diverging token does not start with '-' — must not hint the workaround: {err}"
    );
}
