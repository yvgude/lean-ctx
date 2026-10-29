// SPDX-License-Identifier: Apache-2.0
//! Text rules of the capture gate (SPEC-ENGINE §3.1.4, G2–G9): one
//! complete, single-line claim of 24–300 characters that is not log output,
//! not truncated and not a list. Rules reject; they never rewrite.

use std::sync::LazyLock;

use regex::Regex;

use super::ReasonCode;

pub const MIN_STATEMENT_CHARS: usize = 24;
pub const MAX_STATEMENT_CHARS: usize = 300;

/// Abbreviations whose dot never ends a sentence (G4).
const ABBREVIATIONS: [&str; 6] = ["e.g.", "i.e.", "vs.", "etc.", "approx.", "no."];

static LOG_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}",
        r"\[exit:",
        r"\x1b\[",
        r"(?m)^\s*at \S+\(",
        r#"File ".*", line \d+"#,
        r"^\s*[{\[]",
        r"```",
    ]
    .iter()
    .map(|p| Regex::new(p).expect("static log pattern"))
    .collect()
});

static LIST_MARKER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*([-*•]|\d+[.)])\s").expect("static list pattern"));

/// G2–G9 on a claim statement (already trimmed).
pub fn check_statement(statement: &str) -> Result<(), ReasonCode> {
    check_length(statement, MIN_STATEMENT_CHARS, MAX_STATEMENT_CHARS)?;
    check_single_line(statement)?;
    check_list(statement)?;
    check_log(statement)?;
    if sentence_count(statement) > 2 {
        return Err(ReasonCode::MultiClaim);
    }
    check_sentence(statement, true)?;
    check_truncated(statement)?;
    check_path_share(statement)
}

/// G2 with the given bounds (Unicode scalar values).
pub fn check_length(text: &str, min: usize, max: usize) -> Result<(), ReasonCode> {
    let n = text.chars().count();
    if n < min {
        Err(ReasonCode::TooShort)
    } else if n > max {
        Err(ReasonCode::TooLong)
    } else {
        Ok(())
    }
}

/// G3.
pub fn check_single_line(text: &str) -> Result<(), ReasonCode> {
    if text.contains(['\n', '\r']) {
        Err(ReasonCode::MultiLine)
    } else {
        Ok(())
    }
}

/// G8.
pub fn check_list(text: &str) -> Result<(), ReasonCode> {
    if LIST_MARKER.is_match(text) {
        Err(ReasonCode::MultiClaim)
    } else {
        Ok(())
    }
}

/// G6: timestamps, exit markers, ANSI, stack frames, JSON, tables, fences.
pub fn check_log(text: &str) -> Result<(), ReasonCode> {
    if LOG_PATTERNS.iter().any(|re| re.is_match(text)) || text.matches('|').count() >= 3 {
        Err(ReasonCode::LogText)
    } else {
        Ok(())
    }
}

/// G4: sentences end at `. ! ?` followed by a space and an uppercase letter,
/// except after a listed abbreviation.
pub fn sentence_count(text: &str) -> usize {
    let chars: Vec<char> = text.chars().collect();
    let mut count = 1;
    for (i, c) in chars.iter().enumerate() {
        if !matches!(c, '.' | '!' | '?') {
            continue;
        }
        let next_upper =
            chars.get(i + 1) == Some(&' ') && chars.get(i + 2).is_some_and(|c| c.is_uppercase());
        if !next_upper {
            continue;
        }
        let head: String = chars[..=i].iter().collect();
        let word = head.rsplit(' ').next().unwrap_or_default().to_lowercase();
        if !ABBREVIATIONS.contains(&word.as_str()) {
            count += 1;
        }
    }
    count
}

/// G5: starts with an uppercase letter, digit or backtick; ends with
/// `. ! ?` (optionally followed by a closing backtick) when `punctuated`;
/// has a word of three or more letters that is not part of a path.
pub fn check_sentence(text: &str, punctuated: bool) -> Result<(), ReasonCode> {
    let first = text.chars().next().ok_or(ReasonCode::NotSentence)?;
    if !(first.is_uppercase() || first.is_ascii_digit() || first == '`') {
        return Err(ReasonCode::NotSentence);
    }
    if punctuated {
        let body = text.strip_suffix('`').unwrap_or(text);
        if !body.ends_with(['.', '!', '?']) {
            return Err(ReasonCode::NotSentence);
        }
    }
    let has_word = text
        .split_whitespace()
        .filter(|t| !is_path_like(t))
        .any(|t| t.chars().filter(|c| c.is_alphabetic()).count() >= 3);
    if has_word {
        Ok(())
    } else {
        Err(ReasonCode::NotSentence)
    }
}

