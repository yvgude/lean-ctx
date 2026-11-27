// SPDX-License-Identifier: Apache-2.0

//! Governor rule R6 for native Grep results.
//!
//! Under the governor, `hook rewrite-enforce` is security only (owner
//! decision O-1): it never rewrites a native Grep, so output shaping has to
//! happen here, after the tool. Before R6, `hook govern` had no Grep arm and
//! its PostToolUse matcher did not name Grep, so no Grep result was ever
//! shaped (Seamit bench: 63 uncompressed Grep calls in one session).
//!
//! Claude Code's Grep result is `{mode, numFiles, filenames, content,
//! numLines}`. Only `mode = "content"` carries the per-line `path:N:text`
//! (match) and `path-N-text` (context) lines; R6 regroups them in ripgrep's
//! `--heading` layout — each path once, then its `N:` / `N-` lines, a blank
//! line between files. The bytes stay recoverable exactly (every view line is
//! a suffix of exactly one original line, and the path heads its group); any
//! line that does not parse as a match, a context line of a path the output
//! names, or a `--` hunk separator refuses the whole rewrite.
//!
//! A line's text may itself contain `:N:` or `-N-` (`src/a.rs-9-// at
//! 10:15:30`), so the parse refuses rather than guesses: every line must have
//! exactly one reading against the known paths, no known path may look like
//! another one plus `-N-`, and a file's lines must be contiguous (a path that
//! reappears after its group closed refuses). R6 needs line numbers, so a
//! Grep run with `-n: false` is never touched.

use super::{Decision, MIN_SAVED_TOKENS, Policy, RuleMode};
use crate::hook_handlers::read_dedup;
use lean_ctx::core::tokens::count_tokens;

/// Below this size the repeated paths cost too little to regroup.
const R6_MIN_TOKENS: usize = 400;
/// The heading view must save at least this share of the original.
const R6_MIN_SAVED_SHARE: f64 = 0.2;

pub(super) fn decide(v: &serde_json::Value, policy: &Policy, session_id: &str) -> Decision {
    if policy.r6 == RuleMode::Off {
        return Decision::default();
    }
    let Some(resp) = v.get("tool_response").and_then(|r| r.as_object()) else {
        return Decision::default();
    };
    let line_numbers_off = v
        .get("tool_input")
        .and_then(|i| i.get("-n"))
        .and_then(serde_json::Value::as_bool)
        == Some(false);
    if line_numbers_off {
        return Decision::default();
    }
    if resp
        .get("mode")
        .is_some_and(|m| m.as_str() != Some("content"))
    {
        return Decision::default();
    }
    let Some(content) = resp.get("content").and_then(|c| c.as_str()) else {
        return Decision::default();
    };
    let baseline = count_tokens(content);
    if baseline < R6_MIN_TOKENS {
        return Decision::default();
    }
    let Some(view) = heading_view(content) else {
        return Decision::default();
    };
    let actual = count_tokens(&view);
    let saved = baseline.saturating_sub(actual);
    if saved < MIN_SAVED_TOKENS || (saved as f64) < baseline as f64 * R6_MIN_SAVED_SHARE {
        return Decision::default();
    }
    let mut updated = serde_json::Value::Object(resp.clone());
    updated["content"] = serde_json::Value::String(view.clone());
    let output = read_dedup::post_tool_use_output(&updated);
    Decision::from_rule(policy.r6, output, "R6", baseline, actual, session_id).with_view(view)
}

/// One parsed result line: the path it belongs to (`None` for a `--` hunk
/// separator) and the part shown under the path's heading.
struct Line<'a> {
    path: Option<&'a str>,
    entry: &'a str,
}

/// Byte offsets `i` where `line[i..]` starts with `<sep><digits><sep>`.
fn numbered_seps(line: &str, sep: u8) -> impl Iterator<Item = usize> + '_ {
    let bytes = line.as_bytes();
    (1..bytes.len()).filter(move |&i| {
        if bytes[i] != sep {
            return false;
        }
        let digits = bytes[i + 1..]
            .iter()
            .take_while(|b| b.is_ascii_digit())
            .count();
        digits > 0 && bytes.get(i + 1 + digits) == Some(&sep)
    })
}

