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

use std::borrow::Cow;
use std::ops::Range;

use super::compound::balanced_paren_at;

/// One expansion the shell performs: the inner text of `$( … )`, `<( … )`,
/// `>( … )`, zsh `=( … )` or a backtick pair, or — `arithmetic` — the
/// expression of `$(( … ))` or an arithmetic command `(( … ))`. A backtick
/// body is the script the shell runs: with its `\\`, `` \` `` and `\$`
/// unescaped ([`backtick_script`]).
pub struct Expansion<'a> {
    pub body: Cow<'a, str>,
    pub arithmetic: bool,
}

/// The script a backtick substitution runs: inside backticks the shell first
/// turns `\\`, `` \` `` and `\$` into `\`, `` ` `` and `$`, then parses the
/// result (POSIX 2.6.3). Any other backslash stays.
pub fn backtick_script(raw: &str) -> Cow<'_, str> {
    if !raw.contains('\\') {
        return Cow::Borrowed(raw);
    }
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut changed = false;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && matches!(bytes.get(i + 1), Some(b'\\' | b'`' | b'$')) {
            out.push(bytes[i + 1]);
            changed = true;
            i += 2;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    if changed {
        // Only ASCII backslashes were removed: still valid UTF-8.
        Cow::Owned(String::from_utf8(out).unwrap_or_else(|_| raw.to_string()))
    } else {
        Cow::Borrowed(raw)
    }
}

/// A heredoc and the text its body occupies.
pub struct Heredoc {
    pub delim: String,
    pub quoted: bool,
    /// From the newline ending the operator's line through the end of the
    /// terminator line (its own newline excluded): removing it leaves the
    /// operator line followed by the line after the terminator.
    pub body: Range<usize>,
}

/// What [`lex`] found in a command.
#[derive(Default)]
pub struct Lexed<'a> {
    /// Top-level expansions in source order; nested ones are inside the bodies.
    pub expansions: Vec<Expansion<'a>>,
    /// `#` to the end of its line, wherever the shell starts a comment.
    pub comments: Vec<Range<usize>>,
    /// Every heredoc, in operator order, at any nesting depth.
    pub heredocs: Vec<Heredoc>,
    /// Every `\`+newline the shell removes as a line continuation: not one
    /// that is itself escaped (`\\⏎`), single-quoted or in a quoted heredoc.
    pub continuations: Vec<Range<usize>>,
    /// The first construct the shells disagree on, or that cannot be
    /// validated (an unterminated heredoc). The gate blocks when set.
    pub problem: Option<&'static str>,
    /// Nesting went past `MAX_NESTING`; the rest of that level was not
    /// scanned (and `problem` is set).
    pub too_deep: bool,
}

/// Deepest nesting of substitutions, backticks and `${…}` the lexer follows.
/// Each level is one recursive scan: without a bound, a line of thousands of
/// nested `$(` overflowed the hook's worker stack, and a crashed hook fails
/// open. Real commands stay far below it.
const MAX_NESTING: usize = 64;

