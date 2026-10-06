// SPDX-License-Identifier: Apache-2.0
//! GH #2003: a heredoc after a double-quoted string that spans lines.

use super::heredoc::strip_quoted_heredoc_bodies;
use super::strip_all_heredoc_bodies;
use super::{check_all_segments, tests::allow};

/// The segment check as `enforce_shell_allowlist` runs it: heredoc bodies are
/// stripped first, then every remaining segment must be allowlisted.
fn gate(command: &str) -> Result<(), super::ShellError> {
    check_all_segments(
        &strip_all_heredoc_bodies(command),
        &allow(&["echo", "cat", "python3"]),
    )
}

#[test]
fn heredoc_after_multiline_double_quoted_string_is_stripped() {
    let command = "echo \"a\nb\"; cat <<'EOF'\nprint(1)\nEOF";
    assert_eq!(
        strip_quoted_heredoc_bodies(command),
        "echo \"a\nb\"; cat <<'EOF'"
    );
    assert_eq!(
        strip_all_heredoc_bodies(command),
        "echo \"a\nb\"; cat <<'EOF'"
    );
    let result = gate(command);
    assert!(result.is_ok(), "heredoc body must not be gated: {result:?}");
}

#[test]
fn heredoc_script_after_multiline_string_is_stripped() {
    // The shape from the report: a string opening with a newline, then a
    // heredoc whose first body line is `import json`. (The report fed it to
    // `python3 -`; interpreter stdin stays blocked by its own policy.)
    let command = "echo \"\nnote; more\"; cat <<'EOF'\nimport json\nprint('heredoc')\nEOF";
    let result = gate(command);
    assert!(result.is_ok(), "heredoc body must not be gated: {result:?}");
}

#[test]
fn heredoc_operator_inside_a_multiline_string_is_not_a_heredoc() {
    // `<<'EOF'` sits inside the still-open string: no heredoc starts there, so
    // the following lines stay part of the command line.
    let command = "echo \"start\n<<'EOF' still quoted\"\necho after";
    assert_eq!(strip_quoted_heredoc_bodies(command), command);
}

#[test]
fn apostrophe_in_a_comment_does_not_hide_a_later_heredoc() {
    let command = "# don't gate the script below\ncat <<'EOF'\nprint(1)\nEOF";
    assert_eq!(
        strip_quoted_heredoc_bodies(command),
        "# don't gate the script below\ncat <<'EOF'"
    );
    let command = "echo hi # it's fine\ncat <<'EOF'\nimport os\nEOF";
    assert_eq!(
        strip_quoted_heredoc_bodies(command),
        "echo hi # it's fine\ncat <<'EOF'"
    );
}

#[test]
fn commands_after_the_heredoc_terminator_stay_gated() {
    let command = "echo \"a\nb\"; cat <<'EOF'\nprint(1)\nEOF\nevil_command";
    let err = gate(command)
        .expect_err("a command after the terminator must still be gated")
        .to_string();
    assert!(err.contains("evil_command"), "{err}");
}
