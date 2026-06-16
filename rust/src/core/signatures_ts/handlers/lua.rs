use tree_sitter::Node;

use crate::core::signatures::{Signature, compact_params};

use super::super::helpers::strip_parens;

/// Lua / Luau `function name(params)`, `local function name(...)`,
/// `function Table.field(...)` and method `function Table:method(...)`.
///
/// `local function` is module-private; global functions and table members are
/// part of the file's public surface. A `:`-qualified name is a method; a
/// `.`-qualified or bare name is a plain function.
pub(crate) fn lua_function(node: &Node, name: &str, source: &[u8]) -> Signature {
    let is_method = node
        .child_by_field_name("name")
        .is_some_and(|n| n.kind() == "method_index_expression");
    Signature {
        kind: if is_method { "method" } else { "fn" },
        name: name.to_string(),
        params: compact_params(&strip_parens(&params_text(node, source))),
        return_type: luau_return_type(node, source),
        is_async: false,
        is_exported: !is_local_decl(node, source),
        indent: 0,
        ..Signature::no_span()
    }
}

/// Function value bound to a variable: `local f = function() … end` or
/// `M.f = function() … end`. `local` bindings are private; table-field bindings
/// (`M.f`) are public. The `function_definition` value carries the parameters.
pub(crate) fn lua_assigned_function(node: &Node, name: &str, source: &[u8]) -> Signature {
    let params = find_function_definition(node)
        .map(|f| compact_params(&strip_parens(&params_text(&f, source))))
        .unwrap_or_default();
    Signature {
        kind: "fn",
        name: name.to_string(),
        params,
        return_type: String::new(),
        is_async: false,
        is_exported: !is_local_decl(node, source),
        indent: 0,
        ..Signature::no_span()
    }
}

/// Luau `type X = …` / `export type X = …`. Only `export` makes the alias part
/// of the module's public surface.
pub(crate) fn luau_type(node: &Node, name: &str, source: &[u8]) -> Signature {
    Signature {
        kind: "type",
        name: name.to_string(),
        params: String::new(),
        return_type: String::new(),
        is_async: false,
        is_exported: has_leading_keyword(node, source, "export"),
        indent: 0,
        ..Signature::no_span()
    }
}

fn params_text(node: &Node, source: &[u8]) -> String {
    node.child_by_field_name("parameters")
        .and_then(|p| p.utf8_text(source).ok())
        .unwrap_or("")
        .to_string()
}

/// Best-effort Luau return type: the typed child that follows the parameter list
/// (`function f(...): T`). Lua is dynamically typed, so this is usually empty.
fn luau_return_type(node: &Node, source: &[u8]) -> String {
    let Some(params_end) = node.child_by_field_name("parameters").map(|p| p.end_byte()) else {
        return String::new();
    };
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.start_byte() >= params_end
            && child.kind() != "block"
            && (child.kind() == "type" || child.kind().ends_with("_type"))
            && let Ok(t) = child.utf8_text(source)
        {
            return t.trim().to_string();
        }
    }
    String::new()
}

fn find_function_definition<'a>(node: &Node<'a>) -> Option<Node<'a>> {
    let mut stack = vec![*node];
    while let Some(n) = stack.pop() {
        if n.kind() == "function_definition" {
            return Some(n);
        }
        let mut cursor = n.walk();
        for child in n.children(&mut cursor) {
            stack.push(child);
        }
    }
    None
}

/// Whether a declaration is `local` (module-private).
fn is_local_decl(node: &Node, source: &[u8]) -> bool {
    has_leading_keyword(node, source, "local")
}

/// Whether a statement is introduced by `keyword` (e.g. `local`, `export`).
/// The grammar keeps such keywords as tokens attached to the wrapping statement,
/// so they can sit inside the node's range or immediately before it; both the
/// node text and the preceding identifier token are checked (word-boundary safe).
fn has_leading_keyword(node: &Node, source: &[u8], keyword: &str) -> bool {
    if node
        .utf8_text(source)
        .is_ok_and(|t| t.trim_start().starts_with(keyword))
    {
        return true;
    }
    std::str::from_utf8(&source[..node.start_byte()])
        .ok()
        .and_then(|p| {
            p.trim_end()
                .rsplit(|c: char| !(c.is_alphanumeric() || c == '_'))
                .next()
                .map(|w| w == keyword)
        })
        .unwrap_or(false)
}