/// Lex `command` as the shell would.
pub fn lex(command: &str) -> Lexed<'_> {
    let mut lexer = Lexer {
        s: command,
        out: Lexed::default(),
        pending: Vec::new(),
        nesting: 0,
    };
    if has_stop_parsing_token(command) && super::ps_statements::host_is_powershell() {
        lexer.flag(STOP_PARSING);
    }
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
pub fn remove_ranges(command: &str, mut ranges: Vec<Range<usize>>) -> String {
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
const TOO_DEEP: &str = "substitutions, backticks, `${…}` or PowerShell `( … )` groups nested \
     more than 64 levels deep";
const ATTACHED_BLOCK_COMMENT: &str =
    "a PowerShell block comment `<#` attached to a word, where it may not start a comment";
const UNTERMINATED_BLOCK_COMMENT: &str = "a PowerShell block comment `<#` without its closing `#>`";
const HEREDOC_IN_REGEX_GROUP: &str = "`<<` inside a `( … )` group of a `[[ … =~ … ]]` regex, which \
     bash reads as regex text and other shells may read as a here-document";
const COMMENT_IN_REGEX: &str = "a `#` after a `(`, `)` or `|` in the regex of `[[ … =~ … ]]` \
     with quotes, `$`, backticks or a backslash after it on its line: bash reads it as regex \
     text, zsh may read it as a comment";
const QUOTE_ESCAPE_IN_BACKTICK: &str = "a `\\\"` in a backtick substitution inside double quotes, \
     which bash unescapes and other shells may not";
const HERE_STRING: &str = "a PowerShell here-string (`@'` or `@\"` ending its line), whose \
     line-anchored closing `'@`/`\"@` the gate does not follow (write the text to a file instead)";
const ESCAPED_DELIMITER: &str = "a PowerShell backtick escape before a quote, `#`, `;`, `|` or \
     `&`, which the gate reads as a substitution";
const PS_BRACED_VARIABLE: &str = "a PowerShell braced variable `${…}` holding a quote, `$`, a \
     backtick, a bracket, a line break or a non-ASCII character, where PowerShell ends the name \
     at the first `}` but the gate would read past it";
const STOP_PARSING: &str = "the PowerShell stop-parsing token `--%`, after which PowerShell \
     passes quotes literally up to the next `|` or line break";
const PS_HEREDOC: &str = "`<<` under PowerShell, which has no here-documents: PowerShell keeps \
     the next line as a command where the gate would read it as here-document text";
const PS_ATTACHED_COMMENT: &str = "a `#` attached to a PowerShell quote, group, member access, \
     parameter, redirection or non-ASCII character, or after a `[`, `(` or `{`, where PowerShell \
     starts a comment the gate would read as part of a word";

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
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
    /// Scans currently open (see [`MAX_NESTING`]).
    nesting: usize,
}

impl<'a> Lexer<'a> {
    fn flag(&mut self, problem: &'static str) {
        self.out.problem.get_or_insert(problem);
    }

    fn push(&mut self, collect: bool, body: impl Into<Cow<'a, str>>, arithmetic: bool) {
        if collect {
            self.out.expansions.push(Expansion {
                body: body.into(),
                arithmetic,
            });
        }
    }

    /// Lex a backtick body the shell unescaped first, for the constructs the
    /// shells disagree on. Its positions are not positions in the command, so
    /// its comments and heredocs stay in the command's text; the gate strips
    /// them when it reads the body as its own script.
    fn scan_unescaped(&mut self, script: &str) {
        let mut inner = Lexer {
            s: script,
            out: Lexed::default(),
            pending: Vec::new(),
            nesting: self.nesting,
        };
        inner.scan(script.len(), 0, Mode::Shell, Close::Eof, false);
        if inner.out.too_deep {
            self.out.too_deep = true;
        }
        if let Some(problem) = inner.out.problem {
            self.flag(problem);
        }
    }

    /// Scan `s[start..end]` in `mode`. With [`Close::Paren`] or
    /// [`Close::Brace`] it returns the index just past the closing
    /// delimiter, or `None` when it is unterminated (the rest of the input
    /// was scanned). With [`Close::Eof`] it returns `Some(end)`. Expansions
    /// are collected only with `collect`; comments and heredocs always.
    /// Past [`MAX_NESTING`] it flags the command and returns `None`.
    fn scan(
        &mut self,
        end: usize,
        start: usize,
        mode: Mode,
        close: Close,
        collect: bool,
    ) -> Option<usize> {
        if self.nesting >= MAX_NESTING {
            self.out.too_deep = true;
            self.flag(TOO_DEEP);
            return None;
        }
        self.nesting += 1;
        let after = self.scan_level(end, start, mode, close, collect);
        self.nesting -= 1;
        after
    }

