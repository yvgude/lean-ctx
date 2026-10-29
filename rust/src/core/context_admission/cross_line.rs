// SPDX-License-Identifier: Apache-2.0

//! What an admission rule can match across a line end — the one derivation
//! shared by everything that admits a text in pieces cut at line ends: the
//! gateway's chunked scan ([`super::detectors`]) and the unit reader of a
//! transcript above the read cap (`tools::ctx_read`, `core::import::bounded`).
//!
//! BUILT-IN rules. Every one is line-local (its match, or at least the
//! existence of its match, depends on one line only) except:
//!
//! | rule | crosses a line end by | reach |
//! |---|---|---|
//! | phone candidate, `input_filters::pii` — validation reads 24 characters before and after the match | its context | bounded: under [`BOUNDED_REACH_BYTES`] |
//! | classification field, `input_filters::classification` — `^\s*key\s*[:=]\s*(.+)$` | white space between key, separator and value | NOT bounded |
//! | private-key block mask, `core::redaction` — `(?s)BEGIN … .+? … END` | anything | NOT bounded |
//!
//! A piece boundary is safe for a bounded rule when the text within its reach
//! on both sides is judged together (a seam). For a rule without a bound
//! only a boundary outside any possible match is safe
//! ([`ends_inside_classification_field`]; the private-key block is applied to
//! whole texts by the gateway and is checked where units are admitted).
//!
//! CONFIGURED rules (policy-pack patterns, `secret_detection.custom_patterns`)
//! are free regular expressions with no bound on what they match. One is
//! applied to pieces only when it is PROVEN unable to consume a line feed
//! ([`pattern_is_single_line`]); any other is applied to the whole text, or
//! the text is not admitted in pieces at all.

use std::sync::OnceLock;

use regex::Regex;

/// The longest reach of a built-in rule across a line end that HAS a bound,
/// in bytes on one side of the line end.
///
/// - Phone candidate: the match (no line end inside; under 100 bytes even
///   with non-ASCII digits) plus 24 characters of context (at most 96 bytes)
///   on the side that crosses the line end.
///
/// 256 covers it with room to spare.
pub(crate) const BOUNDED_REACH_BYTES: usize = 256;

/// Whether `text` ends INSIDE a possible classification field: after the key
/// (`classification`, `sensitivity`, `data class`) at the start of a line, or
/// after the key and its `:` / `=`, with nothing but white space — any
/// amount, line ends included — behind it. The value the rule would read is
/// then in the text that follows.
pub(crate) fn ends_inside_classification_field(text: &str) -> bool {
    static KEY: OnceLock<Regex> = OnceLock::new();
    let mut open = text.trim_end();
    if let Some(key) = open.strip_suffix([':', '=']) {
        open = key.trim_end();
    }
    let line = open.rsplit('\n').next().unwrap_or(open).trim_start();
    KEY.get_or_init(|| {
        Regex::new(r"(?i)^(?:classification|sensitivity|data[ _-]?class)$")
            .expect("valid classification key pattern")
    })
    .is_match(line)
}

/// Whether `pattern` (regex-crate syntax) is PROVEN unable to consume a line
/// feed, so every match lies inside one line. Conservative: `false` for
/// anything not proven — such a pattern counts as multi-line and unbounded.
///
/// Refused, because each can match `\n` or hide something that can: a literal
/// control character; any escape other than `\d \w \S \b \B \A \z`, an
/// escaped punctuation character, or `\t` not used as the start of a range;
/// a negated class `[^…]`; a POSIX class `[:…:]`; a flag group that sets `s`.
/// What remains — literals, `.` without `s`, positive classes of literals
/// and the escapes above, anchors, groups, repetition — cannot match `\n`:
/// a range reaches it only from a start below it, and those are control
/// characters.
pub(crate) fn pattern_is_single_line(pattern: &str) -> bool {
    if pattern.contains("[:") && pattern.contains(":]") {
        return false;
    }
    let mut chars = pattern.chars().peekable();
    while let Some(character) = chars.next() {
        match character {
            '\\' => match chars.next() {
                Some('d' | 'w' | 'S' | 'b' | 'B' | 'A' | 'z') => {}
                Some('t') if chars.peek() != Some(&'-') => {}
                Some(escaped) if escaped.is_ascii_punctuation() => {}
                _ => return false,
            },
            '[' if chars.peek() == Some(&'^') => return false,
            '(' if chars.peek() == Some(&'?') => {
                chars.next();
                // A named group has no flags; any other `(?` lists flags up
                // to `:` or `)`.
                if !matches!(chars.peek(), Some('P' | '<')) {
                    loop {
                        match chars.next() {
                            Some('s') | None => return false,
                            Some(':' | ')') => break,
                            Some(_) => {}
                        }
                    }
                }
            }
            character if character.is_control() => return false,
            _ => {}
        }
    }
    true
}
