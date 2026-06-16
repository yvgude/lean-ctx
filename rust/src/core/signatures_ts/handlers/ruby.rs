use tree_sitter::Node;

use crate::core::signatures::{Signature, compact_params};

use super::super::helpers::{field_text, strip_parens};

pub(crate) fn ruby_method(node: &Node, name: &str, source: &[u8]) -> Signature {
    let params = field_text(node, "parameters", source);
    Signature {
        kind: "method",
        name: name.to_string(),
        params: compact_params(&strip_parens(&params)),
        return_type: String::new(),
        is_async: false,
        is_exported: true,
        indent: 2,
        ..Signature::no_span()
    }
}
