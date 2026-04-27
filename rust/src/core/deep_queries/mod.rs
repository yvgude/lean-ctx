//! Tree-sitter deep queries for extracting imports, call sites, and type definitions.
//!
//! Replaces regex-based extraction in `deps.rs` with precise AST parsing.
//! Supports: TypeScript/JavaScript, Python, Rust, Go, Java.

mod calls;
mod imports;
mod type_defs;
mod types;

pub use types::*;

#[cfg(feature = "tree-sitter")]
use tree_sitter::{Language, Node, Parser};

pub fn analyze(content: &str, ext: &str) -> DeepAnalysis {
    #[cfg(feature = "tree-sitter")]
    {
        if let Some(result) = analyze_with_tree_sitter(content, ext) {
            return result;
        }
    }

    let _ = (content, ext);
    DeepAnalysis::empty()
}

#[cfg(feature = "tree-sitter")]
fn analyze_with_tree_sitter(content: &str, ext: &str) -> Option<DeepAnalysis> {
    let language = get_language(ext)?;
    let mut parser = Parser::new();
    parser.set_language(&language).ok()?;
    let tree = parser.parse(content.as_bytes(), None)?;
    let root = tree.root_node();

    let imports = imports::extract_imports(root, content, ext);
    let calls = calls::extract_calls(root, content, ext);
    let types = type_defs::extract_types(root, content, ext);
    let exports = type_defs::extract_exports(root, content, ext);

    Some(DeepAnalysis {
        imports,
        calls,
        types,
        exports,
    })
}

#[cfg(feature = "tree-sitter")]
fn get_language(ext: &str) -> Option<Language> {
    match ext {
        "rs" => Some(tree_sitter_rust::LANGUAGE.into()),
        "ts" | "tsx" => Some(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()),
        "js" | "jsx" => Some(tree_sitter_javascript::LANGUAGE.into()),
        "py" => Some(tree_sitter_python::LANGUAGE.into()),
        "go" => Some(tree_sitter_go::LANGUAGE.into()),
        "java" => Some(tree_sitter_java::LANGUAGE.into()),
        "c" | "h" => Some(tree_sitter_c::LANGUAGE.into()),
        "cpp" | "cc" | "cxx" | "hpp" | "hxx" | "hh" => Some(tree_sitter_cpp::LANGUAGE.into()),
        "rb" => Some(tree_sitter_ruby::LANGUAGE.into()),
        "cs" => Some(tree_sitter_c_sharp::LANGUAGE.into()),
        "kt" | "kts" => Some(tree_sitter_kotlin_ng::LANGUAGE.into()),
        "swift" => Some(tree_sitter_swift::LANGUAGE.into()),
        "php" => Some(tree_sitter_php::LANGUAGE_PHP.into()),
        "sh" | "bash" => Some(tree_sitter_bash::LANGUAGE.into()),
        "dart" => Some(tree_sitter_dart::LANGUAGE.into()),
        "scala" | "sc" => Some(tree_sitter_scala::LANGUAGE.into()),
        "ex" | "exs" => Some(tree_sitter_elixir::LANGUAGE.into()),
        "zig" => Some(tree_sitter_zig::LANGUAGE.into()),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Shared helpers (accessible by child modules via `super::`)
// ---------------------------------------------------------------------------

#[cfg(feature = "tree-sitter")]
fn node_text<'a>(node: Node, src: &'a str) -> &'a str {
    &src[node.byte_range()]
}

#[cfg(feature = "tree-sitter")]
fn find_child_by_kind<'a>(node: Node<'a>, kind: &str) -> Option<Node<'a>> {
    let mut cursor = node.walk();
    let result = node.children(&mut cursor).find(|c| c.kind() == kind);
    result
}

