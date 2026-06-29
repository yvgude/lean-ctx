//! Cognitive complexity (SonarQube **S3776**-style) via tree-sitter.
//!
//! Cyclomatic complexity counts independent paths (good for "how many tests");
//! cognitive complexity models how hard code is to *follow* by adding a
//! **nesting penalty**, so deeply nested control flow scores higher than flat
//! code with the same number of branches. That nesting is the signal the Sonar
//! study links to agent token cost: a deeply nested function cannot be
//! navigated by name, so the agent reads all of it.
//!
//! ## Increment rules
//! - **+1 plus the current nesting depth** for each control-flow construct that
//!   nests: `if`, loops, `switch`/`match`, `catch`/`except`, ternary, `try`.
//!   Each such construct also raises the nesting level for its body.
//! - **+1 (flat)** for each *sequence* of binary boolean operators
//!   (`&&`/`||`/`and`/`or`) — consecutive identical operators count once.
//! - **+1 (flat)** for flow-breaking jumps that carry a label (`break`/
//!   `continue` with a label) and for `goto`.
//! - `else` / `else if` do **not** add a nesting level (handled via the
//!   `alternative` field), so else-if chains stay roughly linear.
//!
//! Nested function bodies are scored independently (mirrors
//! [`crate::core::cyclomatic`]). The traversal uses the heap-stack walk pattern
//! to stay safe on pathologically deep trees (#378).

use serde::Serialize;

/// Cognitive complexity of a single function-like definition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FunctionCognitive {
    pub name: String,
    /// 1-based start line of the function.
    pub line: usize,
    /// 1-based end line of the function (for span/token estimates).
    pub end_line: usize,
    pub cognitive: u32,
}

impl FunctionCognitive {
    /// Number of source lines the function spans (at least 1).
    pub fn line_span(&self) -> usize {
        self.end_line.saturating_sub(self.line).saturating_add(1)
    }
}

/// Compute cognitive complexity per function for `source` of the given file
/// `extension`. Returns `None` when tree-sitter is disabled, the extension is
/// unsupported, or the file has no functions.
pub fn cognitive_per_function(source: &str, extension: &str) -> Option<Vec<FunctionCognitive>> {
    #[cfg(feature = "tree-sitter")]
    {
        cognitive_impl(source, extension)
    }
    #[cfg(not(feature = "tree-sitter"))]
    {
        let _ = (source, extension);
        None
    }
}

#[cfg(feature = "tree-sitter")]
fn cognitive_impl(source: &str, extension: &str) -> Option<Vec<FunctionCognitive>> {
    let mut out: Vec<FunctionCognitive> = Vec::new();
    super::astutil::for_each_function(source, extension, |fn_node, name, src| {
        let body = super::astutil::logical_body_root(fn_node);
        let cognitive = cognitive_for_body(body, src, extension);
        let line = fn_node.start_position().row.saturating_add(1);
        let end_line = fn_node.end_position().row.saturating_add(1);
        out.push(FunctionCognitive {
            name: name.to_string(),
            line,
            end_line,
            cognitive,
        });
    })?;
    if out.is_empty() {
        None
    } else {
        // Deterministic order independent of traversal: by line then name.
        out.sort_by(|a, b| a.line.cmp(&b.line).then_with(|| a.name.cmp(&b.name)));
        Some(out)
    }
}

/// Classification of a node's contribution to cognitive complexity.
#[cfg(feature = "tree-sitter")]
#[derive(Clone, Copy, PartialEq, Eq)]
enum Incr {
    /// No contribution.
    None,
    /// +1 with no nesting penalty and no nesting increase (boolean ops, jumps).
    Flat,
    /// +1 plus the current nesting depth; raises nesting for the body.
    Nesting,
}

