mod queries;

use tree_sitter::{Node, Parser, Query, QueryCursor, StreamingIterator};

use super::signatures::Signature;
use queries::{get_language, get_query};

pub fn extract_signatures_ts(content: &str, file_ext: &str) -> Option<Vec<Signature>> {
    if matches!(file_ext, "svelte" | "vue") {
        return extract_sfc_signatures(content);
    }

    let language = get_language(file_ext)?;
    let query_src = get_query(file_ext)?;

    thread_local! {
        static PARSER: std::cell::RefCell<Parser> = std::cell::RefCell::new(Parser::new());
    }

    let tree = PARSER.with(|p| {
        let mut parser = p.borrow_mut();
        let _ = parser.set_language(&language);
        parser.parse(content, None)
    })?;
    let query = Query::new(&language, query_src).ok()?;

    let def_idx = find_capture_index(&query, "def")?;
    let name_idx = find_capture_index(&query, "name")?;

    let source = content.as_bytes();
    let mut sigs = Vec::new();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, tree.root_node(), source);

    while let Some(m) = matches.next() {
        let mut def_node: Option<Node> = None;
        let mut name_text = String::new();

        for cap in m.captures {
            if cap.index == def_idx {
                def_node = Some(cap.node);
            } else if cap.index == name_idx {
                if let Ok(text) = cap.node.utf8_text(source) {
                    name_text = text.to_string();
                }
            }
        }

        if let Some(node) = def_node {
            if !name_text.is_empty() {
                if let Some(sig) = node_to_signature(&node, &name_text, file_ext, source) {
                    sigs.push(sig);
                }
            }
        }
    }

    Some(sigs)
}

fn find_capture_index(query: &Query, name: &str) -> Option<u32> {
    query
        .capture_names()
        .iter()
        .position(|n| *n == name)
        .map(|i| i as u32)
}

fn node_to_signature(node: &Node, name: &str, ext: &str, source: &[u8]) -> Option<Signature> {
    let kind_str = node.kind();
    let start_col = node.start_position().column;

    let mut sig = match kind_str {
        "function_item" => Some(rust_function(node, name, source)),
        "struct_item" => Some(rust_struct_like(node, name, "struct")),
        "enum_item" => Some(rust_struct_like(node, name, "enum")),
        "trait_item" => Some(rust_struct_like(node, name, "trait")),
        "impl_item" => Some(rust_impl(node, name, source)),
        "type_item" => Some(rust_struct_like(node, name, "type")),
        "const_item" => Some(rust_const(node, name, source)),

        "function_declaration" => Some(match ext {
            "kt" | "kts" => kotlin_function(node, name, source),
            "swift" => swift_function(node, name, source),
            "zig" => zig_function(node, name, source),
            _ => ts_or_go_function(node, name, ext, source),
        }),
        "protocol_function_declaration" => Some(swift_protocol_function(node, name, source)),
        "function_definition" => Some(match ext {
            "sh" | "bash" => simple_def(name, "fn"),
            "scala" | "sc" => scala_function(node, name, source),
            _ => py_or_c_function(node, name, ext, start_col, source),
        }),
        "method_definition" => Some(ts_method(node, name, source)),
        "method_declaration" => go_or_java_method(node, name, ext, source),
        "variable_declarator" => ts_arrow_function(node, name, source),

        "class_declaration" | "abstract_class_declaration" | "class_specifier" => {
            if ext == "swift" {
                Some(swift_class_declaration(node, name, source))
            } else {
                Some(class_like(node, name, "class", ext, source))
            }
        }
        "object_declaration" | "record_declaration" => {
            Some(class_like(node, name, "class", ext, source))
        }
        "protocol_declaration" | "interface_declaration" => {
            Some(class_like(node, name, "interface", ext, source))
        }
        "trait_declaration" => Some(class_like(node, name, "trait", ext, source)),
        "namespace_declaration"
        | "namespace_definition"
        | "mixin_declaration"
        | "object_definition" => Some(simple_def(name, "class")),
        "struct_declaration" => Some(class_like(node, name, "struct", ext, source)),
        "class_definition" => Some(Signature {
            kind: "class",
            name: name.to_string(),
            params: String::new(),
            return_type: String::new(),
            is_async: false,
            is_exported: !name.starts_with('_'),
            indent: 0,
            ..Signature::no_span()
        }),
        "class" | "module" => Some(Signature {
            kind: "class",
            name: name.to_string(),
            params: String::new(),
            return_type: String::new(),
            is_async: false,
            is_exported: true,
            indent: 0,
            ..Signature::no_span()
        }),

        "struct_specifier" => Some(simple_def(name, "struct")),
        "type_alias_declaration" => Some(Signature {
            kind: "type",
            name: name.to_string(),
            params: String::new(),
            return_type: String::new(),
            is_async: false,
            is_exported: is_in_export(node),
            indent: 0,
            ..Signature::no_span()
        }),
        "type_definition" | "type_alias" => Some(simple_def(name, "type")),

        "enum_declaration" | "enum_specifier" | "enum_definition" => {
            let exported = match ext {
                "java" => has_modifier(node, "public", source),
                "cs" => csharp_has_modifier_text(node, "public", source),
                _ => true,
            };
            Some(Signature {
                kind: "enum",
                name: name.to_string(),
                params: String::new(),
                return_type: String::new(),
                is_async: false,
                is_exported: exported,
                indent: 0,
                ..Signature::no_span()
            })
        }

        "type_spec" => Some(go_type_spec(node, name, source)),

        "method" | "singleton_method" => Some(ruby_method(node, name, source)),
        "constructor_declaration" => Some(java_constructor(node, name, source)),

        // Scala
        "trait_definition" => Some(simple_def(name, "trait")),

        // Elixir — defmodule/def/defp are all `call` nodes
        "call" if ext == "ex" || ext == "exs" => elixir_call(node, name, source),

        _ => None,
    }?;

    if matches!(ext, "kt" | "kts") {
        sig.start_line = Some(node.start_position().row + 1);
        sig.end_line = Some(node.end_position().row + 1);
    }

    Some(sig)
}

