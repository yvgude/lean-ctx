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
    /// The name is not defined in this file. P10: answering with the outline
    /// cost 12k chars on a 4,188-line file; the answer is now the closest
    /// names (in `note`) plus the lines that mention the name (`hits`, 1-based,
    /// at most [`MAX_HITS`]), served as a `lines:` multi-select.
    NotFound { note: String, hits: Vec<usize> },
}

/// Lines that mention a missing symbol, shown instead of the outline.
const MAX_HITS: usize = 8;
/// Closest defined names offered for a missing symbol.
const MAX_SUGGESTIONS: usize = 5;

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
    let (mut sigs, backend) = extract_signatures_with_backend(content, ext);
    if !matches!(backend, SigBackend::TreeSitter) {
        // A file of bindings only (no functions) falls back to the regex
        // extractor; its bindings still resolve through the grammar below.
        sigs.clear();
    }
    if !matches!(backend, SigBackend::TreeSitter) && !has_grammar(ext) {
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
    if ranges.is_empty() && parents.is_empty() {
        // Module-level constants and variables are no signatures (P10:
        // `_SIZE_UNITS = {...}` fell back to the whole outline).
        ranges = top_level_bindings(content, ext, target);
    }
    ranges.sort_unstable();
    ranges.dedup();
    if ranges.is_empty() {
        return Ok(not_found(content, symbol, target, &spans));
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

/// The small answer for a name the file does not define: the closest defined
/// names and the first lines that mention it as a whole word.
fn not_found(
    content: &str,
    symbol: &str,
    target: &str,
    spans: &[(&str, usize, usize)],
) -> SymbolRead {
    let hits: Vec<usize> = content
        .lines()
        .enumerate()
        .filter(|(_, line)| mentions_word(line, target))
        .map(|(i, _)| i + 1)
        .take(MAX_HITS)
        .collect();
    let lower = target.to_ascii_lowercase();
    let mut names: Vec<&str> = spans.iter().map(|&(n, _, _)| n).collect();
    names.sort_unstable();
    names.dedup();
    names.sort_by_cached_key(|n| {
        let candidate = n.to_ascii_lowercase();
        let unrelated = !(candidate.contains(&lower) || lower.contains(&candidate));
        (unrelated, edit_distance(&candidate, &lower), *n)
    });
    let closest = if names.is_empty() {
        String::new()
    } else {
        let shown: Vec<&str> = names.iter().take(MAX_SUGGESTIONS).copied().collect();
        format!("; closest: {}", shown.join(", "))
    };
    let where_ = if hits.is_empty() {
        "no line mentions it".to_string()
    } else {
        format!("lines mentioning it above ({} shown)", hits.len())
    };
    SymbolRead::NotFound {
        note: format!("[lean-ctx: symbol `{symbol}` not defined in this file{closest}; {where_}]"),
        hits,
    }
}

fn mentions_word(line: &str, word: &str) -> bool {
    let is_ident = |c: char| c.is_alphanumeric() || c == '_';
    line.match_indices(word).any(|(at, _)| {
        let before = line[..at].chars().next_back();
        let after = line[at + word.len()..].chars().next();
        !before.is_some_and(is_ident) && !after.is_some_and(is_ident)
    })
}

fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, &cb) in b.iter().enumerate() {
            let substitution = diagonal + usize::from(ca != cb);
            diagonal = row[j + 1];
            row[j + 1] = substitution.min(row[j] + 1).min(row[j + 1] + 1);
        }
    }
    row[b.len()]
}

