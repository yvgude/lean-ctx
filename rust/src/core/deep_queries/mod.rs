//! Tree-sitter deep queries for extracting imports, call sites, and type definitions.
//!
//! Replaces regex-based extraction in `deps.rs` with precise AST parsing.
//! Supported languages are gated by `get_language` (and kept in sync with
//! `core::language_capabilities`): the TypeScript/JavaScript family, Python,
//! Rust, Go, Java, C/C++, Ruby, C#, Kotlin, Swift, PHP, Bash, Dart, Scala,
//! Elixir, Zig, and GDScript.

mod calls;
mod imports;
mod type_defs;
mod type_uses;
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

    thread_local! {
        static PARSER: std::cell::RefCell<Parser> = std::cell::RefCell::new(Parser::new());
    }

    let tree = PARSER.with(|p| {
        let mut parser = p.borrow_mut();
        let _ = parser.set_language(&language);
        parser.parse(content.as_bytes(), None)
    })?;
    let root = tree.root_node();

    let imports = imports::extract_imports(root, content, ext);
    let calls = calls::extract_calls(root, content, ext);
    let types = type_defs::extract_types(root, content, ext);
    let exports = type_defs::extract_exports(root, content, ext);
    let type_uses = type_uses::extract_type_uses(root, content, ext);

    Some(DeepAnalysis {
        imports,
        calls,
        types,
        exports,
        type_uses,
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
        "gd" => Some(tree_sitter_gdscript::LANGUAGE.into()),
        "lua" => Some(tree_sitter_lua::LANGUAGE.into()),
        "luau" => Some(tree_sitter_luau::LANGUAGE.into()),
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

    node.children(&mut cursor).find(|c| c.kind() == kind)
}

#[cfg(feature = "tree-sitter")]
fn find_descendant_by_kind<'a>(node: Node<'a>, kind: &str) -> Option<Node<'a>> {
    // Iterative (heap-stack) search — see core::ast_walk (#378 SIGABRT).
    crate::core::ast_walk::find_descendant_by_kind(node, kind)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[cfg(feature = "tree-sitter")]
mod tests {
    use super::*;

    /// Indexing a deeply nested AST must not overflow the worker-thread stack
    /// (the #378 SIGABRT) through the real `analyze` entry point. The depth is
    /// well past what a recursive walk survives on a default stack, yet because
    /// every walk is iterative now it returns normally. (The dedicated, much
    /// deeper overflow guard lives in `core::ast_walk`.)
    #[test]
    fn deeply_nested_source_does_not_overflow() {
        let depth = 12_000;
        // Nested Rust call expressions drive the call walk through the real
        // entry point at a depth far past what a recursive walk survives on a
        // default stack; it returns normally because the walks are iterative.
        let rs = format!(
            "fn m() {{ let _ = {}0{}; }}",
            "f(".repeat(depth),
            ")".repeat(depth)
        );
        let analysis = analyze(&rs, "rs");
        assert!(!analysis.calls.is_empty());
    }

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
    fn python_call_sites() {
        // Regression for GH #365: Python uses a bare `call` node, so class
        // instantiation and method calls must both be extracted as call sites.
        let src = r"
from models.engine import Engine

def boot():
    engine = Engine(power=100)
    engine.run()
    return engine
";
        let analysis = analyze(src, "py");
        let callees: Vec<&str> = analysis.calls.iter().map(|c| c.callee.as_str()).collect();
        assert!(
            callees.contains(&"Engine"),
            "class instantiation should be a call site, got {callees:?}"
        );
        assert!(
            callees.contains(&"run"),
            "method call must resolve to the method name (not the receiver), got {callees:?}"
        );
    }

