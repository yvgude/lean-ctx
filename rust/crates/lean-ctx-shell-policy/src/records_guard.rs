// SPDX-License-Identifier: Apache-2.0
//! The agent records (§37.4), judged over a whole command line.
//!
//! The line is parsed once — quotes, `$'…'`, substitutions, heredocs,
//! subshells — and its commands are evaluated in order, following the
//! variables it assigns and the directory it moves to (`cd`, `cd -`,
//! `pushd`, `git -C`). Every word becomes a path pattern: a part this check
//! cannot know (a substitution, an unset variable, `$((…))`) stands for any
//! number of path components.
//!
//! One rule covers every program, by what it does with a path:
//!
//! - a word that names a path inside the records — through a component of
//!   their own path (`agents`, `lean-ctx`) or a record's name — is refused
//!   for any program;
//! - a program that reads every file below a directory (`grep -r`, `rg`,
//!   `diff -r`, `git diff --no-index`, `cp -r`, `tar`) is refused for a
//!   directory that is, contains or may be the records;
//! - a program that lists names below a directory (`find`, `fd`, `tree`,
//!   `du`, `ls -R`, `locate`) is refused when the depth it walks reaches
//!   the records and its name filter can match them;
//! - output a later command turns into arguments (`xargs`, `$(…)`, `read`)
//!   is unknown in that command; a line whose producers name the data
//!   directory or a directory above it, and that also reads contents, is
//!   refused.
//!
//! What one command writes to a file and a separate, later command reads
//! is outside this check (see the contract's known limits).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::{RELEASE_IDS_NAME, ShellError, basename, homes, resolve_existing, spellings};

/// Nesting depth of substitutions, subshells and shell scripts followed.
const MAX_DEPTH: usize = 24;
/// Expansions of one word (braces, variable values) followed.
const MAX_ALTS: usize = 256;
/// Directories a line may be in before its directory counts as unknown.
const MAX_CWDS: usize = 8;
/// How an unknown part is spelled in a script handed on to a shell.
const UNKNOWN_VAR: &str = "${__lean_ctx_unknown}";

/// The records' file names, besides those present on disk.
const KNOWN_ENTRIES: &[&str] = &[
    "directives.json",
    "directive-release-ids.json",
    "registry.json",
    "leases.json",
    "tasks.json",
    "scent_field.json",
    "process-identities.json",
    "friction.jsonl",
    // LeanCTX Code's per-launch app key (Local Service app-only routes).
    "app-launch.key",
];

/// Reserved words a command may start with.
const RESERVED: &[&str] = &[
    "if", "then", "else", "elif", "fi", "do", "done", "while", "until", "!", "{", "}", "time",
    "esac", "coproc",
];

/// Programs that print names, not contents.
const NAME_ONLY: &[&str] = &["ls", "echo", "printf", "basename", "dirname"];
/// Programs that list names below a directory.
const LISTERS: &[&str] = &[
    "find", "fd", "fdfind", "tree", "du", "locate", "mlocate", "plocate",
];
/// Programs that turn input into arguments or variables.
const FEEDERS: &[&str] = &["xargs", "parallel", "rush", "read", "mapfile", "readarray"];
/// Programs that change the shell's state and read no files.
const STRUCTURAL: &[&str] = &[
    "cd", "pushd", "popd", "pwd", "export", "local", "declare", "typeset", "readonly", "unset",
    "set", "shift", "true", "false", ":", "test", "[", "[[", "]]", "exit", "return", "wait",
    "alias", "unalias", "shopt", "type", "hash", "for", "select", "case", "function", "in",
];
/// Shells whose `-c` script, heredoc or herestring is a command line.
const SHELLS: &[&str] = &["bash", "sh", "zsh", "dash", "ksh", "mksh", "ash", "fish"];

// ---------------------------------------------------------------- parsing

#[derive(Clone, Debug)]
enum Part {
    Ch(char),
    Star,
    Quest,
    Class,
    BrOpen,
    BrComma,
    BrClose,
    /// `~` or `~name`.
    Tilde(String),
    /// `$name`, `${name}`; the flag tells whether it was quoted.
    Var(String),
    /// `${name:-word}` and its kin: the value or the word (`""`: none or the
    /// word, for `${name:+word}`).
    Default(String, Vec<Part>),
    /// `$(…)`, `` `…` ``, `<(…)`, `>(…)`: output of a script.
    Sub(Script),
    /// A value this check cannot know, with the scripts it runs.
    Unknown(Vec<Script>),
}

#[derive(Clone, Debug, Default)]
struct Word {
    parts: Vec<Part>,
    raw: String,
}

#[derive(Clone, Debug, Default)]
struct Simple {
    words: Vec<Word>,
    redirs: Vec<(String, Word)>,
    heredocs: Vec<Word>,
    subshell: Option<Script>,
}

impl Simple {
    fn is_empty(&self) -> bool {
        self.words.is_empty()
            && self.redirs.is_empty()
            && self.heredocs.is_empty()
            && self.subshell.is_none()
    }
}

/// Pipelines of simple commands.
type Script = Vec<Vec<Simple>>;

struct Parser {
    s: Vec<char>,
    i: usize,
    depth: usize,
    too_deep: bool,
}

fn end_stage(cur: &mut Simple, pipeline: &mut Vec<Simple>) {
    if !cur.is_empty() {
        pipeline.push(std::mem::take(cur));
    }
}

fn end_pipeline(pipeline: &mut Vec<Simple>, script: &mut Script) {
    if !pipeline.is_empty() {
        script.push(std::mem::take(pipeline));
    }
}

fn is_meta(c: char) -> bool {
    matches!(
        c,
        ' ' | '\t' | '\n' | '\r' | ';' | '&' | '|' | '<' | '>' | '(' | ')'
    )
}

impl Parser {
    fn new(text: &str, depth: usize) -> Self {
        Self {
            s: text.chars().collect(),
            i: 0,
            depth,
            too_deep: depth > MAX_DEPTH,
        }
    }

    fn peek(&self) -> Option<char> {
        self.s.get(self.i).copied()
    }

    fn at(&self, k: usize) -> Option<char> {
        self.s.get(self.i + k).copied()
    }

    /// A nested parser's result, its depth limit carried over.
    fn nested(&mut self, text: &str) -> Script {
        let mut inner = Parser::new(text, self.depth + 1);
        let script = if inner.too_deep {
            Vec::new()
        } else {
            inner.script(false)
        };
        self.too_deep |= inner.too_deep;
        script
    }

    fn script(&mut self, close: bool) -> Script {
        if self.depth > MAX_DEPTH {
            self.too_deep = true;
            self.i = self.s.len();
            return Vec::new();
        }
        let mut script: Script = Vec::new();
        let mut pipeline: Vec<Simple> = Vec::new();
        let mut cur = Simple::default();
        while let Some(c) = self.peek() {
            match c {
                ' ' | '\t' | '\r' => self.i += 1,
                '\\' if self.at(1) == Some('\n') => self.i += 2,
                '#' => {
                    while self.peek().is_some_and(|c| c != '\n') {
                        self.i += 1;
                    }
                }
                '\n' | ';' => {
                    self.i += 1;
                    if c == ';' && matches!(self.peek(), Some(';' | '&')) {
                        self.i += 1;
                    }
                    end_stage(&mut cur, &mut pipeline);
                    end_pipeline(&mut pipeline, &mut script);
                }
                '&' if self.at(1) == Some('>') => self.redirect(&mut cur),
                '&' => {
                    self.i += if self.at(1) == Some('&') { 2 } else { 1 };
                    end_stage(&mut cur, &mut pipeline);
                    end_pipeline(&mut pipeline, &mut script);
                }
                '|' => {
                    if self.at(1) == Some('|') {
                        self.i += 2;
                        end_stage(&mut cur, &mut pipeline);
                        end_pipeline(&mut pipeline, &mut script);
                    } else {
                        self.i += if self.at(1) == Some('&') { 2 } else { 1 };
                        end_stage(&mut cur, &mut pipeline);
                    }
                }
                ')' => {
                    self.i += 1;
                    if close {
                        break;
                    }
                    end_stage(&mut cur, &mut pipeline);
                }
                '(' => {
                    if self.at(1) == Some('(') {
                        self.skip_parens();
                        continue;
                    }
                    if !cur.is_empty() {
                        let mut j = self.i + 1;
                        while matches!(self.s.get(j), Some(' ' | '\t')) {
                            j += 1;
                        }
                        end_stage(&mut cur, &mut pipeline);
                        if self.s.get(j) == Some(&')') {
                            // `name() {`: a function definition.
                            self.i = j + 1;
                            continue;
                        }
                    }
                    self.i += 1;
                    self.depth += 1;
                    let inner = self.script(true);
                    self.depth -= 1;
                    cur.subshell = Some(inner);
                }
                '<' | '>' if self.at(1) == Some('(') => {
                    let word = self.word(false);
                    cur.words.push(word);
                }
                '<' | '>' => self.redirect(&mut cur),
                '0'..='9' if self.fd_redirect() => self.redirect(&mut cur),
                _ => {
                    let word = self.word(false);
                    if word.raw.is_empty() {
                        self.i += 1;
                    } else {
                        cur.words.push(word);
                    }
                }
            }
            if self.too_deep {
                break;
            }
        }
        end_stage(&mut cur, &mut pipeline);
        end_pipeline(&mut pipeline, &mut script);
        script
    }

    /// Whether the digits here are a descriptor before a redirection.
    fn fd_redirect(&self) -> bool {
        let mut j = self.i;
        while self.s.get(j).is_some_and(char::is_ascii_digit) {
            j += 1;
        }
        matches!(self.s.get(j), Some('<' | '>')) && self.s.get(j + 1) != Some(&'(')
    }

