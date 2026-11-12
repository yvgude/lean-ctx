// SPDX-License-Identifier: Apache-2.0
//! GHSA-9jrf-q94p-frhr: a `( … )` subshell or `{ …; }` group is one segment
//! to the operator splitter, and its first word (`(eval`, `(rm`) matched no
//! rule; a redirection after the group (`(echo) >f`, `done >f`) was dropped
//! by the leaf walk. [`flatten`] opens every group into plain segments, so
//! the per-segment checks see each command inside it and each redirection
//! after it (with the `:` builtin as its program). The result is only read
//! by checks; the line that runs is never rewritten.

/// Index after the `)` matching the `(` at `open`, stepping over quotes and
/// nested parentheses; the line's end when it never closes.
fn matching_paren(s: &[u8], open: usize) -> usize {
    let mut depth = 0usize;
    let mut i = open;
    while i < s.len() {
        match s[i] {
            b'\\' => i += 1,
            b'\'' => i = skip_quote(s, i, b'\''),
            b'"' => i = skip_quote(s, i, b'"'),
            b'`' => i = skip_quote(s, i, b'`'),
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return i + 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    s.len()
}

/// Index of the quote closing the one opened at `open` (the line's last
/// index when it never closes). Single quotes take no escapes.
fn skip_quote(s: &[u8], open: usize, quote: u8) -> usize {
    let mut i = open + 1;
    while i < s.len() {
        if s[i] == b'\\' && quote != b'\'' {
            i += 2;
            continue;
        }
        if s[i] == quote {
            return i;
        }
        i += 1;
    }
    s.len().saturating_sub(1)
}

fn is_blank(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | b'\r')
}

fn is_separator(c: u8) -> bool {
    matches!(c, b';' | b'&' | b'|' | b'\n')
}

/// Whether a redirection (`>f`, `2>f`, `&>f`, `{fd}>f`, `<f`) starts at the
/// first non-blank byte from `from`.
fn redirection_follows(s: &[u8], from: usize) -> bool {
    let mut i = from;
    while i < s.len() && matches!(s[i], b' ' | b'\t') {
        i += 1;
    }
    if s.get(i) == Some(&b'{') {
        while i < s.len() && s[i] != b'}' {
            i += 1;
        }
        i += 1;
    } else if s.get(i) == Some(&b'&') {
        i += 1;
    } else {
        while i < s.len() && s[i].is_ascii_digit() {
            i += 1;
        }
    }
    matches!(s.get(i), Some(b'<' | b'>'))
}

/// `line` with every grouping parenthesis and every `{`/`}` group word made
/// a command separator: `(` becomes `;`, and a `)` or closing `}` that a
/// redirection follows becomes `;:`, so the redirection reads as one of the
/// `:` builtin. `$(…)`, `<(…)`, `>(…)` and `name=(…)` stay as written; so do quotes.
pub(super) fn flatten(line: &str) -> String {
    let s = line.as_bytes();
    if !s.iter().any(|c| matches!(c, b'(' | b')' | b'{' | b'}')) {
        return line.to_string();
    }
    let mut out = Vec::with_capacity(s.len() + 16);
    let mut i = 0;
    while i < s.len() {
        let c = s[i];
        let prev = if i == 0 { None } else { Some(s[i - 1]) };
        match c {
            b'\\' => {
                out.push(c);
                if let Some(&next) = s.get(i + 1) {
                    out.push(next);
                }
                i += 2;
                continue;
            }
            b'\'' | b'"' | b'`' => {
                let end = skip_quote(s, i, c);
                out.extend_from_slice(&s[i..=end.min(s.len() - 1)]);
                i = end + 1;
                continue;
            }
            b'(' if matches!(prev, Some(b'$' | b'<' | b'>' | b'=')) => {
                let end = matching_paren(s, i);
                out.extend_from_slice(&s[i..end]);
                i = end;
                continue;
            }
            // A `case` pattern's `)` (`x) eval …`) ends no group: the command
            // after it must lead its own segment, not follow `:`.
            b')' if redirection_follows(s, i + 1) => out.extend_from_slice(b";:"),
            b'(' | b')' => out.push(b';'),
            b'{' if prev.is_none_or(|p| is_blank(p) || is_separator(p))
                && s.get(i + 1).is_none_or(|&n| is_blank(n)) =>
            {
                out.push(b';');
            }
            b'}' if prev.is_none_or(|p| is_blank(p) || p == b';')
                && s.get(i + 1).is_none_or(|&n| {
                    is_blank(n) || is_separator(n) || matches!(n, b'<' | b'>')
                }) =>
            {
                if redirection_follows(s, i + 1) {
                    out.extend_from_slice(b";:");
                } else {
                    out.push(b';');
                }
            }
            _ => out.push(c),
        }
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| line.to_string())
}
