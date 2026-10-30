// SPDX-License-Identifier: Apache-2.0

use super::*;

/// Words a private path in these tests is made of; none may reach a template.
const PRIVATE_WORDS: &[&str] = &[
    "anna",
    "Anna",
    "Health",
    "health",
    "Notes",
    "notes",
    "med",
    "list",
    "Docs",
    "befund",
    "Medical",
    "Records",
    "blood",
    "test",
    "Gesundheit",
    "Befund",
    "März",
    "Diabetes",
    "Insulin",
];

fn assert_no_private_words(template: &str, message: &str) {
    for word in template.split(|c: char| !c.is_alphanumeric() && c != '\'') {
        assert!(
            !PRIVATE_WORDS.contains(&word),
            "{template:?} leaks {word:?} from {message:?}"
        );
    }
}

/// The plain words of a template (placeholders and identifiers aside).
fn plain_words(template: &str) -> Vec<String> {
    template
        .split_whitespace()
        .filter(|token| !token.contains('‹'))
        .map(|token| token.trim_matches(EDGE).to_ascii_lowercase())
        .filter(|word| {
            // A lone dash is clause punctuation (`—` is scrubbed to `-`).
            word.bytes().any(|b| b.is_ascii_alphabetic())
                && word
                    .bytes()
                    .all(|b| b.is_ascii_alphabetic() || b == b'-' || b == b'\'')
        })
        .collect()
}

#[test]
fn variable_parts_become_placeholders_and_static_wording_stays() {
    for (message, expected) in [
        (
            "old_string not found in /Users/anna/acme/src/payroll.rs",
            "old_string not found in ‹path›",
        ),
        (
            "Permission denied (os error 13): C:\\Users\\bob\\secret.txt",
            "Permission denied (os error ‹n›): ‹path›",
        ),
        (
            "command timed out after 120s",
            "command timed out after ‹n›",
        ),
        (
            "unknown parameter 'customerName' for ctx_read",
            "unknown parameter ‹str› for ctx_read",
        ),
        (
            "no symbol PayrollService::compute_net in index",
            "no symbol ‹sym› in index",
        ),
        (
            "failed to fetch https://intranet.acme.example/api?id=42 connection refused",
            "failed to fetch ‹url› connection refused",
        ),
        // `invite` is not LeanCTX's wording, so it does not survive either.
        (
            "invite for anna.meier@acme.ch rejected",
            "‹text› for ‹email› rejected",
        ),
        (
            "CONFLICT: line 12 hash 9f3a2b1c changed since read",
            "CONFLICT: line ‹n› hash ‹n› changed since read",
        ),
    ] {
        assert_eq!(
            message_template(message).as_deref(),
            Some(expected),
            "{message}"
        );
    }
}

#[test]
fn only_the_first_line_and_a_bounded_length_are_reported() {
    let template =
        message_template("first line fails here\nsecret second line /etc/passwd").unwrap();
    assert_eq!(template, "first line fails here");
    let long = format!("error {}", "file ".repeat(100));
    assert!(message_template(&long).unwrap().chars().count() <= MAX_TEMPLATE_CHARS);
}

#[test]
fn messages_without_safe_wording_are_dropped() {
    for message in [
        "",
        "   ",
        "/Users/anna/acme/src/payroll.rs",
        "42",
        "Ünïcödé ñämes ønly",
        "x",
        // Wording that is not LeanCTX's own is not reported at all.
        "Datei \"Gehaltsliste März.xlsx\" wurde nicht gefunden",
        "Diabetes Insulin Dosierung",
    ] {
        assert_eq!(message_template(message), None, "{message:?}");
    }
}

#[test]
fn templates_never_contain_digits_paths_or_quotes() {
    for message in [
        "read failed for ~/notes/2026-q3.md after 3 retries",
        "token abc123def rejected by provider",
        "edit at offset 1234 of `fn main() { println!(\"hi\") }` failed",
        "user \"Anna Meier\" lacks role admin",
    ] {
        let template = message_template(message).unwrap_or_default();
        assert!(!template.chars().any(|c| c.is_ascii_digit()), "{template}");
        for leak in [
            "anna", "Anna", "Meier", "notes", "abc123", "println", "main()",
        ] {
            assert!(!template.contains(leak), "{template} leaks {leak}");
        }
    }
}

