//! Import extraction from AST nodes for all supported languages.

#[cfg(feature = "tree-sitter")]
use tree_sitter::Node;

#[cfg(feature = "tree-sitter")]
use super::types::{ImportInfo, ImportKind};
#[cfg(feature = "tree-sitter")]
use super::{find_child_by_kind, find_descendant_by_kind, node_text};

#[cfg(feature = "tree-sitter")]
pub(super) fn extract_imports(root: Node, src: &str, ext: &str) -> Vec<ImportInfo> {
    match ext {
        "ts" | "tsx" | "js" | "jsx" => extract_imports_ts(root, src),
        "rs" => extract_imports_rust(root, src),
        "py" => extract_imports_python(root, src),
        "go" => extract_imports_go(root, src),
        "java" => extract_imports_java(root, src),
        "c" | "h" | "cpp" | "cc" | "cxx" | "hpp" | "hxx" | "hh" => {
            extract_imports_c_like(root, src)
        }
        "rb" => extract_imports_ruby(root, src),
        "cs" => extract_imports_csharp(root, src),
        "kt" | "kts" => extract_imports_kotlin(root, src),
        "swift" => extract_imports_swift(root, src),
        "php" => extract_imports_php(root, src),
        "sh" | "bash" => extract_imports_bash(root, src),
        "dart" => extract_imports_dart(root, src),
        "scala" | "sc" => extract_imports_scala(root, src),
        "ex" | "exs" => extract_imports_elixir(root, src),
        "zig" => extract_imports_zig(root, src),
        "gd" => extract_imports_gd(root, src),
        "lua" | "luau" => extract_imports_lua(root, src),
        _ => Vec::new(),
    }
}

/// Lua / Luau dependencies: `require("a.b")`, `require "a.b"` and
/// `require('a/b')`. `require` is a plain function call (parens optional for a
/// single string literal), so the whole tree is scanned for it.
#[cfg(feature = "tree-sitter")]
fn extract_imports_lua(root: Node, src: &str) -> Vec<ImportInfo> {
    let mut imports = Vec::new();
    crate::core::ast_walk::for_each_descendant(root, |node| {
        if node.kind() != "function_call" {
            return;
        }
        let Some(callee) = node.child_by_field_name("name") else {
            return;
        };
        if callee.kind() != "identifier" || node_text(callee, src) != "require" {
            return;
        }
        let Some(args) = node.child_by_field_name("arguments") else {
            return;
        };
        if let Some(s) = find_descendant_by_kind(args, "string") {
            let source_text = unquote(node_text(s, src));
            if !source_text.is_empty() {
                imports.push(ImportInfo {
                    source: source_text,
                    names: Vec::new(),
                    kind: ImportKind::Named,
                    line: node.start_position().row + 1,
                    is_type_only: false,
                });
            }
        }
    });
    imports
}

/// `GDScript` dependencies: `extends "res://base.gd"` plus `preload(...)` /
/// `load(...)` resource references (which can appear in `const`, `var`, or
/// inline expressions, so the whole tree is scanned for them).
#[cfg(feature = "tree-sitter")]
fn extract_imports_gd(root: Node, src: &str) -> Vec<ImportInfo> {
    let mut imports = Vec::new();

    let mut cursor = root.walk();
    for node in root.children(&mut cursor) {
        if node.kind() == "extends_statement"
            && let Some(s) = find_descendant_by_kind(node, "string")
        {
            let source_text = unquote(node_text(s, src));
            if !source_text.is_empty() {
                imports.push(ImportInfo {
                    source: source_text,
                    names: Vec::new(),
                    kind: ImportKind::SideEffect,
                    line: node.start_position().row + 1,
                    is_type_only: false,
                });
            }
        }
    }

    crate::core::ast_walk::for_each_descendant(root, |node| {
        collect_gd_preload(node, src, &mut imports);
    });
    imports
}