/// Line spans of the top-level statements that bind `name`: Python
/// assignments, Rust `const`/`static`, JS/TS `const`/`let`/`var` (also
/// exported), Go `const`/`var`.
#[cfg(feature = "tree-sitter")]
fn top_level_bindings(content: &str, ext: &str, name: &str) -> Vec<(usize, usize)> {
    use tree_sitter::Node;

    fn named(n: Node<'_>) -> Vec<Node<'_>> {
        let mut cursor = n.walk();
        n.named_children(&mut cursor).collect()
    }

    fn binds(node: Node, src: &str, name: &str) -> bool {
        let text = |n: Node| &src[n.byte_range()];
        match node.kind() {
            "expression_statement" | "export_statement" => {
                named(node).into_iter().any(|c| binds(c, src, name))
            }
            "assignment" | "assignment_expression" => {
                node.child_by_field_name("left").is_some_and(|left| {
                    text(left) == name
                        || matches!(left.kind(), "pattern_list" | "tuple_pattern")
                            && named(left).into_iter().any(|c| text(c) == name)
                })
            }
            "lexical_declaration"
            | "variable_declaration"
            | "const_declaration"
            | "var_declaration" => named(node)
                .into_iter()
                .any(|declarator| binds(declarator, src, name)),
            "variable_declarator" | "const_item" | "static_item" => node
                .child_by_field_name("name")
                .is_some_and(|n| text(n) == name),
            "const_spec" | "var_spec" => {
                let mut cursor = node.walk();
                node.children_by_field_name("name", &mut cursor)
                    .any(|n| text(n) == name)
            }
            _ => false,
        }
    }

    let Some(language) = crate::core::deep_queries::get_language(ext) else {
        return Vec::new();
    };
    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(&language).is_err() {
        return Vec::new();
    }
    let Some(tree) = parser.parse(content.as_bytes(), None) else {
        return Vec::new();
    };
    let root = tree.root_node();
    let mut cursor = root.walk();
    root.named_children(&mut cursor)
        .filter(|node| binds(*node, content, name))
        .map(|node| (node.start_position().row + 1, node.end_position().row + 1))
        .collect()
}

#[cfg(feature = "tree-sitter")]
fn has_grammar(ext: &str) -> bool {
    crate::core::deep_queries::get_language(ext).is_some()
}

#[cfg(not(feature = "tree-sitter"))]
fn has_grammar(_ext: &str) -> bool {
    false
}

#[cfg(not(feature = "tree-sitter"))]
fn top_level_bindings(_content: &str, _ext: &str, _name: &str) -> Vec<(usize, usize)> {
    Vec::new()
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
            SymbolRead::NotFound { note, .. } => panic!("expected a window, got {note}"),
        }
    }

    /// P10: module-level bindings are symbols too (`_SIZE_UNITS = {...}`
    /// returned the whole 12k-char outline).
    #[test]
    fn top_level_bindings_resolve_to_their_statement() {
        let py = "import re\n\n_SIZE_UNITS = {\n    \"kb\": 1024,\n}\nA, LIMIT = 1, 2\n\ndef f():\n    _SIZE_UNITS = 0\n";
        let (mode, _) = window(resolve(py, "m.py", "_SIZE_UNITS", false).unwrap());
        assert_eq!(mode, "lines:3-5", "the nested rebinding is not top-level");
        let (mode, _) = window(resolve(py, "m.py", "LIMIT", false).unwrap());
        assert_eq!(mode, "lines:6-6");
        for (src, path, name, expected) in [
            (
                "const MAX: usize = 3;\nstatic NAME: &str = \"x\";\n",
                "m.rs",
                "NAME",
                "lines:2-2",
            ),
            (
                "import x from 'y';\nexport const limits = {\n  a: 1,\n};\nlet n = 2;\n",
                "m.ts",
                "limits",
                "lines:2-4",
            ),
            ("let n = 2;\nvar m = {\n};\n", "m.js", "m", "lines:2-3"),
        ] {
            let (mode, _) = window(resolve(src, path, name, false).unwrap());
            assert_eq!(mode, expected, "{path} {name}");
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
    fn unknown_name_answers_with_closest_names_and_mentions() {
        let SymbolRead::NotFound { note, hits } = resolve(PY, "m.py", "Ledgr", false).unwrap()
        else {
            panic!("Ledgr is not defined");
        };
        assert!(
            note.contains("`Ledgr`") && note.contains("closest: Ledger"),
            "{note}"
        );
        assert!(hits.is_empty(), "{hits:?}");
        // Mentions are whole words: `total` is on 10-11, never inside `totals`.
        let src = format!("{PY}totals = 1\n");
        let SymbolRead::NotFound { hits, .. } = resolve(&src, "m.py", "total", false).unwrap()
        else {
            panic!("total is a local, not a symbol");
        };
        assert_eq!(hits, vec![10, 11]);
    }

    #[test]
    fn file_without_grammar_is_rejected_with_the_alternatives() {
        let err = resolve("a = 1\n", "notes.txt", "a", false).unwrap_err();
        assert!(err.contains("lines:N-M"), "{err}");
    }
}