    fn redirect(&mut self, cur: &mut Simple) {
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.i += 1;
        }
        let ops = [
            "<<<", "<<-", "<<", "<>", "<&", "<", "&>>", "&>", ">>", ">&", ">|", ">",
        ];
        let rest: String = self.s[self.i..].iter().take(3).collect();
        let op = ops
            .iter()
            .find(|op| rest.starts_with(**op))
            .copied()
            .unwrap_or(">");
        self.i += op.chars().count();
        while matches!(self.peek(), Some(' ' | '\t')) {
            self.i += 1;
        }
        let target = self.word(false);
        if op == "<<" || op == "<<-" {
            let delim: String = target
                .parts
                .iter()
                .filter_map(|p| match p {
                    Part::Ch(c) => Some(*c),
                    _ => None,
                })
                .collect();
            let quoted = target.raw.contains(['\'', '"', '\\']);
            let body = self.take_heredoc(&delim, op == "<<-");
            let word = if quoted {
                Word {
                    parts: body.chars().map(Part::Ch).collect(),
                    raw: body,
                }
            } else {
                let mut inner = Parser::new(&body, self.depth + 1);
                let mut parts = Vec::new();
                inner.dquote(&mut parts, true);
                self.too_deep |= inner.too_deep;
                Word { parts, raw: body }
            };
            cur.heredocs.push(word);
            return;
        }
        let duplicates = (op == ">&" || op == "<&")
            && (target.raw == "-"
                || target
                    .raw
                    .trim_end_matches('-')
                    .chars()
                    .all(|c| c.is_ascii_digit()));
        if !duplicates {
            cur.redirs.push((op.to_string(), target));
        }
    }

    /// The body of a heredoc whose operator was just read, taken out of
    /// the text: the lines after the current one, up to `delim`.
    fn take_heredoc(&mut self, delim: &str, strip_tabs: bool) -> String {
        let Some(nl) = (self.i..self.s.len()).find(|&k| self.s[k] == '\n') else {
            return String::new();
        };
        let start = nl + 1;
        let mut a = start;
        while a <= self.s.len() {
            let b = (a..self.s.len())
                .find(|&k| self.s[k] == '\n')
                .unwrap_or(self.s.len());
            let line: String = self.s[a..b].iter().collect();
            let line = if strip_tabs {
                line.trim_start_matches('\t').to_string()
            } else {
                line
            };
            if line == delim {
                let body: String = self.s[start..a].iter().collect();
                let end = (b + 1).min(self.s.len());
                self.s.drain(start..end);
                return body;
            }
            if b >= self.s.len() {
                break;
            }
            a = b + 1;
        }
        let body: String = self.s[start..].iter().collect();
        self.s.truncate(start);
        body
    }

    /// Skips a balanced `(…)` starting at the current `(`.
    fn skip_parens(&mut self) {
        let mut depth = 0usize;
        while let Some(c) = self.peek() {
            self.i += 1;
            match c {
                '(' => depth += 1,
                ')' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return;
                    }
                }
                '\\' => self.i += 1,
                '\'' => {
                    while self.peek().is_some_and(|c| c != '\'') {
                        self.i += 1;
                    }
                    self.i += 1;
                }
                _ => {}
            }
        }
    }

    /// One word; `loose` reads to the end of the text (a `${x:-…}` operand).
    fn word(&mut self, loose: bool) -> Word {
        let start = self.i;
        let mut parts = Vec::new();
        let mut tilde_ok = true;
        while let Some(c) = self.peek() {
            if !loose && is_meta(c) {
                if (c == '<' || c == '>') && self.at(1) == Some('(') {
                    self.i += 2;
                    let sub = self.sub_script();
                    parts.push(Part::Sub(sub));
                    tilde_ok = false;
                    continue;
                }
                break;
            }
            let mut next_tilde = false;
            match c {
                '\\' => {
                    self.i += 1;
                    match self.peek() {
                        Some('\n') => self.i += 1,
                        Some(n) => {
                            parts.push(Part::Ch(n));
                            self.i += 1;
                        }
                        None => parts.push(Part::Ch('\\')),
                    }
                }
                '\'' => {
                    self.i += 1;
                    while let Some(n) = self.peek() {
                        self.i += 1;
                        if n == '\'' {
                            break;
                        }
                        parts.push(Part::Ch(n));
                    }
                }
                '"' => {
                    self.i += 1;
                    self.dquote(&mut parts, false);
                }
                '$' => self.dollar(&mut parts, false),
                '`' => {
                    self.i += 1;
                    let sub = self.backtick();
                    parts.push(Part::Sub(sub));
                }
                '*' | '?' | '+' | '@' | '!' if self.at(1) == Some('(') => {
                    self.i += 1;
                    self.skip_parens();
                    parts.push(Part::Star);
                }
                '*' => {
                    self.i += 1;
                    parts.push(Part::Star);
                }
                '?' => {
                    self.i += 1;
                    parts.push(Part::Quest);
                }
                '[' => {
                    if let Some(end) = self.class_end() {
                        self.i = end + 1;
                        parts.push(Part::Class);
                    } else {
                        self.i += 1;
                        parts.push(Part::Ch('['));
                    }
                }
                '{' => {
                    self.i += 1;
                    parts.push(Part::BrOpen);
                    next_tilde = true;
                }
                ',' => {
                    self.i += 1;
                    parts.push(Part::BrComma);
                    next_tilde = true;
                }
                '}' => {
                    self.i += 1;
                    parts.push(Part::BrClose);
                }
                '~' if tilde_ok => {
                    let mut j = self.i + 1;
                    while self
                        .s
                        .get(j)
                        .is_some_and(|c| c.is_alphanumeric() || matches!(c, '.' | '_' | '-' | '+'))
                    {
                        j += 1;
                    }
                    let ends = self.s.get(j).is_none_or(|c| {
                        matches!(c, '/' | ':' | ',' | '}') || (!loose && is_meta(*c))
                    });
                    if ends {
                        let name: String = self.s[self.i + 1..j].iter().collect();
                        self.i = j;
                        parts.push(Part::Tilde(name));
                    } else {
                        self.i += 1;
                        parts.push(Part::Ch('~'));
                    }
                }
                '=' | ':' => {
                    self.i += 1;
                    parts.push(Part::Ch(c));
                    next_tilde = true;
                }
                _ => {
                    self.i += 1;
                    parts.push(Part::Ch(c));
                }
            }
            tilde_ok = next_tilde;
            if self.too_deep {
                break;
            }
        }
        Word {
            parts,
            raw: self.s[start..self.i.min(self.s.len())].iter().collect(),
        }
    }

    /// The index of the `]` closing a bracket expression at the current `[`.
    fn class_end(&self) -> Option<usize> {
        let mut j = self.i + 1;
        if matches!(self.s.get(j), Some('!' | '^')) {
            j += 1;
        }
        if self.s.get(j) == Some(&']') {
            j += 1;
        }
        while let Some(&c) = self.s.get(j) {
            if c == ']' {
                return Some(j);
            }
            if c == '/' || is_meta(c) {
                return None;
            }
            j += 1;
        }
        None
    }

    /// The inside of `"…"` (or a heredoc body, to its end).
    fn dquote(&mut self, parts: &mut Vec<Part>, heredoc: bool) {
        while let Some(c) = self.peek() {
            match c {
                '"' if !heredoc => {
                    self.i += 1;
                    return;
                }
                '\\' => match self.at(1) {
                    Some('\n') => self.i += 2,
                    Some(n) if matches!(n, '$' | '`' | '\\') || (n == '"' && !heredoc) => {
                        parts.push(Part::Ch(n));
                        self.i += 2;
                    }
                    _ => {
                        parts.push(Part::Ch('\\'));
                        self.i += 1;
                    }
                },
                '$' => self.dollar(parts, true),
                '`' => {
                    self.i += 1;
                    let sub = self.backtick();
                    parts.push(Part::Sub(sub));
                }
                _ => {
                    parts.push(Part::Ch(c));
                    self.i += 1;
                }
            }
            if self.too_deep {
                return;
            }
        }
    }

    fn sub_script(&mut self) -> Script {
        self.depth += 1;
        let script = self.script(true);
        self.depth -= 1;
        script
    }

    fn backtick(&mut self) -> Script {
        let mut text = String::new();
        while let Some(c) = self.peek() {
            self.i += 1;
            match c {
                '`' => break,
                '\\' => match self.peek() {
                    Some(n @ ('`' | '$' | '\\')) => {
                        text.push(n);
                        self.i += 1;
                    }
                    _ => text.push('\\'),
                },
                _ => text.push(c),
            }
        }
        self.nested(&text)
    }

    fn dollar(&mut self, parts: &mut Vec<Part>, quoted: bool) {
        match self.at(1) {
            Some('\'') if !quoted => {
                self.i += 2;
                self.ansi_c(parts);
            }
            Some('"') if !quoted => {
                self.i += 2;
                self.dquote(parts, false);
            }
            Some('(') if self.at(2) == Some('(') => {
                self.i += 1;
                self.skip_parens();
                parts.push(Part::Unknown(Vec::new()));
            }
            Some('(') => {
                self.i += 2;
                let sub = self.sub_script();
                parts.push(Part::Sub(sub));
            }
            Some('{') => {
                self.i += 2;
                let inner = self.brace_body();
                let part = self.parameter(&inner);
                parts.push(part);
            }
            Some(c) if c == '_' || c.is_ascii_alphabetic() => {
                self.i += 1;
                let mut name = String::new();
                while let Some(c) = self
                    .peek()
                    .filter(|c| *c == '_' || c.is_ascii_alphanumeric())
                {
                    name.push(c);
                    self.i += 1;
                }
                parts.push(Part::Var(name));
            }
            Some(c) if c.is_ascii_digit() || "@*#?$!-".contains(c) => {
                self.i += 2;
                parts.push(Part::Var(c.to_string()));
            }
            _ => {
                self.i += 1;
                parts.push(Part::Ch('$'));
            }
        }
    }

    /// The text of `${…}` after the `${`, up to its `}`.
    fn brace_body(&mut self) -> String {
        let mut depth = 0usize;
        let mut text = String::new();
        while let Some(c) = self.peek() {
            self.i += 1;
            match c {
                '}' if depth == 0 => break,
                '}' => depth -= 1,
                '{' => depth += 1,
                '\\' => {
                    text.push(c);
                    if let Some(n) = self.peek() {
                        text.push(n);
                        self.i += 1;
                    }
                    continue;
                }
                '\'' => {
                    text.push(c);
                    while let Some(n) = self.peek() {
                        self.i += 1;
                        text.push(n);
                        if n == '\'' {
                            break;
                        }
                    }
                    continue;
                }
                _ => {}
            }
            text.push(c);
        }
        text
    }

    fn parameter(&mut self, inner: &str) -> Part {
        let name: String = inner
            .chars()
            .take_while(|c| *c == '_' || c.is_ascii_alphanumeric())
            .collect();
        let rest = &inner[name.len()..];
        let valid = name
            .chars()
            .next()
            .is_some_and(|c| c == '_' || c.is_ascii_alphabetic());
        if valid && rest.is_empty() {
            return Part::Var(name);
        }
        if valid {
            for op in [":-", ":=", ":+", ":?", "-", "=", "+", "?"] {
                if let Some(operand) = rest.strip_prefix(op) {
                    let mut p = Parser::new(operand, self.depth + 1);
                    let word = p.word(true);
                    self.too_deep |= p.too_deep;
                    return match op {
                        ":?" | "?" => Part::Var(name),
                        ":+" | "+" => Part::Default(String::new(), word.parts),
                        _ => Part::Default(name, word.parts),
                    };
                }
            }
        }
        let mut p = Parser::new(inner, self.depth + 1);
        let word = p.word(true);
        self.too_deep |= p.too_deep;
        let mut scripts = Vec::new();
        collect_scripts(&word.parts, &mut scripts);
        Part::Unknown(scripts)
    }

    /// The inside of `$'…'`, its escapes decoded.
    fn ansi_c(&mut self, parts: &mut Vec<Part>) {
        while let Some(c) = self.peek() {
            self.i += 1;
            if c == '\'' {
                return;
            }
            if c != '\\' {
                parts.push(Part::Ch(c));
                continue;
            }
            let Some(e) = self.peek() else {
                parts.push(Part::Ch('\\'));
                return;
            };
            self.i += 1;
            let simple = match e {
                'a' => Some('\u{7}'),
                'b' => Some('\u{8}'),
                'e' | 'E' => Some('\u{1b}'),
                'f' => Some('\u{c}'),
                'n' => Some('\n'),
                'r' => Some('\r'),
                't' => Some('\t'),
                'v' => Some('\u{b}'),
                '\\' | '\'' | '"' | '?' => Some(e),
                _ => None,
            };
            if let Some(ch) = simple {
                parts.push(Part::Ch(ch));
                continue;
            }
            let (radix, max) = match e {
                '0'..='7' => {
                    self.i -= 1;
                    (8, 3)
                }
                'x' => (16, 2),
                'u' => (16, 4),
                'U' => (16, 8),
                'c' => {
                    if let Some(n) = self.peek() {
                        self.i += 1;
                        parts.push(Part::Ch(char::from((n as u8) & 0x1f)));
                    }
                    continue;
                }
                _ => {
                    parts.push(Part::Ch('\\'));
                    parts.push(Part::Ch(e));
                    continue;
                }
            };
            let mut value = 0u32;
            let mut n = 0;
            while n < max {
                let Some(d) = self.peek().and_then(|c| c.to_digit(radix)) else {
                    break;
                };
                value = value * radix + d;
                self.i += 1;
                n += 1;
            }
            if n == 0 {
                parts.push(Part::Ch('\\'));
                parts.push(Part::Ch(e));
            } else if let Some(ch) = char::from_u32(value) {
                parts.push(Part::Ch(ch));
            }
        }
    }
}

fn collect_scripts(parts: &[Part], out: &mut Vec<Script>) {
    for part in parts {
        match part {
            Part::Sub(s) => out.push(s.clone()),
            Part::Unknown(scripts) => out.extend(scripts.iter().cloned()),
            Part::Default(_, inner) => collect_scripts(inner, out),
            _ => {}
        }
    }
}

// ---------------------------------------------------------------- values

