use tree_sitter::Node;

use crate::core::signatures::{Signature, compact_params};

use super::super::helpers::{clean_return_type, field_text, has_keyword_child, strip_parens};

pub(crate) fn py_or_c_function(
    node: &Node,
    name: &str,
    ext: &str,
    start_col: usize,
    source: &[u8],
) -> Signature {
    match ext {
        "py" | "php" => {
            let params = field_text(node, "parameters", source);
            let ret = field_text(node, "return_type", source);
            let is_method = start_col > 0;
            Signature {
                kind: if is_method { "method" } else { "fn" },
                name: name.to_string(),
                params: compact_params(&strip_parens(&params)),
                return_type: clean_return_type(&ret),
                is_async: has_keyword_child(node, "async"),
                is_exported: !name.starts_with('_'),
                indent: if is_method { 2 } else { 0 },
                ..Signature::no_span()
            }
        }
        _ => {
            let ret = field_text(node, "type", source);
            let params = node
                .child_by_field_name("declarator")
                .and_then(|d| d.child_by_field_name("parameters"))
                .and_then(|p| p.utf8_text(source).ok())
                .unwrap_or("")
                .to_string();
            Signature {
                kind: "fn",
                name: name.to_string(),
                params: compact_params(&strip_parens(&params)),
                return_type: ret.trim().to_string(),
                is_async: false,
                is_exported: true,
                indent: 0,
                ..Signature::no_span()
            }
        }
    }
}
