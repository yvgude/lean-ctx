use super::heredoc::strip_for_analysis;
use super::lexer::{Expansion, lex};
use super::{
    SHELL_BUILTINS, ShellError, effective_allowlist, extract_all_commands,
    extract_base_from_segment,
};

/// $(), backticks, <() in arguments: a substituted command runs like any other,
/// so it is gated like one (GHSA-rxv5-4c3x-g4q6 E, owner decision 26.10.;
/// replaces the warn-only default of #1024). Whether a violation blocks or only
/// warns is decided by the shell security mode in `check_shell_allowlist`.
///
/// `command` is the raw line (line continuations normalized, nothing else
/// stripped): the scanner below reads heredocs and comments itself, so a
/// pre-stripped line would hide the lines after a quoted heredoc operator.
pub(crate) fn check_substitution_in_args(command: &str) -> Result<(), ShellError> {
    if !has_expanding_substitution_in_args(command) {
        return Ok(());
    }

    // Extract inner commands from $(...) and check against allowlist + builtins.
    // Only warn/block if the inner command is genuinely non-allowlisted (#1024).
    let mut inner_cmds = extract_substitution_commands(command)?;
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
        inner_cmds.extend(extract_substitution_commands(&rewritten)?);
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

/// The commands every expanding substitution in `command` runs, recursively.
///
/// A substitution body is a script of its own: heredoc bodies in it are
/// stdin data and comments are no commands, so both are stripped before the
/// body is split into commands (`git commit -m "$(cat <<EOF … EOF)"` must not
/// turn a commit message line into a command). Substitutions nested in the
/// body — including those in an unquoted heredoc body, which do expand — are
/// found by the recursive scan of the unstripped body.
fn extract_substitution_commands(command: &str) -> Result<Vec<String>, ShellError> {
    let mut results = Vec::new();
    for Expansion { body, arithmetic } in lex(command).expansions {
        // `$(( expr ))` / `$[ expr ]` / `(( expr ))` is arithmetic (#1664, bash's rule as in
        // #2134): it runs nothing itself, only the substitutions inside it.
        let bodies = if arithmetic {
            super::compound::arithmetic_substitutions(body, command)?
        } else {
            vec![body]
        };
        for body in bodies {
            let trimmed = body.trim();
            if trimmed.is_empty() {
                continue;
            }
            results.extend(extract_all_commands(&strip_for_analysis(
                trimmed, false, true,
            )));
            results.extend(extract_substitution_commands(trimmed)?);
        }
    }
    Ok(results)
}

/// Check for $(), backticks, <(, >( wherever the shell would expand them —
/// unquoted, inside double quotes, or in an unquoted heredoc body (single
/// quotes, `$'…'`, a backslash and a quoted heredoc inhibit expansion).
/// `git commit -m "$(cat f)"` expands; `grep '$(x)' f` does not. Command
/// position is scanned too: a substitution there is blocked earlier by
/// `has_dangerous_patterns`, and one in a redirect target right after a
/// subshell (`(true)>$(cmd)`) must be found without any space before it.
pub(crate) fn has_expanding_substitution_in_args(command: &str) -> bool {
    !lex(command).expansions.is_empty()
}
