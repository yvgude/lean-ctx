// SPDX-License-Identifier: Apache-2.0
//! The one lexical pass the shell gate trusts (GHSA-rxv5-4c3x-g4q6 E).
//!
//! Comment stripping, heredoc stripping and the substitution check used to run
//! four separate scanners that disagreed about quotes, `${…}`, arithmetic and
//! heredocs. Wherever they disagreed with the shell, text the shell executes
//! was dropped from the check: a `#` inside `${x:- #…}` read as a comment, a
//! `<<` inside `${…}` or `(( … ))` read as a heredoc operator that swallowed
//! every following line. [`lex`] is now the single source of those decisions;
//! `strip_comments`, the heredoc strippers and `heredoc_delims` derive from it.
//!
//! Where bash, dash and zsh read the same text differently (all three can be
//! the executing `$SHELL`), the lexer does not pick one: it reports a
//! [`Lexed::problem`] and the gate blocks the command.

use std::ops::Range;

use super::compound::balanced_paren_at;

/// One expansion the shell performs: the inner text of `$( … )`, `<( … )`,
/// `>( … )`, zsh `=( … )` or a backtick pair, or — `arithmetic` — the
/// expression of `$(( … ))` or an arithmetic command `(( … ))`.
pub(super) struct Expansion<'a> {
    pub(super) body: &'a str,
    pub(super) arithmetic: bool,
}

/// A heredoc and the text its body occupies.
pub(super) struct Heredoc {
    pub(super) delim: String,
    pub(super) quoted: bool,
    /// From the newline ending the operator's line through the end of the
    /// terminator line (its own newline excluded): removing it leaves the
    /// operator line followed by the line after the terminator.
    pub(super) body: Range<usize>,
}

/// What [`lex`] found in a command.
#[derive(Default)]
pub(super) struct Lexed<'a> {
    /// Top-level expansions in source order; nested ones are inside the bodies.
    pub(super) expansions: Vec<Expansion<'a>>,
    /// `#` to the end of its line, wherever the shell starts a comment.
    pub(super) comments: Vec<Range<usize>>,
    /// Every heredoc, in operator order, at any nesting depth.
    pub(super) heredocs: Vec<Heredoc>,
    /// Every `\`+newline the shell removes as a line continuation: not one
    /// that is itself escaped (`\\⏎`), single-quoted or in a quoted heredoc.
    pub(super) continuations: Vec<Range<usize>>,
    /// The first construct the shells disagree on, or that cannot be
    /// validated (an unterminated heredoc). The gate blocks when set.
    pub(super) problem: Option<&'static str>,
}

/// Lex `command` as the shell would.
pub(super) fn lex(command: &str) -> Lexed<'_> {
    let mut lexer = Lexer {
        s: command,
        out: Lexed::default(),
        pending: Vec::new(),
    };
    lexer.scan(command.len(), 0, Mode::Shell, Close::Eof, true);
    // Operators with no line after them have an empty body.
    let len = command.len();
    for heredoc in std::mem::take(&mut lexer.pending) {
        lexer.out.heredocs.push(Heredoc {
            delim: heredoc.delim,
            quoted: heredoc.quoted,
            body: len..len,
        });
    }
    lexer.out
}

/// `command` without the given byte ranges (overlaps merged).
pub(super) fn remove_ranges(command: &str, mut ranges: Vec<Range<usize>>) -> String {
    if ranges.is_empty() {
        return command.to_string();
    }
    ranges.sort_by_key(|r| r.start);
    let mut out = String::with_capacity(command.len());
    let mut at = 0;
    for range in ranges {
        if range.start > at {
            out.push_str(&command[at..range.start]);
        }
        at = at.max(range.end);
    }
    out.push_str(&command[at.min(command.len())..]);
    out
}

const UNTERMINATED_HEREDOC: &str = "a here-document whose terminator line never follows";
const NESTED_HEREDOC: &str = "here-documents queued across a substitution boundary";
const ANSI_C_ESCAPED_QUOTE: &str =
    "a `$'…'` string with an escaped quote, which bash/zsh and dash end at different quotes";
const QUOTE_IN_QUOTED_PARAM: &str =
    "a `'` inside a double-quoted `${…}`, which bash and dash/zsh end differently";
const BRACE_IN_PARAM: &str = "a `{` inside `${…}`, which bash/dash and zsh end differently";
const SHIFT_IN_ARITHMETIC_COMMAND: &str =
    "`<<` inside an arithmetic command `(( … ))`, which dash reads as a here-document";
