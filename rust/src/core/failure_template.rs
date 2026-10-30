// SPDX-License-Identifier: Apache-2.0

//! Turns a tool's error message into a template that is safe to report.
//!
//! The approach follows crash-reporting scrubbers (Sentry's PII scrubbing,
//! VS Code's telemetry path cleaning), but is stricter: instead of removing
//! what looks sensitive, only LeanCTX's own static wording survives. A word
//! is kept only when it is in `vocabulary::VOCABULARY`, the words LeanCTX's
//! error messages are written in; everything else becomes a placeholder:
//!
//! - quoted text → `‹str›`, URLs → `‹url›`, e-mail addresses → `‹email›`
//! - a path, from its first segment through its end, spaces included, and
//!   file names → `‹path›`
//! - numbers and anything containing a digit → `‹n›`
//! - identifiers (camelCase, snake_case, `::`, long tokens) → `‹sym›`,
//!   except LeanCTX's own tool and parameter names
//! - any other word (not LeanCTX's wording, or not ASCII) → `‹text›`
//!
//! A template without plain words, or with characters outside a small safe
//! set, is dropped entirely; only the failure class is reported then.

use std::sync::OnceLock;

use regex::Regex;

#[path = "failure_vocabulary.rs"]
mod vocabulary;

/// Longest template that is reported.
pub const MAX_TEMPLATE_CHARS: usize = 160;
/// Input beyond this is not considered (the first line is what matters).
const MAX_INPUT_CHARS: usize = 400;

/// LeanCTX's own parameter names: kept verbatim because they explain errors.
const KNOWN_IDENTIFIERS: &[&str] = &[
    "old_string",
    "new_string",
    "replace_all",
    "file_path",
    "start_line",
    "end_line",
    "max_results",
    "dry_run",
    "old_text",
    "new_text",
    "replace_unique",
    "set_line",
    "replace_lines",
    "insert_after",
    "replace_symbol",
    "timeout_ms",
    "run_in_background",
    "job_id",
    "doc_id",
    "if_version",
];

/// Punctuation that frames a token without being part of it.
const EDGE: &[char] = &[
    '(', ')', '[', ']', '{', '}', ',', ';', ':', '.', '!', '?', '<', '>',
];

fn quoted() -> &'static Regex {
    static QUOTED: OnceLock<Regex> = OnceLock::new();
    QUOTED.get_or_init(|| {
        Regex::new(r#""[^"\n]{0,300}"|`[^`\n]{0,300}`|'[^'\s][^'\n]{0,300}'|«[^»\n]{0,300}»"#)
            .expect("static regex")
    })
}

fn is_known_identifier(token: &str) -> bool {
    token.starts_with("ctx_")
        && token[4..]
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b == b'_')
        || KNOWN_IDENTIFIERS.contains(&token)
}

fn is_vocabulary_word(word: &str) -> bool {
    let lower = word.to_ascii_lowercase();
    vocabulary::VOCABULARY
        .binary_search(&lower.as_str())
        .is_ok()
}

/// A plain word of LeanCTX's own wording: the word itself, its possessive
/// (`file's`), or a hyphenated compound of such words (`read-only`).
fn is_own_word(word: &str) -> bool {
    is_vocabulary_word(word)
        || word.strip_suffix("'s").is_some_and(is_vocabulary_word)
        || (word.contains('-')
            && word
                .split('-')
                .all(|part| !part.is_empty() && is_vocabulary_word(part)))
}

/// Unmistakably the start of a file-system path: absolute (`/`), home (`~`),
/// explicitly relative (`./`, `../`), a drive letter (`C:\`) or UNC share, or
/// separated segments with a dot (`src/main.rs`). Words joined by a slash
/// (`whitespace/indentation`) are not.
fn starts_path(core: &str) -> bool {
    if core.contains("://") {
        return false;
    }
    let bytes = core.as_bytes();
    let drive = bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'\\' | b'/');
    (core.len() > 1 && core.starts_with('/'))
        || core.starts_with('~')
        || ["./", "../", ".\\", "..\\", "\\\\"]
            .iter()
            .any(|prefix| core.starts_with(prefix))
        || drive
        || ((core.contains('/') || core.contains('\\')) && core.contains('.'))
}

/// `file.ext` with a short alphanumeric extension: main.rs, Cargo.toml.
fn is_file_name(core: &str) -> bool {
    match core.rsplit_once('.') {
        Some((stem, ext)) => {
            !stem.is_empty()
                && (1..=8).contains(&ext.len())
                && ext.bytes().all(|b| b.is_ascii_alphanumeric())
                && ext.bytes().any(|b| b.is_ascii_alphabetic())
        }
        None => false,
    }
}

fn is_identifier_like(token: &str) -> bool {
    let has_inner_upper = token.chars().skip(1).any(|c| c.is_ascii_uppercase());
    let has_lower = token.chars().any(|c| c.is_ascii_lowercase());
    token.contains("::")
        || token.contains('_')
        || token.contains('$')
        || token.contains('#')
        || token.contains('=')
        || (has_inner_upper && has_lower)
        || token.len() > 24
}

/// A path that reached `token` ends with it: the token closes a clause
/// (`: `, `, `, `; `, `. `, a closing bracket).
fn closes_path(token: &str) -> bool {
    token.ends_with([':', ',', ';', '.', ')', ']', '!', '?'])
}

/// The token after a path opens a new clause (` (`, ` [`, a placeholder, a
/// dash or an arrow), so the path ended before it.
fn opens_clause(token: &str) -> bool {
    token.starts_with(['(', '[', '{', '<', '‹'])
        || matches!(token, "—" | "–" | "-" | "--" | "->" | "=>" | "|")
}

