// SPDX-License-Identifier: Apache-2.0
//! `ctx_read(symbol=...)`: one function/class by name, located by its
//! tree-sitter span. It replaces a `full` read or a run of scrolling `lines:`
//! windows with a single call that returns exactly the symbol's lines.
//!
//! The symbol only picks the line window; content, admission, redaction and
//! caching stay on the ordinary `lines:` / `anchored:` path.

use crate::core::signatures::{SigBackend, extract_signatures_with_backend};

/// The read a `symbol` request turns into.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum SymbolRead {
    /// Serve this mode (`lines:S-E[,S-E…]` or `anchored:S-E`); `note` names
    /// the matches an `anchored` window could not include.
    Window { mode: String, note: Option<String> },
    /// The name is not a symbol of this file: serve the outline instead, so
    /// the caller gets the available names in the same call.
    NotFound(String),
}

/// Resolve `symbol` (`name`, `Class.method` or `Class::method`) in `content`.
/// Errors when the file has no tree-sitter grammar: the regex fallback only
/// knows declaration lines, so it cannot bound a body.
pub(super) fn resolve(
    content: &str,
    path: &str,
    symbol: &str,
    anchored: bool,
) -> Result<SymbolRead, String> {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    let (sigs, backend) = extract_signatures_with_backend(content, ext);
    if !matches!(backend, SigBackend::TreeSitter) {
        return Err(format!(
            "symbol reads need a tree-sitter grammar; .{ext} has none here. \
             Use mode=\"lines:N-M\" or ctx_search(action=\"symbol\")"
        ));
    }
    let parts: Vec<&str> = symbol
        .split(['.', ':'])
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();
    let Some((target, parents)) = parts.split_last() else {
        return Err("symbol must name a function or class".to_string());
    };
    let spans: Vec<(&str, usize, usize)> = sigs
        .iter()
        .filter_map(|s| Some((s, s.start_line?, s.end_line?)))
        .map(|(s, start, end)| (s.name.as_str(), start, end))
        .collect();
    let encloses = |parent: &str, start: usize, end: usize| {
        spans
            .iter()
            .any(|&(n, ps, pe)| n == parent && ps <= start && end <= pe && (ps, pe) != (start, end))
    };
    let mut ranges: Vec<(usize, usize)> = spans
        .iter()
        .filter(|&&(n, s, e)| n == *target && parents.iter().all(|p| encloses(p, s, e)))
        .map(|&(_, s, e)| (s, e))
        .collect();
    ranges.sort_unstable();
    ranges.dedup();
    if ranges.is_empty() {
        return Ok(SymbolRead::NotFound(format!(
            "[lean-ctx: symbol `{symbol}` not in this file — outline instead]"
        )));
    }
    if anchored {
        let (s, e) = ranges[0];
        let note = (ranges.len() > 1).then(|| {
            let rest: Vec<String> = ranges[1..].iter().map(|(s, _)| format!("L{s}")).collect();
            format!(
                "[lean-ctx: `{symbol}` also at {} — qualify it (Class.method)]",
                rest.join(", ")
            )
        });
        return Ok(SymbolRead::Window {
            mode: format!("anchored:{s}-{e}"),
            note,
        });
    }
    let joined: Vec<String> = ranges.iter().map(|(s, e)| format!("{s}-{e}")).collect();
    Ok(SymbolRead::Window {
        mode: format!("lines:{}", joined.join(",")),
        note: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const PY: &str = "\
import os


def helper(x):
    return x + 1


class Ledger:
    def add(self, amount):
        total = helper(amount)
        return total

    def remove(self, amount):
        return -amount


class Other:
    def add(self):
        return 0
";

    fn window(read: SymbolRead) -> (String, Option<String>) {
        match read {
            SymbolRead::Window { mode, note } => (mode, note),
            SymbolRead::NotFound(n) => panic!("expected a window, got {n}"),
        }
    }

    #[test]
    fn top_level_function_maps_to_its_exact_lines() {
        let (mode, note) = window(resolve(PY, "m.py", "helper", false).unwrap());
        assert_eq!(mode, "lines:4-5");
        assert!(note.is_none());
    }

    #[test]
    fn qualified_method_picks_the_enclosed_definition_only() {
        let (mode, _) = window(resolve(PY, "m.py", "Ledger.add", false).unwrap());
        assert_eq!(mode, "lines:9-11");
        let (mode, _) = window(resolve(PY, "m.py", "Other::add", false).unwrap());
        assert_eq!(mode, "lines:18-19");
    }

    #[test]
    fn ambiguous_name_selects_every_match_or_first_anchored_window() {
        let (mode, note) = window(resolve(PY, "m.py", "add", false).unwrap());
        assert_eq!(mode, "lines:9-11,18-19");
        assert!(note.is_none());
        let (mode, note) = window(resolve(PY, "m.py", "add", true).unwrap());
        assert_eq!(mode, "anchored:9-11");
        assert!(note.unwrap().contains("L18"));
    }

    #[test]
    fn unknown_name_falls_back_to_the_outline() {
        let read = resolve(PY, "m.py", "missing", false).unwrap();
        assert!(matches!(read, SymbolRead::NotFound(n) if n.contains("`missing`")));
    }

    #[test]
    fn file_without_grammar_is_rejected_with_the_alternatives() {
        let err = resolve("a = 1\n", "notes.txt", "a", false).unwrap_err();
        assert!(err.contains("lines:N-M"), "{err}");
    }
}