const LEGACY_ARITHMETIC: &str = "legacy `$[ … ]` arithmetic, which dash reads as shell syntax \
     (use `$(( … ))`)";
const ZSH_PARAM_FLAGS: &str =
    "zsh parameter flags `${(…)…}`, which can evaluate a value as code (`(e)`)";
const PROMPT_TRANSFORM: &str =
    "a `${…@P}` prompt expansion, which runs the substitutions in the value";
const ATTACHED_PAREN: &str = "a `(` attached to a word, which zsh reads as glob qualifiers that can \
     run code (`*(e:…:)`, `*(+cmd)`) and bash as an extended glob";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Ordinary shell text: quotes, comments and heredoc operators apply.
    Shell,
    /// An unquoted heredoc body: only `\`, `$…` expansions and backticks are
    /// special; quotes are literal text (`it's $(cmd)` expands).
    HeredocBody,
    /// The word of a `${…}`: no comments and no heredoc operators. `quoted`:
    /// the `${` itself stands inside double quotes.
    Param { quoted: bool },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Close {
    /// Scan to the end of the input.
    Eof,
    /// The `)` closing a substitution opened before the start.
    Paren,
    /// The `}` closing a `${`.
    Brace,
}

struct PendingHeredoc {
    delim: String,
    quoted: bool,
    strip_tabs: bool,
}

struct Lexer<'a> {
    s: &'a str,
    out: Lexed<'a>,
    /// Heredocs whose body starts after the current line.
    pending: Vec<PendingHeredoc>,
}

impl<'a> Lexer<'a> {
    fn flag(&mut self, problem: &'static str) {
        self.out.problem.get_or_insert(problem);
    }

    fn push(&mut self, collect: bool, body: &'a str, arithmetic: bool) {
        if collect {
            self.out.expansions.push(Expansion { body, arithmetic });
        }
    }

