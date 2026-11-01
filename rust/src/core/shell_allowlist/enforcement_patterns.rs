// SPDX-License-Identifier: Apache-2.0

//! Coarse whole-command pattern checks of the enforcement module: unconditional blocks and
//! substitutions at command position.

use super::{UNCONDITIONAL_BLOCKED, shell_tokenize, skip_env_assignments, split_on_operators};

/// Detect dangerous shell patterns that bypass allowlist intent.
///
/// Only blocks patterns that are genuinely dangerous at command position.
/// `$()` and backticks in *arguments* are allowed — the base command is
/// already validated by the allowlist, and blocking substitutions in
/// arguments breaks legitimate workflows (e.g. `git commit -m "$(cat ...)"`,
/// pre-commit hooks, playwright scripts).
pub(super) fn has_dangerous_patterns(command: &str) -> bool {
    let trimmed = command.trim();
    // #1829: the separator scan below is not quote-aware, so single-quoted
    // text (a jq program such as `'x | . as $r | ($r|z)'`) matched `| . ` and
    // was blocked. Single quotes are pure data to the shell; mask them out.
    // The real command-position check runs per parsed segment elsewhere.
    let unquoted = mask_single_quoted(trimmed);

    for blocked in UNCONDITIONAL_BLOCKED {
        let with_space = format!("{blocked} ");
        if unquoted.starts_with(&with_space) {
            return true;
        }
        for sep in ["; ", "&& ", "|| ", "| ", "\n"] {
            if unquoted.contains(&format!("{sep}{blocked} ")) {
                return true;
            }
        }
    }

    if has_substitution_at_command_pos(trimmed) {
        return true;
    }

    false
}

/// Replace the contents of single-quoted strings with spaces, keeping the
/// quotes and everything else byte-for-byte. A `'` inside double quotes or
/// escaped with `\` does not open a string. This only feeds the coarse
/// separator scan above; the per-segment `UNCONDITIONAL_BLOCKED` check in
/// `check_all_segments` / `check_unconditional_blocked_only` stays the
/// authority on what runs at command position.
fn mask_single_quoted(command: &str) -> String {
    let mut out = String::with_capacity(command.len());
    let (mut in_single, mut in_double, mut escaped) = (false, false, false);
    for ch in command.chars() {
        if in_single {
            if ch == '\'' {
                in_single = false;
                out.push(ch);
            } else {
                out.push(' ');
            }
            continue;
        }
        out.push(ch);
        if escaped {
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == '"' {
            in_double = !in_double;
        } else if ch == '\'' && !in_double {
            in_single = true;
        }
    }
    out
}

/// Check if `$()` or backticks appear at command position (first token
/// of any segment). Substitutions in *arguments* are intentionally
/// allowed — the security boundary is the base-command allowlist check.
fn has_substitution_at_command_pos(command: &str) -> bool {
    let segments = split_on_operators(command);
    for seg in segments {
        let trimmed = seg.trim();
        let cmd_start = skip_env_assignments(trimmed);

        if cmd_start.starts_with("$(") {
            return true;
        }

        let tokens = shell_tokenize(cmd_start);
        let first_token = tokens.first().map_or("", std::string::String::as_str);
        if first_token.starts_with('`') || first_token == "`" {
            return true;
        }
    }
    false
}