/// A character of an expanded word: literal, glob, or unknown (`Deep`,
/// any text including `/`).
#[derive(Clone, Copy, Debug, PartialEq)]
enum C {
    Ch(char),
    Star,
    Quest,
    Class,
    Deep,
}

type Alt = Vec<C>;

fn alt_of(text: &str) -> Alt {
    text.chars()
        .map(|c| if c == '\u{0}' { C::Deep } else { C::Ch(c) })
        .collect()
}

/// `alt` as text: globs as written, an unknown part as NUL.
fn alt_text(alt: &[C]) -> String {
    alt.iter()
        .map(|c| match c {
            C::Ch(c) => *c,
            C::Star => '*',
            C::Quest | C::Class => '?',
            C::Deep => '\u{0}',
        })
        .collect()
}

/// A word after expansion: its possible values.
#[derive(Clone, Debug)]
struct RWord {
    alts: Vec<Alt>,
    raw: String,
    parts: Option<Vec<Part>>,
}

impl RWord {
    fn from_text(text: &str) -> Self {
        Self {
            alts: vec![alt_of(text)],
            raw: text.replace('\u{0}', "…"),
            parts: None,
        }
    }

    fn deep() -> Self {
        Self {
            alts: vec![vec![C::Deep]],
            raw: "{}".to_string(),
            parts: None,
        }
    }

    /// The word's only value when it is plain text.
    fn lit(&self) -> Option<String> {
        match self.alts.as_slice() {
            [alt] if alt.iter().all(|c| matches!(c, C::Ch(_))) => Some(alt_text(alt)),
            _ => None,
        }
    }

    /// The word's first value as text (for option names).
    fn text(&self) -> String {
        self.alts.first().map(|a| alt_text(a)).unwrap_or_default()
    }
}

fn cross(acc: &[Alt], piece: &[Alt]) -> Vec<Alt> {
    let mut out = Vec::with_capacity(acc.len() * piece.len().max(1));
    for a in acc {
        for p in piece {
            if out.len() > MAX_ALTS {
                return out;
            }
            let mut v = a.clone();
            v.extend_from_slice(p);
            out.push(v);
        }
    }
    out
}

// ---------------------------------------------------------------- paths

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Lit(String),
    Pat(Vec<C>),
    /// Any number of components.
    Deep,
}

type Pat = Vec<Tok>;

/// Record names compare without case on every platform: macOS and Windows
/// volumes ignore it by default, and Linux ones can (casefold, vfat, SMB).
fn fold(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

/// A path as the records compare it: `/` separators, case folded.
fn path_key(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/").to_lowercase()
}

/// Whether `text` holds `form` as a whole path: what follows it there ends
/// the name (`…/lean-ctx` in `…/lean-ctx/a`, not in `…/lean-ctx-x`). For an
/// agent record, a name that starts with the record's stem is that record
/// (`…/ledgerx`, `…/context_ledgerx`, `…/dashboard.token.bak`).
pub(super) fn holds_path(text: &str, form: &str) -> bool {
    if form.is_empty() {
        return false;
    }
    let (parent, name) = form.rsplit_once('/').unwrap_or(("", form));
    if is_record_name(name) {
        let stem = record_stem(name);
        return if parent.is_empty() {
            text.contains(stem)
        } else {
            text.contains(&format!("{parent}/{stem}"))
        };
    }
    text.match_indices(form)
        .any(|(at, _)| ends_name(&text[at + form.len()..]))
}

/// A record's name up to its extension (`context_ledger` of
/// `context_ledger.json`): any name that starts with it is the record.
fn record_stem(name: &str) -> &str {
    name.split_once('.').map_or(name, |(stem, _)| stem)
}

/// Whether `rest`, the text after a name, ends that name: nothing, a
/// separator, or a copy's suffix (`.bak`, `~`; not a `.lock`).
fn ends_name(rest: &str) -> bool {
    match rest.chars().next() {
        None | Some('~') => true,
        Some('.') => !rest[1..].starts_with("lock"),
        Some(c) => !(c.is_alphanumeric() || matches!(c, '_' | '-')),
    }
}

/// Whether `name` (case folded) is the name of an agent record: the
/// records directory or one of the [`STATE_RECORDS`].
fn is_record_name(name: &str) -> bool {
    name == "agents" || STATE_RECORDS.iter().any(|r| r.eq_ignore_ascii_case(name))
}

/// Whether the path component `comp` names `name` or a copy of it beside
/// it (`….json.bak`, `…~`); for an agent record, any name that starts with
/// the record's stem (`context_ledgerx`, `dashboardx`). Both already case
/// folded.
pub(super) fn names_record_or_copy(comp: &str, name: &str) -> bool {
    comp.strip_prefix(name).is_some_and(|rest| {
        rest.is_empty() || (!rest.contains('/') && rest != "." && ends_name(rest))
    }) || (is_record_name(name) && comp.starts_with(record_stem(name)))
}

fn comps(path: &Path) -> Vec<String> {
    path_key(path)
        .split('/')
        .filter(|c| !c.is_empty())
        .map(str::to_string)
        .collect()
}

fn push_comp(base: &mut Pat, comp: &[C]) {
    if comp.is_empty() || comp == [C::Ch('.')] {
        return;
    }
    if comp == [C::Ch('.'), C::Ch('.')] {
        if base.last() != Some(&Tok::Deep) {
            base.pop();
        }
        return;
    }
    if comp.contains(&C::Deep) || comp == [C::Star, C::Star] {
        if base.last() != Some(&Tok::Deep) {
            base.push(Tok::Deep);
        }
        return;
    }
    if comp.iter().all(|c| matches!(c, C::Ch(_))) {
        base.push(Tok::Lit(
            comp.iter()
                .map(|c| match c {
                    C::Ch(c) => fold(*c),
                    _ => '?',
                })
                .collect(),
        ));
    } else {
        base.push(Tok::Pat(
            comp.iter()
                .map(|c| match c {
                    C::Ch(c) => C::Ch(fold(*c)),
                    other => *other,
                })
                .collect(),
        ));
    }
}

/// The path patterns `alt` stands for, relative words placed in `cwds`.
fn to_pats(alt: &[C], cwds: &[Pat]) -> Vec<Pat> {
    let parts: Vec<&[C]> = alt.split(|c| *c == C::Ch('/')).collect();
    let absolute = alt.first() == Some(&C::Ch('/'));
    let unknown = !absolute && parts.first().is_some_and(|c| c.contains(&C::Deep));
    let bases: Vec<Pat> = if absolute || unknown {
        vec![Vec::new()]
    } else {
        cwds.to_vec()
    };
    bases
        .into_iter()
        .map(|mut base| {
            for comp in &parts {
                push_comp(&mut base, comp);
            }
            base
        })
        .collect()
}

/// The values a word gives a program as paths: itself, the value after an
/// `=` (`--file=…`), and an absolute value attached to a short option.
fn candidates(alt: &Alt) -> Vec<Alt> {
    let mut out = vec![alt.clone()];
    if let Some(eq) = alt.iter().position(|c| *c == C::Ch('=')) {
        out.push(alt[eq + 1..].to_vec());
    }
    if alt.len() > 2 && alt[0] == C::Ch('-') && alt[1] != C::Ch('-') && alt[2] == C::Ch('/') {
        out.push(alt[2..].to_vec());
    }
    out
}

fn glob(p: &[C], s: &[char]) -> bool {
    match p.split_first() {
        None => s.is_empty(),
        Some((C::Star | C::Deep, rest)) => (0..=s.len()).any(|k| glob(rest, &s[k..])),
        Some((C::Quest | C::Class, rest)) => !s.is_empty() && glob(rest, &s[1..]),
        Some((C::Ch(c), rest)) => s.first() == Some(c) && glob(rest, &s[1..]),
    }
}

fn tok_fits(tok: &Tok, comp: &str) -> bool {
    match tok {
        Tok::Lit(s) => s == comp,
        Tok::Pat(p) => glob(p, &comp.chars().collect::<Vec<_>>()),
        Tok::Deep => true,
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    /// The pattern is the target.
    Exact,
    /// The pattern is the target or below it.
    Within,
    /// The pattern is a strict ancestor of the target.
    Ancestor,
}

/// Matches a pattern against a target directory. With `anchors`, one of
/// those target components must be matched by a written (not unknown)
/// part, or the first part below the target must name a record.
struct Fit<'a> {
    p: &'a [Tok],
    t: &'a [String],
    mode: Mode,
    anchors: &'a [usize],
    entries: &'a [String],
}

impl Fit<'_> {
    fn holds(&self) -> bool {
        self.go(0, 0, false)
    }

    fn go(&self, pi: usize, ti: usize, anchored: bool) -> bool {
        let ok = self.anchors.is_empty() || anchored;
        if ti == self.t.len() {
            return match self.mode {
                Mode::Exact => self.p[pi..].iter().all(|t| *t == Tok::Deep) && ok,
                Mode::Within => {
                    ok || self.p.get(pi).is_some_and(|tok| {
                        *tok != Tok::Deep && self.entries.iter().any(|e| tok_fits(tok, e))
                    })
                }
                Mode::Ancestor => false,
            };
        }
        let Some(tok) = self.p.get(pi) else {
            return self.mode == Mode::Ancestor && ok;
        };
        if *tok == Tok::Deep {
            return (ti..=self.t.len()).any(|k| self.go(pi + 1, k, anchored));
        }
        // The last target component is a record's name; a copy beside it
        // (`….json.bak`) holds the same.
        let last = ti + 1 == self.t.len();
        let fits = tok_fits(tok, &self.t[ti])
            || (last && matches!(tok, Tok::Lit(s) if names_record_or_copy(s, &self.t[ti])));
        fits && self.go(pi + 1, ti + 1, anchored || self.anchors.contains(&ti))
    }
}

/// A name test of a lister (`-name`, an `fd` pattern).
#[derive(Clone, Debug)]
enum Test {
    Glob(Alt, bool),
    Substr(String, bool),
    Any,
}

impl Test {
    fn fits(&self, name: &str) -> bool {
        let fold_all = |s: &str, ci: bool| -> Vec<char> {
            s.chars()
                .map(|c| {
                    if ci {
                        c.to_lowercase().next().unwrap_or(c)
                    } else {
                        fold(c)
                    }
                })
                .collect()
        };
        match self {
            Test::Any => true,
            Test::Glob(p, ci) => {
                let p: Alt = p
                    .iter()
                    .map(|c| match c {
                        C::Ch(c) if *ci => C::Ch(c.to_lowercase().next().unwrap_or(*c)),
                        C::Ch(c) => C::Ch(fold(*c)),
                        other => *other,
                    })
                    .collect();
                glob(&p, &fold_all(name, *ci))
            }
            Test::Substr(s, ci) => {
                let hay: String = fold_all(name, *ci).into_iter().collect();
                let needle: String = fold_all(s, *ci).into_iter().collect();
                hay.contains(&needle)
            }
        }
    }
}

/// Name tests ANDed, each an OR of alternatives; empty matches every name.
type Filter = Vec<Vec<Test>>;

fn filter_fits(filter: &Filter, name: &str) -> bool {
    filter.iter().all(|alts| alts.iter().any(|t| t.fits(name)))
}