#[cfg(feature = "tree-sitter")]
fn collect_gd_preload(node: Node, src: &str, imports: &mut Vec<ImportInfo>) {
    if node.kind() == "call"
        && let Some(callee) = find_child_by_kind(node, "identifier")
    {
        let name = node_text(callee, src);
        if (name == "preload" || name == "load")
            && let Some(args) = find_child_by_kind(node, "arguments")
            && let Some(s) = find_descendant_by_kind(args, "string")
        {
            let source_text = unquote(node_text(s, src));
            if !source_text.is_empty() {
                imports.push(ImportInfo {
                    source: source_text,
                    names: Vec::new(),
                    kind: ImportKind::Dynamic,
                    line: node.start_position().row + 1,
                    is_type_only: false,
                });
            }
        }
    }
}

#[cfg(feature = "tree-sitter")]
fn extract_imports_c_like(root: Node, src: &str) -> Vec<ImportInfo> {
    let mut imports = Vec::new();
    let mut cursor = root.walk();

    for node in root.children(&mut cursor) {
        if node.kind() == "preproc_include"
            && let Some(s) = find_descendant_by_kind(node, "string_literal")
                .or_else(|| find_descendant_by_kind(node, "system_lib_string"))
        {
            let raw = node_text(s, src);
            let cleaned = raw
                .trim()
                .trim_start_matches('"')
                .trim_end_matches('"')
                .trim_start_matches('<')
                .trim_end_matches('>')
                .to_string();
            if !cleaned.is_empty() {
                imports.push(ImportInfo {
                    source: cleaned,
                    names: Vec::new(),
                    kind: ImportKind::Named,
                    line: node.start_position().row + 1,
                    is_type_only: false,
                });
            }
        }
    }
    imports
}

#[cfg(feature = "tree-sitter")]
fn extract_imports_ruby(root: Node, src: &str) -> Vec<ImportInfo> {
    let mut imports = Vec::new();
    let mut cursor = root.walk();
    for node in root.children(&mut cursor) {
        let text = node_text(node, src).trim_start().to_string();
        if (text.starts_with("require ") || text.starts_with("require_relative "))
            && let Some(s) = find_descendant_by_kind(node, "string")
        {
            let source_text = unquote(node_text(s, src));
            if !source_text.is_empty() {
                imports.push(ImportInfo {
                    source: source_text,
                    names: Vec::new(),
                    kind: ImportKind::Named,
                    line: node.start_position().row + 1,
                    is_type_only: false,
                });
            }
        }
    }
    imports
}

#[cfg(feature = "tree-sitter")]
fn extract_imports_csharp(root: Node, src: &str) -> Vec<ImportInfo> {
    // `using` directives appear at file scope, inside `namespace { ... }` blocks,
    // and as `global using` — so the whole tree is walked rather than only the root.
    let mut imports = Vec::new();
    crate::core::ast_walk::for_each_descendant(root, |node| {
        collect_csharp_using(node, src, &mut imports);
    });
    imports
}

#[cfg(feature = "tree-sitter")]
fn collect_csharp_using(node: Node, src: &str, imports: &mut Vec<ImportInfo>) {
    if node.kind() == "using_directive"
        && let Some(info) = parse_csharp_using(node, src)
    {
        imports.push(info);
    }
}

/// Normalize a `using_directive` to the imported namespace, handling every form:
/// `using X.Y;`, `global using X.Y;`, `using static X.Y;`, and the alias form
/// `using Alias = X.Y;` (where the right-hand side is the real dependency).
#[cfg(feature = "tree-sitter")]
fn parse_csharp_using(node: Node, src: &str) -> Option<ImportInfo> {
    let raw = node_text(node, src).trim().trim_end_matches(';').trim();

    // Strip leading `global` / `using` / `static` keywords without touching the
    // namespace path (token-wise, so a namespace segment is never mangled).
    let mut rest: Vec<&str> = Vec::new();
    for tok in raw.split_whitespace() {
        if rest.is_empty() && matches!(tok, "global" | "using" | "static") {
            continue;
        }
        rest.push(tok);
    }
    let joined = rest.join(" ");

    // Alias form `Alias = Namespace.Path` -> keep the right-hand dependency.
    let source = joined
        .split_once('=')
        .map_or_else(|| joined.trim(), |(_, rhs)| rhs.trim())
        .to_string();

    if source.is_empty() {
        return None;
    }
    Some(ImportInfo {
        source,
        names: Vec::new(),
        kind: ImportKind::Named,
        line: node.start_position().row + 1,
        is_type_only: false,
    })
}