    /// Scan `s[start..end]` in `mode`. With [`Close::Paren`] or
    /// [`Close::Brace`] it returns the index just past the closing
    /// delimiter, or `None` when it is unterminated (the rest of the input
    /// was scanned). With [`Close::Eof`] it returns `Some(end)`. Expansions
    /// are collected only with `collect`; comments and heredocs always.
    fn scan(
        &mut self,
        end: usize,
        start: usize,
        mode: Mode,
        close: Close,
        collect: bool,
    ) -> Option<usize> {
        let s = self.s;
        let bytes = &s.as_bytes()[..end];
        let unterminated = (close == Close::Eof).then_some(end);
        let pending_base = self.pending.len();
        let param = matches!(mode, Mode::Param { .. });
        let mut i = start;
        let mut in_double = false;
        let mut depth: u32 = 0;
        // Inside an arithmetic command `(( … ))` up to this index.
        let mut arithmetic_until = 0;
        // Index of the last `)` that closed a group (not a substitution):
        // a `#` right after it starts a comment, as after any operator.
        let mut bare_close_at = usize::MAX;

        while i < end {
            let ch = bytes[i];
            if ch == b'\\' {
                if bytes.get(i + 1) == Some(&b'\n') {
                    self.out.continuations.push(i..i + 2);
                }
                i += 2;
                continue;
            }
            if ch == b'$' {
                match bytes.get(i + 1) {
                    Some(b'(') => {
                        // `$(( expr ))` is arithmetic when the inner group
                        // closes right before the outer `)`.
                        if bytes.get(i + 2) == Some(&b'(')
                            && let Some((expr, after)) = balanced_paren_at(&s[..end], i + 2)
                            && bytes.get(after) == Some(&b')')
                        {
                            self.push(collect, expr, true);
                            i = after + 1;
                            continue;
                        }
                        match self.substitution(end, i + 1, collect) {
                            Some(after) => i = after,
                            None => return unterminated,
                        }
                        continue;
                    }
                    Some(b'{') if mode != Mode::HeredocBody => {
                        if bytes.get(i + 2) == Some(&b'(') {
                            self.flag(ZSH_PARAM_FLAGS);
                        }
                        let quoted = in_double || mode == Mode::Param { quoted: true };
                        match self.scan(end, i + 2, Mode::Param { quoted }, Close::Brace, collect) {
                            Some(after) => i = after,
                            None => return unterminated,
                        }
                        continue;
                    }
                    Some(b'[') => {
                        // bash/zsh arithmetic, plain text with live quotes to
                        // dash: where it ends depends on the shell, wherever
                        // it stands. Refused; the scan walks on through it.
                        self.flag(LEGACY_ARITHMETIC);
                        i += 2;
                        continue;
                    }
                    _ => {}
                }
            }
            if ch == b'`' {
                let close_at = closing_backtick(bytes, i + 1);
                let body_end = close_at.unwrap_or(end);
                self.push(collect, &s[i + 1..body_end], false);
                // The body is a script: record its comments and heredocs.
                self.scan(body_end, i + 1, Mode::Shell, Close::Eof, false);
                match close_at {
                    Some(close_at) => i = close_at + 1,
                    None => return unterminated,
                }
                continue;
            }
            if mode == Mode::HeredocBody {
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
                b'\'' if mode == (Mode::Param { quoted: true }) => {
                    // Literal in bash, a quote in dash and zsh.
                    self.flag(QUOTE_IN_QUOTED_PARAM);
                }
                // Up to the closing quote; the `i += 1` below steps past it.
                b'\'' => match s[i + 1..end].find('\'') {
                    Some(k) => i += k + 1,
                    None => return unterminated,
                },
                b'$' if bytes.get(i + 1) == Some(&b'\'') => match self.ansi_c_end(bytes, i + 2) {
                    Some(close_at) => i = close_at,
                    None => return unterminated,
                },
                b'#' if !param && starts_word(bytes, i, start, bare_close_at) => {
                    let line_end = s[i..end].find('\n').map_or(end, |k| i + k);
                    self.out.comments.push(i..line_end);
                    i = line_end;
                    continue;
                }
                b'<' if !param && bytes.get(i + 1) == Some(&b'<') => {
                    if bytes.get(i + 2) == Some(&b'<') {
                        // `<<<` is a here-string: its word is scanned normally.
                        i += 3;
                        continue;
                    }
                    if i < arithmetic_until {
                        self.flag(SHIFT_IN_ARITHMETIC_COMMAND);
                        i += 2;
                        continue;
                    }
                    i = self.heredoc_operator(bytes, i + 2);
                    continue;
                }
                b'<' | b'>' if bytes.get(i + 1) == Some(&b'(') => {
                    match self.substitution(end, i + 1, collect) {
                        Some(after) => i = after,
                        None => return unterminated,
                    }
                    continue;
                }
                b'{' if param => self.flag(BRACE_IN_PARAM),
                b'}' if close == Close::Brace => {
                    if s[..i].ends_with("@P") {
                        self.flag(PROMPT_TRANSFORM);
                    }
                    return Some(i + 1);
                }
                // zsh `=( … )` process substitution, where `=` starts a word
                // (`a=(…)` is an array assignment).
                b'=' if !param
                    && bytes.get(i + 1) == Some(&b'(')
                    && starts_word(bytes, i, start, bare_close_at) =>
                {
                    match self.substitution(end, i + 1, collect) {
                        Some(after) => i = after,
                        None => return unterminated,
                    }
                    continue;
                }
                b'(' => {
                    if !param
                        && i >= arithmetic_until
                        && !starts_word(bytes, i, start, bare_close_at)
                        && bytes[i - 1] != b'='
                        && bytes.get(i + 1) != Some(&b')')
                    {
                        // `*(e:…:)`, `x(+cmd)`, `@(a|b)`: only a function
                        // head `f()` or an array `a=(…)` may touch a word.
                        self.flag(ATTACHED_PAREN);
                    }
                    if !param
                        && bytes.get(i + 1) == Some(&b'(')
                        && let Some((expr, after)) = balanced_paren_at(&s[..end], i + 1)
                        && bytes.get(after) == Some(&b')')
                    {
                        // An arithmetic command: its substitutions are gated
                        // like those of `$(( … ))`; the scan still walks it.
                        self.push(collect, expr, true);
                        arithmetic_until = after + 1;
                    }
                    depth += 1;
                }
                b')' => {
                    if close == Close::Paren && depth == 0 {
                        return Some(i + 1);
                    }
                    depth = depth.saturating_sub(1);
                    bare_close_at = i;
                }
                b'\n' if !param && !self.pending.is_empty() => {
                    if pending_base > 0 {
                        // Queued outside this substitution, read inside it.
                        self.flag(NESTED_HEREDOC);
                    }
                    i = self.heredoc_bodies(end, i + 1, collect);
                    continue;
                }
                _ => {}
            }
            i += 1;
        }
        unterminated
    }

