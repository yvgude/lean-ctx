// SPDX-License-Identifier: Apache-2.0
//! GHSA-4gm5-2mgw-9r9r: bash reads a redirection anywhere in a simple
//! command, before the program (`>f cmd`, `2>/dev/null eval …`) or glued to a
//! word (`cmd>f`, `x 2>&1>f`, `ls {fd}>f`). Every check reads the first word
//! of a segment as its program and splits words on blanks only, so such a
//! line hid both the program (`>/tmp/cat rm …` read as `cat`) and the write
//! target. [`canonical`] rewrites each simple command the way bash reads it,
//! its words first and then every redirection as a word of its own, before
//! any check reads the line. The line that runs is never rewritten; this is
//! the text the checks analyse.

use crate::ShellError;

/// Reserved words a redirection may not precede (`>f if …` is a syntax
/// error in bash; refused rather than guessed).
const RESERVED: &[&str] = &[
    "if", "then", "else", "elif", "fi", "do", "done", "while", "until", "for", "case", "esac",
    "select", "function", "in", "{", "}", "!", "time", "[[", "]]",
];

/// Redirection operators, longest first.
const OPERATORS: &[&str] = &[
    "&>>", "&>", "<<<", "<<-", "<<", "<>", "<&", ">>", ">|", ">&", "<", ">",
];

#[derive(Debug)]
enum Lex {
    /// Blanks between words.
    Space,
    /// A command separator or grouping character (`;`, `&&`, `|`, newline,
    /// `(`, `)`), kept as written: the byte itself.
    Sep(u8),
    /// A word; `start..end` in the line.
    Word { start: usize, end: usize },
    /// A redirection: descriptor (`2`, or empty for `{name}` and none),
    /// operator, target word, and whether it was glued to the word before.
    Redir {
        fd: String,
        op: &'static str,
        target: (usize, usize),
        glued: bool,
        named_fd: bool,
    },
}

