//! Heredoc and comment stripping, derived from the one lexer (`lexer::lex`)
//! so every check agrees with the substitution scanner on where a heredoc
//! body, a comment, a quote or a `${…}` begins and ends (GHSA-rxv5-4c3x-g4q6 E).

use super::lexer::{lex, remove_ranges};

/// Strip the *bodies* of quoted-delimiter heredocs (`<<'EOF' … EOF`,
/// `<<-"E" … E`) prior to allowlist analysis (#876).
///
/// A quoted heredoc delimiter disables all shell expansion, so every body line
/// is pure literal stdin data — never an executable command. Left in place, the
/// operator-splitter dices those lines into "segments" and blocks the first word
/// that isn't allowlisted (e.g. a commit message piped via `git commit -F -`,
/// whose first token is `feat(...)`).
///
/// Only quoted delimiters are stripped. An *unquoted* `<<EOF` heredoc DOES expand
/// `$()`/backticks/`$VAR` in its body, so those bodies are deliberately left
/// intact for the command-substitution checks to see.
#[cfg(test)]
pub(crate) fn strip_quoted_heredoc_bodies(command: &str) -> String {
    strip_for_analysis(command, true, false)
}

/// Like `strip_quoted_heredoc_bodies` but strips bodies for **all** heredocs
/// (quoted *and* unquoted delimiters). Use for checks that must never interpret
/// heredoc body content as commands or redirects (#931).
pub fn strip_all_heredoc_bodies(command: &str) -> String {
    strip_for_analysis(command, false, false)
}

/// `command` without heredoc bodies (only quoted ones with `quoted_only`) and,
/// with `comments`, without comments — in one lexer pass over the raw text, so
/// neither strip can misread what the other one removed.
///
/// A terminator must match its delimiter exactly (after leading tabs for
/// `<<-`), as in the shell. A body whose terminator never follows runs to the
/// end of the input here; `enforce_shell_allowlist` blocks such a command.
pub(crate) fn strip_for_analysis(command: &str, quoted_only: bool, comments: bool) -> String {
    if !command.contains("<<") && !(comments && command.contains('#')) {
        return command.to_string();
    }
    let lexed = lex(command);
    let mut ranges: Vec<_> = lexed
        .heredocs
        .iter()
        .filter(|h| !quoted_only || h.quoted)
        .map(|h| h.body.clone())
        .collect();
    if comments {
        ranges.extend(lexed.comments);
    }
    remove_ranges(command, ranges)
}

/// The delimiters of the heredoc operators in `line`, in source order (only
/// quoted ones with `quoted_only`). A `<<` inside quotes, `${…}` or arithmetic
/// is no operator; a `<<<` here-string has no body.
pub(crate) fn heredoc_delims(line: &str, quoted_only: bool) -> Vec<String> {
    if !line.contains("<<") {
        return Vec::new();
    }
    lex(line)
        .heredocs
        .into_iter()
        .filter(|h| !quoted_only || h.quoted)
        .map(|h| h.delim)
        .collect()
}

/// Parse a heredoc delimiter token starting at `start`, returning its bare name
/// (quotes/escapes removed), whether any part was quoted, and the index just
/// past the token. `None` when no delimiter is present.
pub(crate) fn read_heredoc_delim(bytes: &[u8], start: usize) -> Option<(String, bool, usize)> {
    let len = bytes.len();
    let mut i = start;
    let mut name: Vec<u8> = Vec::new();
    let mut quoted = false;
    while i < len {
        match bytes[i] {
            b'\'' => {
                quoted = true;
                i += 1;
                while i < len && bytes[i] != b'\'' {
                    name.push(bytes[i]);
                    i += 1;
                }
                i += usize::from(i < len); // skip closing quote if present
            }
            b'"' => {
                quoted = true;
                i += 1;
                while i < len && bytes[i] != b'"' {
                    name.push(bytes[i]);
                    i += 1;
                }
                i += usize::from(i < len);
            }
            b'\\' => {
                quoted = true;
                i += 1;
                if i < len {
                    name.push(bytes[i]);
                    i += 1;
                }
            }
            b' ' | b'\t' | b'<' | b'>' | b'|' | b'&' | b';' => break,
            c => {
                name.push(c);
                i += 1;
            }
        }
    }
    if name.is_empty() {
        None
    } else {
        Some((String::from_utf8_lossy(&name).into_owned(), quoted, i))
    }
}

/// Strip shell comments (`#` to end-of-line) so the allowlist tokenizer never
/// mistakes a comment for a command (#1109).
///
/// A `#` starts a comment only where the shell starts one: unquoted, outside
/// `${…}` and arithmetic, at the start of a word — at the start of the command,
/// after a blank, after one of `;`, `&`, `|`, `(`, or after a `)` that closed a
/// group (`(a)#c`, `f()#c`). This deliberately leaves intact:
///   - `#` inside single or double quotes (`echo "# not a comment"`),
///   - parameter expansions like `${#arr}`, `${var#prefix}`, `${x:- #y}`,
///   - arithmetic bases like `$((16#ff))`,
///   - URLs / fragments like `http://host/path#frag`, `$(a)#b`,
///   - an escaped `\#`,
///   - heredoc bodies (data, never comments).
///
/// The gate strips comments together with heredoc bodies through
/// [`strip_for_analysis`]; this form is kept for the comment tests.
#[cfg(test)]
pub(crate) fn strip_comments(command: &str) -> String {
    if !command.contains('#') {
        return command.to_string();
    }
    remove_ranges(command, lex(command).comments)
}
