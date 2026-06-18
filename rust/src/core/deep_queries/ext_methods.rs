//! C# extension-method extraction (GH #398 follow-up).
//!
//! An extension method (`public static T Foo(this X x, …)`) is invoked as if
//! it were an instance method (`value.Foo()`). The call site therefore names
//! neither the defining static class nor any of its types, so import- and
//! type-usage edges miss the dependency entirely. Capturing the method names
//! lets `ctx_impact` link `value.Foo()` consumers to the file that defines the
//! extension.
//!
//! Only C# is handled today; Kotlin/Swift extension functions are a documented
//! follow-up.

#[cfg(feature = "tree-sitter")]
use tree_sitter::Node;

#[cfg(feature = "tree-sitter")]
use super::node_text;
use super::types::ExtMethodDef;

#[cfg(feature = "tree-sitter")]
pub(super) fn extract_ext_methods(root: Node, src: &str, ext: &str) -> Vec<ExtMethodDef> {
    if ext != "cs" {
        return Vec::new();
    }

    let mut out: Vec<ExtMethodDef> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    crate::core::ast_walk::for_each_descendant(root, |node| {
        if node.kind() != "method_declaration" || !is_csharp_extension_method(node, src) {
            return;
        }
        let Some(name_node) = node.child_by_field_name("name") else {
            return;
        };
        let name = node_text(name_node, src).trim().to_string();
        // One entry per name per file: overloads share a host file, and the
        // resolver only needs the file → method link.
        if name.is_empty() || !seen.insert(name.clone()) {
            return;
        }
        out.push(ExtMethodDef {
            name,
            line: node.start_position().row + 1,
            end_line: node.end_position().row + 1,
        });
    });

    out
}

/// A C# method is an extension method iff its first parameter carries the
/// `this` modifier. In tree-sitter-c-sharp `this` is aliased to a `modifier`
/// node inside the `parameter`; a text check guards the rare attribute-prefixed
/// form defensively.
#[cfg(feature = "tree-sitter")]
fn is_csharp_extension_method(method: Node, src: &str) -> bool {
    let Some(params) = method.child_by_field_name("parameters") else {
        return false;
    };
    let mut cursor = params.walk();
    let Some(first) = params
        .children(&mut cursor)
        .find(|c| c.kind() == "parameter")
    else {
        return false;
    };

    let mut pcursor = first.walk();
    for child in first.children(&mut pcursor) {
        if child.kind() == "modifier" && node_text(child, src).trim() == "this" {
            return true;
        }
    }
    node_text(first, src).trim_start().starts_with("this ")
}