/// A pattern the program matches itself (`find -name '*.json'`): quoted
/// `*`, `?` and `[…]` are pattern characters to it, not to the shell.
fn as_pattern(alt: &[C]) -> Alt {
    let mut out = Vec::with_capacity(alt.len());
    let mut i = 0;
    while i < alt.len() {
        match alt[i] {
            C::Ch('\\') if i + 1 < alt.len() => {
                out.push(alt[i + 1]);
                i += 1;
            }
            C::Ch('*') => out.push(C::Star),
            C::Ch('?') => out.push(C::Quest),
            C::Ch('[') => {
                let close = alt[i + 1..]
                    .iter()
                    .skip(1)
                    .position(|c| *c == C::Ch(']'))
                    .map(|k| i + 2 + k);
                if let Some(close) = close {
                    out.push(C::Class);
                    i = close;
                } else {
                    out.push(C::Ch('['));
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    out
}

fn glob_tests(word: &RWord, ci: bool) -> Vec<Test> {
    word.alts
        .iter()
        .map(|a| Test::Glob(as_pattern(a), ci))
        .collect()
}

// ---------------------------------------------------------------- context

struct Ctx<'a> {
    records: Vec<Vec<String>>,
    data: Vec<Vec<String>>,
    spell: Vec<String>,
    entries: Vec<String>,
    homes: Vec<Alt>,
    env: &'a dyn Fn(&str) -> Option<String>,
}

impl Ctx<'_> {
    /// A path inside the records, through a written part of their path.
    fn in_records(&self, p: &Pat) -> bool {
        self.records.iter().any(|r| {
            let n = r.len();
            let anchors = [n.saturating_sub(1), n.saturating_sub(2)];
            Fit {
                p,
                t: r,
                mode: Mode::Within,
                anchors: &anchors,
                entries: &self.entries,
            }
            .holds()
        })
    }

    /// A glob that may name the data directory through its last component.
    fn glob_on_data(&self, p: &Pat) -> bool {
        p.iter().any(|t| !matches!(t, Tok::Lit(_)))
            && self.data.iter().any(|d| {
                !d.is_empty()
                    && Fit {
                        p,
                        t: d,
                        mode: Mode::Exact,
                        anchors: &[d.len() - 1],
                        entries: &[],
                    }
                    .holds()
            })
    }

    /// A directory that is, contains or may be the records.
    fn reaches_records(&self, p: &Pat) -> bool {
        p.is_empty()
            || self.records.iter().any(|r| {
                [Mode::Within, Mode::Ancestor].into_iter().any(|mode| {
                    Fit {
                        p,
                        t: r,
                        mode,
                        anchors: &[],
                        entries: &[],
                    }
                    .holds()
                })
            })
    }

    /// Whether a lister walking `root` to `depth` with `filter` lists the
    /// records or a name inside them. `dirs_only`: it lists directories.
    fn walk_reaches(
        &self,
        root: &Pat,
        depth: Option<usize>,
        filter: &Filter,
        dirs_only: bool,
    ) -> bool {
        let depth = depth.unwrap_or(usize::MAX);
        self.records.iter().any(|r| {
            let n = r.len();
            let anchors = [n.saturating_sub(1), n.saturating_sub(2)];
            let inside = Fit {
                p: root,
                t: r,
                mode: Mode::Within,
                anchors: &anchors,
                entries: &self.entries,
            }
            .holds();
            inside
                || (0..n).any(|k| {
                    let above = Fit {
                        p: root,
                        t: &r[..k],
                        mode: Mode::Exact,
                        anchors: &[],
                        entries: &[],
                    }
                    .holds();
                    let dist = n - k;
                    above
                        && ((depth >= dist && filter_fits(filter, &r[n - 1]))
                            || (!dirs_only
                                && depth > dist
                                && self.entries.iter().any(|e| filter_fits(filter, e))))
                })
        })
    }

    /// A produced path inside the data directory, or a written one above it.
    fn feeds_hit(&self, p: &Pat) -> bool {
        self.data.iter().any(|d| {
            !d.is_empty()
                && (Fit {
                    p,
                    t: d,
                    mode: Mode::Within,
                    anchors: &[d.len() - 1],
                    entries: &[],
                }
                .holds()
                    || (!p.is_empty()
                        && !p.contains(&Tok::Deep)
                        && Fit {
                            p,
                            t: d,
                            mode: Mode::Ancestor,
                            anchors: &[],
                            entries: &[],
                        }
                        .holds()))
        })
    }

    /// Text spelling the records' path or the release ids' name.
    fn spelled(&self, text: &str) -> bool {
        let key = path_key(Path::new(text));
        key.contains(RELEASE_IDS_NAME) || self.spell.iter().any(|s| holds_path(&key, s))
    }

    fn home_pats(&self) -> Vec<Pat> {
        self.homes.iter().flat_map(|h| to_pats(h, &[])).collect()
    }
}

fn render(p: &Pat) -> Alt {
    let mut out = Vec::new();
    for tok in p {
        out.push(C::Ch('/'));
        match tok {
            Tok::Lit(s) => out.extend(s.chars().map(C::Ch)),
            Tok::Pat(cs) => out.extend_from_slice(cs),
            Tok::Deep => out.push(C::Deep),
        }
    }
    if out.is_empty() {
        out.push(C::Ch('/'));
    }
    out
}

fn is_identifier(text: &str) -> bool {
    text.chars()
        .next()
        .is_some_and(|c| c == '_' || c.is_ascii_alphabetic())
        && text.chars().all(|c| c == '_' || c.is_ascii_alphanumeric())
}

/// `NAME=value` (or `NAME+=value`): the name and the value's parts.
fn assignment(parts: &[Part]) -> Option<(String, &[Part])> {
    let mut name = String::new();
    for (i, part) in parts.iter().enumerate() {
        match part {
            Part::Ch('=') if is_identifier(&name) => return Some((name, &parts[i + 1..])),
            Part::Ch('+')
                if is_identifier(&name) && matches!(parts.get(i + 1), Some(Part::Ch('='))) =>
            {
                return Some((name, &parts[i + 2..]));
            }
            Part::Ch(c) if *c == '_' || c.is_ascii_alphanumeric() => name.push(*c),
            _ => return None,
        }
    }
    None
}

/// Replaces `{}`-style placeholders with an unknown value; `None` when the
/// words hold none.
fn replace_placeholders(words: &[RWord], marker: Option<&str>) -> Option<Vec<RWord>> {
    let mut found = false;
    let out = words
        .iter()
        .map(|w| {
            let alts = w
                .alts
                .iter()
                .map(|alt| {
                    let mut out = Vec::new();
                    let mut i = 0;
                    while i < alt.len() {
                        let len = match marker {
                            Some(m) => {
                                let m: Vec<C> = m.chars().map(C::Ch).collect();
                                (!m.is_empty() && alt[i..].starts_with(&m)).then_some(m.len())
                            }
                            None => brace_placeholder(&alt[i..]),
                        };
                        if let Some(len) = len {
                            out.push(C::Deep);
                            found = true;
                            i += len;
                        } else {
                            out.push(alt[i]);
                            i += 1;
                        }
                    }
                    out
                })
                .collect();
            RWord {
                alts,
                raw: w.raw.clone(),
                parts: None,
            }
        })
        .collect();
    found.then_some(out)
}

/// The length of a `{}`, `{.}`, `{/}`, `{//}`, `{/.}`, `{#}`, `{1}` placeholder.
fn brace_placeholder(alt: &[C]) -> Option<usize> {
    if alt.first() != Some(&C::Ch('{')) {
        return None;
    }
    let end = alt.iter().take(6).position(|c| *c == C::Ch('}'))?;
    alt[1..end]
        .iter()
        .all(|c| matches!(c, C::Ch(c) if "./#%".contains(*c) || c.is_ascii_digit()))
        .then_some(end + 1)
}

// ---------------------------------------------------------------- options

#[derive(Default)]
struct Scan {
    positional: Vec<RWord>,
    shorts: Vec<char>,
    longs: Vec<String>,
    values: Vec<(String, RWord)>,
}

impl Scan {
    fn value(&self, keys: &[&str]) -> Option<&RWord> {
        self.values
            .iter()
            .rev()
            .find(|(k, _)| keys.contains(&k.as_str()))
            .map(|(_, v)| v)
    }

    fn long_prefix(&self, names: &[&str], min: usize) -> bool {
        self.longs
            .iter()
            .any(|l| l.len() >= min && names.iter().any(|n| n.starts_with(l.as_str())))
    }
}

/// Options and operands: `arg_short` letters and `arg_long` names take a
/// value (attached or the next word; long names by unique prefix).
fn scan(args: &[RWord], arg_short: &str, arg_long: &[&str]) -> Scan {
    let mut s = Scan::default();
    let mut i = 0;
    while i < args.len() {
        let t = args[i].text();
        i += 1;
        if t == "--" {
            s.positional.extend(args[i..].iter().cloned());
            break;
        }
        if let Some(long) = t.strip_prefix("--") {
            if let Some((name, value)) = long.split_once('=') {
                s.longs.push(name.to_string());
                s.values.push((name.to_string(), RWord::from_text(value)));
            } else {
                s.longs.push(long.to_string());
                if long.len() >= 2
                    && arg_long.iter().any(|l| l.starts_with(long))
                    && let Some(v) = args.get(i)
                {
                    s.values.push((long.to_string(), v.clone()));
                    i += 1;
                }
            }
            continue;
        }
        if t.len() > 1 && t.starts_with('-') {
            let chars: Vec<char> = t[1..].chars().collect();
            for (k, c) in chars.iter().enumerate() {
                if arg_short.contains(*c) {
                    let rest: String = chars[k + 1..].iter().collect();
                    let value = if rest.is_empty() {
                        let v = args.get(i).cloned();
                        if v.is_some() {
                            i += 1;
                        }
                        v
                    } else {
                        Some(RWord::from_text(&rest))
                    };
                    if let Some(v) = value {
                        s.values.push((format!("-{c}"), v));
                    }
                    break;
                }
                s.shorts.push(*c);
            }
            continue;
        }
        s.positional.push(args[i - 1].clone());
    }
    s
}

fn depth_of(word: Option<&RWord>) -> Option<usize> {
    word.and_then(RWord::lit)
        .and_then(|t| t.trim().parse().ok())
}

fn dot() -> RWord {
    RWord::from_text(".")
}

// ---------------------------------------------------------------- evaluation

type Verdict = Result<(), String>;

struct Env<'a> {
    ctx: &'a Ctx<'a>,
    vars: HashMap<String, Vec<Alt>>,
    cwds: Vec<Pat>,
    seen: Vec<Pat>,
    hits: Vec<String>,
    consumer: bool,
    depth: usize,
}

type Saved = (HashMap<String, Vec<Alt>>, Vec<Pat>, Vec<Pat>);

impl Env<'_> {
    fn save(&self) -> Saved {
        (self.vars.clone(), self.cwds.clone(), self.seen.clone())
    }

    fn restore(&mut self, saved: Saved) {
        (self.vars, self.cwds, self.seen) = saved;
    }

    // -- expansion

    fn resolve(&self, parts: &[Part]) -> Vec<Alt> {
        let (alts, _) = self.seq(parts, 0, false);
        if alts.len() > MAX_ALTS {
            vec![vec![C::Deep]]
        } else {
            alts
        }
    }

    fn seq(&self, parts: &[Part], mut i: usize, inner: bool) -> (Vec<Alt>, usize) {
        let mut acc: Vec<Alt> = vec![Vec::new()];
        while i < parts.len() {
            let piece = match &parts[i] {
                Part::BrComma | Part::BrClose if inner => break,
                Part::BrOpen => {
                    if let Some((options, next)) = self.brace(parts, i) {
                        i = next;
                        options
                    } else {
                        i += 1;
                        vec![vec![C::Ch('{')]]
                    }
                }
                part => {
                    i += 1;
                    self.piece(part)
                }
            };
            acc = cross(&acc, &piece);
        }
        (acc, i)
    }

    fn brace(&self, parts: &[Part], open: usize) -> Option<(Vec<Alt>, usize)> {
        let mut depth = 0usize;
        let mut comma = false;
        let mut close = None;
        for (j, part) in parts.iter().enumerate().skip(open) {
            match part {
                Part::BrOpen => depth += 1,
                Part::BrClose => {
                    depth -= 1;
                    if depth == 0 {
                        close = Some(j);
                        break;
                    }
                }
                Part::BrComma if depth == 1 => comma = true,
                _ => {}
            }
        }
        let close = close?;
        if !comma {
            let text: String = parts[open + 1..close]
                .iter()
                .filter_map(|p| match p {
                    Part::Ch(c) => Some(*c),
                    _ => None,
                })
                .collect();
            return text
                .contains("..")
                .then(|| (vec![vec![C::Deep]], close + 1));
        }
        let mut options = Vec::new();
        let mut j = open + 1;
        loop {
            let (alts, k) = self.seq(parts, j, true);
            options.extend(alts);
            if k >= close || matches!(parts.get(k), Some(Part::BrClose)) {
                break;
            }
            j = k + 1;
        }
        Some((options, close + 1))
    }

    fn piece(&self, part: &Part) -> Vec<Alt> {
        match part {
            Part::Ch(c) => vec![vec![C::Ch(*c)]],
            Part::Star => vec![vec![C::Star]],
            Part::Quest => vec![vec![C::Quest]],
            Part::Class => vec![vec![C::Class]],
            Part::BrOpen => vec![vec![C::Ch('{')]],
            Part::BrComma => vec![vec![C::Ch(',')]],
            Part::BrClose => vec![vec![C::Ch('}')]],
            Part::Tilde(name) => self.tilde(name),
            Part::Var(name) => self.var(name),
            Part::Default(name, word) => {
                let mut out = if name.is_empty() {
                    vec![Vec::new()]
                } else {
                    self.var(name)
                };
                out.extend(self.resolve(word));
                out
            }
            Part::Sub(_) | Part::Unknown(_) => vec![vec![C::Deep]],
        }
    }

    fn tilde(&self, name: &str) -> Vec<Alt> {
        match name {
            "" => self.ctx.homes.clone(),
            "+" => self.cwds.iter().map(render).collect(),
            "-" => self.seen.iter().map(render).collect(),
            _ => {
                let named: Vec<Alt> = self
                    .ctx
                    .homes
                    .iter()
                    .filter(|h| {
                        let text = alt_text(h);
                        basename(text.trim_end_matches('/')) == name
                    })
                    .cloned()
                    .collect();
                if named.is_empty() {
                    vec![alt_of(&format!("~{name}"))]
                } else {
                    named
                }
            }
        }
    }

    fn var(&self, name: &str) -> Vec<Alt> {
        if let Some(values) = self.vars.get(name) {
            return values.clone();
        }
        let under_home = |rel: &str| -> Vec<Alt> {
            self.ctx
                .homes
                .iter()
                .map(|h| {
                    let mut a = h.clone();
                    a.extend(rel.chars().map(C::Ch));
                    a
                })
                .collect()
        };
        match name {
            "HOME" => self.ctx.homes.clone(),
            "PWD" => self.cwds.iter().map(render).collect(),
            "OLDPWD" => self.seen.iter().map(render).collect(),
            "XDG_DATA_HOME" => (self.ctx.env)(name)
                .map_or_else(|| under_home("/.local/share"), |v| vec![alt_of(&v)]),
            "XDG_CONFIG_HOME" => {
                (self.ctx.env)(name).map_or_else(|| under_home("/.config"), |v| vec![alt_of(&v)])
            }
            "XDG_STATE_HOME" => (self.ctx.env)(name)
                .map_or_else(|| under_home("/.local/state"), |v| vec![alt_of(&v)]),
            // Unset, lean-ctx keeps its state in the XDG default.
            "LEAN_CTX_STATE_DIR" => (self.ctx.env)(name).map_or_else(
                || under_home("/.local/state/lean-ctx"),
                |v| vec![alt_of(&v)],
            ),
            _ if is_identifier(name) => {
                (self.ctx.env)(name).map_or_else(|| vec![vec![C::Deep]], |v| vec![alt_of(&v)])
            }
            _ => vec![vec![C::Deep]],
        }
    }

    fn assign(&mut self, name: String, alts: Vec<Alt>) {
        let entry = self.vars.entry(name).or_default();
        for alt in alts {
            if !entry.contains(&alt) {
                entry.push(alt);
            }
        }
        if entry.len() > MAX_ALTS {
            *entry = vec![vec![C::Deep]];
        }
    }

    fn rword(&self, word: &Word) -> RWord {
        RWord {
            alts: self.resolve(&word.parts),
            raw: word.raw.clone(),
            parts: Some(word.parts.clone()),
        }
    }

    // -- scripts

    fn eval_script(&mut self, script: &Script, producer: bool) -> Verdict {
        for pipeline in script {
            let feeder = pipeline
                .iter()
                .rposition(|stage| FEEDERS.contains(&stage_program(stage).as_str()));
            for (j, stage) in pipeline.iter().enumerate() {
                let producer = producer || feeder.is_some_and(|f| j < f);
                self.eval_simple(stage, producer)?;
            }
        }
        Ok(())
    }

    fn eval_nested(&mut self, script: &Script, producer: bool) -> Verdict {
        let saved = self.save();
        let verdict = self.eval_script(script, producer);
        self.restore(saved);
        verdict
    }

    fn eval_subs(&mut self, parts: &[Part]) -> Verdict {
        let mut scripts = Vec::new();
        collect_scripts(parts, &mut scripts);
        for script in &scripts {
            self.eval_nested(script, true)?;
        }
        Ok(())
    }

    fn eval_simple(&mut self, st: &Simple, producer: bool) -> Verdict {
        if let Some(inner) = &st.subshell {
            self.eval_nested(inner, producer)?;
        }
        for word in &st.words {
            self.eval_subs(&word.parts)?;
        }
        for (_, word) in &st.redirs {
            self.eval_subs(&word.parts)?;
        }
        for word in &st.heredocs {
            self.eval_subs(&word.parts)?;
        }
        let mut k = 0;
        while let Some((name, value)) = st.words.get(k).and_then(|w| assignment(&w.parts)) {
            let alts = self.resolve(value);
            let word = self.rword(&st.words[k]);
            self.read_word(&word, &self.cwds.clone(), true)?;
            self.assign(name, alts);
            k += 1;
        }
        let cwds = self.cwds.clone();
        let mut stdin = Vec::new();
        for (op, target) in &st.redirs {
            let word = self.rword(target);
            self.read_word(&word, &cwds, false)?;
            if op == "<<<" {
                stdin.push(word);
            }
        }
        for body in &st.heredocs {
            let word = self.rword(body);
            for alt in &word.alts {
                if self.ctx.spelled(&alt_text(alt)) {
                    return Err(word.raw.lines().next().unwrap_or_default().to_string());
                }
            }
            stdin.push(word);
        }
        // Output written to a file may be read back later on the line
        // (`… > /tmp/l; xargs cat < /tmp/l`): the command produces it.
        let to_file = st.redirs.iter().any(|(op, target)| {
            let target = self.rword(target).text();
            match op.as_str() {
                ">" | ">>" | ">|" | "&>" | "&>>" => target != "/dev/null",
                ">&" => !target.chars().all(|c| c.is_ascii_digit() || c == '-'),
                _ => false,
            }
        });
        let words: Vec<RWord> = st.words[k..].iter().map(|w| self.rword(w)).collect();
        self.judge(&words, producer || to_file, &stdin)
    }

    /// Runs `word`'s values as a command line (`bash -c`, `eval`).
    fn run_text(&mut self, word: &RWord, producer: bool) -> Verdict {
        for alt in word.alts.iter().take(16) {
            let text: String = alt
                .iter()
                .map(|c| match c {
                    C::Ch(c) => c.to_string(),
                    C::Star => "*".to_string(),
                    C::Quest | C::Class => "?".to_string(),
                    C::Deep => UNKNOWN_VAR.to_string(),
                })
                .collect();
            let mut parser = Parser::new(&text, self.depth + 1);
            let script = parser.script(false);
            if parser.too_deep {
                return Err(word.raw.clone());
            }
            self.depth += 1;
            let verdict = self.eval_nested(&script, producer);
            self.depth -= 1;
            verdict?;
        }
        Ok(())
    }

    // -- the rules

    /// The reader rule: a word naming a path inside the records, a glob
    /// naming the data directory (not for name-only programs), or text
    /// spelling them.
    fn read_word(&self, word: &RWord, cwds: &[Pat], name_only: bool) -> Verdict {
        for alt in &word.alts {
            if self.ctx.spelled(&alt_text(alt)) {
                return Err(word.raw.clone());
            }
            for cand in candidates(alt) {
                for p in to_pats(&cand, cwds) {
                    if self.ctx.in_records(&p) || (!name_only && self.ctx.glob_on_data(&p)) {
                        return Err(word.raw.clone());
                    }
                }
            }
        }
        Ok(())
    }

    /// A producer's word: notes it when it names the data directory or a
    /// directory above it.
    fn hit_word(&mut self, word: &RWord, cwds: &[Pat]) {
        let hit = word.alts.iter().any(|alt| {
            let text = path_key(Path::new(&alt_text(alt)));
            text.contains("lean-ctx/agents")
                || text.contains(RELEASE_IDS_NAME)
                || candidates(alt)
                    .iter()
                    .any(|c| to_pats(c, cwds).iter().any(|p| self.ctx.feeds_hit(p)))
        });
        if hit {
            self.hits.push(word.raw.clone());
        }
    }

    fn deny_reaching(&self, operands: &[RWord], cwds: &[Pat]) -> Verdict {
        for op in operands {
            for alt in &op.alts {
                for p in to_pats(alt, cwds) {
                    if self.ctx.reaches_records(&p) {
                        return Err(op.raw.clone());
                    }
                }
            }
        }
        Ok(())
    }

    /// The lister rule for `roots`, plus the roots of a producer.
    #[allow(clippy::too_many_arguments)]
    fn walk(
        &mut self,
        roots: &[RWord],
        cwds: &[Pat],
        depth: Option<usize>,
        filter: &Filter,
        dirs_only: bool,
        producer: bool,
        prints_root: bool,
    ) -> Verdict {
        for root in roots {
            for alt in &root.alts {
                for p in to_pats(alt, cwds) {
                    if self.ctx.walk_reaches(&p, depth, filter, dirs_only) {
                        return Err(root.raw.clone());
                    }
                }
            }
            if producer && prints_root {
                self.hit_word(root, cwds);
            }
        }
        Ok(())
    }

    fn judge(&mut self, words: &[RWord], producer: bool, stdin: &[RWord]) -> Verdict {
        if self.depth > MAX_DEPTH {
            return Err(words.first().map(|w| w.raw.clone()).unwrap_or_default());
        }
        let start = words
            .iter()
            .position(|w| !w.lit().is_some_and(|t| RESERVED.contains(&t.as_str())))
            .unwrap_or(words.len());
        let words = &words[start..];
        let cwds = self.cwds.clone();
        let Some(first) = words.first() else {
            return Ok(());
        };
        let head = first.lit().map(|t| basename(&t)).unwrap_or_default();
        if matches!(head.as_str(), "for" | "select" | "case" | "function") {
            for word in words {
                self.read_word(word, &cwds, true)?;
            }
            if matches!(head.as_str(), "for" | "select")
                && let Some(name) = words
                    .get(1)
                    .and_then(RWord::lit)
                    .filter(|n| is_identifier(n))
            {
                let values: Vec<Alt> = if words.get(2).and_then(RWord::lit).as_deref() == Some("in")
                {
                    words[3..].iter().flat_map(|w| w.alts.clone()).collect()
                } else {
                    vec![vec![C::Deep]]
                };
                self.assign(name, values);
            }
            return Ok(());
        }
        let peeled = self.peel(words);
        if let Some(script) = &peeled.split {
            for word in words {
                self.read_word(word, &cwds, false)?;
            }
            return self.run_text(script, producer);
        }
        let prog = &words[peeled.at.min(words.len())..];
        let Some(program_word) = prog.first() else {
            for word in words {
                self.read_word(word, &cwds, false)?;
            }
            return Ok(());
        };
        let literal = program_word.lit();
        let program = literal.as_deref().map(basename).unwrap_or_default();
        let args = &prog[1..];
        let mut cmd_cwds = cwds.clone();
        for dir in &peeled.cwds {
            extend_cwds(&mut cmd_cwds, dir);
        }
        match program.as_str() {
            "git" => {
                let mut i = 0;
                while let Some(t) = args.get(i).map(RWord::text) {
                    match t.as_str() {
                        "-C" => {
                            if let Some(dir) = args.get(i + 1) {
                                extend_cwds(&mut cmd_cwds, dir);
                            }
                            i += 2;
                        }
                        "-c" | "--git-dir" | "--work-tree" | "--namespace" | "--exec-path"
                        | "--config-env" | "--super-prefix" => i += 2,
                        _ if t.starts_with('-') => i += 1,
                        _ => break,
                    }
                }
            }
            "tar" | "bsdtar" | "gtar" => {
                let s = scan(
                    args,
                    "fCTXbLHKNgV",
                    &["directory", "file", "files-from", "exclude-from"],
                );
                for (key, value) in &s.values {
                    if key == "-C" || (key.len() >= 3 && "directory".starts_with(key.as_str())) {
                        extend_cwds(&mut cmd_cwds, value);
                    }
                }
            }
            _ => {}
        }
        let ls_recursive = program == "ls" && {
            let s = scan(args, "IwT", &["ignore", "width", "tabsize", "hide"]);
            (s.shorts.contains(&'R') || s.long_prefix(&["recursive"], 3))
                && !s.shorts.contains(&'d')
        };
        let name_only = NAME_ONLY.contains(&program.as_str())
            || LISTERS.contains(&program.as_str())
            || ls_recursive;
        for word in words {
            self.read_word(word, &cmd_cwds, name_only)?;
        }
        let known = NAME_ONLY.contains(&program.as_str())
            || LISTERS.contains(&program.as_str())
            || FEEDERS.contains(&program.as_str())
            || STRUCTURAL.contains(&program.as_str())
            || SHELLS.contains(&program.as_str())
            || program == "eval";
        if literal.is_none() || !known {
            self.consumer = true;
        }
        let walker = LISTERS.contains(&program.as_str()) || ls_recursive;
        if producer && !walker {
            for word in args {
                self.hit_word(word, &cmd_cwds);
            }
            if program == "pwd"
                && cmd_cwds.iter().any(|c| {
                    self.ctx.data.iter().any(|d| {
                        !d.is_empty()
                            && Fit {
                                p: c,
                                t: d,
                                mode: Mode::Within,
                                anchors: &[d.len() - 1],
                                entries: &[],
                            }
                            .holds()
                    })
                })
            {
                self.hits.push("pwd".to_string());
            }
        }
        match program.as_str() {
            "cd" | "pushd" => {
                self.change_dir(args);
                Ok(())
            }
            "popd" => {
                self.cwds = vec![vec![Tok::Deep]];
                Ok(())
            }
            "export" | "local" | "declare" | "typeset" | "readonly" => {
                for word in args {
                    if let Some(parts) = &word.parts
                        && let Some((name, value)) = assignment(parts)
                    {
                        let alts = self.resolve(value);
                        self.assign(name, alts);
                    }
                }
                Ok(())
            }
            "read" | "mapfile" | "readarray" => {
                for word in args {
                    if let Some(name) = word.lit().filter(|n| is_identifier(n)) {
                        self.assign(name, vec![vec![C::Deep]]);
                    }
                }
                Ok(())
            }
            "eval" => {
                let text: Vec<String> = args.iter().map(RWord::text).collect();
                self.run_text(&RWord::from_text(&text.join(" ")), producer)
            }
            p if SHELLS.contains(&p) => self.shell(args, stdin, producer),
            "xargs" => self.xargs(args, producer),
            "parallel" | "rush" => self.parallel(args, producer),
            "find" => self.find(args, &cmd_cwds, producer),
            "fd" | "fdfind" => self.fd(args, &cmd_cwds, producer),
            "tree" => {
                let s = scan(
                    args,
                    "LPIoHT",
                    &[
                        "filelimit",
                        "charset",
                        "timefmt",
                        "sort",
                        "hintro",
                        "houtro",
                    ],
                );
                let mut filter = Filter::new();
                if let Some(p) = s.value(&["-P"]) {
                    filter.push(glob_tests(p, false));
                }
                let depth = depth_of(s.value(&["-L"]));
                let roots = or_dot(s.positional);
                self.walk(&roots, &cmd_cwds, depth, &filter, false, producer, true)
            }
            "du" => {
                let s = scan(
                    args,
                    "dBtX",
                    &[
                        "max-depth",
                        "block-size",
                        "threshold",
                        "exclude",
                        "exclude-from",
                        "time-style",
                    ],
                );
                let all = s.shorts.contains(&'a') || s.long_prefix(&["all"], 3);
                let depth = if s.shorts.contains(&'s') || s.long_prefix(&["summarize"], 3) {
                    Some(0)
                } else {
                    depth_of(s.value(&["-d", "max-depth"]))
                };
                let roots = or_dot(s.positional);
                self.walk(
                    &roots,
                    &cmd_cwds,
                    depth,
                    &Filter::new(),
                    !all,
                    producer,
                    true,
                )
            }
            "ls" if ls_recursive => {
                let s = scan(args, "IwT", &["ignore", "width", "tabsize", "hide"]);
                let roots = or_dot(s.positional);
                self.walk(
                    &roots,
                    &cmd_cwds,
                    None,
                    &Filter::new(),
                    false,
                    producer,
                    true,
                )
            }
            "locate" | "mlocate" | "plocate" => self.locate(args),
            "git" => self.git(args, &cmd_cwds),
            _ => match recursive_operands(&program, args) {
                Some(operands) => self.deny_reaching(&operands, &cmd_cwds),
                None => Ok(()),
            },
        }
    }

    fn change_dir(&mut self, args: &[RWord]) {
        let target = args
            .iter()
            .find(|w| w.text() == "-" || !w.text().starts_with('-'));
        let new: Vec<Pat> = match target {
            None => self.ctx.home_pats(),
            Some(w) if w.text() == "-" => self.seen.clone(),
            Some(w) if w.text().starts_with('+') => vec![vec![Tok::Deep]],
            Some(w) => {
                let cdpath = self.vars.contains_key("CDPATH")
                    || (self.ctx.env)("CDPATH").is_some_and(|v| !v.is_empty());
                let mut out = Vec::new();
                for alt in &w.alts {
                    let relative = !matches!(alt.first(), Some(C::Ch('/' | '.')));
                    if cdpath && relative {
                        out = vec![vec![Tok::Deep]];
                        break;
                    }
                    out.extend(to_pats(alt, &self.cwds));
                }
                out
            }
        };
        for p in new {
            if !self.cwds.contains(&p) {
                self.cwds.push(p.clone());
            }
            if !self.seen.contains(&p) {
                self.seen.push(p);
            }
        }
        if self.cwds.len() > MAX_CWDS || self.cwds.iter().any(|c| c.first() == Some(&Tok::Deep)) {
            self.cwds = vec![vec![Tok::Deep]];
        }
        if self.seen.len() > MAX_CWDS {
            self.seen = vec![vec![Tok::Deep]];
        }
    }

    fn shell(&mut self, args: &[RWord], stdin: &[RWord], producer: bool) -> Verdict {
        let mut has_c = false;
        let mut skip = false;
        for word in args {
            let t = word.text();
            if skip {
                skip = false;
                continue;
            }
            if t.starts_with("--") {
                continue;
            }
            if (t.starts_with('-') || t.starts_with('+')) && t.len() > 1 {
                has_c |= t[1..].contains('c');
                skip = t[1..].ends_with('o');
                continue;
            }
            if has_c {
                return self.run_text(word, producer);
            }
            return Ok(());
        }
        if !has_c {
            for word in stdin {
                self.run_text(word, producer)?;
            }
        }
        Ok(())
    }

    fn xargs(&mut self, args: &[RWord], producer: bool) -> Verdict {
        let mut i = 0;
        let mut replace: Option<String> = None;
        while let Some(t) = args.get(i).map(RWord::text) {
            if t == "--" {
                i += 1;
                break;
            }
            if !t.starts_with('-') || t == "-" {
                break;
            }
            i += 1;
            match t.as_str() {
                "-I" => {
                    replace = args.get(i).map(RWord::text);
                    i += 1;
                }
                "-i" | "--replace" => replace = Some("{}".to_string()),
                "-a" | "-n" | "-P" | "-L" | "-s" | "-d" | "-E" | "--arg-file" | "--max-args"
                | "--max-procs" | "--max-lines" | "--max-chars" | "--delimiter" | "--eof"
                | "--process-slot-var" => i += 1,
                s if s.starts_with("-I") => replace = Some(s[2..].to_string()),
                s if s.starts_with("-i") => replace = Some(s[2..].to_string()),
                s if s.starts_with("--replace=") => {
                    replace = Some(s["--replace=".len()..].to_string());
                }
                _ => {}
            }
        }
        let command: Vec<RWord> = if i < args.len() {
            args[i..].to_vec()
        } else {
            vec![RWord::from_text("echo")]
        };
        let inner = if let Some(r) = replace.filter(|r| !r.is_empty()) {
            replace_placeholders(&command, Some(&r)).unwrap_or(command)
        } else {
            let mut c = command;
            c.push(RWord::deep());
            c
        };
        self.depth += 1;
        let verdict = self.judge(&inner, producer, &[]);
        self.depth -= 1;
        verdict
    }

    fn parallel(&mut self, args: &[RWord], producer: bool) -> Verdict {
        let mut i = 0;
        while let Some(t) = args.get(i).map(RWord::text) {
            if t == "--" {
                i += 1;
                break;
            }
            if !t.starts_with('-') {
                break;
            }
            i += 1;
            if matches!(
                t.as_str(),
                "-j" | "-P"
                    | "-n"
                    | "-N"
                    | "-S"
                    | "-a"
                    | "-d"
                    | "-C"
                    | "--jobs"
                    | "--arg-file"
                    | "--delimiter"
                    | "--colsep"
                    | "--joblog"
                    | "--results"
                    | "--tmpdir"
                    | "--sshlogin"
                    | "--workdir"
            ) {
                i += 1;
            }
        }
        let command: Vec<RWord> = args[i.min(args.len())..]
            .iter()
            .take_while(|w| !w.text().starts_with(":::"))
            .cloned()
            .collect();
        if command.is_empty() {
            return Ok(());
        }
        let inner = replace_placeholders(&command, None).unwrap_or_else(|| {
            let mut c = command.clone();
            c.push(RWord::deep());
            c
        });
        self.depth += 1;
        let verdict = self.judge(&inner, producer, &[]);
        self.depth -= 1;
        verdict
    }

    fn find(&mut self, args: &[RWord], cwds: &[Pat], producer: bool) -> Verdict {
        let mut i = 0;
        let mut roots = Vec::new();
        while let Some(t) = args.get(i).map(RWord::text) {
            let bsd = t.len() >= 2
                && t.starts_with('-')
                && t[1..].chars().all(|c| "HLPEXdsx".contains(c));
            if bsd || t.starts_with("-O") {
                i += 1;
            } else if t == "-D" {
                i += 2;
            } else if t == "-f" {
                if let Some(root) = args.get(i + 1) {
                    roots.push(root.clone());
                }
                i += 2;
            } else {
                break;
            }
        }
        while let Some(word) = args.get(i) {
            let t = word.text();
            if t.starts_with('-') || t == "(" || t == "!" {
                break;
            }
            roots.push(word.clone());
            i += 1;
        }
        let mut depth = None;
        let mut filter = Filter::new();
        let mut open = false;
        let mut inners = Vec::new();
        while let Some(t) = args.get(i).map(RWord::text) {
            match t.as_str() {
                "-maxdepth" => {
                    depth = depth_of(args.get(i + 1)).or(Some(usize::MAX));
                    i += 2;
                }
                "-name" | "-iname" => {
                    if let Some(p) = args.get(i + 1) {
                        filter.push(glob_tests(p, t == "-iname"));
                    }
                    i += 2;
                }
                "-o" | "-or" | "!" | "-not" | "(" | ")" | "," => {
                    open = true;
                    i += 1;
                }
                "-exec" | "-execdir" | "-ok" | "-okdir" => {
                    let mut j = i + 1;
                    while args
                        .get(j)
                        .is_some_and(|w| !matches!(w.text().as_str(), ";" | "+"))
                    {
                        j += 1;
                    }
                    inners.push(args[i + 1..j.min(args.len())].to_vec());
                    i = j + 1;
                }
                _ => i += 1,
            }
        }
        if depth == Some(usize::MAX) {
            depth = None;
        }
        let roots = or_dot(roots);
        if open {
            filter.clear();
        }
        self.walk(&roots, cwds, depth, &filter, false, false, false)?;
        if producer {
            for root in &roots {
                let printed = root
                    .lit()
                    .map(|t| basename(t.trim_end_matches('/')))
                    .is_none_or(|name| filter_fits(&filter, &name));
                if printed {
                    self.hit_word(root, cwds);
                }
            }
        }
        for inner in inners {
            let inner = replace_placeholders(&inner, Some("{}")).unwrap_or(inner);
            self.depth += 1;
            let verdict = self.judge(&inner, producer, &[]);
            self.depth -= 1;
            verdict?;
        }
        Ok(())
    }

    fn fd(&mut self, args: &[RWord], cwds: &[Pat], producer: bool) -> Verdict {
        let exec_at = args.iter().position(|w| {
            let t = w.text();
            matches!(t.as_str(), "-x" | "-X" | "--exec" | "--exec-batch")
        });
        let (opts, exec) = match exec_at {
            Some(at) => (&args[..at], Some(&args[at + 1..])),
            None => (args, None),
        };
        let s = scan(
            opts,
            "detEcjSo",
            &[
                "max-depth",
                "maxdepth",
                "exact-depth",
                "min-depth",
                "extension",
                "type",
                "exclude",
                "search-path",
                "base-directory",
                "changed-within",
                "changed-before",
                "size",
                "owner",
                "color",
                "threads",
                "ignore-file",
                "path-separator",
                "max-results",
                "batch-size",
                "format",
                "and",
            ],
        );
        let glob_mode = s.shorts.contains(&'g') || s.long_prefix(&["glob"], 3);
        let fixed = s.shorts.contains(&'F') || s.long_prefix(&["fixed-strings"], 3);
        let full_path = s.shorts.contains(&'p') || s.long_prefix(&["full-path"], 3);
        let sensitive = s.shorts.contains(&'s') || s.long_prefix(&["case-sensitive"], 3);
        let insensitive = s.shorts.contains(&'i') || s.long_prefix(&["ignore-case"], 3);
        let test = |word: &RWord| -> Vec<Test> {
            word.alts
                .iter()
                .map(|alt| {
                    let text = alt_text(alt);
                    let ci = insensitive || (!sensitive && !text.chars().any(char::is_uppercase));
                    if full_path {
                        Test::Any
                    } else if glob_mode {
                        Test::Glob(as_pattern(alt), ci)
                    } else if !fixed
                        && (text.contains('\u{0}')
                            || text.chars().any(|c| ".*+?[](){}|^$\\".contains(c)))
                    {
                        Test::Any
                    } else {
                        Test::Substr(text, ci)
                    }
                })
                .collect()
        };
        let mut filter = Filter::new();
        let mut positional = s.positional.clone();
        if !positional.is_empty() {
            let pattern = positional.remove(0);
            if !pattern.text().is_empty() {
                filter.push(test(&pattern));
            }
        }
        // Every `-e`/`--extension` widens one name test: fd keeps a name
        // whose extension is any of them.
        let mut extensions: Vec<Test> = Vec::new();
        for (key, value) in &s.values {
            if key == "and" {
                filter.push(test(value));
            }
            if key == "-e" || key == "extension" {
                for alt in &value.alts {
                    let rest = match alt.first() {
                        Some(C::Ch('.')) => &alt[1..],
                        _ => &alt[..],
                    };
                    let mut ext: Alt = vec![C::Star, C::Ch('.')];
                    ext.extend(as_pattern(rest));
                    extensions.push(Test::Glob(ext, true));
                }
            }
            if key == "search-path" {
                positional.push(value.clone());
            }
        }
        if !extensions.is_empty() {
            filter.push(extensions);
        }
        let mut cmd_cwds = cwds.to_vec();
        if let Some(base) = s.value(&["base-directory"]) {
            extend_cwds(&mut cmd_cwds, base);
        }
        let depth = depth_of(s.value(&["-d", "max-depth", "maxdepth", "exact-depth"]));
        let roots = or_dot(positional);
        self.walk(&roots, &cmd_cwds, depth, &filter, false, false, false)?;
        if let Some(exec) = exec {
            let command: Vec<RWord> = exec
                .iter()
                .take_while(|w| w.text() != ";")
                .cloned()
                .collect();
            if !command.is_empty() {
                let inner = replace_placeholders(&command, None).unwrap_or_else(|| {
                    let mut c = command.clone();
                    c.push(RWord::deep());
                    c
                });
                self.depth += 1;
                let verdict = self.judge(&inner, producer, &[]);
                self.depth -= 1;
                verdict?;
            }
        }
        Ok(())
    }

    fn locate(&self, args: &[RWord]) -> Verdict {
        let s = scan(args, "dln", &["database", "limit"]);
        let ci = s.shorts.contains(&'i') || s.long_prefix(&["ignore-case"], 3);
        let regex = s.shorts.contains(&'r') || s.long_prefix(&["regex", "regexp"], 3);
        let mut paths: Vec<String> = Vec::new();
        for r in &self.ctx.records {
            let dir = format!("/{}", r.join("/"));
            paths.extend(self.ctx.entries.iter().map(|e| format!("{dir}/{e}")));
            paths.push(dir);
        }
        for word in &s.positional {
            for alt in &word.alts {
                let alt = &as_pattern(alt);
                let globbed = alt.iter().any(|c| !matches!(c, C::Ch(_)));
                let test = if regex {
                    Test::Any
                } else if globbed {
                    Test::Glob(alt.clone(), ci)
                } else {
                    Test::Substr(alt_text(alt), ci)
                };
                if paths.iter().any(|p| test.fits(p)) {
                    return Err(word.raw.clone());
                }
            }
        }
        Ok(())
    }

    fn git(&self, args: &[RWord], cwds: &[Pat]) -> Verdict {
        let mut i = 0;
        while let Some(t) = args.get(i).map(RWord::text) {
            match t.as_str() {
                "-C" | "-c" | "--git-dir" | "--work-tree" | "--namespace" | "--exec-path"
                | "--config-env" | "--super-prefix" => i += 2,
                _ if t.starts_with('-') => i += 1,
                _ => break,
            }
        }
        let Some(sub) = args.get(i).map(RWord::text) else {
            return Ok(());
        };
        let rest = &args[i + 1..];
        let no_index = rest
            .iter()
            .take_while(|w| w.text() != "--")
            .any(|w| w.text() == "--no-index");
        if !(no_index && matches!(sub.as_str(), "grep" | "diff")) {
            return Ok(());
        }
        let s = scan(
            rest,
            "efmABCO",
            &[
                "regexp",
                "file",
                "max-count",
                "context",
                "after-context",
                "before-context",
                "max-depth",
                "threads",
                "open-files-in-pager",
                "relative",
                "src-prefix",
                "dst-prefix",
                "output",
            ],
        );
        let by_option = s.values.iter().any(|(k, _)| {
            k == "-e"
                || k == "-f"
                || (k.len() >= 2 && ["regexp", "file"].iter().any(|n| n.starts_with(k.as_str())))
        });
        let mut operands = s.positional;
        if sub == "grep" && !by_option && !operands.is_empty() {
            operands.remove(0);
        }
        self.deny_reaching(&or_dot(operands), cwds)
    }

    /// The wrappers before a command's program, the directories they move
    /// the command to, and a command line they split from one word.
    fn peel(&mut self, words: &[RWord]) -> Peeled {
        let mut p = 0;
        let mut out = Peeled::default();
        let text = |p: usize| words.get(p).map(RWord::text).unwrap_or_default();
        for _ in 0..8 {
            let Some(name) = words.get(p).and_then(RWord::lit).map(|t| basename(&t)) else {
                break;
            };
            match name.as_str() {
                "sudo" | "doas" => {
                    p += 1;
                    while text(p).starts_with('-') {
                        let t = text(p);
                        if t == "--" {
                            p += 1;
                            break;
                        }
                        p += if matches!(
                            t.as_str(),
                            "-u" | "-g" | "-C" | "-h" | "-p" | "-r" | "-t" | "-U" | "-T" | "-D"
                        ) {
                            2
                        } else {
                            1
                        };
                    }
                }
                "env" => {
                    p += 1;
                    loop {
                        let t = text(p);
                        if t.starts_with('-') {
                            match t.as_str() {
                                "-u" | "--unset" => p += 2,
                                "-C" | "--chdir" => {
                                    if let Some(dir) = words.get(p + 1) {
                                        out.cwds.push(dir.clone());
                                    }
                                    p += 2;
                                }
                                "-S" | "--split-string" => {
                                    let mut rest: Vec<String> = Vec::new();
                                    for w in words.iter().skip(p + 1) {
                                        rest.push(w.text());
                                    }
                                    out.split = Some(RWord::from_text(&rest.join(" ")));
                                    return out;
                                }
                                s if s.starts_with("-S") => {
                                    let mut rest = vec![s[2..].to_string()];
                                    for w in words.iter().skip(p + 1) {
                                        rest.push(w.text());
                                    }
                                    out.split = Some(RWord::from_text(&rest.join(" ")));
                                    return out;
                                }
                                "--" => {
                                    p += 1;
                                    break;
                                }
                                _ => p += 1,
                            }
                        } else if let Some(parts) = words.get(p).and_then(|w| w.parts.as_ref())
                            && let Some((name, value)) = assignment(parts)
                        {
                            let alts = self.resolve(value);
                            self.assign(name, alts);
                            p += 1;
                        } else {
                            break;
                        }
                    }
                }
                "nice" => {
                    p += 1;
                    while text(p).starts_with('-') {
                        p += if text(p) == "-n" { 2 } else { 1 };
                    }
                }
                "nohup" | "builtin" => p += 1,
                "command" | "time" => {
                    p += 1;
                    while text(p).starts_with('-') {
                        p += 1;
                    }
                }
                "exec" => {
                    p += 1;
                    while text(p).starts_with('-') {
                        p += if text(p) == "-a" { 2 } else { 1 };
                    }
                }
                "timeout" => {
                    p += 1;
                    while text(p).starts_with('-') {
                        p += if matches!(
                            text(p).as_str(),
                            "-s" | "-k" | "--signal" | "--kill-after"
                        ) {
                            2
                        } else {
                            1
                        };
                    }
                    p += 1;
                }
                "stdbuf" => {
                    p += 1;
                    while text(p).starts_with('-') {
                        p += if text(p).len() == 2 { 2 } else { 1 };
                    }
                }
                "ionice" => {
                    p += 1;
                    while text(p).starts_with('-') {
                        p += if matches!(text(p).as_str(), "-c" | "-n" | "-p") {
                            2
                        } else {
                            1
                        };
                    }
                }
                _ => break,
            }
        }
        out.at = p;
        out
    }
}

