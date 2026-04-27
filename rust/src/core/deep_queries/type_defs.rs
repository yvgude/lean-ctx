//! Type definition, export, and generics extraction from AST nodes.

#[cfg(feature = "tree-sitter")]
use tree_sitter::Node;

#[cfg(feature = "tree-sitter")]
use super::types::{TypeDef, TypeDefKind};
#[cfg(feature = "tree-sitter")]
use super::{find_child_by_kind, node_text};

// ---------------------------------------------------------------------------
// Type Definitions
// ---------------------------------------------------------------------------

#[cfg(feature = "tree-sitter")]
pub(super) fn extract_types(root: Node, src: &str, ext: &str) -> Vec<TypeDef> {
    let mut types = Vec::new();
    walk_types(root, src, ext, &mut types, false);
    types
}

#[cfg(feature = "tree-sitter")]
fn walk_types(node: Node, src: &str, ext: &str, types: &mut Vec<TypeDef>, parent_exported: bool) {
    let exported = parent_exported || is_exported_node(node, src, ext);

    if let Some(td) = match_type_def(node, src, ext, exported) {
        types.push(td);
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_types(child, src, ext, types, exported);
    }
}

#[cfg(feature = "tree-sitter")]
fn match_type_def(node: Node, src: &str, ext: &str, parent_exported: bool) -> Option<TypeDef> {
    let (name, kind) = match ext {
        "ts" | "tsx" | "js" | "jsx" => match_type_def_ts(node, src)?,
        "rs" => match_type_def_rust(node, src)?,
        "py" => match_type_def_python(node, src)?,
        "go" => match_type_def_go(node, src)?,
        "java" => match_type_def_java(node, src)?,
        "kt" | "kts" => match_type_def_kotlin(node, src)?,
        _ => return None,
    };

    let is_exported = parent_exported || is_exported_node(node, src, ext);
    let generics = extract_generics(node, src);

    Some(TypeDef {
        name,
        kind,
        line: node.start_position().row + 1,
        end_line: node.end_position().row + 1,
        is_exported,
        generics,
    })
}

#[cfg(feature = "tree-sitter")]
fn match_type_def_ts(node: Node, src: &str) -> Option<(String, TypeDefKind)> {
    match node.kind() {
        "class_declaration" | "abstract_class_declaration" => {
            let name = find_child_by_kind(node, "type_identifier")
                .or_else(|| find_child_by_kind(node, "identifier"))?;
            Some((node_text(name, src).to_string(), TypeDefKind::Class))
        }
        "interface_declaration" => {
            let name = find_child_by_kind(node, "type_identifier")?;
            Some((node_text(name, src).to_string(), TypeDefKind::Interface))
        }
        "type_alias_declaration" => {
            let name = find_child_by_kind(node, "type_identifier")?;
            let text = node_text(node, src);
            let kind = if text.contains(" | ") {
                TypeDefKind::Union
            } else {
                TypeDefKind::TypeAlias
            };
            Some((node_text(name, src).to_string(), kind))
        }
        "enum_declaration" => {
            let name = find_child_by_kind(node, "identifier")?;
            Some((node_text(name, src).to_string(), TypeDefKind::Enum))
        }
        _ => None,
    }
}

#[cfg(feature = "tree-sitter")]
fn match_type_def_rust(node: Node, src: &str) -> Option<(String, TypeDefKind)> {
    match node.kind() {
        "struct_item" => {
            let name = find_child_by_kind(node, "type_identifier")?;
            Some((node_text(name, src).to_string(), TypeDefKind::Struct))
        }
        "enum_item" => {
            let name = find_child_by_kind(node, "type_identifier")?;
            Some((node_text(name, src).to_string(), TypeDefKind::Enum))
        }
        "trait_item" => {
            let name = find_child_by_kind(node, "type_identifier")?;
            Some((node_text(name, src).to_string(), TypeDefKind::Trait))
        }
        "type_item" => {
            let name = find_child_by_kind(node, "type_identifier")?;
            Some((node_text(name, src).to_string(), TypeDefKind::TypeAlias))
        }
        _ => None,
    }
}

#[cfg(feature = "tree-sitter")]
fn match_type_def_python(node: Node, src: &str) -> Option<(String, TypeDefKind)> {
    if node.kind() == "class_definition" {
        let name = find_child_by_kind(node, "identifier")?;
        let text = node_text(node, src);
        let kind = if text.contains("Protocol") {
            TypeDefKind::Protocol
        } else if text.contains("TypedDict") || text.contains("@dataclass") {
            TypeDefKind::Struct
        } else if text.contains("Enum") {
            TypeDefKind::Enum
        } else {
            TypeDefKind::Class
        };
        Some((node_text(name, src).to_string(), kind))
    } else {
        None
    }
}

#[cfg(feature = "tree-sitter")]
fn match_type_def_go(node: Node, src: &str) -> Option<(String, TypeDefKind)> {
    if node.kind() == "type_spec" {
        let name = find_child_by_kind(node, "type_identifier")?;
        let count = node.child_count();
        let type_body = node.child((count.saturating_sub(1)) as u32)?;
        let kind = match type_body.kind() {
            "struct_type" => TypeDefKind::Struct,
            "interface_type" => TypeDefKind::Interface,
            _ => TypeDefKind::TypeAlias,
        };
        Some((node_text(name, src).to_string(), kind))
    } else {
        None
    }
}