/// Index after the `)` matching the `(` at `open`, stepping over quotes and
/// nested substitutions; `None` when it never closes.
fn skip_parens(s: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut i = open;
    while i < s.len() {
        match s[i] {
            b'\\' => i += 1,
            b'\'' => i = skip_single(s, i)?,
            b'"' => i = skip_double(s, i)?,
            b'`' => i = skip_backtick(s, i)?,
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i + 1);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Index of the `'` closing the single-quoted string opened at `open`.
fn skip_single(s: &[u8], open: usize) -> Option<usize> {
    s[open + 1..]
        .iter()
        .position(|&c| c == b'\'')
        .map(|p| open + 1 + p)
}

/// Index of the `"` closing the double-quoted string opened at `open`.
fn skip_double(s: &[u8], open: usize) -> Option<usize> {
    let mut i = open + 1;
    while i < s.len() {
        match s[i] {
            b'\\' => i += 1,
            b'"' => return Some(i),
            b'$' if s.get(i + 1) == Some(&b'(') => i = skip_parens(s, i + 1)? - 1,
            b'`' => i = skip_backtick(s, i)?,
            _ => {}
        }
        i += 1;
    }
    None
}

/// Index of the backtick closing the one opened at `open`.
fn skip_backtick(s: &[u8], open: usize) -> Option<usize> {
    let mut i = open + 1;
    while i < s.len() {
        match s[i] {
            b'\\' => i += 1,
            b'`' => return Some(i),
            _ => {}
        }
        i += 1;
    }
    None
}

/// Index of the `}` closing the `${` opened at `open` (the `$`).
fn skip_braced(s: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut i = open + 1;
    while i < s.len() {
        match s[i] {
            b'\\' => i += 1,
            b'\'' => i = skip_single(s, i)?,
            b'"' => i = skip_double(s, i)?,
            b'`' => i = skip_backtick(s, i)?,
            b'$' if s.get(i + 1) == Some(&b'(') => i = skip_parens(s, i + 1)? - 1,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// End of the word starting at `start`; `<`/`>` end it unless they open a
/// process substitution, and so does a blank or a separator. `None` for an
/// unterminated quote or substitution.
fn word_end(s: &[u8], start: usize, in_test: bool) -> Option<usize> {
    let mut i = start;
    while i < s.len() {
        match s[i] {
            b'(' if i > start && s[i - 1] == b'=' => i = skip_parens(s, i)? - 1,
            b'(' if i == start && s.get(i + 1) == Some(&b'(') => i = skip_parens(s, i)? - 1,
            b' ' | b'\t' | b'\n' | b'\r' | b';' | b'&' | b'|' | b')' | b'(' => break,
            b'<' | b'>' if s.get(i + 1) == Some(&b'(') => i = skip_parens(s, i + 1)? - 1,
            b'<' | b'>' if !in_test => break,
            b'\\' => i += 1,
            b'\'' => i = skip_single(s, i)?,
            b'"' => i = skip_double(s, i)?,
            b'`' => i = skip_backtick(s, i)?,
            b'$' if s.get(i + 1) == Some(&b'\'') => i = skip_single(s, i + 1)?,
            b'$' if s.get(i + 1) == Some(&b'(') => i = skip_parens(s, i + 1)? - 1,
            b'$' if s.get(i + 1) == Some(&b'{') => i = skip_braced(s, i)?,
            _ => {}
        }
        i += 1;
    }
    Some(i.min(s.len()))
}

/// The redirection operator at `i`, if any.
fn operator_at(line: &str, i: usize) -> Option<&'static str> {
    let rest = &line[i..];
    if rest.starts_with("<(") || rest.starts_with(">(") {
        return None;
    }
    OPERATORS.iter().copied().find(|op| rest.starts_with(op))
}

/// A descriptor prefix of the word at `i` (`2>`, `{fd}>`): its text and
/// the index of the operator after it.
fn descriptor_at(line: &str, i: usize) -> Option<(String, usize, bool)> {
    let s = line.as_bytes();
    let digits = s[i..].iter().take_while(|c| c.is_ascii_digit()).count();
    if digits > 0 && matches!(s.get(i + digits), Some(b'<' | b'>')) {
        return Some((line[i..i + digits].to_string(), i + digits, false));
    }
    if s[i] == b'{' {
        let close = line[i..].find('}')? + i;
        let name = &line[i + 1..close];
        let is_name = name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        if is_name && matches!(s.get(close + 1), Some(b'<' | b'>')) {
            return Some((String::new(), close + 1, true));
        }
    }
    None
}

fn lex(line: &str) -> Option<Vec<Lex>> {
    let s = line.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    let mut in_test = false;
    let mut after_word = false;
    while i < s.len() {
        match s[i] {
            b' ' | b'\t' => {
                out.push(Lex::Space);
                after_word = false;
                i += 1;
                continue;
            }
            b'\n' | b'\r' | b';' | b'|' | b'(' | b')' => {
                out.push(Lex::Sep(s[i]));
                after_word = false;
                i += 1;
                continue;
            }
            b'&' if s.get(i + 1) != Some(&b'>') => {
                out.push(Lex::Sep(s[i]));
                after_word = false;
                i += 1;
                continue;
            }
            _ => {}
        }
        let (fd, op_at, named_fd) = if after_word {
            (String::new(), i, false)
        } else {
            descriptor_at(line, i).unwrap_or((String::new(), i, false))
        };
        if !in_test && let Some(op) = operator_at(line, op_at) {
            let mut t = op_at + op.len();
            while t < s.len() && matches!(s[t], b' ' | b'\t') {
                t += 1;
            }
            let end = word_end(s, t, false)?;
            if end == t {
                // No target: a syntax error bash refuses; leave it as written.
                return None;
            }
            out.push(Lex::Redir {
                fd,
                op,
                target: (t, end),
                glued: after_word,
                named_fd,
            });
            i = end;
            after_word = true;
            continue;
        }
        let end = word_end(s, i, in_test)?;
        if end == i {
            // A lone character no rule above takes: keep it as a word.
            out.push(Lex::Word {
                start: i,
                end: i + 1,
            });
            i += 1;
            after_word = true;
            continue;
        }
        match &line[i..end] {
            "[[" => in_test = true,
            "]]" => in_test = false,
            _ => {}
        }
        out.push(Lex::Word { start: i, end });
        i = end;
        after_word = true;
    }
    Some(out)
}

fn is_assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| {
        name.chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && name
                .trim_end_matches('+')
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

/// `line` with every simple command rewritten as bash reads it: its words in
/// order, then its redirections (`>/tmp/cat rm -rf x` → `rm -rf x >/tmp/cat`,
/// `cat a>f` → `cat a >f`, `ls {fd}>f` → `ls >f`). A command that is only
/// redirections (`>f`) gets the `:` builtin as its program. A redirection
/// before a reserved word (`>f if …`) and a program word that holds a quoted
/// `<` or `>` (`'>/tmp/cat' …`, read as the program `cat` by the checks) are
/// refused. A line this cannot read (an unterminated quote) is returned as
/// written: the lexer before it already refuses those.
pub(super) fn canonical(line: &str) -> Result<String, ShellError> {
    if !line.contains(['<', '>']) {
        return Ok(line.to_string());
    }
    let Some(lexed) = lex(line) else {
        return Ok(line.to_string());
    };
    let mut out = String::with_capacity(line.len() + 8);
    let mut cmd: Vec<&Lex> = Vec::new();
    let flush =
        |cmd: &mut Vec<&Lex>, out: &mut String, after_group: bool| -> Result<(), ShellError> {
            let words: Vec<&str> = cmd
                .iter()
                .filter_map(|l| match l {
                    Lex::Word { start, end } => Some(&line[*start..*end]),
                    _ => None,
                })
                .collect();
            // `(cmd) >f`: the redirection belongs to the group before it; the
            // group checks read that suffix themselves.
            if after_group && words.is_empty() {
                for l in cmd.iter() {
                    push_as_written(line, l, out);
                }
                cmd.clear();
                return Ok(());
            }
            let first_word_at = cmd.iter().position(|l| matches!(l, Lex::Word { .. }));
            let first_redir_at = cmd.iter().position(|l| matches!(l, Lex::Redir { .. }));
            if let Some(program) = words
                .iter()
                .find(|w| !is_assignment(w) && !w.starts_with("(("))
                && program.contains(['<', '>'])
                && !program.starts_with(['<', '>'])
            {
                return Err(format!(
                    "[BLOCKED — DO NOT RETRY] The program word `{program}` holds a `<` or `>`; the \
                 shell gate cannot tell which program runs. Name the program plainly."
                )
                .into());
            }
            let needs = cmd.iter().any(|l| {
                matches!(
                    l,
                    Lex::Redir { glued: true, .. } | Lex::Redir { named_fd: true, .. }
                )
            }) || match (first_redir_at, first_word_at) {
                (Some(r), Some(w)) => r < w,
                (Some(_), None) => true,
                _ => false,
            };
            if !needs {
                for l in cmd.iter() {
                    push_as_written(line, l, out);
                }
                cmd.clear();
                return Ok(());
            }
            if let (Some(r), Some(w)) = (first_redir_at, first_word_at)
                && r < w
                && RESERVED.contains(&words[0])
            {
                return Err(format!(
                    "[BLOCKED — DO NOT RETRY] A redirection stands before the reserved word \
                 `{}`; the shell gate cannot validate that form. Put the redirection after \
                 the command.",
                    words[0]
                )
                .into());
            }
            if cmd.first().is_some_and(|l| matches!(l, Lex::Space)) {
                out.push(' ');
            }
            if words.is_empty() {
                out.push(':');
            } else {
                out.push_str(&words.join(" "));
            }
            for l in cmd.iter() {
                if let Lex::Redir { fd, op, target, .. } = l {
                    out.push(' ');
                    out.push_str(fd);
                    out.push_str(op);
                    out.push_str(&line[target.0..target.1]);
                }
            }
            if cmd.len() > 1 && cmd.last().is_some_and(|l| matches!(l, Lex::Space)) {
                out.push(' ');
            }
            cmd.clear();
            Ok(())
        };
    let mut after_group = false;
    for l in &lexed {
        if let Lex::Sep(byte) = l {
            flush(&mut cmd, &mut out, after_group)?;
            out.push(*byte as char);
            after_group = *byte == b')';
            continue;
        }
        cmd.push(l);
    }
    flush(&mut cmd, &mut out, after_group)?;
    Ok(out)
}

fn push_as_written(line: &str, l: &Lex, out: &mut String) {
    match l {
        Lex::Space => out.push(' '),
        Lex::Sep(_) => {}
        Lex::Word { start, end } => out.push_str(&line[*start..*end]),
        Lex::Redir {
            fd,
            op,
            target,
            named_fd,
            ..
        } => {
            if *named_fd {
                out.push_str(op);
            } else {
                out.push_str(fd);
                out.push_str(op);
            }
            out.push_str(&line[target.0..target.1]);
        }
    }
}
