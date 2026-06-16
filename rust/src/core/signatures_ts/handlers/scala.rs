use tree_sitter::Node;

use crate::core::signatures::{Signature, compact_params};

use super::super::helpers::{clean_return_type, field_text, strip_parens};

pub(crate) fn scala_function(node: &Node, name: &str, source: &[u8]) -> Signature {
    let params = field_text(node, "parameters", source);
    let ret = field_text(node, "return_type", source);
    let is_method = node.start_position().column > 0;
    Signature {
        kind: if is_method { "method" } else { "fn" },
        name: name.to_string(),
        params: compact_params(&strip_parens(&params)),
        return_type: clean_return_type(&ret),
        is_async: false,
        is_exported: !name.starts_with('_'),
        indent: if is_method { 2 } else { 0 },
        ..Signature::no_span()
    }
}