#[cfg(feature = "tree-sitter")]
fn extract_imports_kotlin(root: Node, src: &str) -> Vec<ImportInfo> {
    let mut imports = Vec::new();
    let mut cursor = root.walk();
    for node in root.children(&mut cursor) {
        if node.kind() != "import" {
            continue;
        }
        let Some(path_node) = find_child_by_kind(node, "qualified_identifier") else {
            continue;
        };
        let source = node_text(path_node, src).to_string();
        let text = node_text(node, src);

        let path_end = path_node.end_byte();
        let alias = {
            let mut walk = node.walk();
            let children: Vec<_> = node.children(&mut walk).collect();
            children
                .into_iter()
                .find(|child| child.kind() == "identifier" && child.start_byte() > path_end)
                .map(|child| node_text(child, src).to_string())
        };
        let is_star = text.contains(".*");

        let names = if is_star {
            vec!["*".to_string()]
        } else if let Some(ref alias) = alias {
            vec![alias.clone()]
        } else {
            vec![source.rsplit('.').next().unwrap_or(&source).to_string()]
        };

        imports.push(ImportInfo {
            source,
            names,
            kind: if is_star {
                ImportKind::Star
            } else {
                ImportKind::Named
            },
            line: node.start_position().row + 1,
            is_type_only: false,
        });
    }
    imports
}

#[cfg(feature = "tree-sitter")]
fn extract_imports_swift(root: Node, src: &str) -> Vec<ImportInfo> {
    let mut imports = Vec::new();
    let mut cursor = root.walk();
    for node in root.children(&mut cursor) {
        if node.kind() == "import_declaration" {
            let text = node_text(node, src)
                .trim()
                .trim_start_matches("import")
                .trim()
                .to_string();
            if !text.is_empty() {
                imports.push(ImportInfo {
                    source: text,
                    names: Vec::new(),
                    kind: ImportKind::Named,
                    line: node.start_position().row + 1,
                    is_type_only: false,
                });
            }
        }
    }
    imports
}

#[cfg(feature = "tree-sitter")]
fn extract_imports_php(root: Node, src: &str) -> Vec<ImportInfo> {
    let mut imports = Vec::new();
    let mut cursor = root.walk();
    for node in root.children(&mut cursor) {
        let kind = node.kind();
        if (kind.contains("include") || kind.contains("require"))
            && let Some(s) = find_descendant_by_kind(node, "string")
        {
            let source_text = unquote(node_text(s, src));
            if !source_text.is_empty() {
                imports.push(ImportInfo {
                    source: source_text,
                    names: Vec::new(),
                    kind: ImportKind::Named,
                    line: node.start_position().row + 1,
                    is_type_only: false,
                });
            }
        }
    }
    imports
}

#[cfg(feature = "tree-sitter")]
fn extract_imports_bash(root: Node, src: &str) -> Vec<ImportInfo> {
    let mut imports = Vec::new();
    let mut cursor = root.walk();
    for node in root.children(&mut cursor) {
        if node.kind() == "command" {
            let text = node_text(node, src).trim().to_string();
            if text.starts_with("source ") || text.starts_with(". ") {
                let parts: Vec<&str> = text.split_whitespace().collect();
                if parts.len() >= 2 {
                    let p = parts[1].trim_matches('"').trim_matches('\'').to_string();
                    if !p.is_empty() {
                        imports.push(ImportInfo {
                            source: p,
                            names: Vec::new(),
                            kind: ImportKind::Named,
                            line: node.start_position().row + 1,
                            is_type_only: false,
                        });
                    }
                }
            }
        }
    }
    imports
}

