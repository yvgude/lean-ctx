// SPDX-License-Identifier: Apache-2.0

//! Post-edit syntax gate (#1008): a tree-sitter parse check used to reject an
//! edit that turns a *cleanly parsing* file into a broken one.
//!
//! Principle (plan Säule 3): only the *clean → broken* transition is a real
//! regression. We never reject when the pre-edit file already had parse errors
//! (the model may be fixing them), and we skip entirely for languages without a
//! grammar — so the gate is a safety net, never an obstacle. The decision logic
//! lives in [`gate_edit`]; [`check_syntax`] is the raw parse probe.

/// Outcome of a tree-sitter parse probe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SyntaxCheck {
    /// `true` when the parse tree contains an ERROR or MISSING node.
    pub has_error: bool,
    /// 1-based line of the first error/missing node, when one was located.
    pub first_error_line: Option<usize>,
}

/// Parse `content` as `ext` and report whether the tree has syntax errors.
///
/// Returns `None` when the language is unsupported or tree-sitter is compiled
/// out — the caller then skips the gate rather than guessing.
#[cfg(feature = "tree-sitter")]
pub fn check_syntax(content: &str, ext: &str) -> Option<SyntaxCheck> {
    use std::cell::RefCell;
    use tree_sitter::Parser;

    let language = crate::deep_queries::get_language(ext)?;

    thread_local! {
        static PARSER: RefCell<Parser> = RefCell::new(Parser::new());
    }

    let tree = PARSER.with(|p| {
        let mut parser = p.borrow_mut();
        parser.set_language(&language).ok()?;
        parser.parse(content.as_bytes(), None)
    })?;

    let root = tree.root_node();
    if !root.has_error() {
        return Some(SyntaxCheck {
            has_error: false,
            first_error_line: None,
        });
    }
    Some(SyntaxCheck {
        has_error: true,
        first_error_line: first_error_line(root),
    })
}

#[cfg(not(feature = "tree-sitter"))]
pub fn check_syntax(_content: &str, _ext: &str) -> Option<SyntaxCheck> {
    None
}

/// Depth-first search for the first ERROR/MISSING node, returning its 1-based
/// start line. Only descends into subtrees that actually contain an error, so it
/// is effectively O(error-path), not O(tree).
#[cfg(feature = "tree-sitter")]
fn first_error_line(node: tree_sitter::Node) -> Option<usize> {
    if node.is_error() || node.is_missing() {
        return Some(node.start_position().row + 1);
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.is_error() || child.is_missing() {
            return Some(child.start_position().row + 1);
        }
        if child.has_error()
            && let Some(line) = first_error_line(child)
        {
            return Some(line);
        }
    }
    None
}

/// The post-edit gate decision (#1008). Returns `Some(reason)` only for the
/// clean → broken regression — the single case worth blocking — and `None`
/// (allow the write) for every other situation:
/// unsupported language, tree-sitter off, an already-broken pre-edit file, or a
/// post-edit file that still parses.
#[must_use]
pub fn gate_edit(ext: &str, old_content: &str, new_content: &str) -> Option<String> {
    // Pre-edit must parse cleanly for a regression to be meaningful.
    let pre = check_syntax(old_content, ext)?;
    if pre.has_error {
        return None;
    }
    let post = check_syntax(new_content, ext)?;
    if !post.has_error {
        return None;
    }
    let loc = post
        .first_error_line
        .map_or_else(String::new, |l| format!(" near line {l}"));
    Some(format!(
        "ERROR: edit rejected — it introduces a syntax error{loc} (.{ext}). \
         The current file on disk parses cleanly, but the proposed edit \
         would break it. No write was made. Fix the snippet and retry, \
         or pass validate_syntax=false to override."
    ))
}

#[cfg(all(test, feature = "tree-sitter"))]
mod tests {
    use super::*;

    #[test]
    fn clean_code_has_no_error() {
        let c = check_syntax("fn main() {}\n", "rs").unwrap();
        assert!(!c.has_error);
        assert_eq!(c.first_error_line, None);
    }

    #[test]
    fn broken_code_reports_error_with_line() {
        // Missing closing brace → parse error.
        let c = check_syntax("fn main() {\n    let x =\n", "rs").unwrap();
        assert!(c.has_error);
        assert!(c.first_error_line.is_some());
    }

    #[test]
    fn unsupported_extension_is_none() {
        assert!(check_syntax("anything at all", "unknownext").is_none());
    }

    #[test]
    fn gate_blocks_clean_to_broken() {
        let old = "fn main() {}\n";
        let new = "fn main() {\n"; // unbalanced brace
        let reason = gate_edit("rs", old, new).expect("clean→broken must be gated");
        assert!(reason.contains("syntax error"));
        assert!(reason.contains("validate_syntax=false"));
    }

    #[test]
    fn gate_allows_broken_to_broken() {
        // Pre-edit already broken → never our regression to block (model may fix).
        let old = "fn main() {\n"; // broken
        let new = "fn main( {\n"; // still broken
        assert!(gate_edit("rs", old, new).is_none());
    }

    #[test]
    fn gate_allows_clean_to_clean() {
        let old = "fn main() {}\n";
        let new = "fn main() { let x = 1; }\n";
        assert!(gate_edit("rs", old, new).is_none());
    }

    #[test]
    fn gate_skips_unsupported_language() {
        // No grammar → no opinion, always allow.
        assert!(gate_edit("unknownext", "valid", "{[(").is_none());
    }

    #[test]
    fn gate_allows_broken_being_fixed() {
        let old = "fn main() {\n"; // broken
        let new = "fn main() {}\n"; // fixed
        assert!(gate_edit("rs", old, new).is_none());
    }
}