/// The production leak: a path with spaces in it kept every word but the
/// first. A path now collapses from its first segment through its end.
#[test]
fn paths_with_spaces_collapse_into_one_placeholder() {
    for (message, expected) in [
        (
            "ERROR: old_string not found in /Users/anna/My Health Notes/med list.md. \
             Make sure it matches exactly (including whitespace/indentation).",
            "ERROR: old_string not found in ‹path›. Make sure it matches exactly \
             (including whitespace/indentation).",
        ),
        (
            "ERROR: cannot read C:\\Users\\anna\\My Docs\\befund.txt: Access is denied. (os error 5)",
            "ERROR: cannot read ‹path›: Access is denied. (os error ‹n›)",
        ),
        (
            "ERROR: cannot open ~/Medical Records/blood test.pdf: No such file or directory (os error 2)",
            "ERROR: cannot open ‹path›: No such file or directory (os error ‹n›)",
        ),
        (
            "/Users/anna/My Health Notes/med list.md is outside the active project — \
             path_jail_scope = projects opens other projects for reading only.",
            "‹path› is outside the active project - ‹sym› projects opens other projects \
             for reading only.",
        ),
        (
            "ERROR: /Users/anna/Gesundheit März/Befund 2026.pdf is a symlink — refusing to \
             edit through it (TOCTOU protection).",
            "ERROR: ‹path› is a symlink - refusing to edit through it (TOCTOU protection).",
        ),
        (
            "ERROR: old_string found 2 times in ./My Health Notes/med list.md (lines 3, 9).",
            "ERROR: old_string found ‹n› times in ‹path› (lines ‹n›, ‹n›).",
        ),
        // A file name without a directory: the leading words are not
        // LeanCTX's wording and become placeholders of their own.
        (
            "cannot open My Health Notes.md: Permission denied",
            "cannot open ‹text› ‹path›: Permission denied",
        ),
        // A trailing directory segment without an extension.
        (
            "ERROR: cannot create directory /Users/anna/Medical Records: Permission denied",
            "ERROR: cannot create directory ‹path› ‹text›: Permission denied",
        ),
    ] {
        assert_eq!(
            message_template(message).as_deref(),
            Some(expected),
            "{message}"
        );
        assert_no_private_words(expected, message);
    }
}

#[test]
fn quoted_and_unusual_paths_leak_nothing() {
    for message in [
        "ERROR: cannot read \"/Users/anna/My Health Notes/med list.md\": Permission denied",
        "ERROR: cannot read '/Users/anna/My Health Notes/med list.md': Permission denied",
        "ERROR: cannot read `C:\\Users\\anna\\My Docs\\befund.txt`: Access is denied",
        "ERROR: cannot read \\\\server\\Medical Records\\blood test.pdf: Access is denied",
        "ERROR: cannot read ../Medical Records/blood test.pdf, retry",
        "ERROR: cannot read Medical Records/blood test, retry",
        "ERROR: cannot read /Users/anna/Gesundheit/Befund März",
        "No matches for Diabetes Insulin in /Users/anna/notes",
    ] {
        let template = message_template(message).unwrap_or_default();
        assert_no_private_words(&template, message);
        for word in plain_words(&template) {
            assert!(
                is_own_word(&word) || is_known_identifier(&word),
                "{template:?} keeps {word:?}"
            );
        }
    }
}

#[test]
fn words_joined_by_a_slash_are_wording_not_a_path() {
    assert_eq!(
        message_template("Make sure it matches exactly (including whitespace/indentation).")
            .as_deref(),
        Some("Make sure it matches exactly (including whitespace/indentation)."),
    );
    assert_eq!(
        message_template("file is not valid UTF-8 (binary/encoding)").as_deref(),
        Some("file is not valid ‹n› (binary/encoding)"),
    );
    // Segments that are not LeanCTX's wording stay a path.
    assert_eq!(
        message_template("cannot open docs/records for reading").as_deref(),
        Some("cannot open ‹path› for reading"),
    );
}