#[derive(Default)]
struct Peeled {
    at: usize,
    cwds: Vec<RWord>,
    split: Option<RWord>,
}

fn or_dot(mut roots: Vec<RWord>) -> Vec<RWord> {
    if roots.is_empty() {
        roots.push(dot());
    }
    roots
}

/// Adds the directories `dir` names (relative to the current ones).
fn extend_cwds(cwds: &mut Vec<Pat>, dir: &RWord) {
    let mut add = Vec::new();
    for alt in &dir.alts {
        add.extend(to_pats(alt, cwds));
    }
    for p in add {
        if !cwds.contains(&p) {
            cwds.push(p);
        }
    }
}

/// The program a pipeline stage runs, read from its literal words.
fn stage_program(stage: &Simple) -> String {
    let lit = |w: &Word| -> String {
        w.parts
            .iter()
            .map(|p| match p {
                Part::Ch(c) => *c,
                Part::BrOpen => '{',
                Part::BrClose => '}',
                _ => '\u{0}',
            })
            .collect()
    };
    let mut skip_options = false;
    for word in &stage.words {
        if assignment(&word.parts).is_some() {
            continue;
        }
        let t = lit(word);
        if RESERVED.contains(&t.as_str()) {
            continue;
        }
        if skip_options && t.starts_with('-') {
            continue;
        }
        let base = basename(&t);
        if matches!(
            base.as_str(),
            "sudo"
                | "doas"
                | "env"
                | "nice"
                | "nohup"
                | "command"
                | "builtin"
                | "exec"
                | "time"
                | "stdbuf"
                | "ionice"
        ) {
            skip_options = true;
            continue;
        }
        return base;
    }
    String::new()
}