    #[test]
    fn java_object_creation_is_a_call_site() {
        let src = r"
class App {
    void boot() {
        Engine e = new Engine(100);
        e.run();
    }
}
";
        let analysis = analyze(src, "java");
        let callees: Vec<&str> = analysis.calls.iter().map(|c| c.callee.as_str()).collect();
        assert!(
            callees.contains(&"Engine"),
            "`new Engine()` should be a call site, got {callees:?}"
        );
        assert!(
            callees.contains(&"run"),
            "method call expected, got {callees:?}"
        );
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

    #[test]
    fn gdscript_imports_extends_and_preload() {
        let src = r#"
extends "res://actors/base_actor.gd"

const Bullet = preload("res://weapons/bullet.gd")
var sfx = load("res://audio/shot.wav")
"#;
        let analysis = analyze(src, "gd");
        let sources: Vec<&str> = analysis.imports.iter().map(|i| i.source.as_str()).collect();
        assert!(
            sources.contains(&"res://actors/base_actor.gd"),
            "expected extends import, got {sources:?}"
        );
        assert!(
            sources.contains(&"res://weapons/bullet.gd"),
            "expected preload import, got {sources:?}"
        );
        assert!(
            sources.contains(&"res://audio/shot.wav"),
            "expected load import, got {sources:?}"
        );
    }

    #[test]
    fn gdscript_types_class_name_and_enum() {
        let src = r"
class_name Player

enum State { IDLE, RUNNING }

class Inventory:
    var items = []
";
        let analysis = analyze(src, "gd");
        let names: Vec<&str> = analysis.types.iter().map(|t| t.name.as_str()).collect();
        assert!(
            names.contains(&"Player"),
            "expected class_name, got {names:?}"
        );
        assert!(names.contains(&"State"), "expected enum, got {names:?}");
        assert!(
            names.contains(&"Inventory"),
            "expected inner class, got {names:?}"
        );
        let player = analysis.types.iter().find(|t| t.name == "Player").unwrap();
        assert_eq!(player.kind, TypeDefKind::Class);
        assert!(player.is_exported);
        let state = analysis.types.iter().find(|t| t.name == "State").unwrap();
        assert_eq!(state.kind, TypeDefKind::Enum);
    }

    #[test]
    fn csharp_imports_all_using_forms() {
        let src = r"
using System;
using System.Collections.Generic;
global using MyApp.Core;
using static System.Math;
using Json = Newtonsoft.Json;
namespace MyApp.Services {
    using MyApp.Data.Repositories;
}
";
        let analysis = analyze(src, "cs");
        let sources: Vec<&str> = analysis.imports.iter().map(|i| i.source.as_str()).collect();
        assert!(sources.contains(&"System"), "plain using, got {sources:?}");
        assert!(
            sources.contains(&"System.Collections.Generic"),
            "dotted using, got {sources:?}"
        );
        assert!(
            sources.contains(&"MyApp.Core"),
            "global using must drop the `global` keyword, got {sources:?}"
        );
        assert!(
            sources.contains(&"System.Math"),
            "using static must drop the `static` keyword, got {sources:?}"
        );
        assert!(
            sources.contains(&"Newtonsoft.Json"),
            "alias using must keep the right-hand namespace, got {sources:?}"
        );
        assert!(
            sources.contains(&"MyApp.Data.Repositories"),
            "using nested inside a namespace block must be found, got {sources:?}"
        );
    }

    /// GH #398: types consumed without any `using` (same-namespace visibility)
    /// must surface as `type_uses` so the property graph can build TypeRef
    /// edges. Covers fields, ctor parameters, return types, base list,
    /// generic arguments, casts and `typeof`.
    #[test]
    fn csharp_type_uses_without_using_directive() {
        let src = r"
namespace App.Core;

public class Motor : VehiclePart, IStartable
{
    private readonly Engine _engine;
    public List<Sensor> Sensors { get; set; }

    public Motor(Engine engine) { _engine = engine; }

    public Gearbox BuildGearbox(Clutch clutch)
    {
        var t = typeof(Telemetry);
        var d = (Dashboard)GetPart();
        return null;
    }
}
";
        let analysis = analyze(src, "cs");
        let names: Vec<&str> = analysis.type_uses.iter().map(|u| u.name.as_str()).collect();
        for expected in [
            "Engine",
            "VehiclePart",
            "IStartable",
            "List",
            "Sensor",
            "Gearbox",
            "Clutch",
            "Telemetry",
            "Dashboard",
        ] {
            assert!(names.contains(&expected), "missing {expected}: {names:?}");
        }
        // Predefined types carry no identifier node and must not appear.
        assert!(!names.contains(&"var"), "var is not a type use: {names:?}");
    }

    /// GH #398 (Java flavour): same-package types are visible without import;
    /// `type_identifier` nodes cover fields, params, returns and extends.
    #[test]
    fn java_type_uses_without_import() {
        let src = r"
package app.core;

public class Motor extends VehiclePart {
    private Engine engine;
    public Gearbox build(Clutch clutch) { return null; }
}
";
        let analysis = analyze(src, "java");
        let names: Vec<&str> = analysis.type_uses.iter().map(|u| u.name.as_str()).collect();
        for expected in ["VehiclePart", "Engine", "Gearbox", "Clutch"] {
            assert!(names.contains(&expected), "missing {expected}: {names:?}");
        }
    }

    /// Languages with mandatory explicit imports skip type-use extraction —
    /// their dependencies are fully covered by the import resolver.
    #[test]
    fn type_uses_empty_for_import_based_languages() {
        let rs = analyze("struct Foo { e: Engine }", "rs");
        assert!(rs.type_uses.is_empty(), "rust: {:?}", rs.type_uses);
        let ts = analyze("const e: Engine = make();", "ts");
        assert!(ts.type_uses.is_empty(), "ts: {:?}", ts.type_uses);
    }

    #[test]
    fn csharp_types_and_visibility() {
        let src = r"
namespace App
{
    public class UserService { }
    internal class Helper { }
    public interface IRepository { }
    public struct Point { public int X; }
    public enum Status { Active, Inactive }
    public record Money(decimal Amount, string Currency);
}
";
        let analysis = analyze(src, "cs");
        let names: Vec<&str> = analysis.types.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"UserService"), "class, got {names:?}");
        assert!(names.contains(&"Helper"), "internal class, got {names:?}");
        assert!(names.contains(&"IRepository"), "interface, got {names:?}");
        assert!(names.contains(&"Point"), "struct, got {names:?}");
        assert!(names.contains(&"Status"), "enum, got {names:?}");
        assert!(names.contains(&"Money"), "record, got {names:?}");

