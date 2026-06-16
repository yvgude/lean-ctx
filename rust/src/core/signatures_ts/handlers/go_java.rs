use tree_sitter::Node;

use crate::core::signatures::{Signature, compact_params};

use super::super::helpers::{clean_return_type, field_text, has_modifier, strip_parens};
use super::kotlin_swift::csharp_has_modifier_text;

pub(crate) fn go_or_java_method(
    node: &Node,
    name: &str,
    ext: &str,
    source: &[u8],
) -> Option<Signature> {
    match ext {
        "go" => {
            let params = field_text(node, "parameters", source);
            let ret = field_text(node, "result", source);
            Some(Signature {
                kind: "method",
                name: name.to_string(),
                params: compact_params(&strip_parens(&params)),
                return_type: clean_return_type(&ret),
                is_async: false,
                is_exported: name.starts_with(|c: char| c.is_uppercase()),
                indent: 2,
                ..Signature::no_span()
            })
        }
        "java" => {
            let params = field_text(node, "parameters", source);
            let ret = field_text(node, "type", source);
            let is_method = node.start_position().column > 0;
            Some(Signature {
                kind: if is_method { "method" } else { "fn" },
                name: name.to_string(),
                params: compact_params(&strip_parens(&params)),
                return_type: ret.trim().to_string(),
                is_async: false,
                is_exported: has_modifier(node, "public", source),
                indent: if is_method { 2 } else { 0 },
                ..Signature::no_span()
            })
        }
        "cs" => {
            let params = field_text(node, "parameters", source);
            let ret = field_text(node, "returns", source);
            let is_method = node.start_position().column > 0;
            Some(Signature {
                kind: if is_method { "method" } else { "fn" },
                name: name.to_string(),
                params: compact_params(&strip_parens(&params)),
                return_type: ret.trim().to_string(),
                is_async: false,
                is_exported: csharp_has_modifier_text(node, "public", source),
                indent: if is_method { 2 } else { 0 },
                ..Signature::no_span()
            })
        }
        "php" => {
            let params = field_text(node, "parameters", source);
            let ret = field_text(node, "return_type", source);
            let is_method = node.start_position().column > 0;
            Some(Signature {
                kind: if is_method { "method" } else { "fn" },
                name: name.to_string(),
                params: compact_params(&strip_parens(&params)),
                return_type: clean_return_type(&ret),
                is_async: false,
                is_exported: true,
                indent: if is_method { 2 } else { 0 },
                ..Signature::no_span()
            })
        }
        _ => None,
    }
}

pub(crate) fn go_type_spec(node: &Node, name: &str, _source: &[u8]) -> Signature {
    let type_kind = node
        .child_by_field_name("type")
        .map(|n| n.kind().to_string())
        .unwrap_or_default();
    let kind = match type_kind.as_str() {
        "struct_type" => "struct",
        "interface_type" => "interface",
        _ => "type",
    };
    Signature {
        kind,
        name: name.to_string(),
        params: String::new(),
        return_type: String::new(),
        is_async: false,
        is_exported: name.starts_with(|c: char| c.is_uppercase()),
        indent: 0,
        ..Signature::no_span()
    }
}

pub(crate) fn java_constructor(node: &Node, name: &str, source: &[u8]) -> Signature {
    let params = field_text(node, "parameters", source);
    Signature {
        kind: "fn",
        name: name.to_string(),
        params: compact_params(&strip_parens(&params)),
        return_type: String::new(),
        is_async: false,
        is_exported: has_modifier(node, "public", source),
        indent: 2,
        ..Signature::no_span()
    }
}