/// G7: no trailing ellipsis, dash, comma or open parenthesis; balanced
/// backticks and brackets.
pub fn check_truncated(text: &str) -> Result<(), ReasonCode> {
    if text.ends_with(['…', '-', ',', '(']) || text.ends_with("...") {
        return Err(ReasonCode::Truncated);
    }
    if !text.matches('`').count().is_multiple_of(2) {
        return Err(ReasonCode::Truncated);
    }
    let mut depth: Vec<char> = Vec::new();
    for c in text.chars() {
        match c {
            '(' | '[' | '{' => depth.push(c),
            ')' | ']' | '}' => {
                let open = match c {
                    ')' => '(',
                    ']' => '[',
                    _ => '{',
                };
                if depth.pop() != Some(open) {
                    return Err(ReasonCode::Truncated);
                }
            }
            _ => {}
        }
    }
    if depth.is_empty() {
        Ok(())
    } else {
        Err(ReasonCode::Truncated)
    }
}

/// A token that reads as a path: contains `/` or `\`, or `::`.
fn is_path_like(token: &str) -> bool {
    let t = token.trim_matches(|c: char| matches!(c, '`' | '"' | '\'' | ',' | '.' | '(' | ')'));
    t.contains('/') || t.contains('\\') || t.contains("::")
}

/// G9: characters inside path-like tokens are at most 40 % of the text.
pub fn check_path_share(text: &str) -> Result<(), ReasonCode> {
    let total = text.chars().filter(|c| !c.is_whitespace()).count();
    let paths: usize = text
        .split_whitespace()
        .filter(|t| is_path_like(t))
        .map(|t| t.chars().count())
        .sum();
    if total > 0 && paths * 100 > total * 40 {
        Err(ReasonCode::LogText)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_clean_claim_passes() {
        for ok in [
            "The outbox retries a refused push after the lease is renewed.",
            "Builds use the shared target, e.g. for worktrees. They never clean it.",
            "`cargo fast` builds a development binary without LTO.",
            "3 retries are enough for the relay handshake on slow links.",
        ] {
            assert_eq!(check_statement(ok), Ok(()), "{ok}");
        }
    }

    #[test]
    fn each_rule_names_its_reason() {
        let cases = [
            ("Too short.", ReasonCode::TooShort),
            (
                &*format!("{}.", "A very long claim ".repeat(20)),
                ReasonCode::TooLong,
            ),
            (
                "The relay drops frames.\nIt also logs them twice.",
                ReasonCode::MultiLine,
            ),
            (
                "The relay drops frames. It logs twice. It never retries.",
                ReasonCode::MultiClaim,
            ),
            (
                "- The relay drops frames on slow links.",
                ReasonCode::MultiClaim,
            ),
            (
                "the relay drops frames on slow links.",
                ReasonCode::NotSentence,
            ),
            (
                "The relay drops frames on slow links",
                ReasonCode::NotSentence,
            ),
            (
                "Build failed at 2026-10-24T14:02 with code 101.",
                ReasonCode::LogText,
            ),
            (
                "Command ended with [exit:1] after the build.",
                ReasonCode::LogText,
            ),
            (
                "Columns are | a | b | c in the report table.",
                ReasonCode::LogText,
            ),
            (
                "The relay drops frames when the (link is slow.",
                ReasonCode::Truncated,
            ),
            (
                "Run `cargo test and then check the output now.",
                ReasonCode::Truncated,
            ),
            (
                "See rust/src/core/hive/outbox.rs and rust/src/core/hive/capture/mod.rs.",
                ReasonCode::LogText,
            ),
        ];
        for (text, reason) in cases {
            assert_eq!(check_statement(text), Err(reason), "{text}");
        }
    }

    #[test]
    fn truncated_endings_are_refused() {
        for t in [
            "Then the relay…",
            "Then the relay...",
            "Then the relay -",
            "Then the relay,",
        ] {
            assert_eq!(check_truncated(t), Err(ReasonCode::Truncated), "{t}");
        }
    }

    #[test]
    fn abbreviations_do_not_split_sentences() {
        assert_eq!(
            sentence_count("Use a lease, e.g. Hive gives one. Then renew."),
            2
        );
        assert_eq!(sentence_count("Compare A vs. B before merging."), 1);
    }
}
