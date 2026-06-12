//! Type-usage extraction (GH #398).
//!
//! C# and Java resolve types in the same namespace/package **without any
//! import statement**, so import edges alone miss those file dependencies.
//! This module extracts the *names of types a file consumes* — fields,
//! parameters, properties, return types, base classes/interfaces, generic
//! arguments, casts, `typeof` — so the property graph can link consumer
//! files to definer files via `TypeRef` edges.

#[cfg(feature = "tree-sitter")]
use tree_sitter::Node;

#[cfg(feature = "tree-sitter")]
use super::node_text;
use super::types::TypeUse;

/// Languages whose visibility rules make type-usage edges necessary.
/// Other languages express file dependencies through explicit imports,
/// which the import resolver already covers.
#[cfg(feature = "tree-sitter")]
pub(super) fn extract_type_uses(root: Node, src: &str, ext: &str) -> Vec<TypeUse> {
    match ext {
        "cs" => extract_csharp(root, src),
        "java" => extract_java(root, src),
        _ => Vec::new(),
    }
}

/// C#: collect type names from every grammar position that carries a type —
/// nodes with a `type` field (parameter, variable_declaration,
/// property_declaration, cast_expression, typeof_expression, …), method
/// `returns` fields, and `base_list` children (inheritance / interfaces).
#[cfg(feature = "tree-sitter")]
fn extract_csharp(root: Node, src: &str) -> Vec<TypeUse> {
    let mut uses: Vec<TypeUse> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    crate::core::ast_walk::for_each_descendant(root, |node| {
        if let Some(type_node) = node
            .child_by_field_name("type")
            .or_else(|| node.child_by_field_name("returns"))
        {
            collect_csharp_type_names(type_node, src, &mut uses, &mut seen);
        }
        if node.kind() == "base_list" {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                collect_csharp_type_names(child, src, &mut uses, &mut seen);
            }
        }
    });

    uses
}

/// Reduce a C# type subtree to its named components: `Engine` -> Engine,
/// `List<Engine>` -> List + Engine, `App.Core.Engine` -> Engine,
/// `Engine[]` / `Engine?` -> Engine. Predefined types (`int`, `string`) have
/// no `identifier` node and contribute nothing.
#[cfg(feature = "tree-sitter")]
fn collect_csharp_type_names(
    node: Node,
    src: &str,
    uses: &mut Vec<TypeUse>,
    seen: &mut std::collections::HashSet<String>,
) {
    match node.kind() {
        "identifier" => {
            push_use(node_text(node, src), node, uses, seen);
        }
        "qualified_name" | "alias_qualified_name" => {
            // Only the last segment names the type; leading segments are
            // namespaces and would create bogus matches.
            let text = node_text(node, src);
            if let Some(last) = text.rsplit(['.', ':']).find(|seg| !seg.trim().is_empty()) {
                let bare = last.split('<').next().unwrap_or(last).trim();
                push_use(bare, node, uses, seen);
            }
            // Generic arguments inside a qualified name (`Ns.List<Engine>`)
            // still need a structural walk.
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "generic_name" || child.kind() == "type_argument_list" {
                    collect_csharp_type_names(child, src, uses, seen);
                }
            }
        }
        "generic_name" => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                match child.kind() {
                    "identifier" => push_use(node_text(child, src), child, uses, seen),
                    "type_argument_list" => {
                        collect_csharp_type_names(child, src, uses, seen);
                    }
                    _ => {}
                }
            }
        }
        // Wrappers (array_type, nullable_type, type_argument_list,
        // primary_constructor_base_type, tuple_type, ref_type, …): recurse.
        _ => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                collect_csharp_type_names(child, src, uses, seen);
            }
        }
    }
}

/// Java: the grammar gives every used type a dedicated `type_identifier`
/// node (fields, parameters, returns, extends/implements, generics, casts),
/// so a single descendant scan is precise.
#[cfg(feature = "tree-sitter")]
fn extract_java(root: Node, src: &str) -> Vec<TypeUse> {
    let mut uses: Vec<TypeUse> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    crate::core::ast_walk::for_each_descendant(root, |node| {
        if node.kind() == "type_identifier" {
            push_use(node_text(node, src), node, &mut uses, &mut seen);
        }
    });

    uses
}

#[cfg(feature = "tree-sitter")]
fn push_use(
    name: &str,
    node: Node,
    uses: &mut Vec<TypeUse>,
    seen: &mut std::collections::HashSet<String>,
) {
    let name = name.trim();
    if name.is_empty() || !seen.insert(name.to_string()) {
        return;
    }
    uses.push(TypeUse {
        name: name.to_string(),
        line: node.start_position().row + 1,
    });
}
