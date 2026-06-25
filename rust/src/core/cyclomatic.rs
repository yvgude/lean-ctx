//! Cyclomatic complexity via tree-sitter (decision-point counting).
//!
//! Uses the same structural chunk roots as [`super::chunks_ts`] and walks each function-like
//! subtree, skipping nested function bodies so inner items get their own scores.

use serde::Serialize;

#[cfg(feature = "tree-sitter")]
use tree_sitter::Node;

/// McCabe-style complexity for one function-like root (minimum 1).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FunctionComplexity {
    pub name: String,
    /// 1-based start line of this function-like node.
    pub line: usize,
    pub cyclomatic: u32,
}

/// AST-backed cyclomatic complexity for every function-like node under structural chunks.
///
/// Returns `None` when tree-sitter is disabled or `extension` is unsupported.
#[must_use]
pub fn cyclomatic_per_function(source: &str, extension: &str) -> Option<Vec<FunctionComplexity>> {
    #[cfg(feature = "tree-sitter")]
    {
        cyclomatic_per_function_impl(source, extension)
    }
    #[cfg(not(feature = "tree-sitter"))]
    {
        let _ = (source, extension);
        None
    }
}

#[cfg(feature = "tree-sitter")]
fn cyclomatic_per_function_impl(source: &str, extension: &str) -> Option<Vec<FunctionComplexity>> {
    let mut out = Vec::new();
    let src_bytes = source.as_bytes();

    super::chunks_ts::for_each_chunk_node(
        source,
        extension,
        |chunk_root, _chunk_name, _kind, _, _| {
            let mut fn_nodes = Vec::new();
            crate::core::ast_walk::for_each_descendant(chunk_root, |node| {
                if is_fn_like(node.kind()) {
                    fn_nodes.push(node);
                }
            });

            for fn_node in fn_nodes {
                let name = fn_name(fn_node, src_bytes).unwrap_or_else(|| "<anonymous>".to_string());
                let cyclomatic = cyclomatic_for_fn_like(fn_node, src_bytes, extension);
                let fn_line = fn_node.start_position().row.saturating_add(1);
                out.push(FunctionComplexity {
                    name,
                    line: fn_line,
                    cyclomatic,
                });
            }
        },
    )?;

    if out.is_empty() { None } else { Some(out) }
}

#[cfg(feature = "tree-sitter")]
fn is_fn_like(kind: &str) -> bool {
    matches!(
        kind,
        "function_item"
            | "function_declaration"
            | "function_definition"
            | "closure_expression"
            | "arrow_function"
            | "method_definition"
            | "method_declaration"
            | "constructor_declaration"
            | "lambda"
            | "func_literal"
    )
}

#[cfg(feature = "tree-sitter")]
fn fn_name(node: Node, source: &[u8]) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "identifier" | "type_identifier" | "property_identifier" | "field_identifier" => {
                if let Ok(t) = child.utf8_text(source) {
                    return Some(t.to_string());
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(feature = "tree-sitter")]
fn logical_body_root(fn_like: Node<'_>) -> Node<'_> {
    fn_like
        .child_by_field_name("body")
        .or_else(|| fn_like.child_by_field_name("value"))
        .unwrap_or(fn_like)
}

#[cfg(feature = "tree-sitter")]
fn cyclomatic_for_fn_like(fn_node: Node, source: &[u8], ext: &str) -> u32 {
    let root = logical_body_root(fn_node);
    1 + count_decisions_skip_nested_fn(root, source, ext)
}

#[cfg(feature = "tree-sitter")]
fn count_decisions_skip_nested_fn(root: Node, source: &[u8], ext: &str) -> u32 {
    // Iterative (heap-stack) walk that prunes nested function subtrees so they
    // are scored independently. Heap stack avoids the #378 SIGABRT on deep ASTs.
    let mut sum = 0;
    crate::core::ast_walk::for_each_descendant_pruned(root, |node| {
        if node != root && skip_nested_fn_root(node) {
            return false;
        }
        sum += tally_decision(node, source, ext);
        true
    });
    sum
}

#[cfg(feature = "tree-sitter")]
fn skip_nested_fn_root(node: Node) -> bool {
    is_fn_like(node.kind())
}

#[cfg(feature = "tree-sitter")]
fn tally_decision(node: Node, source: &[u8], ext: &str) -> u32 {
    match node.kind() {
        "if_statement"
        | "if_expression"
        | "while_statement"
        | "while_expression"
        | "for_statement"
        | "for_expression"
        | "do_statement"
        | "loop_expression"
        | "case_statement"
        | "switch_case"
        | "switch_rule"
        | "catch_clause"
        | "except_clause"
        | "conditional_expression"
        | "ternary_expression" => 1,
        "match_arm" => u32::from(matches!(ext, "rs")),
        "boolean_operator" => python_boolean_operator(node, source),
        "binary_expression" => binary_boolean_shortcircuit(node, source),
        _ => 0,
    }
}

#[cfg(feature = "tree-sitter")]
fn python_boolean_operator(node: Node, source: &[u8]) -> u32 {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if let Ok(t) = child.utf8_text(source)
            && (t == "and" || t == "or")
        {
            return 1;
        }
    }
    0
}

#[cfg(feature = "tree-sitter")]
fn binary_boolean_shortcircuit(node: Node, source: &[u8]) -> u32 {
    node.child_by_field_name("operator")
        .and_then(|op| op.utf8_text(source).ok())
        .map_or(0, |t| u32::from(matches!(t, "&&" | "||" | "and" | "or")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "tree-sitter")]
    #[test]
    fn cyclomatic_counts_branches_rust() {
        let src = r"pub fn f(x: i32) -> i32 {
    if x > 0 {
        1
    } else if x < 0 {
        -1
    } else {
        0
    }
}";
        let v = cyclomatic_per_function(src, "rs").expect("parse");
        let f = v.iter().find(|e| e.name == "f").expect("fn f");
        assert!(
            f.cyclomatic >= 3,
            "expected >=3 (McCabe paths), got {}",
            f.cyclomatic
        );
    }

    #[cfg(feature = "tree-sitter")]
    #[test]
    fn cyclomatic_match_arms_rust() {
        let src = r"pub fn g(e: u8) -> u8 {
    match e {
        0 => 0,
        1 => 1,
        _ => 2,
    }
}";
        let v = cyclomatic_per_function(src, "rs").expect("parse");
        let g = v.iter().find(|e| e.name == "g").expect("fn g");
        assert!(g.cyclomatic >= 4, "match + arms: got {}", g.cyclomatic);
    }

    #[cfg(not(feature = "tree-sitter"))]
    #[test]
    fn cyclomatic_disabled_returns_none() {
        assert!(cyclomatic_per_function("fn a() {}", "rs").is_none());
    }
}
