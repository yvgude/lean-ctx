use super::{
    SHELL_BUILTINS, ShellError, effective_allowlist, extract_all_commands,
    extract_base_from_segment,
};

/// $(), backticks, <() in arguments: warn by default, **block** when
/// `shell_strict_mode = true` (GH #391 — the strict knob previously only
/// changed the log line and never actually blocked).
pub(crate) fn check_substitution_in_args(command: &str, strict: bool) -> Result<(), ShellError> {
    if !has_expanding_substitution_in_args(command) {
        return Ok(());
    }

    // Extract inner commands from $(...) and check against allowlist + builtins.
    // Only warn/block if the inner command is genuinely non-allowlisted (#1024).
    let inner_cmds = extract_substitution_commands(command);
    if inner_cmds.is_empty() {
        return Ok(());
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

    if strict {
        tracing::warn!(
            "[SECURITY] Command substitution blocked (shell_strict_mode=true): {}",
            names.join(", ")
        );
        return Err(format!(
            "[BLOCKED — DO NOT RETRY] Command substitution with non-allowlisted command: {}. \
             Add to allowlist with `lean-ctx allow <cmd>` or set shell_strict_mode=false.\n\
             Command: {command}",
            names.join(", ")
        )
        .into());
    }
    // Advisory only: at `warn` it lands in the wrapped command's stderr, i.e.
    // in the agent's tool result, on every call (#1866).
    tracing::info!(
        "[SECURITY] Command substitution with non-allowlisted command (warn-only): {}",
        names.join(", ")
    );
    Ok(())
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
                if ch == b'$'
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
                i += 1;
            }
        }
    }
    results
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
