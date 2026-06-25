use tree_sitter::{Node, Parser, Query, QueryCursor, StreamingIterator};

use crate::core::signatures::Signature;

use super::handlers::{
    csharp_has_modifier_text, elixir_call, gdscript_function, gdscript_signal, gdscript_variable,
    go_or_java_method, go_type_spec, java_constructor, kotlin_function, lua_assigned_function,
    lua_function, luau_type, py_or_c_function, ruby_method, rust_const, rust_function, rust_impl,
    rust_struct_like, scala_function, swift_class_declaration, swift_function,
    swift_protocol_function, ts_arrow_function, ts_method, ts_or_go_function, zig_function,
};
use super::helpers::{class_like, has_modifier, is_in_export, simple_def};
use super::queries::get_language;
use super::query_cache::get_cached_sig_query;
use super::sfc::extract_sfc_signatures;

#[must_use]
pub fn extract_signatures_ts(content: &str, file_ext: &str) -> Option<Vec<Signature>> {
    if matches!(file_ext, "svelte" | "vue") {
        return extract_sfc_signatures(content);
    }

    let language = get_language(file_ext)?;

    thread_local! {
        static PARSER: std::cell::RefCell<Parser> = std::cell::RefCell::new(Parser::new());
    }

    let tree = PARSER.with(|p| {
        let mut parser = p.borrow_mut();
        let _ = parser.set_language(&language);
        parser.parse(content, None)
    })?;
    let query = get_cached_sig_query(file_ext)?;

    let def_idx = find_capture_index(query, "def")?;
    let name_idx = find_capture_index(query, "name")?;

    let source = content.as_bytes();
    let mut sigs = Vec::new();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, tree.root_node(), source);

    while let Some(m) = matches.next() {
        let mut def_node: Option<Node> = None;
        let mut name_text = String::new();

        for cap in m.captures {
            if cap.index == def_idx {
                def_node = Some(cap.node);
            } else if cap.index == name_idx
                && let Ok(text) = cap.node.utf8_text(source)
            {
                name_text = text.to_string();
            }
        }

        if let Some(node) = def_node
            && !name_text.is_empty()
            && let Some(sig) = node_to_signature(&node, &name_text, file_ext, source)
        {
            sigs.push(sig);
        }
    }

    Some(sigs)
}

pub(crate) fn find_capture_index(query: &Query, name: &str) -> Option<u32> {
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
            "lua" | "luau" => lua_function(node, name, source),
            _ => ts_or_go_function(node, name, ext, source),
        }),
        "assignment_statement" if ext == "lua" || ext == "luau" => {
            Some(lua_assigned_function(node, name, source))
        }
        "protocol_function_declaration" => Some(swift_protocol_function(node, name, source)),
        "function_definition" => Some(match ext {
            "sh" | "bash" => simple_def(name, "fn"),
            "scala" | "sc" => scala_function(node, name, source),
            "gd" => gdscript_function(node, name, source),
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
        "signal_statement" => Some(gdscript_signal(node, name, source)),
        "const_statement"
        | "variable_statement"
        | "export_variable_statement"
        | "onready_variable_statement"
            if ext == "gd" =>
        {
            Some(gdscript_variable(node, name))
        }
        // GDScript `class_name X` declares the script's globally-registered,
        // always-public class (same shape as a top-level `class`/`module`).
        "class_name_statement" | "class" | "module" => Some(Signature {
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
        "type_definition" | "type_alias" => Some(match ext {
            "luau" => luau_type(node, name, source),
            _ => simple_def(name, "type"),
        }),

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

        "trait_definition" => Some(simple_def(name, "trait")),

        "call" if ext == "ex" || ext == "exs" => elixir_call(node, name, source),

        _ => None,
    }?;

    sig.start_line = Some(node.start_position().row + 1);
    sig.end_line = Some(node.end_position().row + 1);
    sig.minhash = crate::core::minhash::compute_minhash(node);

    Some(sig)
}