    /// A substitution whose `(` sits at `open` (`$(`, `<(`, `>(`): records its
    /// body and returns the index past its `)`. Unterminated, the body is the
    /// rest of the input and `None` is returned.
    fn substitution(&mut self, end: usize, open: usize, collect: bool) -> Option<usize> {
        let after = self.scan(end, open + 1, Mode::Shell, Close::Paren, false);
        let s = self.s;
        self.push(collect, &s[open + 1..after.map_or(end, |e| e - 1)], false);
        after
    }

    /// Index of the `'` closing an ANSI-C string whose body starts at `from`.
    /// A backslash escapes the next byte in bash and zsh; dash has no `$'…'`
    /// and ends the string at an escaped quote, so that form is flagged.
    fn ansi_c_end(&mut self, bytes: &[u8], from: usize) -> Option<usize> {
        let mut j = from;
        while j < bytes.len() {
            match bytes[j] {
                b'\\' => {
                    if bytes.get(j + 1) == Some(&b'\'') {
                        self.flag(ANSI_C_ESCAPED_QUOTE);
                    }
                    j += 2;
                }
                b'\'' => return Some(j),
                _ => j += 1,
            }
        }
        None
    }

    /// Parse the delimiter after a `<<`/`<<-` operator (`from` is just past
    /// `<<`), queue the heredoc and return the index past the delimiter word.
    /// The word ends at a shell metacharacter or the end of the line.
    fn heredoc_operator(&mut self, bytes: &[u8], from: usize) -> usize {
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
                self.pending.push(PendingHeredoc {
                    delim,
                    quoted,
                    strip_tabs,
                });
                next
            }
            None => j,
        }
    }

    /// Consume the bodies of the queued heredocs, starting at the line that
    /// begins at `pos`; return the index of the line after the last
    /// terminator (or `end`).
    ///
    /// The terminator must match exactly (after leading tabs for `<<-`), as
    /// in the shell. A missing terminator is flagged rather than swallowing
    /// the rest of the input. An unquoted body expands substitutions, so it is
    /// scanned as [`Mode::HeredocBody`]; a quoted one is pure data.
    fn heredoc_bodies(&mut self, end: usize, mut pos: usize, collect: bool) -> usize {
        let s = self.s;
        let mut range_start = pos - 1;
        for heredoc in std::mem::take(&mut self.pending) {
            let body_start = pos;
            let mut body_end = end;
            let mut range_end = end;
            let mut terminated = false;
            // In an unquoted body a `\`+newline joins two lines, so the line
            // after one cannot be the terminator (`x\⏎EOF` is `xEOF`).
            let mut continued = false;
            while pos < end {
                let line_end = s[pos..end].find('\n').map_or(end, |k| pos + k);
                let line = &s[pos..line_end];
                let joined = continued;
                continued = !heredoc.quoted
                    && line.bytes().rev().take_while(|&b| b == b'\\').count() % 2 == 1;
                let line = if heredoc.strip_tabs {
                    line.trim_start_matches('\t')
                } else {
                    line
                };
                if !joined && line == heredoc.delim {
                    body_end = pos;
                    range_end = line_end;
                    pos = line_end + 1;
                    terminated = true;
                    break;
                }
                pos = line_end + 1;
            }
            if !terminated {
                self.flag(UNTERMINATED_HEREDOC);
                pos = end;
            }
            if !heredoc.quoted {
                self.scan(body_end, body_start, Mode::HeredocBody, Close::Eof, collect);
            }
            self.out.heredocs.push(Heredoc {
                delim: heredoc.delim,
                quoted: heredoc.quoted,
                body: range_start..range_end,
            });
            range_start = range_end;
        }
        // Resume at the line after the last terminator.
        pos.min(end)
    }
}

/// Does a `#` at `i` start a word, i.e. a comment? At the start of the
/// scanned text, after a blank or one of `;&|()`, but after `)` only when it
/// closed a group: the `)` of `$(…)` or `<(…)` ends no word (`$(a)#b`).
fn starts_word(bytes: &[u8], i: usize, start: usize, bare_close_at: usize) -> bool {
    if i == start {
        return true;
    }
    match bytes[i - 1] {
        b' ' | b'\t' | b'\n' | b';' | b'&' | b'|' | b'(' => true,
        b')' => bare_close_at == i - 1,
        _ => false,
    }
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
