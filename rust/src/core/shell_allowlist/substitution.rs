use super::{
    SHELL_BUILTINS, ShellError, effective_allowlist, extract_all_commands,
    extract_base_from_segment,
};

/// $(), backticks, <() in arguments: a substituted command runs like any other,
/// so it is gated like one (GHSA-rxv5-4c3x-g4q6 E, owner decision 26.10.;
/// replaces the warn-only default of #1024). Whether a violation blocks or only
/// warns is decided by the shell security mode in `check_shell_allowlist`.
pub(crate) fn check_substitution_in_args(command: &str) -> Result<(), ShellError> {
    if !has_expanding_substitution_in_args(command) {
        return Ok(());
    }

    // Extract inner commands from $(...) and check against allowlist + builtins.
    // Only warn/block if the inner command is genuinely non-allowlisted (#1024).
    let mut inner_cmds = extract_substitution_commands(command);
    // GH #2132: a `case` inside a substitution ends the raw scan at its first
    // pattern `)`, which yields a `case …` fragment and misses the arm bodies.
    // Scan the case-rewritten text as well, so the bodies are what gets
    // checked; the raw scan stays, since it alone still sees substitutions in
    // the case subject and patterns, which the rewrite drops. If the rewrite
    // fails, the fragment keeps flagging `case` as before.
    if let Ok(rewritten) = super::rewrite_case_constructs(command)
        && rewritten != command
    {
        inner_cmds
            .retain(|inner| !matches!(extract_base_from_segment(inner).as_str(), "case" | "esac"));
        inner_cmds.extend(extract_substitution_commands(&rewritten));
    }
    if inner_cmds.is_empty() {
        return Ok(());
    }

    // GHSA-rxv5-4c3x-g4q6 E: a substituted command executes like any other, so
    // the rules that hold regardless of allowlist membership (eval/exec/source,
    // interpreter inline code, dangerous flags, blocked inline env) apply to it
    // in every mode — `echo "$(bash -c '…')"` must not run what `bash -c '…'`
    // may not.
    for inner in &inner_cmds {
        super::enforcement::check_unconditional_blocked_only(inner)?;
    }

    let allowlist = effective_allowlist();
    let dangerous: Vec<&str> = inner_cmds
        .iter()
        .filter(|inner| {
            let base = extract_base_from_segment(inner);
            !base.is_empty()
                && !SHELL_BUILTINS.contains(&base.as_str())
                && !allowlist.iter().any(|a| a == &base)
        })
        .map(String::as_str)
        .collect();

    if dangerous.is_empty() {
        return Ok(());
    }

    let names: Vec<String> = dangerous
        .iter()
        .map(|c| extract_base_from_segment(c))
        .collect();

    Err(format!(
        "[BLOCKED — DO NOT RETRY] Command substitution with non-allowlisted command: {}. \
         A substituted command is gated like one at the top level; add it with \
         `lean-ctx allow <cmd>` if it is safe.\n\
         Command: {command}",
        names.join(", ")
    )
    .into())
}

/// Extracts the base commands from `$(...)` substitutions in argument position.
/// Reuses the same single-quote / backslash-aware scanning as
/// `has_expanding_substitution_in_args` but collects the inner command text.
fn extract_substitution_commands(command: &str) -> Vec<String> {
    let bytes = command.as_bytes();
    let len = bytes.len();
    let mut results = Vec::new();
    let mut i = 0;
    let mut in_single_quote = false;
    let mut seen_space_after_cmd = false;

    while i < len {
        let ch = bytes[i];
        if in_single_quote {
            if ch == b'\'' {
                in_single_quote = false;
            }
            i += 1;
            continue;
        }
        if ch == b'\\' {
            i = (i + 2).min(len);
            continue;
        }
        match ch {
            b'\'' => {
                in_single_quote = true;
                i += 1;
            }
            b' ' | b'\t' if !seen_space_after_cmd => {
                seen_space_after_cmd = true;
                i += 1;
            }
            _ if !seen_space_after_cmd => {
                i += 1;
            }
            _ => {
                // `$(…)` command substitution and `<(…)`/`>(…)` process
                // substitution both run their contents (GHSA-rxv5-4c3x-g4q6 E:
                // only `$(` used to be scanned, so strict mode let the other
                // forms through unchecked).
                if (ch == b'$' || ch == b'<' || ch == b'>')
                    && i + 1 < len
                    && bytes[i + 1] == b'('
                    && let Some((inner, end)) = super::compound::balanced_paren_at(command, i + 1)
                {
                    let trimmed = inner.trim();
                    if !trimmed.is_empty() {
                        results.extend(extract_all_commands(trimmed));
                        results.extend(extract_substitution_commands(trimmed));
                    }
                    i = end;
                    continue;
                }
                if ch == b'`'
                    && let Some(close) = closing_backtick(bytes, i + 1)
                {
                    let trimmed = command[i + 1..close].trim();
                    if !trimmed.is_empty() {
                        results.extend(extract_all_commands(trimmed));
                        results.extend(extract_substitution_commands(trimmed));
                    }
                    i = close + 1;
                    continue;
                }
                i += 1;
            }
        }
    }
    results
}

/// Index of the backtick closing a substitution opened before `from`,
/// skipping backslash escapes. `None` when it is unterminated.
fn closing_backtick(bytes: &[u8], from: usize) -> Option<usize> {
    let mut j = from;
    while j < bytes.len() {
        match bytes[j] {
            b'\\' => j += 2,
            b'`' => return Some(j),
            _ => j += 1,
        }
    }
    None
}

/// Check for $(), backticks, <(, >( in arguments wherever the shell would
/// expand them — i.e. unquoted OR inside double quotes (single quotes inhibit
/// expansion). `git commit -m "$(cat f)"` expands; `grep '$(x)' f` does not.
pub(crate) fn has_expanding_substitution_in_args(command: &str) -> bool {
    let bytes = command.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    let mut in_single_quote = false;
    let mut seen_space_after_cmd = false;

    while i < len {
        let ch = bytes[i];
        if in_single_quote {
            if ch == b'\'' {
                in_single_quote = false;
            }
            i += 1;
            continue;
        }
        // Backslash inhibits expansion outside single quotes (GL #1160):
        // `\$(`, `\`` and `\<(` are literal data in bash — both unquoted and
        // inside double quotes.
        if ch == b'\\' {
            i = (i + 2).min(len);
            continue;
        }
        match ch {
            b'\'' => {
                in_single_quote = true;
                i += 1;
            }
            b' ' | b'\t' if !seen_space_after_cmd => {
                seen_space_after_cmd = true;
                i += 1;
            }
            _ if !seen_space_after_cmd => {
                i += 1;
            }
            _ => {
                if ch == b'$' && i + 1 < len && bytes[i + 1] == b'(' {
                    return true;
                }
                if ch == b'`' {
                    return true;
                }
                if (ch == b'<' || ch == b'>') && i + 1 < len && bytes[i + 1] == b'(' {
                    return true;
                }
                i += 1;
            }
        }
    }
    false
}