#[cfg(feature = "tree-sitter")]
fn find_descendant_by_kind<'a>(node: Node<'a>, kind: &str) -> Option<Node<'a>> {
    if node.kind() == kind {
        return Some(node);
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if let Some(found) = find_descendant_by_kind(child, kind) {
            return Some(found);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[cfg(feature = "tree-sitter")]
mod tests {
    use super::*;

    #[test]
    fn ts_named_import() {
        let src = r"import { useState, useEffect } from 'react';";
        let analysis = analyze(src, "ts");
        assert_eq!(analysis.imports.len(), 1);
        assert_eq!(analysis.imports[0].source, "react");
        assert_eq!(analysis.imports[0].names, vec!["useState", "useEffect"]);
    }

    #[test]
    fn ts_default_import() {
        let src = r"import React from 'react';";
        let analysis = analyze(src, "ts");
        assert_eq!(analysis.imports.len(), 1);
        assert_eq!(analysis.imports[0].kind, ImportKind::Default);
        assert_eq!(analysis.imports[0].names, vec!["React"]);
    }

    #[test]
    fn ts_star_import() {
        let src = r"import * as path from 'path';";
        let analysis = analyze(src, "ts");
        assert_eq!(analysis.imports.len(), 1);
        assert_eq!(analysis.imports[0].kind, ImportKind::Star);
    }

    #[test]
    fn ts_side_effect_import() {
        let src = r"import './styles.css';";
        let analysis = analyze(src, "ts");
        assert_eq!(analysis.imports.len(), 1);
        assert_eq!(analysis.imports[0].kind, ImportKind::SideEffect);
        assert_eq!(analysis.imports[0].source, "./styles.css");
    }

    #[test]
    fn ts_type_only_import() {
        let src = r"import type { User } from './types';";
        let analysis = analyze(src, "ts");
        assert_eq!(analysis.imports.len(), 1);
        assert!(analysis.imports[0].is_type_only);
    }

    #[test]
    fn ts_reexport() {
        let src = r"export { foo, bar } from './utils';";
        let analysis = analyze(src, "ts");
        assert_eq!(analysis.imports.len(), 1);
        assert_eq!(analysis.imports[0].kind, ImportKind::Reexport);
    }

    #[test]
    fn ts_call_sites() {
        let src = r"
const x = foo(1);
const y = obj.method(2);
";
        let analysis = analyze(src, "ts");
        assert!(analysis.calls.len() >= 2);
        let fns: Vec<&str> = analysis.calls.iter().map(|c| c.callee.as_str()).collect();
        assert!(fns.contains(&"foo"));
        assert!(fns.contains(&"method"));
    }

    #[test]
    fn ts_interface() {
        let src = r"
export interface User {
    name: string;
    age: number;
}
";
        let analysis = analyze(src, "ts");
        assert_eq!(analysis.types.len(), 1);
        assert_eq!(analysis.types[0].name, "User");
        assert_eq!(analysis.types[0].kind, TypeDefKind::Interface);
    }

    #[test]
    fn ts_type_alias_union() {
        let src = r"type Result = Success | Error;";
        let analysis = analyze(src, "ts");
        assert_eq!(analysis.types.len(), 1);
        assert_eq!(analysis.types[0].kind, TypeDefKind::Union);
    }

    #[test]
    fn rust_use_statements() {
        let src = r"
use crate::core::session;
use anyhow::Result;
use std::collections::HashMap;
";
        let analysis = analyze(src, "rs");
        assert_eq!(analysis.imports.len(), 2);
        let sources: Vec<&str> = analysis.imports.iter().map(|i| i.source.as_str()).collect();
        assert!(sources.contains(&"crate::core::session"));
        assert!(sources.contains(&"anyhow::Result"));
    }

    #[test]
    fn rust_pub_use_reexport() {
        let src = r"pub use crate::tools::ctx_read;";
        let analysis = analyze(src, "rs");
        assert_eq!(analysis.imports.len(), 1);
        assert_eq!(analysis.imports[0].kind, ImportKind::Reexport);
    }

    #[test]
    fn rust_struct_and_trait() {
        let src = r"
pub struct Config {
    pub name: String,
}

pub trait Service {
    fn run(&self);
}
";
        let analysis = analyze(src, "rs");
        assert_eq!(analysis.types.len(), 2);
        let names: Vec<&str> = analysis.types.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"Config"));
        assert!(names.contains(&"Service"));
    }

    #[test]
    fn rust_call_sites() {
        let src = r"
fn main() {
    let x = calculate(42);
    let y = self.process();
    Vec::new();
}
";
        let analysis = analyze(src, "rs");
        assert!(analysis.calls.len() >= 2);
        let fns: Vec<&str> = analysis.calls.iter().map(|c| c.callee.as_str()).collect();
        assert!(fns.contains(&"calculate"));
    }

    #[test]
    fn python_imports() {
        let src = r"
import os
from pathlib import Path
from . import utils
from ..models import User, Role
";
        let analysis = analyze(src, "py");
        assert!(analysis.imports.len() >= 3);
    }

    #[test]
    fn python_class_protocol() {
        let src = r"
class MyProtocol(Protocol):
    def method(self) -> None: ...

class User:
    name: str
";
        let analysis = analyze(src, "py");
        assert_eq!(analysis.types.len(), 2);
        assert_eq!(analysis.types[0].kind, TypeDefKind::Protocol);
        assert_eq!(analysis.types[1].kind, TypeDefKind::Class);
    }

    #[test]
    fn go_imports() {
        let src = r#"
package main

import (
    "fmt"
    "net/http"
    _ "github.com/lib/pq"
)
"#;
        let analysis = analyze(src, "go");
        assert!(analysis.imports.len() >= 3);
        let side_effect = analysis.imports.iter().find(|i| i.source.contains("pq"));
        assert!(side_effect.is_some());
        assert_eq!(side_effect.unwrap().kind, ImportKind::SideEffect);
    }

    #[test]
    fn go_struct_and_interface() {
        let src = r"
package main

type Server struct {
    Port int
}

type Handler interface {
    Handle(r *Request)
}
";
        let analysis = analyze(src, "go");
        assert_eq!(analysis.types.len(), 2);
        let kinds: Vec<&TypeDefKind> = analysis.types.iter().map(|t| &t.kind).collect();
        assert!(kinds.contains(&&TypeDefKind::Struct));
        assert!(kinds.contains(&&TypeDefKind::Interface));
    }

    #[test]
    fn java_imports() {
        let src = r"
import java.util.List;
import java.util.Map;
import static org.junit.Assert.*;
";
        let analysis = analyze(src, "java");
        assert!(analysis.imports.len() >= 2);
    }

    #[test]
    fn java_class_and_interface() {
        let src = r"
public class UserService {
    public void save(User u) {}
}

public interface Repository<T> {
    T findById(int id);
}

public enum Status { ACTIVE, INACTIVE }

public record Point(int x, int y) {}
";
        let analysis = analyze(src, "java");
        assert!(analysis.types.len() >= 3);
        let kinds: Vec<&TypeDefKind> = analysis.types.iter().map(|t| &t.kind).collect();
        assert!(kinds.contains(&&TypeDefKind::Class));
        assert!(kinds.contains(&&TypeDefKind::Interface));
        assert!(kinds.contains(&&TypeDefKind::Enum));
    }

    #[test]
    fn kotlin_imports_and_aliases() {
        let src = r"
package com.example.app

import com.example.services.UserService
import com.example.factories.WidgetFactory as Factory
import com.example.shared.*
";
        let analysis = analyze(src, "kt");
        assert_eq!(analysis.imports.len(), 3);
        assert_eq!(
            analysis.imports[0].source,
            "com.example.services.UserService"
        );
        assert_eq!(analysis.imports[1].names, vec!["Factory"]);
        assert_eq!(analysis.imports[2].kind, ImportKind::Star);
    }

    #[test]
    fn kotlin_call_sites() {
        let src = r"
class UserService {
    fun run() {
        prepare()
        repository.save(user)
        Factory.create()
    }
}
";
        let analysis = analyze(src, "kt");
        let callees: Vec<&str> = analysis.calls.iter().map(|c| c.callee.as_str()).collect();
        assert!(callees.contains(&"prepare"));
        assert!(callees.contains(&"save"));
        assert!(callees.contains(&"create"));
    }

    #[test]
    fn kotlin_types_and_visibility() {
        let src = r"
sealed interface Handler
data class User(val id: String)
enum class Status { ACTIVE, INACTIVE }
object Registry
private typealias UserId = String
";
        let analysis = analyze(src, "kt");
        let names: Vec<&str> = analysis.types.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"Handler"));
        assert!(names.contains(&"User"));
        assert!(names.contains(&"Status"));
        assert!(names.contains(&"Registry"));
        assert!(names.contains(&"UserId"));
        let handler = analysis.types.iter().find(|t| t.name == "Handler").unwrap();
        assert_eq!(handler.kind, TypeDefKind::Interface);
        let alias = analysis.types.iter().find(|t| t.name == "UserId").unwrap();
        assert!(!alias.is_exported);
    }

    #[test]
    fn ts_generics_extracted() {
        let src = r"interface Result<T, E> { ok: T; err: E; }";
        let analysis = analyze(src, "ts");
        assert_eq!(analysis.types.len(), 1);
        assert!(!analysis.types[0].generics.is_empty());
    }

    #[test]
    fn mixed_analysis_ts() {
        let src = r"
import { Request, Response } from 'express';
import type { User } from './models';

export interface Handler {
    handle(req: Request): Response;
}

export class Router {
    register(path: string, handler: Handler) {
        this.handlers.set(path, handler);
    }
}

const app = express();
app.listen(3000);
";
        let analysis = analyze(src, "ts");
        assert!(analysis.imports.len() >= 2, "Should find imports");
        assert!(!analysis.types.is_empty(), "Should find types");
        assert!(!analysis.calls.is_empty(), "Should find calls");
    }

    #[test]
    fn empty_file() {
        let analysis = analyze("", "ts");
        assert!(analysis.imports.is_empty());
        assert!(analysis.calls.is_empty());
        assert!(analysis.types.is_empty());
    }

    #[test]
    fn unsupported_extension() {
        let analysis = analyze("some content", "txt");
        assert!(analysis.imports.is_empty());
    }

    #[test]
    fn c_include_import() {
        let src = r#"
#include "foo/bar.h"
#include <stdio.h>
"#;
        let analysis = analyze(src, "c");
        assert!(analysis.imports.iter().any(|i| i.source == "foo/bar.h"));
    }

    #[test]
    fn bash_source_import() {
        let src = r#"
source "./scripts/env.sh"
. ../common.sh
"#;
        let analysis = analyze(src, "sh");
        assert!(
            analysis
                .imports
                .iter()
                .any(|i| i.source.contains("scripts/env.sh")),
            "expected source import"
        );
    }

    #[test]
    fn zig_at_import() {
        let src = r#"
const m = @import("lib/math.zig");
const std = @import("std");
"#;
        let analysis = analyze(src, "zig");
        assert!(analysis.imports.iter().any(|i| i.source == "lib/math.zig"));
    }
}
