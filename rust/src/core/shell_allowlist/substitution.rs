use super::{
    SHELL_BUILTINS, ShellError, effective_allowlist, extract_all_commands,
    extract_base_from_segment, strip_all_heredoc_bodies, strip_comments,
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
    for Expansion { body, arithmetic } in expansions(command) {
        // `$(( expr ))` / `(( expr ))` is arithmetic (#1664, bash's rule as in
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
            results.extend(extract_all_commands(&strip_comments(
                &strip_all_heredoc_bodies(trimmed),
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
    !expansions(command).is_empty()
}

/// One expansion the shell performs: the inner text of `$( … )`, `<( … )`,
/// `>( … )` or a backtick pair, or — `arithmetic` — the expression of
/// `$(( … ))` or of an arithmetic command `(( … ))`.
struct Expansion<'a> {
    body: &'a str,
    arithmetic: bool,
}

/// The lexical context the scanner starts in.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Lex {
    /// Ordinary shell text: quotes, comments and heredoc operators apply.
    Shell,
    /// An unquoted heredoc body: only `\`, `$( … )` and backticks are
    /// special; quotes are literal text (`it's $(cmd)` expands).
    HeredocBody,
}

/// Every top-level expansion in `command`, in source order. Nested ones are
/// inside the returned bodies; callers recurse into those.
fn expansions(command: &str) -> Vec<Expansion<'_>> {
    let mut found = Vec::new();
    scan(command, 0, Lex::Shell, false, Some(&mut found));
    found
}

/// A heredoc whose body starts after the current line.
struct PendingHeredoc {
    delim: String,
    quoted: bool,
    strip_tabs: bool,
}

/// The one quote-state machine behind substitution detection and extraction
/// (GHSA-rxv5-4c3x-g4q6 E). It tracks double quotes (a `'` inside them is
/// literal), single quotes, ANSI-C `$'…'` with its backslash escapes,
/// backslashes, comments and heredocs, and it walks into nothing it does not
/// understand: an unterminated substitution takes the rest of the input as
/// its body, so it is checked rather than skipped (fail closed).
///
/// With `close_paren` it scans the body of a substitution opened just before
/// `start` and returns the index just past its closing `)`, or `None` when it
/// is unterminated. Otherwise it scans to the end and returns `Some(len)`.
/// Expansions are collected into `out` only at this level.
fn scan<'a>(
    s: &'a str,
    start: usize,
    lex: Lex,
    close_paren: bool,
    mut out: Option<&mut Vec<Expansion<'a>>>,
) -> Option<usize> {
    let bytes = s.as_bytes();
    let len = bytes.len();
    let unterminated = if close_paren { None } else { Some(len) };
    let mut i = start;
    let mut in_double = false;
    let mut depth: u32 = 0;
    // Inside an arithmetic command `(( … ))` up to this index: its `<<` is a
    // shift, not a heredoc operator.
    let mut arithmetic_until = 0;
    let mut heredocs: Vec<PendingHeredoc> = Vec::new();

    while i < len {
        let ch = bytes[i];
        if ch == b'\\' {
            i += 2;
            continue;
        }
        if ch == b'$' && bytes.get(i + 1) == Some(&b'(') {
            // `$(( expr ))` is arithmetic when the inner group closes right
            // before the outer `)`; anything else is a command substitution.
            if bytes.get(i + 2) == Some(&b'(')
                && let Some((expr, end)) = super::compound::balanced_paren_at(s, i + 2)
                && bytes.get(end) == Some(&b')')
            {
                push(&mut out, expr, true);
                i = end + 1;
                continue;
            }
            match substitution_at(s, i + 1, &mut out) {
                Some(end) => i = end,
                None => return unterminated,
            }
            continue;
        }
        if ch == b'`' {
            let close = closing_backtick(bytes, i + 1);
            push(&mut out, &s[i + 1..close.unwrap_or(len)], false);
            match close {
                Some(close) => i = close + 1,
                None => return unterminated,
            }
            continue;
        }
        if lex == Lex::HeredocBody {
            i += 1;
            continue;
        }
        if in_double {
            in_double = ch != b'"';
            i += 1;
            continue;
        }
        match ch {
            b'"' => in_double = true,
            // Up to the closing quote; the `i += 1` below steps past it.
            b'\'' => match s[i + 1..].find('\'') {
                Some(k) => i += k + 1,
                None => return unterminated,
            },
            b'$' if bytes.get(i + 1) == Some(&b'\'') => match ansi_c_end(bytes, i + 2) {
                Some(close) => i = close,
                None => return unterminated,
            },
            b'#' if i == start
                || matches!(
                    bytes[i - 1],
                    b' ' | b'\t' | b'\n' | b'\r' | b';' | b'&' | b'|' | b'('
                ) =>
            {
                // A comment runs to the end of the line; the newline itself
                // is scanned next (it may start heredoc bodies).
                i = s[i..].find('\n').map_or(len, |k| i + k);
                continue;
            }
            b'<' if bytes.get(i + 1) == Some(&b'<') && i >= arithmetic_until => {
                if bytes.get(i + 2) == Some(&b'<') {
                    // `<<<` is a here-string: its word is scanned normally.
                    i += 3;
                    continue;
                }
                i = read_heredoc_operator(bytes, i + 2, &mut heredocs);
                continue;
            }
            b'<' | b'>' if bytes.get(i + 1) == Some(&b'(') => {
                match substitution_at(s, i + 1, &mut out) {
                    Some(end) => i = end,
                    None => return unterminated,
                }
                continue;
            }
            b'(' => {
                if bytes.get(i + 1) == Some(&b'(')
                    && let Some((expr, end)) = super::compound::balanced_paren_at(s, i + 1)
                    && bytes.get(end) == Some(&b')')
                {
                    // An arithmetic command: its substitutions are gated like
                    // those of `$(( … ))`; the scan still walks through it.
                    push(&mut out, expr, true);
                    arithmetic_until = end + 1;
                }
                depth += 1;
            }
            b')' if close_paren => {
                if depth == 0 {
                    return Some(i + 1);
                }
                depth -= 1;
            }
            b'\n' if !heredocs.is_empty() => {
                i = heredoc_bodies(s, i + 1, &mut heredocs, &mut out);
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    unterminated
}

fn push<'a>(out: &mut Option<&mut Vec<Expansion<'a>>>, body: &'a str, arithmetic: bool) {
    if let Some(out) = out.as_deref_mut() {
        out.push(Expansion { body, arithmetic });
    }
}

/// A substitution whose `(` sits at `open` (`$(`, `<(`, `>(`): records its
/// body and returns the index past its `)`. Unterminated, the body is the rest
/// of the input and `None` is returned.
fn substitution_at<'a>(
    s: &'a str,
    open: usize,
    out: &mut Option<&mut Vec<Expansion<'a>>>,
) -> Option<usize> {
    let end = scan(s, open + 1, Lex::Shell, true, None);
    push(out, &s[open + 1..end.map_or(s.len(), |e| e - 1)], false);
    end
}

/// Index of the `'` closing an ANSI-C string whose body starts at `from`;
/// a backslash escapes the next byte (`$'\''` is one quote character).
fn ansi_c_end(bytes: &[u8], from: usize) -> Option<usize> {
    let mut j = from;
    while j < bytes.len() {
        match bytes[j] {
            b'\\' => j += 2,
            b'\'' => return Some(j),
            _ => j += 1,
        }
    }
    None
}

/// Parse the delimiter after a `<<`/`<<-` operator (`from` is just past
/// `<<`), queue the heredoc and return the index past the delimiter word.
/// The word ends at a shell metacharacter or the end of the line.
fn read_heredoc_operator(bytes: &[u8], from: usize, heredocs: &mut Vec<PendingHeredoc>) -> usize {
    let mut j = from;
    let strip_tabs = bytes.get(j) == Some(&b'-');
    if strip_tabs {
        j += 1;
    }
    while matches!(bytes.get(j), Some(b' ' | b'\t')) {
        j += 1;
    }
    let word_limit = bytes[j..]
        .iter()
        .position(|b| matches!(b, b'\n' | b'(' | b')'))
        .map_or(bytes.len(), |k| j + k);
    match super::read_heredoc_delim(&bytes[..word_limit], j) {
        Some((delim, quoted, next)) => {
            heredocs.push(PendingHeredoc {
                delim,
                quoted,
                strip_tabs,
            });
            next
        }
        None => j,
    }
}

/// Consume the bodies of the queued heredocs, starting at the line beginning
/// at `pos`; return the index of the first line after the last terminator.
///
/// The terminator must match exactly (after leading tabs for `<<-`), as in
/// bash: a lenient match would end a body early and resume the quote scan
/// inside data, where a stray `'` could hide the real lines that follow. An
/// unquoted body expands substitutions, so it is scanned as
/// [`Lex::HeredocBody`]; a quoted one is pure data.
fn heredoc_bodies<'a>(
    s: &'a str,
    mut pos: usize,
    heredocs: &mut Vec<PendingHeredoc>,
    out: &mut Option<&mut Vec<Expansion<'a>>>,
) -> usize {
    let len = s.len();
    for heredoc in heredocs.drain(..) {
        let body_start = pos;
        let mut body_end = len;
        while pos < len {
            let line_end = s[pos..].find('\n').map_or(len, |k| pos + k);
            let line = &s[pos..line_end];
            let line = if heredoc.strip_tabs {
                line.trim_start_matches('\t')
            } else {
                line
            };
            let next = (line_end + 1).min(len);
            if line == heredoc.delim {
                body_end = pos;
                pos = next;
                break;
            }
            pos = next;
        }
        if !heredoc.quoted && out.is_some() {
            scan(
                &s[..body_end],
                body_start,
                Lex::HeredocBody,
                false,
                out.as_deref_mut(),
            );
        }
    }
    pos
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
