use tree_sitter::Node;

use crate::core::signatures::Signature;

pub(crate) fn class_like(
    node: &Node,
    name: &str,
    kind: &'static str,
    ext: &str,
    source: &[u8],
) -> Signature {
    let exported = match ext {
        "ts" | "tsx" | "js" | "jsx" => is_in_export(node),
        "java" => has_modifier(node, "public", source),
        "cs" => super::handlers::csharp_has_modifier_text(node, "public", source),
        "kt" | "kts" => super::handlers::kotlin_declaration_exported(node, source),
        _ => true,
    };
    Signature {
        kind,
        name: name.to_string(),
        params: String::new(),
        return_type: String::new(),
        is_async: false,
        is_exported: exported,
        indent: 0,
        ..Signature::no_span()
    }
}

pub(crate) fn simple_def(name: &str, kind: &'static str) -> Signature {
    Signature {
        kind,
        name: name.to_string(),
        params: String::new(),
        return_type: String::new(),
        is_async: false,
        is_exported: true,
        indent: 0,
        ..Signature::no_span()
    }
}

pub(crate) fn field_text(node: &Node, field: &str, source: &[u8]) -> String {
    node.child_by_field_name(field)
        .and_then(|n| n.utf8_text(source).ok())
        .unwrap_or("")
        .to_string()
}

pub(crate) fn strip_parens(s: &str) -> String {
    let s = s.trim();
    let s = s.strip_prefix('(').unwrap_or(s);
    let s = s.strip_suffix(')').unwrap_or(s);
    s.to_string()
}

pub(crate) fn clean_return_type(ret: &str) -> String {
    let ret = ret.trim();
    if ret.is_empty() {
        return String::new();
    }
    let ret = ret.strip_prefix("->").unwrap_or(ret).trim();
    let ret = ret.strip_prefix(':').unwrap_or(ret).trim();
    ret.to_string()
}

pub(crate) fn has_named_child(node: &Node, kind: &str) -> bool {
    let mut cursor = node.walk();

    node.children(&mut cursor).any(|c| c.kind() == kind)
}

pub(crate) fn has_keyword_child(node: &Node, keyword: &str) -> bool {
    let mut cursor = node.walk();

    node.children(&mut cursor)
        .any(|c| !c.is_named() && c.kind() == keyword)
}

pub(crate) fn is_in_export(node: &Node) -> bool {
    node.parent()
        .is_some_and(|p| p.kind() == "export_statement")
}

pub(crate) fn has_modifier(node: &Node, modifier: &str, source: &[u8]) -> bool {
    node.child_by_field_name("modifiers")
        .and_then(|m| m.utf8_text(source).ok())
        .is_some_and(|t| t.contains(modifier))
}
