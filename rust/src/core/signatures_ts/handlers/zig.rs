use tree_sitter::Node;

use crate::core::signatures::{Signature, compact_params};

use super::super::helpers::{clean_return_type, field_text, has_keyword_child, strip_parens};

pub(crate) fn zig_function(node: &Node, name: &str, source: &[u8]) -> Signature {
    let params = field_text(node, "parameters", source);
    let ret = field_text(node, "return_type", source);
    let exported = has_keyword_child(node, "pub");
    Signature {
        kind: "fn",
        name: name.to_string(),
        params: compact_params(&strip_parens(&params)),
        return_type: clean_return_type(&ret),
        is_async: false,
        is_exported: exported,
        indent: 0,
        ..Signature::no_span()
    }
}