/// Programs that read every file below a directory they are given, and
/// the operands they read; `None` for any other program.
fn recursive_operands(program: &str, args: &[RWord]) -> Option<Vec<RWord>> {
    let with_pattern = |s: Scan, pattern_opts: &[&str]| -> Vec<RWord> {
        let by_option = s.values.iter().any(|(k, _)| {
            pattern_opts.contains(&k.as_str())
                || (k.len() >= 3 && ["regexp", "file"].iter().any(|n| n.starts_with(k.as_str())))
        });
        let count = s.positional.len();
        let mut operands = s.positional;
        if count == 0 || (!by_option && count == 1) {
            operands.push(dot());
        }
        operands
    };
    match program {
        "grep" | "egrep" | "fgrep" | "zgrep" | "zegrep" | "zfgrep" => {
            let s = scan(
                args,
                "efmABCdD",
                &[
                    "regexp",
                    "file",
                    "max-count",
                    "label",
                    "context",
                    "after-context",
                    "before-context",
                    "directories",
                    "devices",
                    "exclude",
                    "include",
                    "exclude-from",
                    "exclude-dir",
                    "binary-files",
                    "group-separator",
                ],
            );
            let by_value = s.values.iter().any(|(k, v)| {
                let key = k.as_str();
                let value = v.text();
                (key == "-d" || (key.len() >= 2 && "directories".starts_with(key)))
                    && !value.is_empty()
                    && "recurse".starts_with(value.as_str())
            });
            let recursive = s.shorts.contains(&'r')
                || s.shorts.contains(&'R')
                || s.long_prefix(&["recursive", "dereference-recursive"], 3)
                || by_value;
            if !recursive {
                return None;
            }
            let by_option = s.values.iter().any(|(k, _)| {
                k == "-e"
                    || k == "-f"
                    || (k.len() >= 2
                        && ["regexp", "file"].iter().any(|n| n.starts_with(k.as_str())))
            });
            let mut operands = s.positional;
            if !by_option && !operands.is_empty() {
                operands.remove(0);
            }
            Some(or_dot(operands))
        }
        "ugrep" | "ug" => {
            let s = scan(
                args,
                "efmABCdDgtNKM",
                &[
                    "regexp",
                    "file",
                    "max-count",
                    "context",
                    "include",
                    "exclude",
                    "glob",
                    "file-type",
                    "max-depth",
                ],
            );
            let recursive = s.shorts.contains(&'r')
                || s.shorts.contains(&'R')
                || s.long_prefix(&["recursive", "dereference-recursive"], 3);
            recursive.then(|| with_pattern(s, &["-e", "-f"]))
        }
        "rg" | "ag" | "ack" | "ack-grep" => {
            let s = scan(
                args,
                "ABCEMTdefgjmrtG",
                &[
                    "glob",
                    "iglob",
                    "type",
                    "type-not",
                    "type-add",
                    "max-count",
                    "max-depth",
                    "regexp",
                    "file",
                    "replace",
                    "context",
                    "after-context",
                    "before-context",
                    "encoding",
                    "threads",
                    "max-filesize",
                    "sort",
                    "sortr",
                    "colors",
                    "color",
                    "path-separator",
                    "pre",
                    "pre-glob",
                    "engine",
                    "depth",
                    "ignore-dir",
                ],
            );
            Some(with_pattern(s, &["-e", "-f"]))
        }
        "diff" | "colordiff" => {
            let s = scan(
                args,
                "CUxXIFDSLW",
                &[
                    "exclude",
                    "exclude-from",
                    "ignore-matching-lines",
                    "label",
                    "starting-file",
                    "width",
                    "tabsize",
                    "line-format",
                    "from-file",
                    "to-file",
                ],
            );
            let recursive = s.shorts.contains(&'r') || s.long_prefix(&["recursive"], 3);
            recursive.then(|| {
                let mut operands = s.positional.clone();
                operands.extend(
                    s.values
                        .iter()
                        .filter(|(k, _)| {
                            k.len() >= 3
                                && ["from-file", "to-file"]
                                    .iter()
                                    .any(|n| n.starts_with(k.as_str()))
                        })
                        .map(|(_, v)| v.clone()),
                );
                operands
            })
        }
        "cp" | "scp" => {
            let s = scan(args, "tSPioFclJ", &["target-directory", "suffix"]);
            let recursive = s.shorts.iter().any(|c| matches!(c, 'r' | 'R' | 'a'))
                || s.long_prefix(&["recursive", "archive"], 3);
            recursive.then_some(s.positional)
        }
        "rsync" | "tar" | "bsdtar" | "gtar" | "zip" | "7z" | "7za" | "7zz" => {
            let s = scan(
                args,
                "fCTXbeB",
                &["directory", "file", "files-from", "exclude-from", "rsh"],
            );
            Some(s.positional)
        }
        _ => None,
    }
}

