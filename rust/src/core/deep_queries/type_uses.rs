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

/// C#: collect type names from every grammar position that carries a type.
///
/// Declaration positions — nodes with a `type` field (parameter,
/// variable_declaration, property_declaration, cast_expression,
/// typeof_expression, …), method `returns` fields, and `base_list` children
/// (inheritance / interfaces).
///
/// Expression positions (GH #398 follow-up) — a type used only as the receiver
/// of a static call/field or enum value (`Engine.Create()`, `Status.Active`)
/// or as an attribute (`[ApiController]`) carries no `type` field, yet it still
/// makes the consuming file depend on the definer. Those are collected here so
/// the property graph can build the `TypeRef` edge. The def-index resolution in
/// `ctx_impact` discards any name that is not an actual project type, so this
/// stays precise even though it is intentionally broad at the AST level.
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
        match node.kind() {
            "base_list" => {
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    collect_csharp_type_names(child, src, &mut uses, &mut seen);
                }
            }
            "member_access_expression" => {
                if let Some(expr) = node.child_by_field_name("expression") {
                    collect_csharp_receiver_type(expr, src, &mut uses, &mut seen);
                }
            }
            "attribute" => {
                if let Some(name) = node
                    .child_by_field_name("name")
                    .or_else(|| node.named_child(0))
                {
                    collect_csharp_attribute_type(name, src, &mut uses, &mut seen);
                }
            }
            _ => {}
        }
    });

    uses
}

/// Last dotted segment of a C# name, generic suffix stripped:
/// `App.Core.Engine` -> `Engine`, `List<int>` -> `List`, `Engine` -> `Engine`.
#[cfg(feature = "tree-sitter")]
fn csharp_last_segment(text: &str) -> Option<&str> {
    let last = text.rsplit(['.', ':']).find(|seg| !seg.trim().is_empty())?;
    let bare = last.split('<').next().unwrap_or(last).trim();
    (!bare.is_empty()).then_some(bare)
}

/// A type-valued receiver in expression position (`Engine.Create()`,
/// `Engine.Default`, `Status.Active`). Only PascalCase names — the C#
/// convention for types/enums — are accepted, so instance receivers
/// (`_engine`, `obj`, `this`) are dropped here; any remaining non-type name is
/// filtered later by the def-index resolution. Receivers that are not simple
/// names (calls, indexers, parenthesized expressions) are ignored.
#[cfg(feature = "tree-sitter")]
fn collect_csharp_receiver_type(
    expr: Node,
    src: &str,
    uses: &mut Vec<TypeUse>,
    seen: &mut std::collections::HashSet<String>,
) {
    let text = match expr.kind() {
        "identifier"
        | "qualified_name"
        | "alias_qualified_name"
        | "member_access_expression"
        | "generic_name" => node_text(expr, src),
        _ => return,
    };
    if let Some(name) = csharp_last_segment(text)
        && name.chars().next().is_some_and(|c| c.is_ascii_uppercase())
    {
        push_use(name, expr, uses, seen);
    }
}

/// An attribute reference `[Foo]`. C# resolves the short form to the class
/// `FooAttribute`, so both spellings are emitted and the def index matches
/// whichever the definition declares.
#[cfg(feature = "tree-sitter")]
fn collect_csharp_attribute_type(
    name: Node,
    src: &str,
    uses: &mut Vec<TypeUse>,
    seen: &mut std::collections::HashSet<String>,
) {
    let Some(bare) = csharp_last_segment(node_text(name, src)) else {
        return;
    };
    push_use(bare, name, uses, seen);
    if !bare.ends_with("Attribute") {
        push_use(&format!("{bare}Attribute"), name, uses, seen);
    }
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