#[cfg(feature = "tree-sitter")]
fn extract_imports_dart(root: Node, src: &str) -> Vec<ImportInfo> {
    let mut imports = Vec::new();
    let mut cursor = root.walk();
    for node in root.children(&mut cursor) {
        if (node.kind() == "import_or_export" || node.kind() == "library_import")
            && let Some(s) = find_descendant_by_kind(node, "string_literal")
                .or_else(|| find_descendant_by_kind(node, "string"))
        {
            let source_text = unquote(node_text(s, src));
            if !source_text.is_empty() {
                imports.push(ImportInfo {
                    source: source_text,
                    names: Vec::new(),
                    kind: ImportKind::Named,
                    line: node.start_position().row + 1,
                    is_type_only: false,
                });
            }
        }
    }
    imports
}

#[cfg(feature = "tree-sitter")]
fn extract_imports_scala(root: Node, src: &str) -> Vec<ImportInfo> {
    let mut imports = Vec::new();
    let mut cursor = root.walk();
    for node in root.children(&mut cursor) {
        if node.kind() == "import_declaration" {
            let text = node_text(node, src)
                .trim()
                .trim_start_matches("import")
                .trim()
                .to_string();
            if !text.is_empty() {
                imports.push(ImportInfo {
                    source: text,
                    names: Vec::new(),
                    kind: ImportKind::Named,
                    line: node.start_position().row + 1,
                    is_type_only: false,
                });
            }
        }
    }
    imports
}

#[cfg(feature = "tree-sitter")]
fn extract_imports_elixir(root: Node, src: &str) -> Vec<ImportInfo> {
    let mut imports = Vec::new();
    let mut cursor = root.walk();
    for node in root.children(&mut cursor) {
        let text = node_text(node, src).trim().to_string();
        for kw in ["alias ", "import ", "require ", "use "] {
            if text.starts_with(kw) {
                let rest = text.trim_start_matches(kw).trim();
                if !rest.is_empty() {
                    let module = rest
                        .split_whitespace()
                        .next()
                        .unwrap_or("")
                        .trim_end_matches(',')
                        .trim_end_matches(';')
                        .to_string();
                    if !module.is_empty() {
                        imports.push(ImportInfo {
                            source: module,
                            names: Vec::new(),
                            kind: ImportKind::Named,
                            line: node.start_position().row + 1,
                            is_type_only: false,
                        });
                    }
                }
            }
        }
    }
    imports
}

#[cfg(feature = "tree-sitter")]
fn extract_imports_zig(root: Node, src: &str) -> Vec<ImportInfo> {
    let mut imports = Vec::new();
    let mut cursor = root.walk();
    for node in root.children(&mut cursor) {
        let text = node_text(node, src);
        if text.contains("@import")
            && let Some(s) = find_descendant_by_kind(node, "string_literal")
                .or_else(|| find_descendant_by_kind(node, "string"))
        {
            let source_text = unquote(node_text(s, src));
            if !source_text.is_empty() {
                imports.push(ImportInfo {
                    source: source_text,
                    names: Vec::new(),
                    kind: ImportKind::Named,
                    line: node.start_position().row + 1,
                    is_type_only: false,
                });
            }
        }
    }
    imports
}

#[cfg(feature = "tree-sitter")]
fn extract_imports_ts(root: Node, src: &str) -> Vec<ImportInfo> {
    let mut imports = Vec::new();
    let mut cursor = root.walk();

    for node in root.children(&mut cursor) {
        match node.kind() {
            "import_statement" => {
                if let Some(info) = parse_ts_import(node, src) {
                    imports.push(info);
                }
            }
            "export_statement" => {
                if let Some(source) = find_child_by_kind(node, "string") {
                    let source_text = unquote(node_text(source, src));
                    let names = collect_named_imports(node, src);
                    imports.push(ImportInfo {
                        source: source_text,
                        names,
                        kind: ImportKind::Reexport,
                        line: node.start_position().row + 1,
                        is_type_only: false,
                    });
                }
            }
            _ => {}
        }
    }

    crate::core::ast_walk::for_each_descendant(root, |node| {
        collect_dynamic_import(node, src, &mut imports);
    });

    imports
}