// ---------------------------------------------------------------- entry points

/// The word of `line` through which it reads the agent records in
/// `records`, `None` when it reads none. `homes` are the directories `~`
/// stands for, `cwd` the line's starting directory, `env` the environment
/// its unassigned variables come from, `entries` the records' file names.
pub(super) fn records_in_line(
    line: &str,
    records: &[PathBuf],
    homes: &[PathBuf],
    cwd: Option<&Path>,
    env: &dyn Fn(&str) -> Option<String>,
    entries: &[String],
) -> Option<String> {
    let records_comps: Vec<Vec<String>> = records.iter().map(|r| comps(r)).collect();
    let data: Vec<Vec<String>> = records_comps
        .iter()
        .filter(|r| !r.is_empty())
        .map(|r| r[..r.len() - 1].to_vec())
        .collect();
    let mut spell = Vec::new();
    for r in records {
        for form in spellings(r, homes) {
            let form = form.to_lowercase();
            if !spell.contains(&form) {
                spell.push(form);
            }
        }
        for home in homes {
            if let Ok(rel) = r.strip_prefix(home) {
                let form = format!("/{}", path_key(rel));
                if !spell.contains(&form) {
                    spell.push(form);
                }
            }
        }
    }
    let mut names: Vec<String> = KNOWN_ENTRIES
        .iter()
        .map(|e| path_key(Path::new(e)))
        .collect();
    for e in entries {
        let e = path_key(Path::new(e));
        if !names.contains(&e) {
            names.push(e);
        }
    }
    let ctx = Ctx {
        records: records_comps,
        data,
        spell,
        entries: names,
        homes: homes
            .iter()
            .map(|h| alt_of(h.to_string_lossy().trim_end_matches('/')))
            .collect(),
        env,
    };
    let start: Vec<Pat> = match cwd {
        Some(dir) => to_pats(&alt_of(&dir.to_string_lossy()), &[]),
        None => vec![vec![Tok::Deep]],
    };
    let mut parser = Parser::new(line, 0);
    let script = parser.script(false);
    if parser.too_deep {
        return Some(line.chars().take(80).collect());
    }
    let mut env = Env {
        ctx: &ctx,
        vars: HashMap::new(),
        cwds: start.clone(),
        seen: start,
        hits: Vec::new(),
        consumer: false,
        depth: 0,
    };
    if let Err(word) = env.eval_script(&script, false) {
        return Some(word);
    }
    if env.consumer {
        env.hits.first().cloned()
    } else {
        None
    }
}

