// SPDX-License-Identifier: Apache-2.0

//! Partial runs of a `;`/newline command list (owner decision 2026-10-29).
//!
//! A blocked inline script used to reject the whole call, so an agent that
//! chained `pytest …; python3 -c …` lost the test run too and repeated it.
//! When the line is a plain list of independent statements and every blocked
//! statement is a plain inline-interpreter call, the other statements run and
//! each skipped one is named in one short line.
//!
//! Narrow by construction (security review 2026-10-29). Nothing runs that the
//! gate would not run as a command of its own: the statements come from the
//! gate's own scanner, and the joined statements that will run are checked
//! again, as one command, by the full enforcer. A skipped statement must be a
//! bare interpreter invocation — one command, no `&&`/`||`/pipe/`&`, no
//! unquoted redirect, substitution, group or separator, no assignment prefix —
//! so skipping it cannot change what a later statement does. The whole call
//! stays rejected for lines with comments (comment text must never become a
//! command), heredocs, line continuations, ambiguous line breaks, lexer
//! problems in the line or any statement, control structures, and any use of
//! the words that change how later statements run (`exit`, `set`, `command`,
//! `eval`, `trap` …).

use super::enforcement::{INTERPRETER_COMMANDS, enforce_with};
use super::lexer::lex;
use super::mode::ShellSecurity;
use super::tokenizer::{
    extract_base_from_segment, segments_with_separators, sequence_statements, skip_env_assignments,
};

/// The statements that may run, and one note per skipped statement.
#[derive(Debug, PartialEq, Eq)]
pub struct PartialRun {
    /// The allowed statements, in order, one per line.
    pub run: String,
    /// `skipped: <statement> (<reason>)`, one per blocked statement.
    pub skipped: Vec<String>,
}

/// Words that open or continue a compound command: cutting a list that
/// contains one at `;` would break the construct apart.
const RESERVED: &[&str] = &[
    "if", "then", "elif", "else", "fi", "for", "while", "until", "do", "done", "case", "esac",
    "select", "function", "time", "coproc", "!", "{", "}", "[[",
];

/// Words that change how the rest of the line runs or ends (error modes,
/// early exits, wrappers that hide the real command, traps): a line using one
/// anywhere is never cut.
const LINE_CONTROL: &[&str] = &[
    "exit", "return", "set", "shopt", "command", "builtin", "eval", "exec", "source", ".", "trap",
    "logout",
];

/// Plans a partial run for `command`, which the gate refused as a whole.
/// `None` keeps the whole-call rejection.
#[must_use]
pub fn partial_run(command: &str, granted: &[String]) -> Option<PartialRun> {
    if ShellSecurity::resolve() != ShellSecurity::Enforce || cfg!(windows) {
        return None;
    }
    if command.contains("<<")
        || command.contains(['\r', '\u{2028}', '\u{2029}'])
        || command.contains("\\\n")
        || !lexes_clean(command)
    {
        return None;
    }
    let statements = sequence_statements(command);
    if statements.len() < 2 {
        return None;
    }
    let mut run = Vec::new();
    let mut skipped = Vec::new();
    for statement in statements {
        if !lexes_clean(statement) {
            return None;
        }
        let segments = segments_with_separators(statement);
        if segments.iter().any(|(segment, _)| controls_line(segment)) {
            return None;
        }
        match enforce_with(statement, granted, None) {
            Ok(()) => run.push(statement),
            Err(error) => {
                if !is_bare_interpreter_call(statement, segments.len()) {
                    return None;
                }
                skipped.push(format!(
                    "skipped: {} ({})",
                    preview(statement),
                    short_reason(error.message())
                ));
            }
        }
    }
    if run.is_empty() || skipped.is_empty() {
        return None;
    }
    let run = run.join("\n");
    // What runs is checked once more as one command: no context-dependent
    // rule (a `cd` before a command, a pipe into an interpreter) is lost by
    // judging the statements one at a time.
    enforce_with(&run, granted, None).ok()?;
    Some(PartialRun { run, skipped })
}

/// No comment (its text must never be re-joined into a command line) and no
/// construct the shells read differently.
fn lexes_clean(text: &str) -> bool {
    let lexed = lex(text);
    lexed.problem.is_none() && lexed.comments.is_empty()
}

fn first_word(segment: &str) -> &str {
    let rest = skip_env_assignments(segment.trim());
    rest.split_whitespace().next().unwrap_or("")
}

fn controls_line(segment: &str) -> bool {
    let word = first_word(segment);
    RESERVED.contains(&word)
        || word.starts_with('(')
        || word.starts_with('{')
        || LINE_CONTROL.contains(&extract_base_from_segment(segment).as_str())
}

/// One interpreter command and nothing else: no operator, no assignment
/// prefix, and no unquoted redirect, substitution, group or separator.
fn is_bare_interpreter_call(statement: &str, segment_count: usize) -> bool {
    let trimmed = statement.trim();
    segment_count == 1
        && skip_env_assignments(trimmed) == trimmed
        && !has_unquoted_meta(trimmed)
        && INTERPRETER_COMMANDS.contains(&extract_base_from_segment(trimmed).as_str())
}

/// An unquoted shell metacharacter that could redirect, substitute, group or
/// separate. Quoted text is data to the skipped command, which never runs.
fn has_unquoted_meta(text: &str) -> bool {
    let mut chars = text.chars();
    let (mut single, mut double) = (false, false);
    while let Some(c) = chars.next() {
        match c {
            '\'' if !double => single = !single,
            '"' if !single => double = !double,
            '\\' if !single => {
                chars.next();
            }
            '<' | '>' | '|' | '&' | ';' | '(' | ')' | '$' | '`' | '{' | '}'
                if !single && !double =>
            {
                return true;
            }
            _ => {}
        }
    }
    single || double
}

/// The statement on one line, at most 60 characters.
fn preview(statement: &str) -> String {
    let flat = statement.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= 60 {
        return flat;
    }
    let cut: String = flat.chars().take(57).collect();
    format!("{cut}…")
}

/// The first sentence of a block message, without its marker.
fn short_reason(message: &str) -> String {
    let first = message.lines().next().unwrap_or_default();
    let reason = first
        .strip_prefix("[BLOCKED — DO NOT RETRY] ")
        .or_else(|| first.strip_prefix("[BLOCKED] "))
        .unwrap_or(first);
    let reason = reason.split(". ").next().unwrap_or(reason);
    reason.trim_end_matches('.').to_string()
}
