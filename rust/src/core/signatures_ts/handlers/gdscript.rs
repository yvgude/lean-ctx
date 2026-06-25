use tree_sitter::Node;

use crate::core::signatures::{Signature, compact_params};

use super::super::helpers::{clean_return_type, field_text, strip_parens};

fn child_by_kind<'a>(node: &Node<'a>, kind: &str) -> Option<Node<'a>> {
    let mut cursor = node.walk();

    node.children(&mut cursor).find(|c| c.kind() == kind)
}

fn params_text(node: &Node, source: &[u8]) -> String {
    let params = field_text(node, "parameters", source);
    if params.is_empty() {
        child_by_kind(node, "parameters")
            .and_then(|p| p.utf8_text(source).ok())
            .unwrap_or("")
            .to_string()
    } else {
        params
    }
}

/// `GDScript` `func name(params) -> ret:`.
///
/// Top-level functions are exported; names starting with `_` are Godot
/// private/virtual callbacks (`_ready`, `_draw`, …). Indented definitions are
/// inner-class methods.
pub(crate) fn gdscript_function(node: &Node, name: &str, source: &[u8]) -> Signature {
    let ret = child_by_kind(node, "type")
        .and_then(|t| t.utf8_text(source).ok())
        .unwrap_or("")
        .to_string();
    let is_method = node.start_position().column > 0;
    Signature {
        kind: if is_method { "method" } else { "fn" },
        name: name.to_string(),
        params: compact_params(&strip_parens(&params_text(node, source))),
        return_type: clean_return_type(&ret),
        is_async: false,
        is_exported: !name.starts_with('_'),
        indent: if is_method { 2 } else { 0 },
        ..Signature::no_span()
    }
}

/// `GDScript` member storage declarations: `const X`, `var x`, `@export var x`,
/// `@onready var y`. `@export` always belongs to the public/editor-facing API;
/// otherwise the leading-underscore Godot privacy convention applies.
pub(crate) fn gdscript_variable(node: &Node, name: &str) -> Signature {
    let kind = if node.kind() == "const_statement" {
        "const"
    } else {
        "var"
    };
    let is_exported = node.kind() == "export_variable_statement" || !name.starts_with('_');
    Signature {
        kind,
        name: name.to_string(),
        params: String::new(),
        return_type: String::new(),
        is_async: false,
        is_exported,
        indent: 0,
        ..Signature::no_span()
    }
}

/// `GDScript` `signal name(params)` — an emittable member symbol.
pub(crate) fn gdscript_signal(node: &Node, name: &str, source: &[u8]) -> Signature {
    Signature {
        kind: "signal",
        name: name.to_string(),
        params: compact_params(&strip_parens(&params_text(node, source))),
        return_type: String::new(),
        is_async: false,
        is_exported: true,
        indent: 0,
        ..Signature::no_span()
    }
}