// ---------------------------------------------------------------------------
// Rust handlers
// ---------------------------------------------------------------------------

fn rust_function(node: &Node, name: &str, source: &[u8]) -> Signature {
    let params = field_text(node, "parameters", source);
    let ret = field_text(node, "return_type", source);
    let exported = has_named_child(node, "visibility_modifier");
    let is_async = has_keyword_child(node, "async");
    let start_col = node.start_position().column;
    let is_method = start_col > 0;

    Signature {
        kind: if is_method { "method" } else { "fn" },
        name: name.to_string(),
        params: super::signatures::compact_params(&strip_parens(&params)),
        return_type: clean_return_type(&ret),
        is_async,
        is_exported: exported,
        indent: if is_method { 2 } else { 0 },
        ..Signature::no_span()
    }
}

fn rust_struct_like(node: &Node, name: &str, kind: &'static str) -> Signature {
    Signature {
        kind,
        name: name.to_string(),
        params: String::new(),
        return_type: String::new(),
        is_async: false,
        is_exported: has_named_child(node, "visibility_modifier"),
        indent: 0,
        ..Signature::no_span()
    }
}

fn rust_impl(node: &Node, name: &str, source: &[u8]) -> Signature {
    let trait_name = node
        .child_by_field_name("trait")
        .and_then(|n| n.utf8_text(source).ok())
        .map(std::string::ToString::to_string);
    let full_name = match trait_name {
        Some(t) => format!("{t} for {name}"),
        None => name.to_string(),
    };
    Signature {
        kind: "class",
        name: full_name,
        params: String::new(),
        return_type: String::new(),
        is_async: false,
        is_exported: false,
        indent: 0,
        ..Signature::no_span()
    }
}

fn rust_const(node: &Node, name: &str, source: &[u8]) -> Signature {
    let ret = field_text(node, "type", source);
    Signature {
        kind: "const",
        name: name.to_string(),
        params: String::new(),
        return_type: clean_return_type(&ret),
        is_async: false,
        is_exported: has_named_child(node, "visibility_modifier"),
        indent: 0,
        ..Signature::no_span()
    }
}

// ---------------------------------------------------------------------------
// TypeScript / JavaScript handlers
// ---------------------------------------------------------------------------

fn ts_or_go_function(node: &Node, name: &str, ext: &str, source: &[u8]) -> Signature {
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
        params: super::signatures::compact_params(&strip_parens(&params)),
        return_type: clean_return_type(&ret),
        is_async,
        is_exported: exported,
        indent: 0,
        ..Signature::no_span()
    }
}

fn ts_method(node: &Node, name: &str, source: &[u8]) -> Signature {
    let params = field_text(node, "parameters", source);
    let ret = field_text(node, "return_type", source);

    Signature {
        kind: "method",
        name: name.to_string(),
        params: super::signatures::compact_params(&strip_parens(&params)),
        return_type: clean_return_type(&ret),
        is_async: has_keyword_child(node, "async"),
        is_exported: false,
        indent: 2,
        ..Signature::no_span()
    }
}

fn ts_arrow_function(node: &Node, name: &str, source: &[u8]) -> Option<Signature> {
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
        params: super::signatures::compact_params(&strip_parens(&params)),
        return_type: clean_return_type(&ret),
        is_async: has_keyword_child(&arrow, "async"),
        is_exported: exported,
        indent: 0,
        ..Signature::no_span()
    })
}

// ---------------------------------------------------------------------------
// Python / C / C++ handlers
// ---------------------------------------------------------------------------

