use tree_sitter::Node;

use crate::core::signatures::{Signature, compact_params};

use super::super::helpers::{
    clean_return_type, field_text, has_keyword_child, is_in_export, strip_parens,
};

pub(crate) fn ts_or_go_function(node: &Node, name: &str, ext: &str, source: &[u8]) -> Signature {
    let params = field_text(node, "parameters", source);
    let (ret, exported, is_async) = match ext {
        "go" => (
            field_text(node, "result", source),
            name.starts_with(|c: char| c.is_uppercase()),
            false,
        ),
        _ => (
            field_text(node, "return_type", source),
            is_in_export(node),
            has_keyword_child(node, "async"),
        ),
    };

    Signature {
        kind: "fn",
        name: name.to_string(),
        params: compact_params(&strip_parens(&params)),
        return_type: clean_return_type(&ret),
        is_async,
        is_exported: exported,
        indent: 0,
        ..Signature::no_span()
    }
}

pub(crate) fn ts_method(node: &Node, name: &str, source: &[u8]) -> Signature {
    let params = field_text(node, "parameters", source);
    let ret = field_text(node, "return_type", source);

    Signature {
        kind: "method",
        name: name.to_string(),
        params: compact_params(&strip_parens(&params)),
        return_type: clean_return_type(&ret),
        is_async: has_keyword_child(node, "async"),
        is_exported: false,
        indent: 2,
        ..Signature::no_span()
    }
}

pub(crate) fn ts_arrow_function(node: &Node, name: &str, source: &[u8]) -> Option<Signature> {
    let arrow = node.child_by_field_name("value")?;
    let params = field_text(&arrow, "parameters", source);
    let ret = field_text(&arrow, "return_type", source);
    let exported = node
        .parent()
        .and_then(|p| p.parent())
        .is_some_and(|gp| gp.kind() == "export_statement");

    Some(Signature {
        kind: "fn",
        name: name.to_string(),
        params: compact_params(&strip_parens(&params)),
        return_type: clean_return_type(&ret),
        is_async: has_keyword_child(&arrow, "async"),
        is_exported: exported,
        indent: 0,
        ..Signature::no_span()
    })
}