/// A later part of a path with spaces in it: a segment with a separator that
/// is not LeanCTX's wording (`Notes/med`), or a file name (`list.md`).
fn continues_path(core: &str) -> bool {
    is_file_name(core)
        || ((core.contains('/') || core.contains('\\'))
            && !core
                .split(['/', '\\'])
                .all(|part| !part.is_empty() && is_own_word(part)))
}

/// The index of the last token of a path that starts at `tokens[start]`, or
/// `None` when no path starts there. Within the clause the path starts in
/// (up to `: `, `, `, `. `, ` (` or the end of the line), the path runs
/// through spaces to its last segment or file name, so `/Users/x/My Health
/// Notes/med list.md is outside` ends at `list.md` and keeps the wording after
/// it. Words of a path that are left over (`My Notes` in `/x/My Notes`) are
/// not LeanCTX's wording and become placeholders of their own.
fn path_span(tokens: &[&str], start: usize) -> Option<usize> {
    let core = tokens[start].trim_matches(EDGE);
    if core.starts_with('‹') || !starts_path(core) {
        return None;
    }
    let mut end = start;
    let mut probe = start;
    while !closes_path(tokens[probe])
        && tokens
            .get(probe + 1)
            .is_some_and(|next| !opens_clause(next))
    {
        probe += 1;
        if continues_path(tokens[probe].trim_matches(EDGE)) {
            end = probe;
        }
    }
    Some(end)
}

/// `‹path›` framed by the opening punctuation of the path's first token and
/// the closing punctuation of its last one. Dots right before a separator
/// (`./`, `../`) belong to the path, not to the punctuation.
fn path_placeholder(first: &str, last: &str) -> String {
    let rest = first.trim_start_matches(EDGE);
    let mut lead = &first[..first.len() - rest.len()];
    if rest.starts_with(['/', '\\']) {
        lead = lead.trim_end_matches('.');
    }
    let trail = &last[last.trim_end_matches(EDGE).len()..];
    format!("{lead}‹path›{trail}")
}

/// Classify one whitespace-separated token, keeping its outer punctuation.
fn scrub_token(token: &str) -> String {
    // A dash between clauses is punctuation, not a word.
    if matches!(token, "—" | "–") {
        return "-".to_string();
    }
    let core = token.trim_matches(EDGE);
    if core.is_empty() {
        return token.to_string();
    }
    let start = token.find(core).unwrap_or(0);
    let (lead, trail) = (&token[..start], &token[start + core.len()..]);
    let replaced = if core.starts_with('‹') && core.ends_with('›') {
        core.to_string()
    } else if !core.is_ascii() {
        "‹text›".to_string()
    } else if core.contains("://") || core.starts_with("www.") {
        "‹url›".to_string()
    } else if core.contains('@') && core.contains('.') {
        "‹email›".to_string()
    } else if is_known_identifier(core) {
        core.to_string()
    } else if starts_path(core) || is_file_name(core) {
        "‹path›".to_string()
    } else if core.bytes().any(|b| b.is_ascii_digit()) {
        "‹n›".to_string()
    } else if is_identifier_like(core) {
        "‹sym›".to_string()
    } else if core
        .bytes()
        .all(|b| b.is_ascii_alphabetic() || b == b'-' || b == b'\'')
    {
        if is_own_word(core) {
            core.to_string()
        } else {
            "‹text›".to_string()
        }
    } else if core.contains('/') || core.contains('\\') {
        // `whitespace/indentation` is wording; `docs/records` is a path.
        let own = core
            .split(['/', '\\'])
            .all(|part| !part.is_empty() && is_own_word(part));
        if own {
            core.to_string()
        } else {
            "‹path›".to_string()
        }
    } else {
        "‹sym›".to_string()
    };
    // `./x` and `../x`: the dots are part of the path, not punctuation.
    let lead = if replaced == "‹path›" && core.starts_with(['/', '\\']) {
        lead.trim_end_matches('.')
    } else {
        lead
    };
    format!("{lead}{replaced}{trail}")
}

/// The reportable template of an error message, or `None` when nothing safe
/// and meaningful is left.
pub fn message_template(message: &str) -> Option<String> {
    let line = message
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())?;
    let line: String = line.chars().take(MAX_INPUT_CHARS).collect();
    let unquoted = quoted().replace_all(&line, " ‹str› ");
    let tokens: Vec<&str> = unquoted.split_whitespace().collect();
    let mut words: Vec<String> = Vec::new();
    let mut index = 0;
    while index < tokens.len() {
        let scrubbed = if let Some(end) = path_span(&tokens, index) {
            let placeholder = path_placeholder(tokens[index], tokens[end]);
            index = end + 1;
            placeholder
        } else {
            index += 1;
            scrub_token(tokens[index - 1])
        };
        // Collapse runs of the same placeholder ("‹n› ‹n›" → "‹n›").
        if scrubbed.starts_with('‹') && words.last() == Some(&scrubbed) {
            continue;
        }
        words.push(scrubbed);
    }
    let mut template = words.join(" ");
    if template.chars().count() > MAX_TEMPLATE_CHARS {
        let cut: String = template.chars().take(MAX_TEMPLATE_CHARS - 1).collect();
        template = match cut.rfind(' ') {
            Some(space) if space > MAX_TEMPLATE_CHARS / 2 => format!("{}…", &cut[..space]),
            _ => format!("{cut}…"),
        };
    }
    let safe = template
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || " .,:;()[]{}<>!?'-_/‹›…".contains(c));
    let words = template
        .split_whitespace()
        .filter(|word| {
            word.chars().filter(char::is_ascii_alphabetic).count() >= 2 && !word.contains('‹')
        })
        .count();
    (safe && words >= 2).then_some(template)
}

#[cfg(test)]
#[path = "failure_template_tests.rs"]
mod tests;