/// Every failure message `ctx_edit`, `ctx_patch`, `ctx_read` and `ctx_shell`
/// write, with private values filled in: the template keeps the static
/// wording (every word of it is in the vocabulary), and every word it keeps
/// is a word of that static wording, never one of the filled-in values.
#[test]
fn tool_failure_messages_keep_only_their_own_wording() {
    const P: &str = "/Users/anna/My Health Notes/med list.md";
    const W: &str = "C:\\Users\\anna\\My Docs\\befund.txt";
    let cases: Vec<(&str, String)> = vec![
        // ctx_edit
        (
            "ERROR: old_string not found in . Make sure it matches exactly (including whitespace/indentation).",
            format!(
                "ERROR: old_string not found in {P}. Make sure it matches exactly (including whitespace/indentation).\nSearched for: Diabetes"
            ),
        ),
        (
            "ERROR: old_string not found in (file uses CRLF line endings). Make sure it matches exactly (including whitespace/indentation).",
            format!(
                "ERROR: old_string not found in {W} (file uses CRLF line endings). Make sure it matches exactly (including whitespace/indentation)."
            ),
        ),
        (
            "ERROR: old_string found times in (lines , , ). Use replace_all=true to replace every occurrence, or add surrounding lines to old_string until it matches exactly one.",
            format!(
                "ERROR: old_string found 3 times in {P} (lines 4, 9, 17). Use replace_all=true to replace every occurrence, or add surrounding lines to old_string until it matches exactly one."
            ),
        ),
        (
            "No change: this exact edit was already applied by an earlier ctx_edit call (old_string is absent, new_string is present).",
            format!(
                "No change: {P} — this exact edit was already applied by an earlier ctx_edit call (old_string is absent, new_string is present)."
            ),
        ),
        (
            "ERROR: old_string must not be empty for a non-empty file (an empty old_string only creates a missing or empty file; use create=true to replace the whole file)",
            "ERROR: old_string must not be empty for a non-empty file (an empty old_string only creates a missing or empty file; use create=true to replace the whole file)".to_string(),
        ),
        (
            "ERROR: old_string and new_string are identical — nothing to change.",
            "ERROR: old_string and new_string are identical — nothing to change.".to_string(),
        ),
        (
            "ERROR: preimage mismatch for : expected_size=, actual_size=",
            format!("ERROR: preimage mismatch for {P}: expected_size=10, actual_size=12"),
        ),
        (
            "ERROR: file changed since read (TOCTOU guard). Re-read and retry:",
            format!("ERROR: file changed since read (TOCTOU guard). Re-read and retry: {P}"),
        ),
        (
            "ERROR: file is not valid UTF-8 (binary/encoding). Refusing to edit:",
            format!("ERROR: file is not valid UTF-8 (binary/encoding). Refusing to edit: {W}"),
        ),
        (
            "ERROR: file too large ( bytes, cap via LCTX_MAX_READ_BYTES):",
            format!("ERROR: file too large (123 bytes, cap 100 via LCTX_MAX_READ_BYTES): {P}"),
        ),
        (
            "ERROR: is a symlink — refusing to edit through it (TOCTOU protection).",
            format!("ERROR: {P} is a symlink — refusing to edit through it (TOCTOU protection)."),
        ),
        (
            "ERROR: cannot open : No such file or directory (os error )",
            format!("ERROR: cannot open {P}: No such file or directory (os error 2)"),
        ),
        (
            "ERROR: cannot create directory : Access is denied. (os error )",
            format!("ERROR: cannot create directory {W}: Access is denied. (os error 5)"),
        ),
        (
            "ERROR: cannot create backup : Read-only file system (os error )",
            format!("ERROR: cannot create backup {P}.bak: Read-only file system (os error 30)"),
        ),
        (
            "ERROR: cannot compute backup path for",
            format!("ERROR: cannot compute backup path for {P}"),
        ),
        (
            "per-file edit lock contention for — another edit to the same file is in progress, retry in a moment",
            format!(
                "per-file edit lock contention for {P} — another edit to the same file is in progress, retry in a moment"
            ),
        ),
        (
            "ERROR: edits produced no change to the file",
            "ERROR: edits produced no change to the file".to_string(),
        ),
        // ctx_patch
        (
            "CONFLICT: stale anchor(s) in — the file changed since you read it. Retry against the fresh anchors below (or re-read with ctx_read(mode=\"anchored\")).",
            "CONFLICT: 2 stale anchor(s) in list.md — the file changed since you read it. Retry against the fresh anchors below (or re-read with ctx_read(mode=\"anchored\")).".to_string(),
        ),
        (
            "line is past end of file ( lines)",
            "line 40 is past end of file (12 lines)".to_string(),
        ),
        (
            "insert_after needs non-empty new_text (use delete to remove lines)",
            "insert_after needs non-empty new_text (use delete to remove lines)".to_string(),
        ),
        (
            "start_line is after end_line",
            "start_line 9 is after end_line 3".to_string(),
        ),
        (
            "overlapping edits: lines and touch the same region",
            "overlapping edits: lines 3-5 and 4-6 touch the same region".to_string(),
        ),
        ("No matches for in", format!("No matches for \"Diabetes\" in {P}")),
        (
            "unknown op (one of: set_line, replace_lines, insert_after, delete, create, replace_symbol, replace_all)",
            "unknown op 'Insulin' (one of: set_line, replace_lines, insert_after, delete, create, replace_symbol, replace_all)".to_string(),
        ),
        (
            "ops[] is empty — provide at least one edit",
            "ops[] is empty — provide at least one edit".to_string(),
        ),
        (
            "replace_unique cannot be batched in ops[] — send each replace_unique as a separate top-level call",
            "replace_unique cannot be batched in ops[] — send each replace_unique as a separate top-level call".to_string(),
        ),
        // ctx_read
        ("file not found:", format!("file not found: {P}")),
        (
            "Cannot read file: : Permission denied (os error )",
            format!("Cannot read file: {P}: Permission denied (os error 13)"),
        ),
        (
            "ERROR: ctx_read timed out after reading . The file may be very large or a blocking I/O issue occurred.",
            format!(
                "ERROR: ctx_read timed out after 30s reading {P}. The file may be very large or a blocking I/O issue occurred."
            ),
        ),
        (
            "[file read failed and no cached version available]",
            format!("[file read failed and no cached version available] {P}"),
        ),
        (
            "is outside the active project — path_jail_scope = projects opens other projects for reading only.",
            format!(
                "{P} is outside the active project — path_jail_scope = projects opens other projects for reading only."
            ),
        ),
        // ctx_shell
        (
            "[BLOCKED — DO NOT RETRY] (inside function body) is not in the shell allowlist.",
            "[BLOCKED — DO NOT RETRY] 'diabetes-tool' (inside function body) is not in the shell allowlist.".to_string(),
        ),
        (
            "[BLOCKED — DO NOT RETRY] Empty command",
            "[BLOCKED — DO NOT RETRY] Empty command".to_string(),
        ),
        (
            "ERROR: command timed out after",
            "ERROR: command timed out after 200ms".to_string(),
        ),
        (
            "[background: not found or expired]",
            "[background:shell_ab12 not found or expired]".to_string(),
        ),
        (
            "job not found or retained terminal verdict expired",
            "job not found or retained terminal verdict expired".to_string(),
        ),
    ];
    for (wording, message) in &cases {
        let template = message_template(message)
            .unwrap_or_else(|| panic!("{message:?} must keep its wording"));
        assert_no_private_words(&template, message);
        let wording: Vec<String> = wording
            .split(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '\''))
            .map(str::to_ascii_lowercase)
            .collect();
        for word in plain_words(&template) {
            assert!(
                wording.contains(&word),
                "{template:?} keeps {word:?}, which is not in {wording:?}"
            );
        }
        // The vocabulary covers the static wording: nothing of it is lost.
        assert!(
            !template.contains("‹text›"),
            "{template:?}: a word of {message:?} is missing from the vocabulary"
        );
    }
}