/// Sum cognitive complexity over a function body, skipping nested function
/// definitions (they are scored separately). Order-independent: the result is a
/// pure sum, so the heap-stack traversal needs no ordering guarantees.
#[cfg(feature = "tree-sitter")]
fn cognitive_for_body(root: tree_sitter::Node<'_>, source: &[u8], ext: &str) -> u32 {
    let root_id = root.id();
    let mut total: u32 = 0;
    let mut stack: Vec<(tree_sitter::Node<'_>, u32)> = vec![(root, 0)];
    while let Some((node, nesting)) = stack.pop() {
        // Nested functions form their own scope and are scored on their own.
        if node.id() != root_id && super::astutil::is_fn_like(node.kind()) {
            continue;
        }
        let class = classify(node, source, ext);
        match class {
            Incr::Nesting => total = total.saturating_add(1).saturating_add(nesting),
            Incr::Flat => total = total.saturating_add(1),
            Incr::None => {}
        }

        let child_nesting = if class == Incr::Nesting {
            nesting + 1
        } else {
            nesting
        };
        // `else`/`else if` must not deepen nesting: the `alternative` branch of
        // an `if` stays at the parent's level so else-if chains remain linear.
        let alternative_id = if class == Incr::Nesting && is_if_kind(node.kind()) {
            node.child_by_field_name("alternative").map(|n| n.id())
        } else {
            None
        };

        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            let cn = if Some(child.id()) == alternative_id {
                nesting
            } else {
                child_nesting
            };
            stack.push((child, cn));
        }
    }
    total
}

#[cfg(feature = "tree-sitter")]
fn classify(node: tree_sitter::Node<'_>, source: &[u8], ext: &str) -> Incr {
    let kind = node.kind();
    if is_nesting_kind(kind) {
        return Incr::Nesting;
    }
    // A boolean operator counts once per sequence: skip it when its parent is
    // the same logical operator (e.g. the inner `&&` of `a && b && c`).
    if let Some(op) = boolean_op_text(node, source) {
        let parent_same = node
            .parent()
            .and_then(|p| boolean_op_text(p, source))
            .is_some_and(|pop| pop == op);
        if !parent_same {
            return Incr::Flat;
        }
        return Incr::None;
    }
    if is_flow_break(node, source) {
        return Incr::Flat;
    }
    let _ = ext;
    Incr::None
}

/// Control-flow constructs that increment *and* raise nesting.
#[cfg(feature = "tree-sitter")]
fn is_nesting_kind(kind: &str) -> bool {
    matches!(
        kind,
        // conditionals
        "if_statement"
            | "if_expression"
            | "conditional_expression"
            | "ternary_expression"
            // loops
            | "for_statement"
            | "for_expression"
            | "for_in_statement"
            | "for_range_loop"
            | "enhanced_for_statement"
            | "while_statement"
            | "while_expression"
            | "do_statement"
            | "loop_expression"
            | "loop_statement"
            // multi-way branches (the container counts once, not each arm)
            | "switch_statement"
            | "switch_expression"
            | "match_expression"
            | "match_statement"
            // exception handlers
            | "catch_clause"
            | "except_clause"
            | "try_statement"
            | "try_expression"
    )
}

#[cfg(feature = "tree-sitter")]
fn is_if_kind(kind: &str) -> bool {
    matches!(kind, "if_statement" | "if_expression")
}

/// Returns the canonical operator string if `node` is a binary boolean
/// operator, else `None`. Handles Python's keyword form and the symbolic form
/// used by Rust/JS/TS/Go/Java/C/C++.
#[cfg(feature = "tree-sitter")]
fn boolean_op_text(node: tree_sitter::Node<'_>, source: &[u8]) -> Option<&'static str> {
    match node.kind() {
        "boolean_operator" => {
            // Python: the operator is a keyword child token.
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                match child.utf8_text(source) {
                    Ok("and") => return Some("&&"),
                    Ok("or") => return Some("||"),
                    _ => {}
                }
            }
            None
        }
        "binary_expression" | "binary_operator" | "logical_expression" => node
            .child_by_field_name("operator")
            .and_then(|op| op.utf8_text(source).ok())
            .and_then(|t| match t {
                "&&" | "and" => Some("&&"),
                "||" | "or" => Some("||"),
                _ => None,
            }),
        _ => None,
    }
}

