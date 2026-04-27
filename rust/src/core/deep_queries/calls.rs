//! Call-site extraction from AST nodes.

#[cfg(feature = "tree-sitter")]
use tree_sitter::Node;

#[cfg(feature = "tree-sitter")]
use super::types::CallSite;
#[cfg(feature = "tree-sitter")]
use super::{find_child_by_kind, find_descendant_by_kind, node_text};

#[cfg(feature = "tree-sitter")]
pub(super) fn extract_calls(root: Node, src: &str, ext: &str) -> Vec<CallSite> {
    let mut calls = Vec::new();
    walk_calls(root, src, ext, &mut calls);
    calls
}

#[cfg(feature = "tree-sitter")]
fn walk_calls(node: Node, src: &str, ext: &str, calls: &mut Vec<CallSite>) {
    if node.kind() == "call_expression" || node.kind() == "method_invocation" {
        if let Some(call) = parse_call(node, src, ext) {
            calls.push(call);
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_calls(child, src, ext, calls);
    }
}

#[cfg(feature = "tree-sitter")]
fn parse_call(node: Node, src: &str, ext: &str) -> Option<CallSite> {
    match ext {
        "ts" | "tsx" | "js" | "jsx" => parse_call_ts(node, src),
        "rs" => parse_call_rust(node, src),
        "py" => parse_call_python(node, src),
        "go" => parse_call_go(node, src),
        "java" => parse_call_java(node, src),
        "kt" | "kts" => parse_call_kotlin(node, src),
        _ => None,
    }
}

#[cfg(feature = "tree-sitter")]
fn parse_call_ts(node: Node, src: &str) -> Option<CallSite> {
    let func = find_child_by_kind(node, "member_expression")
        .or_else(|| find_child_by_kind(node, "identifier"))
        .or_else(|| find_child_by_kind(node, "subscript_expression"))?;

    if func.kind() == "member_expression" {
        let obj =
            find_child_by_kind(func, "identifier").or_else(|| find_child_by_kind(func, "this"))?;
        let prop = find_child_by_kind(func, "property_identifier")?;
        Some(CallSite {
            callee: node_text(prop, src).to_string(),
            line: node.start_position().row + 1,
            col: node.start_position().column,
            receiver: Some(node_text(obj, src).to_string()),
            is_method: true,
        })
    } else {
        Some(CallSite {
            callee: node_text(func, src).to_string(),
            line: node.start_position().row + 1,
            col: node.start_position().column,
            receiver: None,
            is_method: false,
        })
    }
}

#[cfg(feature = "tree-sitter")]
fn parse_call_rust(node: Node, src: &str) -> Option<CallSite> {
    let func = node.child(0)?;
    match func.kind() {
        "field_expression" => {
            let field = find_child_by_kind(func, "field_identifier")?;
            let receiver = func.child(0).map(|r| node_text(r, src).to_string());
            Some(CallSite {
                callee: node_text(field, src).to_string(),
                line: node.start_position().row + 1,
                col: node.start_position().column,
                receiver,
                is_method: true,
            })
        }
        "scoped_identifier" | "identifier" => Some(CallSite {
            callee: node_text(func, src).to_string(),
            line: node.start_position().row + 1,
            col: node.start_position().column,
            receiver: None,
            is_method: false,
        }),
        _ => None,
    }
}

#[cfg(feature = "tree-sitter")]
fn parse_call_python(node: Node, src: &str) -> Option<CallSite> {
    let func = node.child(0)?;
    match func.kind() {
        "attribute" => {
            let attr = find_child_by_kind(func, "identifier");
            let obj = func.child(0).map(|r| node_text(r, src).to_string());
            let name = attr
                .map(|a| node_text(a, src).to_string())
                .or_else(|| {
                    let text = node_text(func, src);
                    text.rsplit('.')
                        .next()
                        .map(std::string::ToString::to_string)
                })
                .unwrap_or_default();
            Some(CallSite {
                callee: name,
                line: node.start_position().row + 1,
                col: node.start_position().column,
                receiver: obj,
                is_method: true,
            })
        }
        "identifier" => Some(CallSite {
            callee: node_text(func, src).to_string(),
            line: node.start_position().row + 1,
            col: node.start_position().column,
            receiver: None,
            is_method: false,
        }),
        _ => None,
    }
}

#[cfg(feature = "tree-sitter")]
fn parse_call_go(node: Node, src: &str) -> Option<CallSite> {
    let func = node.child(0)?;
    match func.kind() {
        "selector_expression" => {
            let field = find_child_by_kind(func, "field_identifier")?;
            let obj = func.child(0).map(|r| node_text(r, src).to_string());
            Some(CallSite {
                callee: node_text(field, src).to_string(),
                line: node.start_position().row + 1,
                col: node.start_position().column,
                receiver: obj,
                is_method: true,
            })
        }
        "identifier" => Some(CallSite {
            callee: node_text(func, src).to_string(),
            line: node.start_position().row + 1,
            col: node.start_position().column,
            receiver: None,
            is_method: false,
        }),
        _ => None,
    }
}

#[cfg(feature = "tree-sitter")]
fn parse_call_java(node: Node, src: &str) -> Option<CallSite> {
    if node.kind() == "method_invocation" {
        let name = find_child_by_kind(node, "identifier")?;
        let obj = find_child_by_kind(node, "field_access")
            .or_else(|| {
                let first = node.child(0)?;
                if first.kind() == "identifier" && first.id() != name.id() {
                    Some(first)
                } else {
                    None
                }
            })
            .map(|o| node_text(o, src).to_string());
        return Some(CallSite {
            callee: node_text(name, src).to_string(),
            line: node.start_position().row + 1,
            col: node.start_position().column,
            receiver: obj,
            is_method: true,
        });
    }

    let func = node.child(0)?;
    Some(CallSite {
        callee: node_text(func, src).to_string(),
        line: node.start_position().row + 1,
        col: node.start_position().column,
        receiver: None,
        is_method: false,
    })
}

#[cfg(feature = "tree-sitter")]
fn parse_call_kotlin(node: Node, src: &str) -> Option<CallSite> {
    let callee = node.child(0)?;

    match callee.kind() {
        "identifier" => Some(CallSite {
            callee: node_text(callee, src).to_string(),
            line: node.start_position().row + 1,
            col: node.start_position().column,
            receiver: None,
            is_method: false,
        }),
        "navigation_expression" => {
            let mut cursor = callee.walk();
            let children: Vec<Node> = callee.children(&mut cursor).collect();
            let callee_name = children
                .iter()
                .rev()
                .find(|child| child.kind() == "identifier")
                .map(|child| node_text(*child, src).to_string())?;
            let receiver = children
                .iter()
                .find(|child| {
                    matches!(
                        child.kind(),
                        "expression"
                            | "primary_expression"
                            | "identifier"
                            | "navigation_expression"
                            | "this_expression"
                            | "super_expression"
                    )
                })
                .map(|child| node_text(*child, src).to_string())
                .filter(|text| text != &callee_name);

            Some(CallSite {
                callee: callee_name,
                line: node.start_position().row + 1,
                col: node.start_position().column,
                receiver,
                is_method: true,
            })
        }
        _ => find_descendant_by_kind(callee, "identifier").map(|name| CallSite {
            callee: node_text(name, src).to_string(),
            line: node.start_position().row + 1,
            col: node.start_position().column,
            receiver: None,
            is_method: false,
        }),
    }
}
