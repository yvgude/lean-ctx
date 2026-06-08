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
    if is_call_node(node.kind()) {
        if let Some(call) = parse_call(node, src, ext) {
            calls.push(call);
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_calls(child, src, ext, calls);
    }
}

/// Call-like AST node kinds across the supported tree-sitter grammars.
///
/// Different grammars name the invocation node differently: most use
/// `call_expression`, Java methods use `method_invocation` and constructors
/// use `object_creation_expression`, while Python (and Elixir) use a bare
/// `call`. Missing `call` here is what previously made Python call sites —
/// including class instantiation `Foo(...)` — invisible to the call graph.
#[cfg(feature = "tree-sitter")]
fn is_call_node(kind: &str) -> bool {
    matches!(
        kind,
        "call_expression"
            | "method_invocation"
            | "call"
            | "object_creation_expression"
            // GDScript method calls and `X.new()` instantiation are `attribute_call`.
            | "attribute_call"
    )
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
        "gd" => parse_call_gd(node, src),
        _ => None,
    }
}

#[cfg(feature = "tree-sitter")]
fn parse_call_gd(node: Node, src: &str) -> Option<CallSite> {
    let line = node.start_position().row + 1;
    let col = node.start_position().column;
    match node.kind() {
        // Direct call: `func(...)` / `preload(...)`.
        "call" => {
            let func = node.child(0)?;
            if func.kind() == "identifier" {
                Some(CallSite {
                    callee: node_text(func, src).to_string(),
                    line,
                    col,
                    receiver: None,
                    is_method: false,
                })
            } else {
                find_descendant_by_kind(func, "identifier").map(|id| CallSite {
                    callee: node_text(id, src).to_string(),
                    line,
                    col,
                    receiver: None,
                    is_method: false,
                })
            }
        }
        // Method call / instantiation: `receiver.method(...)` under an `attribute`.
        "attribute_call" => {
            let method_node = find_child_by_kind(node, "identifier")?;
            let method = node_text(method_node, src).to_string();
            let receiver = node
                .parent()
                .filter(|p| p.kind() == "attribute")
                .and_then(|p| p.child(0))
                .map(|r| node_text(r, src).to_string());
            // `X.new()` instantiates class `X`; attribute the reference to the
            // class so it registers in the call graph / dead-code analysis (#365).
            if method == "new" {
                if let Some(class) = receiver {
                    return Some(CallSite {
                        callee: class,
                        line,
                        col,
                        receiver: None,
                        is_method: false,
                    });
                }
            }
            Some(CallSite {
                callee: method,
                line,
                col,
                receiver,
                is_method: true,
            })
        }
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
            // `obj.method(...)` — the callee is the trailing `attribute` field,
            // not the leading `object`. Fall back to text parsing only if the
            // grammar fields are unavailable.
            let attr = func
                .child_by_field_name("attribute")
                .or_else(|| func.child_by_field_name("attr"));
            let obj = func
                .child_by_field_name("object")
                .or_else(|| func.child(0))
                .map(|r| node_text(r, src).to_string());
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
    if node.kind() == "object_creation_expression" {
        // `new Foo(...)` / `new Foo<T>(...)` — the callee is the constructed type.
        let type_node = node
            .child_by_field_name("type")
            .or_else(|| find_child_by_kind(node, "type_identifier"))
            .or_else(|| find_child_by_kind(node, "scoped_type_identifier"))
            .or_else(|| find_child_by_kind(node, "generic_type"))?;
        let name = find_descendant_by_kind(type_node, "type_identifier").unwrap_or(type_node);
        return Some(CallSite {
            callee: node_text(name, src).to_string(),
            line: node.start_position().row + 1,
            col: node.start_position().column,
            receiver: None,
            is_method: false,
        });
    }

    if node.kind() == "method_invocation" {
        // `obj.method(...)` — the callee is the `name` field, not the leading
        // `object` identifier (which would otherwise be picked up first).
        let name = node
            .child_by_field_name("name")
            .or_else(|| find_child_by_kind(node, "identifier"))?;
        let obj = node
            .child_by_field_name("object")
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