fn py_or_c_function(
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
                params: super::signatures::compact_params(&strip_parens(&params)),
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
                params: super::signatures::compact_params(&strip_parens(&params)),
                return_type: ret.trim().to_string(),
                is_async: false,
                is_exported: true,
                indent: 0,
                ..Signature::no_span()
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Go / Java handlers
// ---------------------------------------------------------------------------

fn go_or_java_method(node: &Node, name: &str, ext: &str, source: &[u8]) -> Option<Signature> {
    match ext {
        "go" => {
            let params = field_text(node, "parameters", source);
            let ret = field_text(node, "result", source);
            Some(Signature {
                kind: "method",
                name: name.to_string(),
                params: super::signatures::compact_params(&strip_parens(&params)),
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
                params: super::signatures::compact_params(&strip_parens(&params)),
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
                params: super::signatures::compact_params(&strip_parens(&params)),
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
                params: super::signatures::compact_params(&strip_parens(&params)),
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

fn go_type_spec(node: &Node, name: &str, _source: &[u8]) -> Signature {
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

fn java_constructor(node: &Node, name: &str, source: &[u8]) -> Signature {
    let params = field_text(node, "parameters", source);
    Signature {
        kind: "fn",
        name: name.to_string(),
        params: super::signatures::compact_params(&strip_parens(&params)),
        return_type: String::new(),
        is_async: false,
        is_exported: has_modifier(node, "public", source),
        indent: 2,
        ..Signature::no_span()
    }
}

// ---------------------------------------------------------------------------
// Ruby handler
// ---------------------------------------------------------------------------

fn ruby_method(node: &Node, name: &str, source: &[u8]) -> Signature {
    let params = field_text(node, "parameters", source);
    Signature {
        kind: "method",
        name: name.to_string(),
        params: super::signatures::compact_params(&strip_parens(&params)),
        return_type: String::new(),
        is_async: false,
        is_exported: true,
        indent: 2,
        ..Signature::no_span()
    }
}

// ---------------------------------------------------------------------------
// Kotlin (tree-sitter-kotlin-ng) / Swift / C# helpers
// ---------------------------------------------------------------------------

fn kotlin_function(node: &Node, name: &str, source: &[u8]) -> Signature {
    let mut params = String::new();
    let mut ret = String::new();
    let mut seen_params = false;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "function_value_parameters" {
            params = child.utf8_text(source).unwrap_or("").to_string();
            seen_params = true;
        } else if seen_params && child.kind() == "type" {
            ret = child.utf8_text(source).unwrap_or("").to_string();
            ret = ret.trim_start_matches(':').trim().to_string();
            break;
        }
    }
    let is_method = node.start_position().column > 0;
    let exported = kotlin_modifiers_text(node, source).is_none_or(|t| !t.contains("private"));
    Signature {
        kind: if is_method { "method" } else { "fn" },
        name: name.to_string(),
        params: super::signatures::compact_params(&strip_parens(&params)),
        return_type: clean_return_type(&ret),
        is_async: false,
        is_exported: exported,
        indent: if is_method { 2 } else { 0 },
        ..Signature::no_span()
    }
}

fn kotlin_modifiers_text<'a>(node: &Node, source: &'a [u8]) -> Option<&'a str> {
    let mut cursor = node.walk();
    for c in node.children(&mut cursor) {
        if c.kind() == "modifiers" {
            return c.utf8_text(source).ok();
        }
    }
    None
}

fn kotlin_declaration_exported(node: &Node, source: &[u8]) -> bool {
    kotlin_modifiers_text(node, source).is_none_or(|t| !t.contains("private"))
}

fn swift_function(node: &Node, name: &str, source: &[u8]) -> Signature {
    let ret = field_text(node, "return_type", source);
    let params = swift_parameters_before_body(node, source);
    let is_async = has_named_child(node, "async");
    let is_method = node.start_position().column > 0;
    Signature {
        kind: if is_method { "method" } else { "fn" },
        name: name.to_string(),
        params: super::signatures::compact_params(&strip_parens(&params)),
        return_type: clean_return_type(&ret),
        is_async,
        is_exported: true,
        indent: if is_method { 2 } else { 0 },
        ..Signature::no_span()
    }
}

fn swift_protocol_function(node: &Node, name: &str, source: &[u8]) -> Signature {
    let ret = field_text(node, "return_type", source);
    Signature {
        kind: "fn",
        name: name.to_string(),
        params: String::new(),
        return_type: clean_return_type(&ret),
        is_async: has_named_child(node, "async"),
        is_exported: true,
        indent: 2,
        ..Signature::no_span()
    }
}

fn swift_class_declaration(node: &Node, name: &str, source: &[u8]) -> Signature {
    let kind = node
        .child_by_field_name("declaration_kind")
        .and_then(|n| n.utf8_text(source).ok())
        .unwrap_or("class");
    let kind_static: &'static str = match kind {
        "struct" => "struct",
        "enum" => "enum",
        _ => "class",
    };
    Signature {
        kind: kind_static,
        name: name.to_string(),
        params: String::new(),
        return_type: String::new(),
        is_async: false,
        is_exported: true,
        indent: 0,
        ..Signature::no_span()
    }
}

fn swift_parameters_before_body(node: &Node, source: &[u8]) -> String {
    let end_byte = node
        .child_by_field_name("body")
        .map_or(usize::MAX, |b| b.start_byte());
    let mut parts: Vec<String> = Vec::new();
    fn walk(n: &Node, end_byte: usize, source: &[u8], parts: &mut Vec<String>) {
        if n.start_byte() >= end_byte {
            return;
        }
        if n.kind() == "parameter" {
            if let Ok(t) = n.utf8_text(source) {
                parts.push(t.to_string());
            }
        }
        let mut c = n.walk();
        for child in n.children(&mut c) {
            if child.start_byte() < end_byte {
                walk(&child, end_byte, source, parts);
            }
        }
    }
    walk(node, end_byte, source, &mut parts);
    if parts.is_empty() {
        String::new()
    } else {
        format!("({})", parts.join(", "))
    }
}

fn csharp_has_modifier_text(node: &Node, needle: &str, source: &[u8]) -> bool {
    let mut cursor = node.walk();
    for c in node.children(&mut cursor) {
        if c.kind() == "modifier" {
            if let Ok(t) = c.utf8_text(source) {
                if t.contains(needle) {
                    return true;
                }
            }
        }
    }
    false
}

// ---------------------------------------------------------------------------
// Scala handlers
// ---------------------------------------------------------------------------

fn scala_function(node: &Node, name: &str, source: &[u8]) -> Signature {
    let params = field_text(node, "parameters", source);
    let ret = field_text(node, "return_type", source);
    let is_method = node.start_position().column > 0;
    Signature {
        kind: if is_method { "method" } else { "fn" },
        name: name.to_string(),
        params: super::signatures::compact_params(&strip_parens(&params)),
        return_type: clean_return_type(&ret),
        is_async: false,
        is_exported: !name.starts_with('_'),
        indent: if is_method { 2 } else { 0 },
        ..Signature::no_span()
    }
}

// ---------------------------------------------------------------------------
// Elixir handlers
// ---------------------------------------------------------------------------

fn elixir_call(node: &Node, name: &str, source: &[u8]) -> Option<Signature> {
    let target = node
        .child_by_field_name("target")
        .and_then(|n| n.utf8_text(source).ok())
        .unwrap_or("");
    match target {
        "defmodule" | "defprotocol" => Some(Signature {
            kind: "class",
            name: name.to_string(),
            params: String::new(),
            return_type: String::new(),
            is_async: false,
            is_exported: true,
            indent: 0,
            ..Signature::no_span()
        }),
        "def" | "defmacro" | "defdelegate" | "defguard" => Some(Signature {
            kind: "fn",
            name: name.to_string(),
            params: String::new(),
            return_type: String::new(),
            is_async: false,
            is_exported: true,
            indent: 2,
            ..Signature::no_span()
        }),
        "defp" | "defmacrop" | "defguardp" => Some(Signature {
            kind: "fn",
            name: name.to_string(),
            params: String::new(),
            return_type: String::new(),
            is_async: false,
            is_exported: false,
            indent: 2,
            ..Signature::no_span()
        }),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Zig handlers
// ---------------------------------------------------------------------------

fn zig_function(node: &Node, name: &str, source: &[u8]) -> Signature {
    let params = field_text(node, "parameters", source);
    let ret = field_text(node, "return_type", source);
    let exported = has_keyword_child(node, "pub");
    Signature {
        kind: "fn",
        name: name.to_string(),
        params: super::signatures::compact_params(&strip_parens(&params)),
        return_type: clean_return_type(&ret),
        is_async: false,
        is_exported: exported,
        indent: 0,
        ..Signature::no_span()
    }
}

// ---------------------------------------------------------------------------
// SFC (Single File Component) support for Svelte/Vue
// ---------------------------------------------------------------------------

fn extract_sfc_signatures(content: &str) -> Option<Vec<Signature>> {
    let script_content = extract_script_block(content)?;
    let is_ts = content.contains("lang=\"ts\"") || content.contains("lang=\"typescript\"");
    let ext = if is_ts { "ts" } else { "js" };
    extract_signatures_ts(&script_content, ext)
}

fn extract_script_block(content: &str) -> Option<String> {
    let lower = content.to_lowercase();
    let start_tag_pos = lower.find("<script")?;
    let tag_end = content[start_tag_pos..].find('>')? + start_tag_pos + 1;
    let end_tag = "</script>";
    let end_pos = lower[tag_end..].find(end_tag)? + tag_end;
    let script = &content[tag_end..end_pos];
    if script.trim().is_empty() {
        return None;
    }
    Some(script.to_string())
}

// ---------------------------------------------------------------------------
// AST-aware pruning: keeps signatures + type defs, strips function bodies
// ---------------------------------------------------------------------------

pub fn ast_prune(content: &str, file_ext: &str) -> Option<String> {
    let language = get_language(file_ext)?;
    let query_src = get_query(file_ext)?;

    thread_local! {
        static PARSER: std::cell::RefCell<Parser> = std::cell::RefCell::new(Parser::new());
    }

    let tree = PARSER.with(|p| {
        let mut parser = p.borrow_mut();
        let _ = parser.set_language(&language);
        parser.parse(content, None)
    })?;
    let query = Query::new(&language, query_src).ok()?;

    let def_idx = find_capture_index(&query, "def")?;
    let source = content.as_bytes();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, tree.root_node(), source);

    let lines: Vec<&str> = content.lines().collect();
    let mut keep = vec![false; lines.len()];

    while let Some(m) = matches.next() {
        for cap in m.captures {
            if cap.index == def_idx {
                let node = cap.node;
                let sig_start = node.start_position().row;

                if let Some(body) = find_body_node(&node) {
                    let body_start = body.start_position().row;
                    for flag in keep
                        .iter_mut()
                        .skip(sig_start)
                        .take(body_start.min(sig_start + 3) - sig_start + 1)
                    {
                        *flag = true;
                    }
                    let body_end = body.end_position().row;
                    if body_end < lines.len() {
                        keep[body_end] = true;
                    }
                } else {
                    let end = node.end_position().row;
                    for flag in keep
                        .iter_mut()
                        .skip(sig_start)
                        .take(end.min(sig_start + 2) - sig_start + 1)
                    {
                        *flag = true;
                    }
                }
            }
        }
    }

    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() && i > 0 && i + 1 < lines.len() && keep.get(i + 1) == Some(&true) {
            keep[i] = true;
        }
        if is_import_line(trimmed, file_ext) {
            keep[i] = true;
        }
    }

    let mut result = Vec::new();
    let mut prev_kept = true;
    for (i, line) in lines.iter().enumerate() {
        if keep[i] {
            if !prev_kept {
                result.push("  // ...".to_string());
            }
            result.push(line.to_string());
            prev_kept = true;
        } else {
            prev_kept = false;
        }
    }

    Some(result.join("\n"))
}

fn find_body_node<'a>(node: &Node<'a>) -> Option<Node<'a>> {
    if let Some(body) = node.child_by_field_name("body") {
        return Some(body);
    }
    if let Some(block) = node.child_by_field_name("block") {
        return Some(block);
    }
    let mut cursor = node.walk();
    let result = node.children(&mut cursor).find(|c| {
        matches!(
            c.kind(),
            "block"
                | "compound_statement"
                | "function_body"
                | "class_body"
                | "declaration_list"
                | "enum_body"
                | "statement_block"
        )
    });
    result
}

fn is_import_line(trimmed: &str, ext: &str) -> bool {
    match ext {
        "rs" => trimmed.starts_with("use ") || trimmed.starts_with("mod "),
        "ts" | "tsx" | "js" | "jsx" => {
            trimmed.starts_with("import ") || trimmed.starts_with("export {")
        }
        "py" => trimmed.starts_with("import ") || trimmed.starts_with("from "),
        "go" => trimmed.starts_with("import ") || trimmed == "import (",
        "java" | "kt" | "kts" => trimmed.starts_with("import ") || trimmed.starts_with("package "),
        "c" | "h" | "cpp" | "hpp" => trimmed.starts_with("#include"),
        "cs" => trimmed.starts_with("using ") || trimmed.starts_with("namespace "),
        "rb" => trimmed.starts_with("require ") || trimmed.starts_with("require_relative "),
        "swift" => trimmed.starts_with("import "),
        "php" => trimmed.starts_with("use ") || trimmed.starts_with("namespace "),
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn class_like(node: &Node, name: &str, kind: &'static str, ext: &str, source: &[u8]) -> Signature {
    let exported = match ext {
        "ts" | "tsx" | "js" | "jsx" => is_in_export(node),
        "java" => has_modifier(node, "public", source),
        "cs" => csharp_has_modifier_text(node, "public", source),
        "kt" | "kts" => kotlin_declaration_exported(node, source),
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

fn simple_def(name: &str, kind: &'static str) -> Signature {
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

fn field_text(node: &Node, field: &str, source: &[u8]) -> String {
    node.child_by_field_name(field)
        .and_then(|n| n.utf8_text(source).ok())
        .unwrap_or("")
        .to_string()
}

fn strip_parens(s: &str) -> String {
    let s = s.trim();
    let s = s.strip_prefix('(').unwrap_or(s);
    let s = s.strip_suffix(')').unwrap_or(s);
    s.to_string()
}

fn clean_return_type(ret: &str) -> String {
    let ret = ret.trim();
    if ret.is_empty() {
        return String::new();
    }
    let ret = ret.strip_prefix("->").unwrap_or(ret).trim();
    let ret = ret.strip_prefix(':').unwrap_or(ret).trim();
    ret.to_string()
}

fn has_named_child(node: &Node, kind: &str) -> bool {
    let mut cursor = node.walk();
    let result = node.children(&mut cursor).any(|c| c.kind() == kind);
    result
}

fn has_keyword_child(node: &Node, keyword: &str) -> bool {
    let mut cursor = node.walk();
    let result = node
        .children(&mut cursor)
        .any(|c| !c.is_named() && c.kind() == keyword);
    result
}

fn is_in_export(node: &Node) -> bool {
    node.parent()
        .is_some_and(|p| p.kind() == "export_statement")
}

fn has_modifier(node: &Node, modifier: &str, source: &[u8]) -> bool {
    node.child_by_field_name("modifiers")
        .and_then(|m| m.utf8_text(source).ok())
        .is_some_and(|t| t.contains(modifier))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rust_signatures() {
        let src = r#"
pub struct Config {
    name: String,
}

pub enum Status {
    Active,
    Inactive,
}

pub trait Handler {
    fn handle(&self);
}

impl Handler for Config {
    fn handle(&self) {
        println!("handling");
    }
}

pub async fn process(input: &str) -> Result<String, Error> {
    Ok(input.to_string())
}

fn helper(x: i32) -> bool {
    x > 0
}
"#;
        let sigs = extract_signatures_ts(src, "rs").unwrap();
        assert!(sigs.len() >= 5, "expected >=5 sigs, got {}", sigs.len());

        let names: Vec<&str> = sigs.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"Config"));
        assert!(names.contains(&"Status"));
        assert!(names.contains(&"Handler"));
        assert!(names.contains(&"process"));
        assert!(names.contains(&"helper"));
    }

    #[test]
    fn test_typescript_signatures() {
        let src = r"
export function greet(name: string): string {
    return `Hello ${name}`;
}

export class UserService {
    async findUser(id: number): Promise<User> {
        return db.find(id);
    }
}

export interface Config {
    host: string;
    port: number;
}

export type UserId = string;

const handler = async (req: Request): Promise<Response> => {
    return new Response();
};
";
        let sigs = extract_signatures_ts(src, "ts").unwrap();
        assert!(sigs.len() >= 5, "expected >=5 sigs, got {}", sigs.len());

        let names: Vec<&str> = sigs.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"greet"));
        assert!(names.contains(&"UserService"));
        assert!(names.contains(&"Config"));
        assert!(names.contains(&"UserId"));
        assert!(names.contains(&"handler"));
    }

    #[test]
    fn test_python_signatures() {
        let src = r"
class AuthService:
    def __init__(self, db):
        self.db = db

    async def authenticate(self, email: str, password: str) -> bool:
        user = await self.db.find(email)
        return check(user, password)

def create_app() -> Flask:
    return Flask(__name__)

def _internal_helper(x):
    return x * 2
";
        let sigs = extract_signatures_ts(src, "py").unwrap();
        assert!(sigs.len() >= 4, "expected >=4 sigs, got {}", sigs.len());

        let names: Vec<&str> = sigs.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"AuthService"));
        assert!(names.contains(&"authenticate"));
        assert!(names.contains(&"create_app"));

        let auth = sigs.iter().find(|s| s.name == "authenticate").unwrap();
        assert!(auth.is_async);
        assert_eq!(auth.kind, "method");

        let helper = sigs.iter().find(|s| s.name == "_internal_helper").unwrap();
        assert!(!helper.is_exported);
    }

    #[test]
    fn test_go_signatures() {
        let src = r"
package main

type Config struct {
    Host string
    Port int
}

type Handler interface {
    Handle() error
}

func NewConfig(host string, port int) *Config {
    return &Config{Host: host, Port: port}
}

func (c *Config) Validate() error {
    return nil
}

func helper() {
}
";
        let sigs = extract_signatures_ts(src, "go").unwrap();
        assert!(sigs.len() >= 4, "expected >=4 sigs, got {}", sigs.len());

        let names: Vec<&str> = sigs.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"Config"));
        assert!(names.contains(&"Handler"));
        assert!(names.contains(&"NewConfig"));
        assert!(names.contains(&"Validate"));

        let nc = sigs.iter().find(|s| s.name == "NewConfig").unwrap();
        assert!(nc.is_exported);

        let h = sigs.iter().find(|s| s.name == "helper").unwrap();
        assert!(!h.is_exported);
    }

    #[test]
    fn test_java_signatures() {
        let src = r"
public class UserController {
    public UserController(UserService service) {
        this.service = service;
    }

    public User getUser(int id) {
        return service.findById(id);
    }

    private void validate(User user) {
        // validation logic
    }
}

public interface Repository {
    User findById(int id);
}

public enum Role {
    ADMIN,
    USER
}
";
        let sigs = extract_signatures_ts(src, "java").unwrap();
        assert!(sigs.len() >= 4, "expected >=4 sigs, got {}", sigs.len());

        let names: Vec<&str> = sigs.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"UserController"));
        assert!(names.contains(&"getUser"));
        assert!(names.contains(&"Repository"));
        assert!(names.contains(&"Role"));
    }

    #[test]
    fn test_c_signatures() {
        let src = r"
typedef unsigned int uint;

struct Config {
    char* host;
    int port;
};

enum Status {
    ACTIVE,
    INACTIVE
};

int process(const char* input, int len) {
    return 0;
}

void cleanup(struct Config* cfg) {
    free(cfg);
}
";
        let sigs = extract_signatures_ts(src, "c").unwrap();
        assert!(sigs.len() >= 3, "expected >=3 sigs, got {}", sigs.len());

        let names: Vec<&str> = sigs.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"process"));
        assert!(names.contains(&"cleanup"));
    }

    #[test]
    fn test_ruby_signatures() {
        let src = r"
module Authentication
  class UserService
    def initialize(db)
      @db = db
    end

    def authenticate(email, password)
      user = @db.find(email)
      user&.check(password)
    end

    def self.create(config)
      new(config[:db])
    end
  end
end
";
        let sigs = extract_signatures_ts(src, "rb").unwrap();
        assert!(sigs.len() >= 3, "expected >=3 sigs, got {}", sigs.len());

        let names: Vec<&str> = sigs.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"UserService"));
        assert!(names.contains(&"authenticate"));
    }

    #[test]
    fn test_multiline_rust_signature() {
        let src = r"
pub fn complex_function<T: Display + Debug>(
    first_arg: &str,
    second_arg: Vec<T>,
    third_arg: Option<HashMap<String, Vec<u8>>>,
) -> Result<(), Box<dyn Error>> {
    Ok(())
}
";
        let sigs = extract_signatures_ts(src, "rs").unwrap();
        assert!(!sigs.is_empty(), "should parse multiline function");
        assert_eq!(sigs[0].name, "complex_function");
        assert!(sigs[0].is_exported);
    }

    #[test]
    fn test_arrow_function_ts() {
        let src = r"
export const fetchData = async (url: string): Promise<Response> => {
    return fetch(url);
};

const internal = (x: number) => x * 2;
";
        let sigs = extract_signatures_ts(src, "ts").unwrap();
        assert!(sigs.len() >= 2, "expected >=2 sigs, got {}", sigs.len());

        let fetch = sigs.iter().find(|s| s.name == "fetchData").unwrap();
        assert!(fetch.is_async);
        assert!(fetch.is_exported);
        assert_eq!(fetch.kind, "fn");

        let internal = sigs.iter().find(|s| s.name == "internal").unwrap();
        assert!(!internal.is_exported);
    }

    #[test]
    fn test_csharp_signatures() {
        let src = r"
namespace Demo;
public record Person(string Name);
public interface IRepo { void Save(); }
public struct Point { public int X; }
public enum Role { Admin, User }
public class Service {
    public string Hello(string name) => name;
}
";
        let sigs = extract_signatures_ts(src, "cs").unwrap();
        let names: Vec<&str> = sigs.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"Person"), "got {names:?}");
        assert!(names.contains(&"IRepo"));
        assert!(names.contains(&"Point"));
        assert!(names.contains(&"Role"));
        assert!(names.contains(&"Service"));
        assert!(names.contains(&"Hello"));
    }

    #[test]
    fn test_kotlin_signatures() {
        let src = r#"
class UserService {
    fun greet(name: String): String = "Hi $name"
}
object Factory {
    fun build(): UserService = UserService()
}
interface Handler {
    fun handle()
}
"#;
        let sigs = extract_signatures_ts(src, "kt").unwrap();
        let names: Vec<&str> = sigs.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"UserService"), "got {names:?}");
        assert!(names.contains(&"Factory"));
        assert!(names.contains(&"Handler"));
        assert!(names.contains(&"greet"));
        assert!(names.contains(&"build"));
        assert!(names.contains(&"handle"));
    }

    #[test]
    fn test_kotlin_signature_spans() {
        let src = r#"
class Service {
    suspend fun release(id: String): Boolean =
        repository.release(id)

    fun block_body(name: String): String {
        return "ok $name"
    }
}
"#;
        let sigs = extract_signatures_ts(src, "kt").unwrap();

        let release = sigs.iter().find(|s| s.name == "release").unwrap();
        assert_eq!(release.start_line, Some(3));
        assert_eq!(release.end_line, Some(4));

        let block_body = sigs.iter().find(|s| s.name == "block_body").unwrap();
        assert_eq!(block_body.start_line, Some(6));
        assert_eq!(block_body.end_line, Some(8));
    }

    #[test]
    fn test_swift_signatures() {
        let src = r"
class Box {
    func size() -> Int { 0 }
}
struct Point {
    var x: Int
}
enum Kind { case a, b }
protocol Drawable {
    func draw()
}
func topLevel() {}
";
        let sigs = extract_signatures_ts(src, "swift").unwrap();
        let names: Vec<&str> = sigs.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"Box"), "got {names:?}");
        assert!(names.contains(&"Point"));
        assert!(names.contains(&"Kind"));
        assert!(names.contains(&"Drawable"));
        assert!(names.contains(&"size"));
        assert!(names.contains(&"draw"));
        assert!(names.contains(&"topLevel"));
    }

    #[test]
    fn test_php_signatures() {
        let src = r"<?php
function helper(int $x): int { return $x; }
class User {
    public function name(): string { return ''; }
}
interface IAuth { public function check(): bool; }
trait Loggable { function log(): void {} }
";
        let sigs = extract_signatures_ts(src, "php").unwrap();
        let names: Vec<&str> = sigs.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"helper"), "got {names:?}");
        assert!(names.contains(&"User"));
        assert!(names.contains(&"name"));
        assert!(names.contains(&"IAuth"));
        assert!(names.contains(&"check"));
        assert!(names.contains(&"Loggable"));
        assert!(names.contains(&"log"));
    }

    #[test]
    fn test_unsupported_extension_returns_none() {
        let sigs = extract_signatures_ts("some content", "xyz");
        assert!(sigs.is_none());
    }

    #[test]
    fn test_bash_signatures() {
        let src = r#"
greet() {
    echo "Hello $1"
}

function cleanup {
    rm -rf /tmp/build
}

function deploy() {
    echo "deploying"
}
"#;
        let sigs = extract_signatures_ts(src, "sh").unwrap();
        assert!(sigs.len() >= 2, "expected >=2 sigs, got {}", sigs.len());
        let names: Vec<&str> = sigs.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"greet"), "got {names:?}");
        assert!(names.contains(&"cleanup"), "got {names:?}");
    }

    #[test]
    fn test_dart_signatures() {
        let src = r"
class UserService {
  Future<User> getUser(int id) async {
    return db.find(id);
  }
}

enum Status { active, inactive }

mixin Logging {
  void log(String msg) => print(msg);
}

typedef JsonMap = Map<String, dynamic>;
";
        let sigs = extract_signatures_ts(src, "dart").unwrap();
        let names: Vec<&str> = sigs.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"UserService"), "got {names:?}");
        assert!(names.contains(&"Status"), "got {names:?}");
        assert!(names.contains(&"Logging"), "got {names:?}");
    }

    #[test]
    fn test_scala_signatures() {
        let src = r"
package example

trait Handler {
  def handle(): Unit
}

class UserService(db: Database) {
  def findUser(id: Int): Option[User] = db.find(id)
  private def validate(user: User): Boolean = true
}

object Factory {
  def create(): UserService = new UserService(db)
}

enum Color:
  case Red, Green, Blue

type UserId = String
";
        let sigs = extract_signatures_ts(src, "scala").unwrap();
        let names: Vec<&str> = sigs.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"Handler"), "got {names:?}");
        assert!(names.contains(&"UserService"), "got {names:?}");
        assert!(names.contains(&"Factory"), "got {names:?}");
        assert!(names.contains(&"findUser"), "got {names:?}");
    }

    #[test]
    fn test_elixir_signatures() {
        let src = r"
defmodule MyApp.UserService do
  def get_user(id) do
    Repo.get(User, id)
  end

  defp validate(user) do
    user.valid?
  end

  defmacro trace(expr) do
    quote do: IO.inspect(unquote(expr))
  end
end

defprotocol Printable do
  def print(data)
end
";
        let sigs = extract_signatures_ts(src, "ex").unwrap();
        let names: Vec<&str> = sigs.iter().map(|s| s.name.as_str()).collect();
        assert!(
            names.contains(&"MyApp.UserService") || names.contains(&"UserService"),
            "got {names:?}"
        );
    }

    #[test]
    fn test_svelte_signatures() {
        let src = r#"
<script lang="ts">
export function greet(name: string): string {
    return `Hello ${name}`;
}

export class Counter {
    count = 0;
    increment() { this.count++; }
}
</script>

<div>{greeting}</div>
"#;
        let sigs = extract_signatures_ts(src, "svelte").unwrap();
        let names: Vec<&str> = sigs.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"greet"), "got {names:?}");
        assert!(names.contains(&"Counter"), "got {names:?}");
    }

    #[test]
    fn test_vue_signatures() {
        let src = r"
<template>
  <div>{{ msg }}</div>
</template>

<script>
export default {
  name: 'MyComponent'
}

export function helper(x) {
    return x * 2;
}

export class DataService {
    fetch() { return []; }
}
</script>
";
        let sigs = extract_signatures_ts(src, "vue").unwrap();
        let names: Vec<&str> = sigs.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"helper"), "got {names:?}");
        assert!(names.contains(&"DataService"), "got {names:?}");
    }

    #[test]
    fn test_zig_signatures() {
        let src = r#"
const std = @import("std");

pub fn init(allocator: std.mem.Allocator) !*Self {
    return allocator.create(Self);
}

fn helper(x: u32) u32 {
    return x * 2;
}

pub fn main() !void {
    std.debug.print("hello\n", .{});
}
"#;
        let sigs = extract_signatures_ts(src, "zig").unwrap();
        let names: Vec<&str> = sigs.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"init"), "got {names:?}");
        assert!(names.contains(&"helper"), "got {names:?}");
        assert!(names.contains(&"main"), "got {names:?}");

        let init_sig = sigs.iter().find(|s| s.name == "init").unwrap();
        assert!(init_sig.is_exported);
        let helper_sig = sigs.iter().find(|s| s.name == "helper").unwrap();
        assert!(!helper_sig.is_exported);
    }
}
