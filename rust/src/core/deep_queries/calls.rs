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
    crate::core::ast_walk::for_each_descendant(root, |node| {
        if is_call_node(node.kind())
            && let Some(call) = parse_call(node, src, ext)
        {
            calls.push(call);
        }
    });
    calls
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
            // C# method calls are `invocation_expression`; `new T(...)` reuses
            // `object_creation_expression` (shared with Java) above.
            | "invocation_expression"
            // GDScript method calls and `X.new()` instantiation are `attribute_call`.
            | "attribute_call"
            // Lua / Luau calls (direct, `t.f(...)` and `t:m(...)`) are `function_call`.
            | "function_call"
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
        "cs" => parse_call_csharp(node, src),
        "lua" | "luau" => parse_call_lua(node, src),
        _ => None,
    }
}

/// Lua / Luau call sites. The `function_call`'s `name` field is the callee:
/// - `f(...)`        -> callee `f`, no receiver
/// - `t.f(...)`      -> callee `f`, receiver `t` (dot access)
/// - `t:m(...)`      -> callee `m`, receiver `t`, method call
#[cfg(feature = "tree-sitter")]
fn parse_call_lua(node: Node, src: &str) -> Option<CallSite> {
    let line = node.start_position().row + 1;
    let col = node.start_position().column;
    let name = node.child_by_field_name("name")?;
    match name.kind() {
        "identifier" => Some(CallSite {
            callee: node_text(name, src).to_string(),
            line,
            col,
            receiver: None,
            is_method: false,
        }),
        "dot_index_expression" => {
            let field = name.child_by_field_name("field")?;
            let receiver = name
                .child_by_field_name("table")
                .map(|t| node_text(t, src).to_string());
            Some(CallSite {
                callee: node_text(field, src).to_string(),
                line,
                col,
                receiver,
                is_method: false,
            })
        }
        "method_index_expression" => {
            let method = name.child_by_field_name("method")?;
            let receiver = name
                .child_by_field_name("table")
                .map(|t| node_text(t, src).to_string());
            Some(CallSite {
                callee: node_text(method, src).to_string(),
                line,
                col,
                receiver,
                is_method: true,
            })
        }
        _ => None,
    }
}

/// C# call sites: `invocation_expression` for method/function calls and
/// `object_creation_expression` (`new T(...)`) for constructor calls.
///
/// - `foo(...)`            -> callee `foo`,  no receiver
/// - `obj.Method(...)`     -> callee `Method`, receiver `obj`, method call
/// - `A.B.Method(...)`     -> callee `Method`, receiver `A.B`,  method call
/// - `Method<T>(...)`      -> callee `Method` (generic name reduced to its identifier)
/// - `new Engine(...)`     -> callee `Engine` (the constructed type), no receiver
#[cfg(feature = "tree-sitter")]
fn parse_call_csharp(node: Node, src: &str) -> Option<CallSite> {
    let line = node.start_position().row + 1;
    let col = node.start_position().column;

    if node.kind() == "object_creation_expression" {
        // `new Foo(...)` / `new Foo<T>(...)` — the callee is the constructed type.
        let type_node = node.child_by_field_name("type")?;
        let callee = csharp_simple_name(type_node, src)?;
        return Some(CallSite {
            callee,
            line,
            col,
            receiver: None,
            is_method: false,
        });
    }

    // invocation_expression: the `function` field holds what is being called.
    let func = node.child_by_field_name("function")?;
    if func.kind() == "member_access_expression" {
        let name_node = func.child_by_field_name("name")?;
        let callee = csharp_simple_name(name_node, src)?;
        let receiver = func
            .child_by_field_name("expression")
            .map(|r| node_text(r, src).to_string());
        return Some(CallSite {
            callee,
            line,
            col,
            receiver,
            is_method: true,
        });
    }

    // Bare invocation: identifier / generic_name / qualified_name.
    let callee = csharp_simple_name(func, src)?;
    Some(CallSite {
        callee,
        line,
        col,
        receiver: None,
        is_method: false,
    })
}

/// Reduce a C# name node to its simple identifier:
/// `Foo` -> `Foo`, `Foo<T>` (`generic_name`) -> `Foo`, `A.B.Foo` (`qualified_name`) -> `Foo`.
/// Returns `None` for nameless constructs (e.g. `new int[]`) so they do not
/// pollute the call graph with junk callees.
#[cfg(feature = "tree-sitter")]
fn csharp_simple_name(node: Node, src: &str) -> Option<String> {
    match node.kind() {
        "identifier" => Some(node_text(node, src).to_string()),
        "generic_name" => {
            find_child_by_kind(node, "identifier").map(|id| node_text(id, src).to_string())
        }
        "qualified_name" | "alias_qualified_name" | "member_access_expression" => {
            let text = node_text(node, src);
            let last = text
                .rsplit(['.', ':'])
                .find(|seg| !seg.trim().is_empty())
                .unwrap_or(text)
                .trim();
            // Drop any generic argument suffix: `Foo<T>` -> `Foo`.
            let bare = last.split('<').next().unwrap_or(last).trim();
            if bare.is_empty() {
                None
            } else {
                Some(bare.to_string())
            }
        }
        // `object_creation_expression`'s `type` field wraps a concrete type node
        // (identifier / generic_name / qualified_name / predefined / array …).
        _ => find_child_by_kind(node, "identifier")
            .or_else(|| find_child_by_kind(node, "generic_name"))
            .or_else(|| find_child_by_kind(node, "qualified_name"))
            .and_then(|inner| csharp_simple_name(inner, src)),
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
            if method == "new"
                && let Some(class) = receiver
            {
                return Some(CallSite {
                    callee: class,
                    line,
                    col,
                    receiver: None,
                    is_method: false,
                });
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
