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
use std::sync::Arc;

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
    /// Namespace path (`A/B/C`) -> representative `.cs` file, for C# `using`
    /// resolution. Keyed by *declared* `namespace` first (authoritative) and by
    /// folder suffix as a fallback (see `build_csharp_namespace_index`).
    csharp_ns_index: HashMap<String, String>,
}

impl ResolverContext {
    /// `file_contents` is an optional in-memory cache (relative path -> source).
    /// It is used to read declared C# namespaces without touching disk; pass an
    /// empty map when contents are not available (a bounded head-read from disk
    /// is the fallback).
    pub fn new(
        project_root: &Path,
        file_paths: Vec<String>,
        file_contents: &HashMap<String, Arc<String>>,
    ) -> Self {
        let file_set: std::collections::HashSet<String> = file_paths.iter().cloned().collect();

        let tsconfig_paths = load_tsconfig_paths(project_root);
        let go_module = load_go_module(project_root);
        let dart_package = load_dart_package(project_root);
        let csharp_ns_index =
            build_csharp_namespace_index(project_root, &file_paths, file_contents);

        Self {
            project_root: project_root.to_path_buf(),
            file_paths,
            tsconfig_paths,
            go_module,
            dart_package,
            file_set,
            csharp_ns_index,
        }
    }

    fn file_exists(&self, rel_path: &str) -> bool {
        self.file_set.contains(rel_path)
    }

    /// Representative `.cs` file for a namespace path (`A/B/C`), matched as a
    /// directory suffix so root prefixes (`src/`, project folder) don't break it.
    fn csharp_namespace_file(&self, namespace_path: &str) -> Option<&str> {
        self.csharp_ns_index.get(namespace_path).map(String::as_str)
    }
}

/// Maps C# namespace paths (`A/B/C`) to a representative `.cs` file so that
/// `using A.B.C` resolves to a real project file. Two sources, in priority order:
///
/// 1. **Declared namespaces** (authoritative): the `namespace A.B.C` written in
///    each file, read from the in-memory content cache (or a bounded head-read
///    from disk). This is the only correct source when the namespace does *not*
///    mirror the folder layout (the common .NET case with a RootNamespace).
/// 2. **Folder suffixes** (fallback): every trailing directory suffix of each
///    file, for sources whose namespace we could not read.
///
/// Deterministic: the lexicographically smallest file wins for a given key.
fn build_csharp_namespace_index(
    project_root: &Path,
    file_paths: &[String],
    file_contents: &HashMap<String, Arc<String>>,
) -> HashMap<String, String> {
    let mut cs_files: Vec<&String> = file_paths
        .iter()
        .filter(|f| {
            Path::new(f.as_str())
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("cs"))
        })
        .collect();
    if cs_files.is_empty() {
        return HashMap::new();
    }
    cs_files.sort();

    let mut map: HashMap<String, String> = HashMap::new();

    // 1) Declared namespaces (authoritative). Content from the cache when present,
    //    otherwise a bounded head-read from disk. Capped to avoid pathological I/O.
    const MAX_CS_FILES_READ: usize = 5000;
    for file in cs_files.iter().take(MAX_CS_FILES_READ) {
        let content: Option<std::borrow::Cow<'_, str>> = match file_contents.get(*file) {
            Some(c) => Some(std::borrow::Cow::Borrowed(c.as_str())),
            None => read_file_head(&project_root.join(file.as_str()), 64 * 1024)
                .map(std::borrow::Cow::Owned),
        };
        let Some(content) = content else { continue };
        for ns in extract_csharp_namespaces(&content) {
            let key = ns.replace('.', "/");
            map.entry(key).or_insert_with(|| (*file).clone());
        }
    }

    // 2) Folder-suffix fallback (does not overwrite declared-namespace entries).
    for file in &cs_files {
        let dir = Path::new(file.as_str())
            .parent()
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default();
        let segs: Vec<&str> = dir.split('/').filter(|s| !s.is_empty()).collect();
        for start in 0..segs.len() {
            let key = segs[start..].join("/");
            map.entry(key).or_insert_with(|| (*file).clone());
        }
    }
    map
}

/// Extract every `namespace A.B.C` declared in a C# source (block or file-scoped).
fn extract_csharp_namespaces(content: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in content.lines() {
        let Some(rest) = line.trim_start().strip_prefix("namespace ") else {
            continue;
        };
        let name: String = rest
            .trim_start()
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '.' || *c == '_')
            .collect();
        if !name.is_empty() && !out.contains(&name) {
            out.push(name);
        }
    }
    out
}

/// Read at most `max_bytes` from the start of a file (namespace declarations are
/// always near the top), tolerating non-UTF-8 bytes. Returns `None` on error.
fn read_file_head(path: &Path, max_bytes: usize) -> Option<String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).ok()?;
    let mut buf = vec![0u8; max_bytes];
    let n = f.read(&mut buf).ok()?;
    buf.truncate(n);
    Some(String::from_utf8_lossy(&buf).into_owned())
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
        // `.tscn` ext_resource paths are `res://` references — identical shape to
        // a GDScript `preload`, so the GDScript resolver handles them. (#316)
        "gd" | "tscn" => resolve_gd(imp, file_path, ctx),
        "lua" | "luau" => resolve_lua(imp, file_path, ctx),
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
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&content)
                && let Some(compiler) = json.get("compilerOptions")
            {
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