        let kind_of = |n: &str| {
            analysis
                .types
                .iter()
                .find(|t| t.name == n)
                .map(|t| t.kind.clone())
        };
        assert_eq!(kind_of("UserService"), Some(TypeDefKind::Class));
        assert_eq!(kind_of("IRepository"), Some(TypeDefKind::Interface));
        assert_eq!(kind_of("Point"), Some(TypeDefKind::Struct));
        assert_eq!(kind_of("Status"), Some(TypeDefKind::Enum));
        assert_eq!(kind_of("Money"), Some(TypeDefKind::Record));

        let exported = |n: &str| {
            analysis
                .types
                .iter()
                .find(|t| t.name == n)
                .is_some_and(|t| t.is_exported)
        };
        assert!(exported("UserService"), "public class is exported");
        assert!(
            !exported("Helper"),
            "internal class must not be marked exported"
        );
        assert!(analysis.exports.contains(&"UserService".to_string()));
    }

    #[test]
    fn csharp_call_sites() {
        let src = r"
namespace App
{
    public class Boot
    {
        public void Run()
        {
            Prepare();
            _repository.Save(user);
            var engine = new Engine(100);
            Factory.Create<Widget>();
        }
    }
}
";
        let analysis = analyze(src, "cs");
        let callees: Vec<&str> = analysis.calls.iter().map(|c| c.callee.as_str()).collect();
        assert!(
            callees.contains(&"Prepare"),
            "direct invocation, got {callees:?}"
        );
        assert!(
            callees.contains(&"Save"),
            "member invocation must resolve to the method name, got {callees:?}"
        );
        assert!(
            callees.contains(&"Engine"),
            "`new Engine()` should reference the constructed type, got {callees:?}"
        );
        assert!(
            callees.contains(&"Create"),
            "generic member call must reduce to the identifier, got {callees:?}"
        );

        let save = analysis.calls.iter().find(|c| c.callee == "Save").unwrap();
        assert_eq!(save.receiver.as_deref(), Some("_repository"));
        assert!(save.is_method);
    }

    #[test]
    fn gdscript_calls_method_and_instantiation() {
        let src = r"
func _ready():
    var mgr = MapDataManager.new()
    mgr.load_map_data()
    update_state()
";
        let analysis = analyze(src, "gd");
        let callees: Vec<&str> = analysis.calls.iter().map(|c| c.callee.as_str()).collect();
        // `MapDataManager.new()` registers a reference to the class itself.
        assert!(
            callees.contains(&"MapDataManager"),
            "expected instantiation to reference class, got {callees:?}"
        );
        assert!(
            callees.contains(&"load_map_data"),
            "expected method call, got {callees:?}"
        );
        assert!(
            callees.contains(&"update_state"),
            "expected direct call, got {callees:?}"
        );
    }

    #[test]
    fn lua_require_imports() {
        let src = r#"
local mod = require("foo.bar")
local helper = require "baz"
local rel = require('a/b')
"#;
        let analysis = analyze(src, "lua");
        let sources: Vec<&str> = analysis.imports.iter().map(|i| i.source.as_str()).collect();
        assert!(
            sources.contains(&"foo.bar"),
            "dotted require, got {sources:?}"
        );
        assert!(
            sources.contains(&"baz"),
            "paren-less require, got {sources:?}"
        );
        assert!(sources.contains(&"a/b"), "slash require, got {sources:?}");
    }

    #[test]
    fn lua_call_sites() {
        let src = r"
local function run()
    helper()
    obj.method(1)
    obj:method2(2)
end
";
        let analysis = analyze(src, "lua");
        let callees: Vec<&str> = analysis.calls.iter().map(|c| c.callee.as_str()).collect();
        assert!(callees.contains(&"helper"), "direct call, got {callees:?}");
        assert!(callees.contains(&"method"), "dot call, got {callees:?}");
        assert!(callees.contains(&"method2"), "method call, got {callees:?}");
        let m = analysis
            .calls
            .iter()
            .find(|c| c.callee == "method2")
            .unwrap();
        assert_eq!(m.receiver.as_deref(), Some("obj"));
        assert!(m.is_method);
    }

    #[test]
    fn luau_require_and_calls() {
        let src = r#"
local mod = require("shared/util")
local function go()
    mod.run()
end
"#;
        let analysis = analyze(src, "luau");
        assert!(
            analysis.imports.iter().any(|i| i.source == "shared/util"),
            "got {:?}",
            analysis.imports
        );
        let callees: Vec<&str> = analysis.calls.iter().map(|c| c.callee.as_str()).collect();
        assert!(callees.contains(&"run"), "got {callees:?}");
    }

    #[test]
    fn luau_type_aliases() {
        let src = r"
type Account = { balance: number }
export type Vec = { x: number, y: number }
";
        let analysis = analyze(src, "luau");
        let names: Vec<&str> = analysis.types.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"Account"), "plain type, got {names:?}");
        assert!(names.contains(&"Vec"), "export type, got {names:?}");
        let vec = analysis.types.iter().find(|t| t.name == "Vec").unwrap();
        assert!(vec.is_exported, "`export type` must be exported");
        let acc = analysis.types.iter().find(|t| t.name == "Account").unwrap();
        assert!(!acc.is_exported, "plain `type` is module-local");
    }

    #[test]
    fn lua_has_no_types() {
        // Lua (unlike Luau) has no type system — only functions/calls/imports.
        let analysis = analyze("type Account = {}", "lua");
        assert!(analysis.types.is_empty());
    }
}