#[test]
fn vocabulary_is_sorted_unique_and_lowercase() {
    let words = vocabulary::VOCABULARY;
    assert!(
        words.windows(2).all(|pair| pair[0] < pair[1]),
        "sorted, unique"
    );
    for word in words {
        assert!(
            !word.is_empty()
                && word
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b == b'-' || b == b'\''),
            "{word:?}"
        );
        for private in PRIVATE_WORDS {
            assert_ne!(*word, private.to_ascii_lowercase(), "{word} is private");
        }
    }
}

#[test]
fn templating_is_deterministic_and_idempotent() {
    for message in [
        "ERROR: old_string not found in /Users/anna/My Health Notes/med list.md. Make sure",
        "Permission denied (os error 13): C:\\Users\\bob\\secret.txt",
        "invite for anna.meier@acme.ch rejected",
    ] {
        let first = message_template(message).unwrap();
        assert_eq!(message_template(message).unwrap(), first);
        // A stored template re-templates to itself.
        assert_eq!(message_template(&first).as_deref(), Some(first.as_str()));
    }
}

/// Templates stored by earlier versions, re-templated with this scrubber:
/// the words they leaked are replaced, the static wording stays.
#[test]
fn leaked_stored_templates_are_retemplated() {
    assert_eq!(
        // What 3.12.0 stored for `old_string not found in /Users/anna/My Health
        // Notes/med list.md. Make sure … (including whitespace/indentation).`
        message_template(
            "ERROR: old_string not found in ‹path› Health ‹path› ‹path›. Make sure it \
             matches exactly (including ‹path›)."
        )
        .as_deref(),
        Some(
            "ERROR: old_string not found in ‹path› ‹text› ‹path› ‹path›. Make sure it \
             matches exactly (including ‹path›)."
        ),
    );
}
