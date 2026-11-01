// SPDX-License-Identifier: Apache-2.0

//! Stack-safe traversal of tree-sitter ASTs.
//!
//! Native recursion over a syntax tree descends once per AST level. Machine
//! generated, minified, or pathologically nested source can produce a tree
//! thousands of levels deep, which overflows the (small, ~2 MiB) worker-thread
//! stack used for background indexing. Rust turns a stack overflow into an
//! immediate `abort()` of the whole process — the SIGABRT reported in #378.
//!
//! These helpers walk the tree with a heap-allocated work stack instead, so the
//! traversal depth is bounded by available heap rather than by the native call
//! stack. Indexing a single deep file can therefore no longer crash the daemon.

#[cfg(feature = "tree-sitter")]
use tree_sitter::Node;

/// Visit `root` and every descendant in pre-order: each node is visited before
/// its children, and children are visited left-to-right — matching the order a
/// naive recursive `for child in node.children() { recurse(child) }` produces.
#[cfg(feature = "tree-sitter")]
pub fn for_each_descendant<'tree>(root: Node<'tree>, mut visit: impl FnMut(Node<'tree>)) {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        visit(node);
        push_children_in_order(&mut stack, node);
    }
}

/// Like [`for_each_descendant`], but `descend` both visits the node and returns
/// whether to recurse into its children. Returning `false` prunes the whole
/// subtree (the node itself has already been visited).
#[cfg(feature = "tree-sitter")]
pub fn for_each_descendant_pruned<'tree>(
    root: Node<'tree>,
    mut descend: impl FnMut(Node<'tree>) -> bool,
) {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if descend(node) {
            push_children_in_order(&mut stack, node);
        }
    }
}

/// Return the first descendant of `root` (including `root`) whose kind equals
/// `kind`, searched in pre-order. Heap-stack equivalent of a recursive search.
#[cfg(feature = "tree-sitter")]
pub fn find_descendant_by_kind<'tree>(root: Node<'tree>, kind: &str) -> Option<Node<'tree>> {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == kind {
            return Some(node);
        }
        push_children_in_order(&mut stack, node);
    }
    None
}

/// Push `node`'s children so that a LIFO stack pops them left-to-right.
#[cfg(feature = "tree-sitter")]
fn push_children_in_order<'tree>(stack: &mut Vec<Node<'tree>>, node: Node<'tree>) {
    let mark = stack.len();
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        stack.push(child);
    }
    stack[mark..].reverse();
}

#[cfg(all(test, feature = "tree-sitter"))]
mod tests {
    use super::*;
    use tree_sitter::Parser;

    fn parse_rust(src: &str) -> tree_sitter::Tree {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_rust::LANGUAGE.into())
            .unwrap();
        parser.parse(src, None).unwrap()
    }

    #[test]
    fn visits_every_node_in_preorder() {
        let tree = parse_rust("fn a() { let x = 1; }");
        let mut kinds = Vec::new();
        for_each_descendant(tree.root_node(), |n| kinds.push(n.kind()));
        assert_eq!(kinds.first(), Some(&"source_file"));
        assert!(kinds.contains(&"function_item"));
        assert!(kinds.contains(&"let_declaration"));
    }

    #[test]
    fn pruned_skips_subtree() {
        let tree = parse_rust("fn a() { let x = 1; }");
        let mut visited_let = false;
        for_each_descendant_pruned(tree.root_node(), |n| {
            if n.kind() == "let_declaration" {
                visited_let = true;
            }
            // Visit the block but do not descend into it.
            n.kind() != "block"
        });
        // The `let` lives inside the pruned block, so it is never visited.
        assert!(!visited_let);
    }

    #[test]
    fn find_descendant_returns_first_match() {
        let tree = parse_rust("fn a() { let x = 1; }");
        let found = find_descendant_by_kind(tree.root_node(), "let_declaration");
        assert!(found.is_some());
        assert!(find_descendant_by_kind(tree.root_node(), "no_such_kind").is_none());
    }

    /// A tree thousands of levels deep must not overflow the stack — this is the
    /// regression guard for the #378 SIGABRT.
    #[test]
    fn deeply_nested_tree_does_not_overflow() {
        let depth = 20_000;
        let src = format!("fn f() {{ {}0{} }}", "(".repeat(depth), ")".repeat(depth));
        let tree = parse_rust(&src);
        let mut count = 0usize;
        for_each_descendant(tree.root_node(), |_| count += 1);
        assert!(count > depth, "should visit the deep parenthesis chain");
    }
}
