use tree_sitter::Node;

use crate::core::signatures::{Signature, compact_params};

use super::super::helpers::{clean_return_type, field_text, has_named_child, strip_parens};

pub(crate) fn kotlin_function(node: &Node, name: &str, source: &[u8]) -> Signature {
    let mut params = String::new();
    let mut ret = String::new();
    let mut seen_params = false;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "function_value_parameters" {
            params = child.utf8_text(source).unwrap_or("").to_string();
            seen_params = true;
        } else if seen_params && child.kind() == "type" {
            ret = child.utf8_text(source).unwrap_or("").to_string();
            ret = ret.trim_start_matches(':').trim().to_string();
            break;
        }
    }
    let is_method = node.start_position().column > 0;
    let exported = kotlin_modifiers_text(node, source).is_none_or(|t| !t.contains("private"));
    Signature {
        kind: if is_method { "method" } else { "fn" },
        name: name.to_string(),
        params: compact_params(&strip_parens(&params)),
        return_type: clean_return_type(&ret),
        is_async: false,
        is_exported: exported,
        indent: if is_method { 2 } else { 0 },
        ..Signature::no_span()
    }
}

fn kotlin_modifiers_text<'a>(node: &Node, source: &'a [u8]) -> Option<&'a str> {
    let mut cursor = node.walk();
    for c in node.children(&mut cursor) {
        if c.kind() == "modifiers" {
            return c.utf8_text(source).ok();
        }
    }
    None
}

pub(crate) fn kotlin_declaration_exported(node: &Node, source: &[u8]) -> bool {
    kotlin_modifiers_text(node, source).is_none_or(|t| !t.contains("private"))
}

pub(crate) fn swift_function(node: &Node, name: &str, source: &[u8]) -> Signature {
    let ret = field_text(node, "return_type", source);
    let params = swift_parameters_before_body(node, source);
    let is_async = has_named_child(node, "async");
    let is_method = node.start_position().column > 0;
    Signature {
        kind: if is_method { "method" } else { "fn" },
        name: name.to_string(),
        params: compact_params(&strip_parens(&params)),
        return_type: clean_return_type(&ret),
        is_async,
        is_exported: true,
        indent: if is_method { 2 } else { 0 },
        ..Signature::no_span()
    }
}

pub(crate) fn swift_protocol_function(node: &Node, name: &str, source: &[u8]) -> Signature {
    let ret = field_text(node, "return_type", source);
    Signature {
        kind: "fn",
        name: name.to_string(),
        params: String::new(),
        return_type: clean_return_type(&ret),
        is_async: has_named_child(node, "async"),
        is_exported: true,
        indent: 2,
        ..Signature::no_span()
    }
}

pub(crate) fn swift_class_declaration(node: &Node, name: &str, source: &[u8]) -> Signature {
    let kind = node
        .child_by_field_name("declaration_kind")
        .and_then(|n| n.utf8_text(source).ok())
        .unwrap_or("class");
    let kind_static: &'static str = match kind {
        "struct" => "struct",
        "enum" => "enum",
        _ => "class",
    };
    Signature {
        kind: kind_static,
        name: name.to_string(),
        params: String::new(),
        return_type: String::new(),
        is_async: false,
        is_exported: true,
        indent: 0,
        ..Signature::no_span()
    }
}

fn swift_parameters_before_body(node: &Node, source: &[u8]) -> String {
    let end_byte = node
        .child_by_field_name("body")
        .map_or(usize::MAX, |b| b.start_byte());
    let mut parts: Vec<String> = Vec::new();
    // Heap-stack walk bounded to the parameter region (before the body), pruning
    // any subtree at/after `end_byte`. Iterative to avoid the #378 SIGABRT.
    crate::core::ast_walk::for_each_descendant_pruned(*node, |n| {
        if n.start_byte() >= end_byte {
            return false;
        }
        if n.kind() == "parameter"
            && let Ok(t) = n.utf8_text(source)
        {
            parts.push(t.to_string());
        }
        true
    });
    if parts.is_empty() {
        String::new()
    } else {
        format!("({})", parts.join(", "))
    }
}

pub(crate) fn csharp_has_modifier_text(node: &Node, needle: &str, source: &[u8]) -> bool {
    let mut cursor = node.walk();
    for c in node.children(&mut cursor) {
        if c.kind() == "modifier"
            && let Ok(t) = c.utf8_text(source)
            && t.contains(needle)
        {
            return true;
        }
    }
    false
}