#[cfg(feature = "tree-sitter")]
fn match_type_def_java(node: Node, src: &str) -> Option<(String, TypeDefKind)> {
    match node.kind() {
        "class_declaration" => {
            let name = find_child_by_kind(node, "identifier")?;
            Some((node_text(name, src).to_string(), TypeDefKind::Class))
        }
        "interface_declaration" => {
            let name = find_child_by_kind(node, "identifier")?;
            Some((node_text(name, src).to_string(), TypeDefKind::Interface))
        }
        "enum_declaration" => {
            let name = find_child_by_kind(node, "identifier")?;
            Some((node_text(name, src).to_string(), TypeDefKind::Enum))
        }
        "record_declaration" => {
            let name = find_child_by_kind(node, "identifier")?;
            Some((node_text(name, src).to_string(), TypeDefKind::Record))
        }
        "annotation_type_declaration" => {
            let name = find_child_by_kind(node, "identifier")?;
            Some((node_text(name, src).to_string(), TypeDefKind::Annotation))
        }
        _ => None,
    }
}

#[cfg(feature = "tree-sitter")]
fn match_type_def_kotlin(node: Node, src: &str) -> Option<(String, TypeDefKind)> {
    match node.kind() {
        "class_declaration" => {
            let name = node
                .child_by_field_name("name")
                .or_else(|| find_child_by_kind(node, "identifier"))?;
            let text = node_text(node, src);
            let kind = if text.contains("interface") {
                TypeDefKind::Interface
            } else if text.contains("enum class") {
                TypeDefKind::Enum
            } else {
                TypeDefKind::Class
            };
            Some((node_text(name, src).to_string(), kind))
        }
        "object_declaration" => {
            let name = node
                .child_by_field_name("name")
                .or_else(|| find_child_by_kind(node, "identifier"))?;
            Some((node_text(name, src).to_string(), TypeDefKind::Class))
        }
        "type_alias" => {
            let name = node
                .child_by_field_name("type")
                .or_else(|| find_child_by_kind(node, "identifier"))?;
            Some((node_text(name, src).to_string(), TypeDefKind::TypeAlias))
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Exports
// ---------------------------------------------------------------------------

#[cfg(feature = "tree-sitter")]
pub(super) fn extract_exports(root: Node, src: &str, ext: &str) -> Vec<String> {
    let mut exports = Vec::new();
    walk_exports(root, src, ext, &mut exports);
    exports
}

#[cfg(feature = "tree-sitter")]
fn walk_exports(node: Node, src: &str, ext: &str, exports: &mut Vec<String>) {
    if is_exported_node(node, src, ext) {
        if let Some(name) = get_declaration_name(node, src) {
            exports.push(name);
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_exports(child, src, ext, exports);
    }
}

#[cfg(feature = "tree-sitter")]
fn is_exported_node(node: Node, src: &str, ext: &str) -> bool {
    match ext {
        "ts" | "tsx" | "js" | "jsx" => {
            node.kind() == "export_statement"
                || node
                    .parent()
                    .is_some_and(|p| p.kind() == "export_statement")
        }
        "rs" => node_text(node, src).trim_start().starts_with("pub "),
        "go" => {
            if let Some(name) = get_declaration_name(node, src) {
                name.starts_with(char::is_uppercase)
            } else {
                false
            }
        }
        "java" => node_text(node, src).trim_start().starts_with("public "),
        "kt" | "kts" => kotlin_declaration_exported(node, src),
        "py" => {
            if let Some(name) = get_declaration_name(node, src) {
                !name.starts_with('_')
            } else {
                false
            }
        }
        _ => false,
    }
}

#[cfg(feature = "tree-sitter")]
fn get_declaration_name(node: Node, src: &str) -> Option<String> {
    for kind in &[
        "identifier",
        "type_identifier",
        "property_identifier",
        "field_identifier",
    ] {
        if let Some(name_node) = find_child_by_kind(node, kind) {
            return Some(node_text(name_node, src).to_string());
        }
    }
    None
}

#[cfg(feature = "tree-sitter")]
fn kotlin_declaration_exported(node: Node, src: &str) -> bool {
    if let Some(modifiers) = find_child_by_kind(node, "modifiers") {
        !node_text(modifiers, src).contains("private")
    } else {
        !node_text(node, src).contains("private")
    }
}

#[cfg(feature = "tree-sitter")]
fn extract_generics(node: Node, src: &str) -> Vec<String> {
    let tp = find_child_by_kind(node, "type_parameters")
        .or_else(|| find_child_by_kind(node, "type_parameter_list"));
    match tp {
        Some(params) => {
            let mut result = Vec::new();
            let mut cursor = params.walk();
            for child in params.children(&mut cursor) {
                if child.kind() == "type_parameter"
                    || child.kind() == "type_identifier"
                    || child.kind() == "identifier"
                {
                    result.push(node_text(child, src).to_string());
                }
            }
            result
        }
        None => Vec::new(),
    }
}