#[cfg(feature = "tree-sitter")]
fn parse_ts_import(node: Node, src: &str) -> Option<ImportInfo> {
    let source_node =
        find_child_by_kind(node, "string").or_else(|| find_descendant_by_kind(node, "string"))?;
    let source = unquote(node_text(source_node, src));

    let is_type_only = node_text(node, src).starts_with("import type");

    let clause = find_child_by_kind(node, "import_clause");
    let (kind, names) = match clause {
        Some(c) => classify_ts_import_clause(c, src),
        None => (ImportKind::SideEffect, Vec::new()),
    };

    Some(ImportInfo {
        source,
        names,
        kind,
        line: node.start_position().row + 1,
        is_type_only,
    })
}

#[cfg(feature = "tree-sitter")]
fn classify_ts_import_clause(clause: Node, src: &str) -> (ImportKind, Vec<String>) {
    let mut names = Vec::new();
    let mut has_default = false;
    let mut has_star = false;

    let mut cursor = clause.walk();
    for child in clause.children(&mut cursor) {
        match child.kind() {
            "identifier" => {
                has_default = true;
                names.push(node_text(child, src).to_string());
            }
            "namespace_import" => {
                has_star = true;
                if let Some(id) = find_child_by_kind(child, "identifier") {
                    names.push(format!("* as {}", node_text(id, src)));
                }
            }
            "named_imports" => {
                let mut inner = child.walk();
                for spec in child.children(&mut inner) {
                    if spec.kind() == "import_specifier" {
                        let name = find_child_by_kind(spec, "identifier")
                            .map(|n| node_text(n, src).to_string());
                        if let Some(n) = name {
                            names.push(n);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    let kind = if has_star {
        ImportKind::Star
    } else if has_default && names.len() == 1 {
        ImportKind::Default
    } else {
        ImportKind::Named
    };

    (kind, names)
}

#[cfg(feature = "tree-sitter")]
fn collect_dynamic_import(node: Node, src: &str, imports: &mut Vec<ImportInfo>) {
    if node.kind() == "call_expression" {
        let callee = find_child_by_kind(node, "import");
        if callee.is_some()
            && let Some(args) = find_child_by_kind(node, "arguments")
            && let Some(first_arg) = find_child_by_kind(args, "string")
        {
            imports.push(ImportInfo {
                source: unquote(node_text(first_arg, src)),
                names: Vec::new(),
                kind: ImportKind::Dynamic,
                line: node.start_position().row + 1,
                is_type_only: false,
            });
        }

        // CommonJS require("...") / require.resolve("...")
        if let Some(func_node) = find_child_by_kind(node, "identifier")
            && node_text(func_node, src) == "require"
            && let Some(args) = find_child_by_kind(node, "arguments")
            && let Some(first_arg) = find_child_by_kind(args, "string")
        {
            imports.push(ImportInfo {
                source: unquote(node_text(first_arg, src)),
                names: Vec::new(),
                kind: ImportKind::Default,
                line: node.start_position().row + 1,
                is_type_only: false,
            });
        }
        if let Some(member) = find_child_by_kind(node, "member_expression") {
            let text = node_text(member, src);
            if text.starts_with("require.resolve")
                && let Some(args) = find_child_by_kind(node, "arguments")
                && let Some(first_arg) = find_child_by_kind(args, "string")
            {
                imports.push(ImportInfo {
                    source: unquote(node_text(first_arg, src)),
                    names: Vec::new(),
                    kind: ImportKind::Dynamic,
                    line: node.start_position().row + 1,
                    is_type_only: false,
                });
            }
        }
    }
}

#[cfg(feature = "tree-sitter")]
fn extract_imports_rust(root: Node, src: &str) -> Vec<ImportInfo> {
    let mut imports = Vec::new();
    let mut cursor = root.walk();

    for node in root.children(&mut cursor) {
        if node.kind() == "mod_item" {
            let text = node_text(node, src);
            if !text.contains('{')
                && let Some(name_node) = find_child_by_kind(node, "identifier")
            {
                let mod_name = node_text(name_node, src).to_string();
                imports.push(ImportInfo {
                    source: mod_name.clone(),
                    names: vec![mod_name],
                    kind: ImportKind::Named,
                    line: node.start_position().row + 1,
                    is_type_only: false,
                });
            }
        } else if node.kind() == "use_declaration" {
            let is_pub = node_text(node, src).trim_start().starts_with("pub");
            let kind = if is_pub {
                ImportKind::Reexport
            } else {
                ImportKind::Named
            };

            if let Some(arg) = find_child_by_kind(node, "use_as_clause")
                .or_else(|| find_child_by_kind(node, "scoped_identifier"))
                .or_else(|| find_child_by_kind(node, "scoped_use_list"))
                .or_else(|| find_child_by_kind(node, "use_wildcard"))
                .or_else(|| find_child_by_kind(node, "identifier"))
            {
                let full_path = node_text(arg, src).to_string();

                let (source, names) = if full_path.contains('{') {
                    let parts: Vec<&str> = full_path.splitn(2, "::").collect();
                    let base = parts[0].to_string();
                    let items: Vec<String> = full_path
                        .split('{')
                        .nth(1)
                        .unwrap_or("")
                        .trim_end_matches('}')
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect();
                    (base, items)
                } else if full_path.ends_with("::*") {
                    (
                        full_path.trim_end_matches("::*").to_string(),
                        vec!["*".to_string()],
                    )
                } else {
                    let name = full_path.rsplit("::").next().unwrap_or(&full_path);
                    (full_path.clone(), vec![name.to_string()])
                };

                let is_std = source.starts_with("std")
                    || source.starts_with("core")
                    || source.starts_with("alloc");
                if !is_std {
                    imports.push(ImportInfo {
                        source,
                        names,
                        kind: if full_path.contains('*') {
                            ImportKind::Star
                        } else {
                            kind.clone()
                        },
                        line: node.start_position().row + 1,
                        is_type_only: false,
                    });
                }
            }
        }
    }

    imports
}

#[cfg(feature = "tree-sitter")]
fn extract_imports_python(root: Node, src: &str) -> Vec<ImportInfo> {
    let mut imports = Vec::new();
    let mut cursor = root.walk();

    for node in root.children(&mut cursor) {
        match node.kind() {
            "import_statement" => {
                let mut inner = node.walk();
                for child in node.children(&mut inner) {
                    if child.kind() == "dotted_name" || child.kind() == "aliased_import" {
                        let text = node_text(child, src);
                        let module = if child.kind() == "aliased_import" {
                            find_child_by_kind(child, "dotted_name")
                                .map_or_else(|| text.to_string(), |n| node_text(n, src).to_string())
                        } else {
                            text.to_string()
                        };
                        imports.push(ImportInfo {
                            source: module,
                            names: Vec::new(),
                            kind: ImportKind::Named,
                            line: node.start_position().row + 1,
                            is_type_only: false,
                        });
                    }
                }
            }
            "import_from_statement" => {
                let module = find_child_by_kind(node, "dotted_name")
                    .or_else(|| find_child_by_kind(node, "relative_import"))
                    .map(|n| node_text(n, src).to_string())
                    .unwrap_or_default();

                let mut names = Vec::new();
                let mut is_star = false;

                let mut inner = node.walk();
                for child in node.children(&mut inner) {
                    if child.kind() == "wildcard_import" {
                        is_star = true;
                    } else if child.kind() == "import_prefix" {
                        // relative import dots handled via module already
                    } else if child.kind() == "dotted_name"
                        && child.start_position() != node.start_position()
                    {
                        names.push(node_text(child, src).to_string());
                    } else if child.kind() == "aliased_import"
                        && let Some(n) = find_child_by_kind(child, "dotted_name")
                            .or_else(|| find_child_by_kind(child, "identifier"))
                    {
                        names.push(node_text(n, src).to_string());
                    }
                }

                imports.push(ImportInfo {
                    source: module,
                    names,
                    kind: if is_star {
                        ImportKind::Star
                    } else {
                        ImportKind::Named
                    },
                    line: node.start_position().row + 1,
                    is_type_only: false,
                });
            }
            _ => {}
        }
    }

    imports
}

#[cfg(feature = "tree-sitter")]
fn extract_imports_go(root: Node, src: &str) -> Vec<ImportInfo> {
    let mut imports = Vec::new();
    let mut cursor = root.walk();

    for node in root.children(&mut cursor) {
        if node.kind() == "import_declaration" {
            let mut inner = node.walk();
            for child in node.children(&mut inner) {
                match child.kind() {
                    "import_spec" => {
                        if let Some(path_node) =
                            find_child_by_kind(child, "interpreted_string_literal")
                        {
                            let source = unquote(node_text(path_node, src));
                            let alias = find_child_by_kind(child, "package_identifier")
                                .or_else(|| find_child_by_kind(child, "dot"))
                                .or_else(|| find_child_by_kind(child, "blank_identifier"));
                            let kind = match alias.map(|a| node_text(a, src)) {
                                Some(".") => ImportKind::Star,
                                Some("_") => ImportKind::SideEffect,
                                _ => ImportKind::Named,
                            };
                            imports.push(ImportInfo {
                                source,
                                names: Vec::new(),
                                kind,
                                line: child.start_position().row + 1,
                                is_type_only: false,
                            });
                        }
                    }
                    "import_spec_list" => {
                        let mut spec_cursor = child.walk();
                        for spec in child.children(&mut spec_cursor) {
                            if spec.kind() == "import_spec"
                                && let Some(path_node) =
                                    find_child_by_kind(spec, "interpreted_string_literal")
                            {
                                let source = unquote(node_text(path_node, src));
                                let alias = find_child_by_kind(spec, "package_identifier")
                                    .or_else(|| find_child_by_kind(spec, "dot"))
                                    .or_else(|| find_child_by_kind(spec, "blank_identifier"));
                                let kind = match alias.map(|a| node_text(a, src)) {
                                    Some(".") => ImportKind::Star,
                                    Some("_") => ImportKind::SideEffect,
                                    _ => ImportKind::Named,
                                };
                                imports.push(ImportInfo {
                                    source,
                                    names: Vec::new(),
                                    kind,
                                    line: spec.start_position().row + 1,
                                    is_type_only: false,
                                });
                            }
                        }
                    }
                    "interpreted_string_literal" => {
                        let source = unquote(node_text(child, src));
                        imports.push(ImportInfo {
                            source,
                            names: Vec::new(),
                            kind: ImportKind::Named,
                            line: child.start_position().row + 1,
                            is_type_only: false,
                        });
                    }
                    _ => {}
                }
            }
        }
    }

    imports
}

#[cfg(feature = "tree-sitter")]
fn extract_imports_java(root: Node, src: &str) -> Vec<ImportInfo> {
    let mut imports = Vec::new();
    let mut cursor = root.walk();

    for node in root.children(&mut cursor) {
        if node.kind() == "import_declaration" {
            let text = node_text(node, src).to_string();
            let _is_static = text.contains("static ");

            let path_node = find_child_by_kind(node, "scoped_identifier")
                .or_else(|| find_child_by_kind(node, "identifier"));
            if let Some(p) = path_node {
                let full_path = node_text(p, src).to_string();

                let is_wildcard = find_child_by_kind(node, "asterisk").is_some();
                let kind = if is_wildcard {
                    ImportKind::Star
                } else {
                    ImportKind::Named
                };

                let name = full_path
                    .rsplit('.')
                    .next()
                    .unwrap_or(&full_path)
                    .to_string();
                imports.push(ImportInfo {
                    source: full_path,
                    names: vec![name],
                    kind,
                    line: node.start_position().row + 1,
                    is_type_only: false,
                });
            }
        }
    }

    imports
}

// ---------------------------------------------------------------------------
// Helpers (import-specific)
// ---------------------------------------------------------------------------

#[cfg(feature = "tree-sitter")]
fn collect_named_imports(node: Node, src: &str) -> Vec<String> {
    let mut names = Vec::new();
    if let Some(named) = find_descendant_by_kind(node, "named_imports") {
        let mut cursor = named.walk();
        for child in named.children(&mut cursor) {
            if (child.kind() == "import_specifier" || child.kind() == "export_specifier")
                && let Some(id) = find_child_by_kind(child, "identifier")
            {
                names.push(node_text(id, src).to_string());
            }
        }
    }
    names
}

fn unquote(s: &str) -> String {
    s.trim_matches(|c| c == '\'' || c == '"' || c == '`')
        .to_string()
}