    fn scan_level(
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
        // After an unquoted `=~` word bash reads the next word as a regex
        // (`parse_cond_command`): nowhere in that word does a `#` start a
        // comment — after a group's `)` or a `|` neither — and inside its
        // `( … )` groups `<<` starts no here-document. It ends only at a
        // blank, newline, `;`, `&`, `<`, `>` or an unmatched `)` outside a
        // group. `regex_await`: the word has not started yet; `regex_word`:
        // the scan is in it; `regex_depth`: its open groups.
        let mut regex_await = false;
        let mut regex_word = false;
        let mut regex_depth: u32 = 0;

        while i < end {
            let ch = bytes[i];
            if regex_await && !matches!(ch, b' ' | b'\t') {
                regex_await = false;
                // A `#` opening the word is a comment, as at any word start.
                regex_word = !matches!(ch, b'\n' | b'#');
            }
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
                        // PowerShell ends a braced variable `${…}` at the first
                        // `}`; quotes, `$`, backticks and groups inside it are
                        // part of the name. The scan below reads it the shell
                        // way, so `${x'}; cmd #'}` was one quoted word to the
                        // gate while PowerShell ran `cmd`. Under the PowerShell
                        // host a body that holds any byte the two readings
                        // could disagree on is refused; with none of them both
                        // end at the same `}`.
                        if super::ps_statements::host_is_powershell()
                            && braced_variable_disagrees(&bytes[i + 2..])
                        {
                            self.flag(PS_BRACED_VARIABLE);
                            return unterminated;
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
                // PowerShell's escape character. Before a quote, `#` or a
                // separator it hides that byte from the readings below, which
                // pair backticks as a substitution whose body ignores quotes:
                // `` `'; cmd; '`' `` ran `cmd` behind one "string".
                //
                // `$` is deliberately NOT in this set. A backtick before `$`
                // escapes the `$` (`` `$5 ``, `` `$( ``), a common and
                // legitimate form in double-quoted strings, and it hides no
                // separator — `` `$x; cmd `` is already blocked because the
                // backtick body `$x; cmd` meets the allowlist and fails it.
                // Adding `$` here would only add false refusals. To refuse it
                // regardless, add `b'$'` to the set below.
                if matches!(
                    bytes.get(i + 1),
                    Some(b'\'' | b'"' | b'#' | b';' | b'|' | b'&')
                ) && super::ps_statements::host_is_powershell()
                {
                    self.flag(ESCAPED_DELIMITER);
                    i += 2;
                    continue;
                }
                let close_at = closing_backtick(bytes, i + 1);
                let body_end = close_at.unwrap_or(end);
                let raw = &s[i + 1..body_end];
                if in_double && raw.contains("\\\"") {
                    self.flag(QUOTE_ESCAPE_IN_BACKTICK);
                }
                match backtick_script(raw) {
                    // The body is a script: record its comments and heredocs.
                    Cow::Borrowed(raw) => {
                        self.push(collect, raw, false);
                        self.scan(body_end, i + 1, Mode::Shell, Close::Eof, false);
                    }
                    // The shell runs the unescaped text, not the raw one.
                    Cow::Owned(script) => {
                        self.scan_unescaped(&script);
                        self.push(collect, script, false);
                    }
                }
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
            if regex_word
                && regex_depth == 0
                && matches!(ch, b' ' | b'\t' | b'\n' | b';' | b'&' | b'<' | b'>')
            {
                regex_word = false;
            }
            match ch {
                // PowerShell here-string `@'`/`@"` ending its line: it closes
                // at a line-anchored `'@`/`"@`, where the quote rules below
                // would pair its quotes as ordinary ones and hide a `;` after
                // the close. The gate does not follow it: refused.
                b'@' if matches!(bytes.get(i + 1), Some(b'\'' | b'"'))
                    && opens_here_string(bytes, i + 2)
                    && super::ps_statements::host_is_powershell() =>
                {
                    self.flag(HERE_STRING);
                    return unterminated;
                }
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
                // Regex text to bash. zsh may read a `#` starting a word as a
                // comment, and so read quotes and substitutions after it
                // differently: such a line is refused.
                b'#' if regex_word => {
                    let line_end = s[i..end].find('\n').map_or(end, |k| i + k);
                    if starts_word(bytes, i, start, bare_close_at)
                        && s[i..line_end].contains(['\'', '"', '`', '$', '\\'])
                    {
                        self.flag(COMMENT_IN_REGEX);
                    }
                }
                b'<' if regex_depth > 0 && bytes.get(i + 1) == Some(&b'<') => {
                    self.flag(HEREDOC_IN_REGEX_GROUP);
                    i += 2;
                    continue;
                }
                b'#' if !param && starts_word(bytes, i, start, bare_close_at) => {
                    let line_end = s[i..end].find('\n').map_or(end, |k| i + k);
                    self.out.comments.push(i..line_end);
                    i = line_end;
                    continue;
                }
                // PowerShell starts a comment wherever a token ends: after a
                // closing quote, `}`, `)`, a member access, `,`, `-Name:` or
                // a redirection, while the rule above reads one word there.
                // `echo 'a'#'⏎cmd⏎#'` ran `cmd` behind one "word". Only a `#`
                // inside a plain word is ordinary text to both readings, and
                // only in argument mode: after any `[`, `(` or `{` in this
                // scan PowerShell may be in expression mode, where `$x[ 0#'`
                // is a number and a comment (review note 23662).
                b'#' if !param
                    && super::ps_statements::host_is_powershell()
                    && (!in_plain_powershell_word(&bytes[start..i])
                        || bytes[start..i]
                            .iter()
                            .any(|b| matches!(b, b'[' | b'(' | b'{'))) =>
                {
                    self.flag(PS_ATTACHED_COMMENT);
                    return unterminated;
                }
                // PowerShell block comment `<# … #>`: it may span lines and
                // ends at `#>`, so the `#` rule above would read its closing
                // `#>` as a line comment and hide what follows it.
                b'<' if !param
                    && bytes.get(i + 1) == Some(&b'#')
                    && super::ps_statements::host_is_powershell() =>
                {
                    if !starts_word(bytes, i, start, bare_close_at) {
                        self.flag(ATTACHED_BLOCK_COMMENT);
                    }
                    if let Some(k) = s[i + 2..end].find("#>") {
                        self.out.comments.push(i..i + 2 + k + 2);
                        i += 2 + k + 2;
                        continue;
                    }
                    self.flag(UNTERMINATED_BLOCK_COMMENT);
                    self.out.comments.push(i..end);
                    return unterminated;
                }
                b'<' if !param && bytes.get(i + 1) == Some(&b'<') => {
                    if bytes.get(i + 2) == Some(&b'<') {
                        // `<<<` is a here-string: its word is scanned normally.
                        i += 3;
                        continue;
                    }
                    // PowerShell has no here-documents: `a<<EOF` is one
                    // argument and the next line a command, which the gate
                    // would skip as here-document text.
                    if super::ps_statements::host_is_powershell() {
                        self.flag(PS_HEREDOC);
                        return unterminated;
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
                b'=' if mode == Mode::Shell
                    && bytes.get(i + 1) == Some(&b'~')
                    && matches!(bytes.get(i + 2), Some(b' ' | b'\t'))
                    && starts_word(bytes, i, start, bare_close_at) =>
                {
                    regex_await = true;
                    i += 2;
                    continue;
                }
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
                        && !(powershell_member_call(bytes, i, start)
                            && super::ps_statements::host_is_powershell())
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
                    if regex_word {
                        regex_depth += 1;
                    }
                    depth += 1;
                    // PowerShell reads each `(` as a nested expression or
                    // pipeline, one recursive scan per level: bound it like a
                    // substitution, or a crashed hook fails open.
                    if self.nesting + depth as usize > MAX_NESTING
                        && !self.out.too_deep
                        && super::ps_statements::host_is_powershell()
                    {
                        self.out.too_deep = true;
                        self.flag(TOO_DEEP);
                    }
                }
                b')' => {
                    if close == Close::Paren && depth == 0 {
                        return Some(i + 1);
                    }
                    if regex_depth > 0 {
                        regex_depth -= 1;
                    } else {
                        regex_word = false;
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

/// Could PowerShell and the shell reading end the braced variable whose body
/// starts at `body` at different places? PowerShell ends it at the first `}`;
/// the shell reading pairs quotes, `$…`, backticks, groups and nested braces
/// first. Any of those (or a line break) before that `}`, or no `}` at all,
/// counts as a disagreement.
fn braced_variable_disagrees(body: &[u8]) -> bool {
    match body.iter().position(|&b| b == b'}') {
        Some(close) => body[..close].iter().any(|b| {
            matches!(
                b,
                b'\'' | b'"' | b'`' | b'$' | b'{' | b'(' | b')' | b'\n' | b'\r' | b'\\'
            ) || *b >= 0x80
        }),
        None => true,
    }
}

/// Does `bytes` start with the stop-parsing token: two dashes, each `-` or one
/// of `– — ―` (U+2013..U+2015), then `%`? Only this check reads those dashes
/// as `-`; every other check sees them unchanged, as a native program does.
fn is_stop_parsing_token(bytes: &[u8]) -> bool {
    let mut rest = bytes;
    for _ in 0..2 {
        rest = match rest {
            [b'-', tail @ ..] | [0xE2, 0x80, 0x93..=0x95, tail @ ..] => tail,
            _ => return false,
        };
    }
    rest.first() == Some(&b'%')
}

/// Does `command` hold the PowerShell stop-parsing token `--%` anywhere, quoted
/// or not, other than glued into a plain word?
///
/// After `--%` PowerShell passes the rest of the line up to a `|` or line
/// break to a native program verbatim, so quotes no longer pair:
/// `echo --% '| cmd #'` pipes into `cmd` while the quote rules read one quoted
/// word. A native-program argument whose value is exactly `--%` (from
/// `"--%"`, `'--%'` or `"$e--%"`) makes pwsh re-split the following arguments
/// at spaces. PowerShell starts a token after whitespace of any kind, `,`, a
/// closing quote, `}`, `)`, a redirection or a member access (`$x.y--%`), so
/// instead of following its tokenizer the gate refuses every occurrence unless
/// the text before it, back to an ASCII space, tab or line break, is a
/// non-empty plain word of ASCII letters, digits, `_`, `-` and `=` ending in
/// a letter, digit, `_` or `=` (`--format=--%`, `a--%`). The dashes `– — ―`
/// count as `-` here (refused conservatively; pwsh 7.4.6 does not stop parsing
/// at them, Windows PowerShell 5.1 is untested).
fn has_stop_parsing_token(command: &str) -> bool {
    let bytes = command.as_bytes();
    (0..bytes.len()).any(|i| {
        matches!(bytes[i], b'-' | 0xE2)
            && is_stop_parsing_token(&bytes[i..])
            && !glued_into_plain_word(&bytes[..i])
    })
}

/// Is `before` (the text before a stop-parsing token) a plain word that
/// glues the token into an ordinary argument? See [`has_stop_parsing_token`].
fn glued_into_plain_word(before: &[u8]) -> bool {
    let start = before
        .iter()
        .rposition(|b| matches!(b, b' ' | b'\t' | b'\n'))
        .map_or(0, |p| p + 1);
    let word = &before[start..];
    word.last()
        .is_some_and(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'='))
        && word
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'='))
}

/// Does a `#` at `i` start a word, i.e. a comment? At the start of the
/// scanned text, after a blank or one of `;&|()`, but after `)` only when it
/// closed a group: the `)` of `$(…)` or `<(…)` ends no word (`$(a)#b`).
///
/// A backslash-escaped blank or operator ends no word (`a\ #b`, `a\;#b` are
/// one word to the shell). The gate joins `\`+newline continuations before it
/// lexes (`normalize_line_continuations`).
fn starts_word(bytes: &[u8], i: usize, start: usize, bare_close_at: usize) -> bool {
    if i == start {
        return true;
    }
    if escaped(bytes, i - 1, start) {
        return false;
    }
    match bytes[i - 1] {
        b' ' | b'\t' | b'\n' | b';' | b'&' | b'|' | b'(' => true,
        b')' => bare_close_at == i - 1,
        _ => false,
    }
}

/// Is the byte at `at` escaped: preceded by an odd run of backslashes that
/// does not reach back before `start`?
fn escaped(bytes: &[u8], at: usize, start: usize) -> bool {
    bytes[start..at]
        .iter()
        .rev()
        .take_while(|&&b| b == b'\\')
        .count()
        % 2
        == 1
}

/// Is a `#` right after `before` ordinary text to PowerShell too? Only when
/// the word it continues (back to an ASCII blank) holds nothing but ASCII
/// letters, digits and `_-=./+~%!?*^`: pwsh 7.4.6 reads such a word as one
/// generic token (`a#b`, `--format=#c`, `-x#c`, `HEAD~1#c`). A quote, `$`,
/// `@`, group, `,`, `:`, redirection or non-ASCII byte may end the token there,
/// so the `#` would open a comment.
fn in_plain_powershell_word(before: &[u8]) -> bool {
    let start = before
        .iter()
        .rposition(|b| matches!(b, b' ' | b'\t' | b'\n'))
        .map_or(0, |p| p + 1);
    let word = &before[start..];
    !word.is_empty()
        && word
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || b"_-=./+~%!?*^".contains(b))
}

/// Is the `(` at `i` the argument list of a PowerShell member call —
/// `[Math]::Max(`, `$p.WaitForExit(`, `(Get-Date).ToString(` — on a word that
/// opens an expression (`$`, `[`, `(`) at the start of a statement or as the
/// value of a local assignment (`$n = …`, `$n += …`)? No shell glob runs in
/// PowerShell, and `ps_statements` reads such a statement and decides which
/// members are inert. Not one: a bare word (`x.y(` — in argument mode its `(`
/// starts another argument), a call in a command's arguments
/// (`echo $x.ToString((cmd))`, `git log --format=$x.y((cmd))`) and the value
/// of a scoped assignment (`$env:X = …`), none of which a reader looks into.
fn powershell_member_call(bytes: &[u8], i: usize, start: usize) -> bool {
    let mut j = i;
    while j > start && (bytes[j - 1].is_ascii_alphanumeric() || bytes[j - 1] == b'_') {
        j -= 1;
    }
    let accessed = j < i
        && j > start
        && (bytes[j - 1] == b'.' && (j - 1 == start || bytes[j - 2] != b'.')
            || bytes[j - 1] == b':' && j - 1 > start && bytes[j - 2] == b':');
    if !accessed {
        return false;
    }
    while j > start
        && !matches!(
            bytes[j - 1],
            b' ' | b'\t' | b'\n' | b';' | b'&' | b'|' | b'{' | b'}' | b'=' | b','
        )
    {
        j -= 1;
    }
    if !matches!(bytes[j], b'$' | b'[' | b'(') {
        return false;
    }
    j = skip_blanks_back(bytes, j, start);
    if j > start && bytes[j - 1] == b'=' {
        // Only `$name =` or `$name <op>=` at statement start: an `=` inside
        // an argument (`--format=`, `a=`) belongs to that argument.
        let mut k = j - 1;
        if k > start && matches!(bytes[k - 1], b'+' | b'-' | b'*' | b'/' | b'%') {
            k -= 1;
        }
        k = skip_blanks_back(bytes, k, start);
        let name_end = k;
        while k > start && (bytes[k - 1].is_ascii_alphanumeric() || bytes[k - 1] == b'_') {
            k -= 1;
        }
        if k == name_end || k == start || bytes[k - 1] != b'$' {
            return false;
        }
        j = skip_blanks_back(bytes, k - 1, start);
    }
    j == start || matches!(bytes[j - 1], b'\n' | b';' | b'{')
}

/// `at` moved back over the spaces and tabs before it, not past `start`.
fn skip_blanks_back(bytes: &[u8], mut at: usize, start: usize) -> usize {
    while at > start && matches!(bytes[at - 1], b' ' | b'\t') {
        at -= 1;
    }
    at
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

/// Does the text after a PowerShell `@'`/`@"` at `from` run to a line break
/// with only blanks between, so the pair opens a here-string?
fn opens_here_string(bytes: &[u8], from: usize) -> bool {
    bytes[from..]
        .iter()
        .find(|b| !matches!(b, b' ' | b'\t' | b'\r'))
        .is_some_and(|b| *b == b'\n')
}
