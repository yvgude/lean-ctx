use regex::Regex;
use std::collections::HashSet;

#[cfg(feature = "tree-sitter")]
use super::deep_queries::{self, ImportKind};

macro_rules! static_regex {
    ($pattern:expr) => {{
        static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        RE.get_or_init(|| {
            regex::Regex::new($pattern).expect(concat!("BUG: invalid static regex: ", $pattern))
        })
    }};
}

fn import_re() -> &'static Regex {
    static_regex!(r#"import\s+(?:\{[^}]*\}\s+from\s+|.*from\s+)['"]([^'"]+)['"]"#)
}
fn require_re() -> &'static Regex {
    static_regex!(r#"require\(['"]([^'"]+)['"]\)"#)
}
fn rust_use_re() -> &'static Regex {
    static_regex!(r"^use\s+([\w:]+)")
}
fn py_import_re() -> &'static Regex {
    static_regex!(r"^(?:from\s+(\S+)\s+import|import\s+(\S+))")
}
fn go_import_re() -> &'static Regex {
    static_regex!(r#""([^"]+)""#)
}

#[derive(Debug, Clone)]
pub struct DepInfo {
    pub imports: Vec<String>,
    pub exports: Vec<String>,
}

pub fn extract_deps(content: &str, ext: &str) -> DepInfo {
    let lang = crate::core::language_capabilities::language_for_ext(ext);
    match lang {
        Some(
            crate::core::language_capabilities::LanguageId::TypeScript
            | crate::core::language_capabilities::LanguageId::JavaScript
            | crate::core::language_capabilities::LanguageId::Vue
            | crate::core::language_capabilities::LanguageId::Svelte,
        ) => extract_ts_deps(content),
        Some(crate::core::language_capabilities::LanguageId::Rust) => extract_rust_deps(content),
        Some(crate::core::language_capabilities::LanguageId::Python) => {
            extract_python_deps(content)
        }
        Some(crate::core::language_capabilities::LanguageId::Go) => extract_go_deps(content),
        Some(
            crate::core::language_capabilities::LanguageId::C
            | crate::core::language_capabilities::LanguageId::Cpp,
        ) => extract_c_like_deps(content),
        Some(crate::core::language_capabilities::LanguageId::Ruby) => extract_ruby_deps(content),
        Some(crate::core::language_capabilities::LanguageId::Php) => extract_php_deps(content),
        Some(crate::core::language_capabilities::LanguageId::Bash) => extract_bash_deps(content),
        Some(crate::core::language_capabilities::LanguageId::Kotlin) => {
            extract_kotlin_deps(content)
        }
        Some(crate::core::language_capabilities::LanguageId::Dart) => {
            let mut imports = HashSet::new();
            let re = static_regex!(r#"^\s*(?:import|export|part)\s+['"]([^'"]+)['"]"#);
            for line in content.lines() {
                let trimmed = line.trim();
                if let Some(caps) = re.captures(trimmed) {
                    let p = caps[1].trim();
                    if p.starts_with('.') || p.starts_with('/') {
                        imports.insert(clean_path_like(p));
                    }
                }
            }
            DepInfo {
                imports: imports.into_iter().collect(),
                exports: Vec::new(),
            }
        }
        Some(crate::core::language_capabilities::LanguageId::Zig) => {
            let mut imports = HashSet::new();
            let re = static_regex!(r#"@import\(\s*"([^"]+)"\s*\)"#);
            for line in content.lines() {
                let trimmed = line.trim();
                if let Some(caps) = re.captures(trimmed) {
                    let p = caps[1].trim();
                    if p.starts_with('.')
                        || p.contains('/')
                        || std::path::Path::new(p)
                            .extension()
                            .is_some_and(|e| e.eq_ignore_ascii_case("zig"))
                    {
                        imports.insert(clean_path_like(p));
                    }
                }
            }
            DepInfo {
                imports: imports.into_iter().collect(),
                exports: Vec::new(),
            }
        }
        _ => DepInfo {
            imports: Vec::new(),
            exports: Vec::new(),
        },
    }
}

fn extract_ts_deps(content: &str) -> DepInfo {
    let mut imports = HashSet::new();
    let mut exports = Vec::new();

    for line in content.lines() {
        let trimmed = line.trim();

        if let Some(caps) = import_re().captures(trimmed) {
            let path = &caps[1];
            if path.starts_with('.') || path.starts_with('/') {
                imports.insert(clean_import_path(path));
            }
        }
        if let Some(caps) = require_re().captures(trimmed) {
            let path = &caps[1];
            if path.starts_with('.') || path.starts_with('/') {
                imports.insert(clean_import_path(path));
            }
        }

        if trimmed.starts_with("export ") {
            if let Some(name) = extract_export_name(trimmed) {
                exports.push(name);
            }
        }
    }

    DepInfo {
        imports: imports.into_iter().collect(),
        exports,
    }
}

fn extract_rust_deps(content: &str) -> DepInfo {
    let mut imports = HashSet::new();
    let mut exports = Vec::new();

    for line in content.lines() {
        let trimmed = line.trim();

        if let Some(caps) = rust_use_re().captures(trimmed) {
            let path = &caps[1];
            if !path.starts_with("std::") && !path.starts_with("core::") {
                imports.insert(path.to_string());
            }
        }

        if trimmed.starts_with("pub fn ") || trimmed.starts_with("pub async fn ") {
            if let Some(name) = trimmed
                .split('(')
                .next()
                .and_then(|s| s.split_whitespace().last())
            {
                exports.push(name.to_string());
            }
        } else if trimmed.starts_with("pub struct ")
            || trimmed.starts_with("pub enum ")
            || trimmed.starts_with("pub trait ")
        {
            if let Some(name) = trimmed.split_whitespace().nth(2) {
                let clean = name.trim_end_matches(|c: char| !c.is_alphanumeric() && c != '_');
                exports.push(clean.to_string());
            }
        }
    }

    DepInfo {
        imports: imports.into_iter().collect(),
        exports,
    }
}

fn extract_python_deps(content: &str) -> DepInfo {
    let mut imports = HashSet::new();
    let mut exports = Vec::new();

    for line in content.lines() {
        let trimmed = line.trim();

        if let Some(caps) = py_import_re().captures(trimmed) {
            if let Some(m) = caps.get(1).or(caps.get(2)) {
                let module = m.as_str();
                if !module.starts_with("os")
                    && !module.starts_with("sys")
                    && !module.starts_with("json")
                {
                    imports.insert(module.to_string());
                }
            }
        }

        if trimmed.starts_with("def ") && !trimmed.contains('_') {
            if let Some(name) = trimmed
                .strip_prefix("def ")
                .and_then(|s| s.split('(').next())
            {
                exports.push(name.to_string());
            }
        } else if trimmed.starts_with("class ") {
            if let Some(name) = trimmed
                .strip_prefix("class ")
                .and_then(|s| s.split(['(', ':']).next())
            {
                exports.push(name.to_string());
            }
        }
    }

    DepInfo {
        imports: imports.into_iter().collect(),
        exports,
    }
}

fn extract_go_deps(content: &str) -> DepInfo {
    let mut imports = HashSet::new();
    let mut exports = Vec::new();

    let mut in_import_block = false;
    for line in content.lines() {
        let trimmed = line.trim();

        if trimmed.starts_with("import (") {
            in_import_block = true;
            continue;
        }
        if in_import_block {
            if trimmed == ")" {
                in_import_block = false;
                continue;
            }
            if let Some(caps) = go_import_re().captures(trimmed) {
                imports.insert(caps[1].to_string());
            }
        }

        if trimmed.starts_with("func ") {
            let name_part = trimmed.strip_prefix("func ").unwrap_or("");
            if let Some(name) = name_part.split('(').next() {
                let name = name.trim();
                if !name.is_empty() && name.starts_with(char::is_uppercase) {
                    exports.push(name.to_string());
                }
            }
        }
    }

    DepInfo {
        imports: imports.into_iter().collect(),
        exports,
    }
}

#[cfg(feature = "tree-sitter")]
fn extract_kotlin_deps(content: &str) -> DepInfo {
    let analysis = deep_queries::analyze(content, "kt");
    let imports = analysis
        .imports
        .into_iter()
        .map(|import| match import.kind {
            ImportKind::Star => format!("{}.*", import.source),
            _ => import.source,
        })
        .collect();

    DepInfo {
        imports,
        exports: analysis.exports,
    }
}

#[cfg(not(feature = "tree-sitter"))]
fn extract_kotlin_deps(_content: &str) -> DepInfo {
    DepInfo {
        imports: Vec::new(),
        exports: Vec::new(),
    }
}

fn clean_import_path(path: &str) -> String {
    path.trim_start_matches("./")
        .trim_end_matches(".js")
        .trim_end_matches(".ts")
        .trim_end_matches(".tsx")
        .trim_end_matches(".jsx")
        .to_string()
}

fn clean_path_like(path: &str) -> String {
    path.trim()
        .trim_start_matches("./")
        .trim_end_matches(".js")
        .trim_end_matches(".ts")
        .trim_end_matches(".tsx")
        .trim_end_matches(".jsx")
        .trim_end_matches(".py")
        .trim_end_matches(".go")
        .trim_end_matches(".rs")
        .trim_end_matches(".c")
        .trim_end_matches(".cpp")
        .trim_end_matches(".h")
        .trim_end_matches(".hpp")
        .trim_end_matches(".php")
        .trim_end_matches(".dart")
        .trim_end_matches(".zig")
        .trim_end_matches(".sh")
        .trim_end_matches(".bash")
        .to_string()
}

fn extract_c_like_deps(content: &str) -> DepInfo {
    let mut imports = HashSet::new();
    let re = static_regex!(r#"^\s*#\s*include\s*[<"]([^">]+)[">]"#);
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(caps) = re.captures(trimmed) {
            let inc = caps[1].trim();
            if inc.starts_with('.') || inc.contains('/') {
                imports.insert(clean_path_like(inc));
            }
        }
    }
    DepInfo {
        imports: imports.into_iter().collect(),
        exports: Vec::new(),
    }
}

fn extract_ruby_deps(content: &str) -> DepInfo {
    let mut imports = HashSet::new();
    let re = static_regex!(r#"^\s*require(?:_relative)?\s+['"]([^'"]+)['"]"#);
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(caps) = re.captures(trimmed) {
            let req = caps[1].trim();
            if req.starts_with('.') || req.contains('/') {
                imports.insert(clean_path_like(req));
            }
        }
    }
    DepInfo {
        imports: imports.into_iter().collect(),
        exports: Vec::new(),
    }
}

fn extract_php_deps(content: &str) -> DepInfo {
    let mut imports = HashSet::new();
    let re = static_regex!(
        r#"\b(?:require|require_once|include|include_once)\s*\(?\s*['"]([^'"]+)['"]"#
    );
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(caps) = re.captures(trimmed) {
            let p = caps[1].trim();
            if p.starts_with('.') || p.starts_with('/') {
                imports.insert(clean_path_like(p));
            }
        }
    }
    DepInfo {
        imports: imports.into_iter().collect(),
        exports: Vec::new(),
    }
}

fn extract_bash_deps(content: &str) -> DepInfo {
    let mut imports = HashSet::new();
    let re = static_regex!(r#"^\s*(?:source|\.)\s+['"]?([^'"\s;]+)['"]?"#);
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(caps) = re.captures(trimmed) {
            let p = caps[1].trim();
            if p.starts_with('.') || p.starts_with('/') {
                imports.insert(clean_path_like(p));
            }
        }
    }
    DepInfo {
        imports: imports.into_iter().collect(),
        exports: Vec::new(),
    }
}

fn extract_export_name(line: &str) -> Option<String> {
    let without_export = line.strip_prefix("export ")?;
    let without_default = without_export
        .strip_prefix("default ")
        .unwrap_or(without_export);

    for keyword in &[
        "function ",
        "async function ",
        "class ",
        "const ",
        "let ",
        "type ",
        "interface ",
        "enum ",
    ] {
        if let Some(rest) = without_default.strip_prefix(keyword) {
            let name = rest
                .split(|c: char| !c.is_alphanumeric() && c != '_')
                .next()?;
            if !name.is_empty() {
                return Some(name.to_string());
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn c_include_relative_is_extracted() {
        let src = r#"#include "foo/bar.h"
#include <stdio.h>
"#;
        let deps = extract_deps(src, "c");
        assert!(deps.imports.contains(&"foo/bar".to_string()));
        assert!(
            !deps.imports.iter().any(|i| i.contains("stdio")),
            "system includes should not be treated as internal deps"
        );
    }

    #[test]
    fn ruby_require_relative_is_extracted() {
        let src = r#"require_relative "./lib/utils"
require "json"
"#;
        let deps = extract_deps(src, "rb");
        assert!(deps.imports.contains(&"lib/utils".to_string()));
        assert!(
            !deps.imports.iter().any(|i| i == "json"),
            "external requires should not be treated as internal deps"
        );
    }

    #[test]
    fn php_require_is_extracted() {
        let src = r#"<?php
require_once "./vendor/autoload.php";
include "http://example.com/a.php";
"#;
        let deps = extract_deps(src, "php");
        assert!(deps.imports.contains(&"vendor/autoload".to_string()));
        assert!(
            deps.imports.iter().all(|i| !i.starts_with("http")),
            "remote includes should not be treated as internal deps"
        );
    }

    #[test]
    fn bash_source_is_extracted() {
        let src = r#"#!/usr/bin/env bash
source "./scripts/env.sh"
. ../common.sh
"#;
        let deps = extract_deps(src, "sh");
        assert!(deps.imports.contains(&"scripts/env".to_string()));
        assert!(deps.imports.contains(&"../common".to_string()));
    }

    #[test]
    fn dart_import_relative_is_extracted() {
        let src = r#"import "./src/util.dart";
import "package:foo/bar.dart";
"#;
        let deps = extract_deps(src, "dart");
        assert!(deps.imports.contains(&"src/util".to_string()));
        assert!(
            deps.imports.iter().all(|i| !i.starts_with("package:")),
            "package imports should not be treated as internal deps"
        );
    }

    #[test]
    fn zig_import_is_extracted() {
        let src = r#"const m = @import("lib/math.zig");
const std = @import("std");
"#;
        let deps = extract_deps(src, "zig");
        assert!(deps.imports.contains(&"lib/math".to_string()));
        assert!(!deps.imports.iter().any(|i| i == "std"), "std is external");
    }

    #[test]
    fn kotlin_deps_are_extracted_from_ast() {
        let content = r"
package com.example.app

import com.example.services.UserService
import com.example.shared.*

class Feature
fun build(): Feature = Feature()
";
        let deps = extract_deps(content, "kt");
        assert!(deps
            .imports
            .contains(&"com.example.services.UserService".to_string()));
        assert!(deps.imports.contains(&"com.example.shared.*".to_string()));
        assert!(deps.exports.contains(&"Feature".to_string()));
        assert!(deps.exports.contains(&"build".to_string()));
    }
}