/// The records lean-ctx keeps in its state directory: captured agent
/// session identifiers, the context and cost ledgers, the value
/// assessments and the dashboard's operator credential. The rest of the
/// state directory (events, logs, command output) stays readable.
pub(super) const STATE_RECORDS: &[&str] = &[
    "agent_runtime_env.json",
    "context_ledger.json",
    "ledger",
    "cost_attribution.json",
    "value_assessments.jsonl",
    "dashboard.token",
    // Earlier tool calls and commands, the paths they read, session ids,
    // task graphs.
    "tool-calls.log",
    "slow-commands.log",
    "heatmap.json",
    "feedback.json",
    "work-graphs",
];

/// The state directories lean-ctx may use: as this process resolves it
/// (`LEAN_CTX_STATE_DIR`, `XDG_STATE_HOME`), at the real
/// user's home and the platform's state directory.
fn state_bases() -> Vec<PathBuf> {
    let mut bases: Vec<PathBuf> = Vec::new();
    for dir in [
        lean_ctx::core::paths::state_dir_read_only().ok(),
        lean_ctx::core::config::shell_policy::real_user_home()
            .map(|home| home.join(".local").join("state").join("lean-ctx")),
        dirs::state_dir().map(|dir| dir.join("lean-ctx")),
    ]
    .into_iter()
    .flatten()
    {
        if !bases.contains(&dir) {
            bases.push(dir);
        }
    }
    bases
}

/// The agent records' directories: below every data directory lean-ctx
/// may use (as this process resolves it, at the real user's home, and the
/// platform's data directories), and below a config directory that holds
/// them; and the [`STATE_RECORDS`] in every state directory. Each also
/// with its symlinks resolved.
pub(super) fn record_dirs() -> Vec<PathBuf> {
    let mut bases: Vec<PathBuf> = Vec::new();
    if let Ok(dir) = lean_ctx::core::paths::data_dir_read_only() {
        bases.push(dir);
    }
    if let Some(home) = lean_ctx::core::config::shell_policy::real_user_home() {
        bases.push(home.join(".local").join("share").join("lean-ctx"));
        // The legacy and config locations hold the records only when the
        // data directory resolves there, i.e. when they already exist.
        for legacy in [
            home.join(".lean-ctx"),
            home.join(".config").join("lean-ctx"),
        ] {
            if legacy.join("agents").exists() {
                bases.push(legacy);
            }
        }
    }
    for dir in [dirs::data_dir(), dirs::data_local_dir()]
        .into_iter()
        .flatten()
    {
        bases.push(dir.join("lean-ctx"));
    }
    if let Ok(config) = lean_ctx::core::paths::config_dir_read_only()
        && config.join("agents").exists()
    {
        bases.push(config);
    }
    let mut out: Vec<PathBuf> = Vec::new();
    let records = bases.into_iter().map(|base| base.join("agents")).chain(
        state_bases().into_iter().flat_map(|base| {
            STATE_RECORDS
                .iter()
                .map(move |name| base.join(name))
                .collect::<Vec<_>>()
        }),
    );
    for dir in records {
        for candidate in [resolve_existing(&dir), dir] {
            if !out.contains(&candidate) {
                out.push(candidate);
            }
        }
    }
    out
}

/// The file names inside the records directories that exist.
fn record_entries(records: &[PathBuf]) -> Vec<String> {
    let mut out = Vec::new();
    for dir in records {
        if let Ok(read) = std::fs::read_dir(dir) {
            for entry in read.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if !out.contains(&name) {
                    out.push(name);
                }
            }
        }
    }
    out
}

/// The refusal for a word that names the agent records.
pub(super) fn records_refusal(word: &str) -> ShellError {
    format!(
        "[BLOCKED — DO NOT RETRY] `{word}` names lean-ctx's agent records, which are not \
         readable from an agent shell. Use `ctx_agent` to read your own directives."
    )
    .into()
}

/// Refuses a command line that reads lean-ctx's agent records (§37.4),
/// judged over the whole line: see the module documentation.
pub(crate) fn check_records_line(line: &str) -> Result<(), ShellError> {
    let records = record_dirs();
    let entries = record_entries(&records);
    let cwd = super::start_cwd();
    let env = |name: &str| std::env::var(name).ok();
    match records_in_line(line, &records, &homes(), cwd.as_deref(), &env, &entries) {
        Some(word) => Err(records_refusal(&word)),
        None => Ok(()),
    }
}

#[cfg(test)]
#[path = "records_guard_tests.rs"]
mod tests;
