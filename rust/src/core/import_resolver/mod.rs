//! Import-to-file resolution (AST-driven import strings → project paths).
//!
//! Resolves import strings from `deep_queries::ImportInfo` to actual file paths
//! within a project. Handles language-specific module systems:
//! - TypeScript/JavaScript: relative paths, index files, package.json, tsconfig paths
//! - Python: dotted modules, __init__.py, relative imports
//! - Rust: crate/super/self resolution, mod.rs
//! - Go: go.mod module path, package = directory
//! - Java: package-to-directory mapping
//! - C/C++: local includes (best-effort)
//! - Ruby: require_relative (best-effort)
//! - PHP: include/require (best-effort)
//! - Bash: source/. (best-effort)
//! - Dart: relative + `package:<name>/` (best-effort)
//! - Zig: @import("path.zig") (best-effort)

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::deep_queries::ImportInfo;

#[derive(Debug, Clone)]
pub struct ResolvedImport {
    pub source: String,
    pub resolved_path: Option<String>,
    pub is_external: bool,
    pub line: usize,
}

#[derive(Debug)]
pub struct ResolverContext {
    pub project_root: PathBuf,
    pub file_paths: Vec<String>,
    pub tsconfig_paths: HashMap<String, String>,
    pub go_module: Option<String>,
    pub dart_package: Option<String>,
    file_set: std::collections::HashSet<String>,
}

impl ResolverContext {
    pub fn new(project_root: &Path, file_paths: Vec<String>) -> Self {
        let file_set: std::collections::HashSet<String> = file_paths.iter().cloned().collect();

        let tsconfig_paths = load_tsconfig_paths(project_root);
        let go_module = load_go_module(project_root);
        let dart_package = load_dart_package(project_root);

        Self {
            project_root: project_root.to_path_buf(),
            file_paths,
            tsconfig_paths,
            go_module,
            dart_package,
            file_set,
        }
    }

    fn file_exists(&self, rel_path: &str) -> bool {
        self.file_set.contains(rel_path)
    }
}

pub fn resolve_imports(
    imports: &[ImportInfo],
    file_path: &str,
    ext: &str,
    ctx: &ResolverContext,
) -> Vec<ResolvedImport> {
    imports
        .iter()
        .map(|imp| {
            let (resolved, is_external) = resolve_one(imp, file_path, ext, ctx);
            ResolvedImport {
                source: imp.source.clone(),
                resolved_path: resolved,
                is_external,
                line: imp.line,
            }
        })
        .collect()
}

fn resolve_one(
    imp: &ImportInfo,
    file_path: &str,
    ext: &str,
    ctx: &ResolverContext,
) -> (Option<String>, bool) {
    match ext {
        "ts" | "tsx" | "js" | "jsx" => resolve_ts(imp, file_path, ctx),
        "rs" => resolve_rust(imp, file_path, ctx),
        "py" => resolve_python(imp, file_path, ctx),
        "go" => resolve_go(imp, ctx),
        "java" => resolve_java(imp, ctx),
        "c" | "h" | "cpp" | "cc" | "cxx" | "hpp" | "hxx" | "hh" => {
            resolve_c_like(imp, file_path, ctx)
        }
        "rb" => resolve_ruby(imp, file_path, ctx),
        "php" => resolve_php(imp, file_path, ctx),
        "sh" | "bash" => resolve_bash(imp, file_path, ctx),
        "dart" => resolve_dart(imp, file_path, ctx),
        "zig" => resolve_zig(imp, file_path, ctx),
        "kt" | "kts" => resolve_kotlin(imp, ctx),
        "cs" => resolve_csharp(imp, ctx),
        "swift" => resolve_swift(imp, file_path, ctx),
        "scala" | "sc" => resolve_scala(imp, ctx),
        "ex" | "exs" => resolve_elixir(imp, file_path, ctx),
        "gd" => resolve_gd(imp, file_path, ctx),
        _ => (None, true),
    }
}

mod languages;
#[allow(clippy::wildcard_imports)]
use languages::*;

// ---------------------------------------------------------------------------
// Config Loaders
// ---------------------------------------------------------------------------

fn load_tsconfig_paths(root: &Path) -> HashMap<String, String> {
    let mut paths = HashMap::new();

    let candidates = ["tsconfig.json", "tsconfig.base.json", "jsconfig.json"];
    for name in &candidates {
        let tsconfig_path = root.join(name);
        if let Ok(content) = std::fs::read_to_string(&tsconfig_path) {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) {
                if let Some(compiler) = json.get("compilerOptions") {
                    let base_url = compiler
                        .get("baseUrl")
                        .and_then(|v| v.as_str())
                        .unwrap_or(".");

                    if let Some(path_map) = compiler.get("paths").and_then(|v| v.as_object()) {
                        for (pattern, targets) in path_map {
                            if let Some(first_target) = targets
                                .as_array()
                                .and_then(|a| a.first())
                                .and_then(|v| v.as_str())
                            {
                                let resolved = if base_url == "." {
                                    first_target.to_string()
                                } else {
                                    format!("{base_url}/{first_target}")
                                };
                                paths.insert(pattern.clone(), resolved);
                            }
                        }
                    }
                }
            }
            break;
        }
    }

    paths
}

fn load_go_module(root: &Path) -> Option<String> {
    let go_mod = root.join("go.mod");
    let content = std::fs::read_to_string(go_mod).ok()?;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("module ") {
            return Some(trimmed.strip_prefix("module ")?.trim().to_string());
        }
    }
    None
}

fn load_dart_package(root: &Path) -> Option<String> {
    let pubspec = root.join("pubspec.yaml");
    let content = std::fs::read_to_string(pubspec).ok()?;
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("name:") {
            let name = rest.trim();
            if !name.is_empty() {
                return Some(name.to_string());
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn normalize_path(path: &Path) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                parts.pop();
            }
            std::path::Component::Normal(s) => {
                parts.push(s.to_str().unwrap_or(""));
            }
            _ => {}
        }
    }
    parts.join("/")
}

#[cfg(test)]
mod tests;
