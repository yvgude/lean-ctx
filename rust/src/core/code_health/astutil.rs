//! Shared AST helpers for the code-health engine.
//!
//! Function enumeration and identifier extraction live here so every health
//! metric (cognitive complexity, naming) scores the *same* set of
//! function-like nodes, mirroring the structural-chunk walk used by
//! [`crate::core::cyclomatic`]. Tree-sitter only; the public engine functions
//! degrade to `None` when the feature is disabled.

#[cfg(feature = "tree-sitter")]
use tree_sitter::Node;

/// Function-like node kinds across the supported grammars. A node of one of
/// these kinds is its own complexity scope (nested ones are scored separately).
#[cfg(feature = "tree-sitter")]
pub(crate) fn is_fn_like(kind: &str) -> bool {
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

/// Best-effort identifier name for a function-like node.
#[cfg(feature = "tree-sitter")]
pub(crate) fn fn_name(node: Node, source: &[u8]) -> Option<String> {
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

/// The logical body subtree of a function-like node (its `body`/`value` field,
/// falling back to the node itself for grammars without an explicit body field).
#[cfg(feature = "tree-sitter")]
pub(crate) fn logical_body_root(fn_like: Node<'_>) -> Node<'_> {
    fn_like
        .child_by_field_name("body")
        .or_else(|| fn_like.child_by_field_name("value"))
        .unwrap_or(fn_like)
}

/// Visit every function-like node under the structural chunks of `content`.
///
/// The callback receives `(fn_node, name, source_bytes)`. Each function is
/// visited exactly once even when grammars match nested functions as their own
/// chunks (deduplicated by start byte). Returns `None` when `ext` is
/// unsupported or parsing fails.
#[cfg(feature = "tree-sitter")]
pub(crate) fn for_each_function(
    content: &str,
    ext: &str,
    mut visit: impl FnMut(Node, &str, &[u8]),
) -> Option<()> {
    let source = content.as_bytes();
    let mut seen: std::collections::HashSet<usize> = std::collections::HashSet::new();
    crate::core::chunks_ts::for_each_chunk_node(content, ext, |chunk_root, _name, _kind, _, _| {
        crate::core::ast_walk::for_each_descendant(chunk_root, |node| {
            if is_fn_like(node.kind()) && seen.insert(node.start_byte()) {
                let name = fn_name(node, source).unwrap_or_else(|| "<anonymous>".to_string());
                visit(node, &name, source);
            }
        });
    })
}