/// Labeled `break`/`continue` and `goto` break linear reading flow → +1.
#[cfg(feature = "tree-sitter")]
fn is_flow_break(node: tree_sitter::Node<'_>, source: &[u8]) -> bool {
    match node.kind() {
        "goto_statement" => true,
        "break_statement" | "break_expression" | "continue_statement" | "continue_expression" => {
            let mut cursor = node.walk();
            node.children(&mut cursor).any(|child| {
                matches!(
                    child.kind(),
                    "label" | "loop_label" | "statement_identifier" | "label_name"
                ) && child.utf8_text(source).is_ok_and(|t| !t.is_empty())
            })
        }
        _ => false,
    }
}

#[cfg(all(test, feature = "tree-sitter"))]
mod tests {
    use super::*;

    fn score(src: &str, ext: &str, name: &str) -> u32 {
        cognitive_per_function(src, ext)
            .unwrap_or_default()
            .into_iter()
            .find(|f| f.name == name)
            .map_or_else(
                || panic!("function `{name}` not found in {ext} source"),
                |f| f.cognitive,
            )
    }

    #[test]
    fn nested_scores_higher_than_flat() {
        let flat = "fn flat(a: bool, b: bool, c: bool) { if a {} if b {} if c {} }";
        let nested = "fn nested(a: bool, b: bool, c: bool) { if a { if b { if c {} } } }";
        let flat_cc = score(flat, "rs", "flat");
        let nested_cc = score(nested, "rs", "nested");
        assert_eq!(flat_cc, 3, "three top-level ifs: 1+1+1");
        assert_eq!(nested_cc, 6, "nested ifs: 1 + 2 + 3 (nesting penalty)");
        assert!(nested_cc > flat_cc);
    }

    #[test]
    fn else_if_chain_stays_linear() {
        let src =
            "fn chain(a: bool, b: bool, c: bool) { if a {} else if b {} else if c {} else {} }";
        let cc = score(src, "rs", "chain");
        // Three `if`s at the same level (else-if does not deepen nesting).
        assert_eq!(cc, 3);
    }

    #[test]
    fn boolean_sequence_counts_once_per_operator() {
        // `a && b && c` is one &&-sequence → +1; mixing in `||` adds another.
        let same = "fn s(a: bool, b: bool, c: bool) -> bool { a && b && c }";
        let mixed = "fn m(a: bool, b: bool, c: bool) -> bool { a && b || c }";
        assert_eq!(score(same, "rs", "s"), 1);
        assert_eq!(score(mixed, "rs", "m"), 2);
    }

    #[test]
    fn nested_function_is_scored_separately() {
        let src = "fn outer(a: bool) { fn inner(b: bool) { if b {} } if a {} }";
        // outer sees only its own `if a` (the nested fn body is excluded).
        assert_eq!(score(src, "rs", "outer"), 1);
        assert_eq!(score(src, "rs", "inner"), 1);
    }

    #[test]
    fn python_nesting_penalty_applies() {
        let src = "def f(a, b):\n    if a:\n        if b:\n            return 1\n    return 0\n";
        // if a → +1, nested if b → +2.
        assert_eq!(score(src, "py", "f"), 3);
    }

    #[test]
    fn deterministic_across_runs() {
        let src = "fn g(a: bool, b: bool) { if a { while b { if a {} } } }";
        let first = cognitive_per_function(src, "rs");
        let second = cognitive_per_function(src, "rs");
        assert_eq!(first, second);
    }

    #[test]
    fn flat_function_is_zero() {
        let src = "fn plain(x: i32) -> i32 { let y = x + 1; y * 2 }";
        assert_eq!(score(src, "rs", "plain"), 0);
    }
}
