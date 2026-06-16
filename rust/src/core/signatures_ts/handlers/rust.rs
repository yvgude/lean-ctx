use tree_sitter::Node;

use crate::core::signatures::{Signature, compact_params};

use super::super::helpers::{
    clean_return_type, field_text, has_keyword_child, has_named_child, strip_parens,
};

pub(crate) fn rust_function(node: &Node, name: &str, source: &[u8]) -> Signature {
    let params = field_text(node, "parameters", source);
    let ret = field_text(node, "return_type", source);
    let exported = has_named_child(node, "visibility_modifier");
    let is_async = has_keyword_child(node, "async");
    let start_col = node.start_position().column;
    let is_method = start_col > 0;

    Signature {
        kind: if is_method { "method" } else { "fn" },
        name: name.to_string(),
        params: compact_params(&strip_parens(&params)),
        return_type: clean_return_type(&ret),
        is_async,
        is_exported: exported,
        indent: if is_method { 2 } else { 0 },
        ..Signature::no_span()
    }
}

pub(crate) fn rust_struct_like(node: &Node, name: &str, kind: &'static str) -> Signature {
    Signature {
        kind,
        name: name.to_string(),
        params: String::new(),
        return_type: String::new(),
        is_async: false,
        is_exported: has_named_child(node, "visibility_modifier"),
        indent: 0,
        ..Signature::no_span()
    }
}

pub(crate) fn rust_impl(node: &Node, name: &str, source: &[u8]) -> Signature {
    let trait_name = node
        .child_by_field_name("trait")
        .and_then(|n| n.utf8_text(source).ok())
        .map(std::string::ToString::to_string);
    let full_name = match trait_name {
        Some(t) => format!("{t} for {name}"),
        None => name.to_string(),
    };
    Signature {
        kind: "class",
        name: full_name,
        params: String::new(),
        return_type: String::new(),
        is_async: false,
        is_exported: false,
        indent: 0,
        ..Signature::no_span()
    }
}

pub(crate) fn rust_const(node: &Node, name: &str, source: &[u8]) -> Signature {
    let ret = field_text(node, "type", source);
    Signature {
        kind: "const",
        name: name.to_string(),
        params: String::new(),
        return_type: clean_return_type(&ret),
        is_async: false,
        is_exported: has_named_child(node, "visibility_modifier"),
        indent: 0,
        ..Signature::no_span()
    }
}