/// `path:N:text` → `path`, split at the first `:<digits>:`.
fn first_match_path(line: &str) -> Option<&str> {
    numbered_seps(line, b':').next().map(|i| &line[..i])
}

/// The one reading of `line` against the known paths: a match `path:N:text`
/// or a context line `path-N-text`, returned as (`path`, `N…text`). `None`
/// when no reading or more than one fits — R6 refuses instead of guessing.
fn parse_line<'a>(line: &'a str, paths: &[&str]) -> Option<(&'a str, &'a str)> {
    let mut readings = numbered_seps(line, b':')
        .chain(numbered_seps(line, b'-'))
        .filter(|&i| paths.binary_search(&&line[..i]).is_ok());
    let i = readings.next()?;
    if readings.next().is_some() {
        return None;
    }
    Some((&line[..i], &line[i + 1..]))
}

/// Whether `path` is another known path followed by `-N-` — the shape a
/// context line whose text contains `:N:` gives its fake match path.
fn looks_like_context(path: &str, paths: &[&str]) -> bool {
    numbered_seps(path, b'-').any(|i| paths.binary_search(&&path[..i]).is_ok())
}

/// The `--heading` view of a Grep content result, or `None` when a line does
/// not parse (the original then passes untouched).
pub(super) fn heading_view(content: &str) -> Option<String> {
    let (body, trailing_newline) = match content.strip_suffix('\n') {
        Some(b) => (b, true),
        None => (content, false),
    };
    let raw: Vec<&str> = body.split('\n').collect();
    let mut paths: Vec<&str> = raw.iter().filter_map(|l| first_match_path(l)).collect();
    paths.sort_unstable();
    paths.dedup();
    if paths.is_empty() || paths.iter().any(|p| looks_like_context(p, &paths)) {
        return None;
    }
    let mut lines = Vec::with_capacity(raw.len());
    for l in &raw {
        let parsed = if *l == "--" {
            Line {
                path: None,
                entry: l,
            }
        } else if let Some((path, entry)) = parse_line(l, &paths) {
            Line {
                path: Some(path),
                entry,
            }
        } else {
            return None;
        };
        lines.push(parsed);
    }
    let mut out = String::with_capacity(content.len());
    let mut current: Option<&str> = None;
    let mut closed: Vec<&str> = Vec::new();
    for line in &lines {
        if let Some(path) = line.path
            && current != Some(path)
        {
            if closed.contains(&path) {
                // Not ripgrep's per-file order: a second group would split
                // the file, so the view could mislead.
                return None;
            }
            if let Some(prev) = current {
                closed.push(prev);
                out.push_str("\n\n");
            }
            out.push_str(path);
            current = Some(path);
        } else if current.is_none() {
            // A leading separator has no heading to sit under.
            return None;
        }
        out.push('\n');
        out.push_str(line.entry);
    }
    if trailing_newline {
        out.push('\n');
    }
    Some(out)
}

/// Inverse of [`heading_view`], for the losslessness tests.
#[cfg(test)]
pub(super) fn expand(view: &str) -> String {
    let (body, trailing_newline) = match view.strip_suffix('\n') {
        Some(b) => (b, true),
        None => (view, false),
    };
    let mut out = Vec::new();
    let mut heading: Option<&str> = None;
    for l in body.split('\n') {
        match heading {
            None => heading = Some(l),
            Some(_) if l.is_empty() => heading = None,
            Some(_) if l == "--" => out.push(l.to_string()),
            Some(path) => {
                let digits = l.bytes().take_while(u8::is_ascii_digit).count();
                let sep = l[digits..].chars().next().unwrap_or(':');
                out.push(format!("{path}{sep}{l}"));
            }
        }
    }
    let mut s = out.join("\n");
    if trailing_newline {
        s.push('\n');
    }
    s
}
