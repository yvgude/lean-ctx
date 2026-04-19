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
//! - Dart: relative + package:<name>/ (best-effort)
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
        _ => (None, true),
    }
}

// ---------------------------------------------------------------------------
// TypeScript / JavaScript
// ---------------------------------------------------------------------------

fn resolve_ts(imp: &ImportInfo, file_path: &str, ctx: &ResolverContext) -> (Option<String>, bool) {
    let source = &imp.source;

    if source.starts_with('.') {
        let dir = Path::new(file_path).parent().unwrap_or(Path::new(""));
        let resolved = dir.join(source);
        let normalized = normalize_path(&resolved);

        if let Some(found) = try_ts_extensions(&normalized, ctx) {
            return (Some(found), false);
        }
        return (None, false);
    }

    if let Some(mapped) = resolve_tsconfig_path(source, ctx) {
        return (Some(mapped), false);
    }

    (None, true)
}

fn try_ts_extensions(base: &str, ctx: &ResolverContext) -> Option<String> {
    let extensions = [".ts", ".tsx", ".js", ".jsx", ".d.ts"];

    if ctx.file_exists(base) {
        return Some(base.to_string());
    }

    for ext in &extensions {
        let with_ext = format!("{base}{ext}");
        if ctx.file_exists(&with_ext) {
            return Some(with_ext);
        }
    }

    let index_extensions = ["index.ts", "index.tsx", "index.js", "index.jsx"];
    for idx in &index_extensions {
        let index_path = format!("{base}/{idx}");
        if ctx.file_exists(&index_path) {
            return Some(index_path);
        }
    }

    None
}

fn resolve_tsconfig_path(source: &str, ctx: &ResolverContext) -> Option<String> {
    for (pattern, target) in &ctx.tsconfig_paths {
        let prefix = pattern.trim_end_matches('*');
        if let Some(remainder) = source.strip_prefix(prefix) {
            let target_base = target.trim_end_matches('*');
            let candidate = format!("{target_base}{remainder}");
            if let Some(found) = try_ts_extensions(&candidate, ctx) {
                return Some(found);
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Rust
// ---------------------------------------------------------------------------

fn resolve_rust(
    imp: &ImportInfo,
    file_path: &str,
    ctx: &ResolverContext,
) -> (Option<String>, bool) {
    let source = &imp.source;

    if source.starts_with("crate::")
        || source.starts_with("super::")
        || source.starts_with("self::")
    {
        let cleaned = source.replace("crate::", "").replace("self::", "");

        let resolved = if source.starts_with("super::") {
            let dir = Path::new(file_path).parent().and_then(|p| p.parent());
            match dir {
                Some(d) => {
                    let rest = source.trim_start_matches("super::");
                    d.join(rest.replace("::", "/"))
                        .to_string_lossy()
                        .to_string()
                }
                None => cleaned.replace("::", "/"),
            }
        } else {
            cleaned.replace("::", "/")
        };

        if let Some(found) = try_rust_paths(&resolved, ctx) {
            return (Some(found), false);
        }
        return (None, false);
    }

    let parts: Vec<&str> = source.split("::").collect();
    if parts.is_empty() {
        return (None, true);
    }

    let is_external = !source.starts_with("crate")
        && !ctx.file_paths.iter().any(|f| {
            let stem = Path::new(f)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("");
            stem == parts[0]
        });

    if is_external {
        return (None, true);
    }

    let as_path = source.replace("::", "/");
    if let Some(found) = try_rust_paths(&as_path, ctx) {
        return (Some(found), false);
    }

    (None, is_external)
}

fn try_rust_paths(base: &str, ctx: &ResolverContext) -> Option<String> {
    let prefixes = ["", "src/", "rust/src/"];
    for prefix in &prefixes {
        let candidate = format!("{prefix}{base}.rs");
        if ctx.file_exists(&candidate) {
            return Some(candidate);
        }
        let mod_candidate = format!("{prefix}{base}/mod.rs");
        if ctx.file_exists(&mod_candidate) {
            return Some(mod_candidate);
        }
    }

    let parts: Vec<&str> = base.rsplitn(2, '/').collect();
    if parts.len() == 2 {
        let parent = parts[1];
        for prefix in &prefixes {
            let candidate = format!("{prefix}{parent}.rs");
            if ctx.file_exists(&candidate) {
                return Some(candidate);
            }
        }
    }

    None
}

// ---------------------------------------------------------------------------
// Python
// ---------------------------------------------------------------------------

fn resolve_python(
    imp: &ImportInfo,
    file_path: &str,
    ctx: &ResolverContext,
) -> (Option<String>, bool) {
    let source = &imp.source;

    if source.starts_with('.') {
        let dot_count = source.chars().take_while(|c| *c == '.').count();
        let module_part = &source[dot_count..];

        let mut dir = Path::new(file_path).parent().unwrap_or(Path::new(""));
        for _ in 1..dot_count {
            dir = dir.parent().unwrap_or(Path::new(""));
        }

        let as_path = module_part.replace('.', "/");
        let base = if as_path.is_empty() {
            dir.to_string_lossy().to_string()
        } else {
            format!("{}/{as_path}", dir.display())
        };

        if let Some(found) = try_python_paths(&base, ctx) {
            return (Some(found), false);
        }
        return (None, false);
    }

    let as_path = source.replace('.', "/");

    if let Some(found) = try_python_paths(&as_path, ctx) {
        return (Some(found), false);
    }

    let is_stdlib = is_python_stdlib(source);
    (
        None,
        is_stdlib || !ctx.file_paths.iter().any(|f| f.contains(&as_path)),
    )
}

fn try_python_paths(base: &str, ctx: &ResolverContext) -> Option<String> {
    let py_file = format!("{base}.py");
    if ctx.file_exists(&py_file) {
        return Some(py_file);
    }

    let init_file = format!("{base}/__init__.py");
    if ctx.file_exists(&init_file) {
        return Some(init_file);
    }

    let prefixes = ["src/", "lib/"];
    for prefix in &prefixes {
        let candidate = format!("{prefix}{base}.py");
        if ctx.file_exists(&candidate) {
            return Some(candidate);
        }
        let init = format!("{prefix}{base}/__init__.py");
        if ctx.file_exists(&init) {
            return Some(init);
        }
    }

    None
}

fn is_python_stdlib(module: &str) -> bool {
    let first = module.split('.').next().unwrap_or(module);
    matches!(
        first,
        "os" | "sys"
            | "json"
            | "re"
            | "math"
            | "datetime"
            | "typing"
            | "collections"
            | "itertools"
            | "functools"
            | "pathlib"
            | "io"
            | "abc"
            | "enum"
            | "dataclasses"
            | "logging"
            | "unittest"
            | "argparse"
            | "subprocess"
            | "threading"
            | "multiprocessing"
            | "socket"
            | "http"
            | "urllib"
            | "hashlib"
            | "hmac"
            | "secrets"
            | "time"
            | "copy"
            | "pprint"
            | "textwrap"
            | "shutil"
            | "tempfile"
            | "glob"
            | "fnmatch"
            | "contextlib"
            | "inspect"
            | "importlib"
            | "pickle"
            | "shelve"
            | "csv"
            | "configparser"
            | "struct"
            | "codecs"
            | "string"
            | "difflib"
            | "ast"
            | "dis"
            | "traceback"
            | "warnings"
            | "concurrent"
            | "asyncio"
            | "signal"
            | "select"
    )
}

// ---------------------------------------------------------------------------
// Go
// ---------------------------------------------------------------------------

fn resolve_go(imp: &ImportInfo, ctx: &ResolverContext) -> (Option<String>, bool) {
    let source = &imp.source;

    if let Some(ref go_mod) = ctx.go_module {
        if source.starts_with(go_mod.as_str()) {
            let relative = source.strip_prefix(go_mod.as_str()).unwrap_or(source);
            let relative = relative.trim_start_matches('/');

            if let Some(found) = try_go_package(relative, ctx) {
                return (Some(found), false);
            }
            return (None, false);
        }
    }

    if let Some(found) = try_go_package(source, ctx) {
        return (Some(found), false);
    }

    (None, true)
}

fn try_go_package(pkg_path: &str, ctx: &ResolverContext) -> Option<String> {
    for file in &ctx.file_paths {
        if file.ends_with(".go") {
            let dir = Path::new(file).parent()?.to_string_lossy();
            if dir == pkg_path || dir.ends_with(pkg_path) {
                return Some(dir.to_string());
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Java
// ---------------------------------------------------------------------------

fn resolve_java(imp: &ImportInfo, ctx: &ResolverContext) -> (Option<String>, bool) {
    let source = &imp.source;

    if source.starts_with("java.") || source.starts_with("javax.") || source.starts_with("sun.") {
        return (None, true);
    }

    let parts: Vec<&str> = source.rsplitn(2, '.').collect();
    if parts.len() < 2 {
        return (None, true);
    }

    let class_name = parts[0];
    let package_path = parts[1].replace('.', "/");
    let file_path = format!("{package_path}/{class_name}.java");

    let search_roots = ["", "src/main/java/", "src/", "app/src/main/java/"];
    for root in &search_roots {
        let candidate = format!("{root}{file_path}");
        if ctx.file_exists(&candidate) {
            return (Some(candidate), false);
        }
    }

    (
        None,
        !ctx.file_paths.iter().any(|f| f.contains(&package_path)),
    )
}

// ---------------------------------------------------------------------------
// C / C++
// ---------------------------------------------------------------------------

fn resolve_c_like(
    imp: &ImportInfo,
    file_path: &str,
    ctx: &ResolverContext,
) -> (Option<String>, bool) {
    let source = imp.source.trim();
    if source.is_empty() {
        return (None, true);
    }

    let try_prefixes = |prefixes: &[&str], rel: &str| -> Option<String> {
        let rel = rel.trim_start_matches("./").trim_start_matches('/');
        let mut candidates: Vec<String> = vec![rel.to_string()];
        for ext in [".h", ".hpp", ".c", ".cpp"] {
            if !rel.ends_with(ext) {
                candidates.push(format!("{rel}{ext}"));
            }
        }
        for prefix in prefixes {
            for c in candidates.iter() {
                let p = format!("{prefix}{c}");
                if ctx.file_exists(&p) {
                    return Some(p);
                }
            }
        }
        None
    };

    if source.starts_with('.') {
        let dir = Path::new(file_path).parent().unwrap_or(Path::new(""));
        let dir_prefix = if dir.as_os_str().is_empty() {
            "".to_string()
        } else {
            format!("{}/", dir.to_string_lossy())
        };
        if let Some(found) = try_prefixes(&[dir_prefix.as_str()], source) {
            return (Some(found), false);
        }
        return (None, false);
    }

    if ctx.file_exists(source) {
        return (Some(source.to_string()), false);
    }

    if let Some(found) = try_prefixes(&["", "include/", "src/"], source) {
        return (Some(found), false);
    }

    (None, true)
}

// ---------------------------------------------------------------------------
// Ruby
// ---------------------------------------------------------------------------

fn resolve_ruby(
    imp: &ImportInfo,
    file_path: &str,
    ctx: &ResolverContext,
) -> (Option<String>, bool) {
    let source = imp.source.trim();
    if source.is_empty() {
        return (None, true);
    }
    let source_rel = source.trim_start_matches("./").trim_start_matches('/');

    let try_prefixes = |prefixes: &[&str]| -> Option<String> {
        let mut candidates: Vec<String> = vec![source_rel.to_string()];
        if !source_rel.ends_with(".rb") {
            candidates.push(format!("{source_rel}.rb"));
        }
        for prefix in prefixes {
            for c in candidates.iter() {
                let p = format!("{prefix}{c}");
                if ctx.file_exists(&p) {
                    return Some(p);
                }
            }
        }
        None
    };

    if source.starts_with('.') || source_rel.contains('/') {
        let dir = Path::new(file_path).parent().unwrap_or(Path::new(""));
        let dir_prefix = if dir.as_os_str().is_empty() {
            "".to_string()
        } else {
            format!("{}/", dir.to_string_lossy())
        };
        if let Some(found) = try_prefixes(&[dir_prefix.as_str()]) {
            return (Some(found), false);
        }
        if let Some(found) = try_prefixes(&["", "lib/", "src/"]) {
            return (Some(found), false);
        }
        return (None, false);
    }

    (None, true)
}

// ---------------------------------------------------------------------------
// PHP
// ---------------------------------------------------------------------------

fn resolve_php(imp: &ImportInfo, file_path: &str, ctx: &ResolverContext) -> (Option<String>, bool) {
    let source = imp.source.trim();
    if source.is_empty() {
        return (None, true);
    }
    if source.starts_with("http://") || source.starts_with("https://") {
        return (None, true);
    }
    let source_rel = source.trim_start_matches("./").trim_start_matches('/');

    let try_prefixes = |prefixes: &[&str]| -> Option<String> {
        let mut candidates: Vec<String> = vec![source_rel.to_string()];
        if !source_rel.ends_with(".php") {
            candidates.push(format!("{source_rel}.php"));
        }
        for prefix in prefixes {
            for c in candidates.iter() {
                let p = format!("{prefix}{c}");
                if ctx.file_exists(&p) {
                    return Some(p);
                }
            }
        }
        None
    };

    if source.starts_with('.') || source.starts_with('/') || source_rel.contains('/') {
        let dir = Path::new(file_path).parent().unwrap_or(Path::new(""));
        let dir_prefix = if dir.as_os_str().is_empty() {
            "".to_string()
        } else {
            format!("{}/", dir.to_string_lossy())
        };
        if let Some(found) = try_prefixes(&[dir_prefix.as_str()]) {
            return (Some(found), false);
        }
        if let Some(found) = try_prefixes(&["", "src/", "lib/"]) {
            return (Some(found), false);
        }
        return (None, false);
    }

    (None, true)
}

// ---------------------------------------------------------------------------
// Bash
// ---------------------------------------------------------------------------

fn resolve_bash(
    imp: &ImportInfo,
    file_path: &str,
    ctx: &ResolverContext,
) -> (Option<String>, bool) {
    let source = imp.source.trim();
    if source.is_empty() {
        return (None, true);
    }
    let source_rel = source.trim_start_matches("./").trim_start_matches('/');

    let try_prefixes = |prefixes: &[&str]| -> Option<String> {
        let mut candidates: Vec<String> = vec![source_rel.to_string()];
        if !source_rel.ends_with(".sh") {
            candidates.push(format!("{source_rel}.sh"));
        }
        for prefix in prefixes {
            for c in candidates.iter() {
                let p = format!("{prefix}{c}");
                if ctx.file_exists(&p) {
                    return Some(p);
                }
            }
        }
        None
    };

    if source.starts_with('.') || source.starts_with('/') || source_rel.contains('/') {
        let dir = Path::new(file_path).parent().unwrap_or(Path::new(""));
        let dir_prefix = if dir.as_os_str().is_empty() {
            "".to_string()
        } else {
            format!("{}/", dir.to_string_lossy())
        };
        if let Some(found) = try_prefixes(&[dir_prefix.as_str()]) {
            return (Some(found), false);
        }
        if let Some(found) = try_prefixes(&["", "scripts/", "bin/"]) {
            return (Some(found), false);
        }
        return (None, false);
    }

    (None, true)
}

// ---------------------------------------------------------------------------
// Dart
// ---------------------------------------------------------------------------

fn resolve_dart(
    imp: &ImportInfo,
    file_path: &str,
    ctx: &ResolverContext,
) -> (Option<String>, bool) {
    let source = imp.source.trim();
    if source.is_empty() {
        return (None, true);
    }
    if source.starts_with("dart:") {
        return (None, true);
    }

    let try_prefixes = |prefixes: &[&str], rel: &str| -> Option<String> {
        let rel = rel.trim_start_matches("./").trim_start_matches('/').trim();
        let mut candidates: Vec<String> = vec![rel.to_string()];
        if !rel.ends_with(".dart") {
            candidates.push(format!("{rel}.dart"));
        }
        for prefix in prefixes {
            for c in candidates.iter() {
                let p = format!("{prefix}{c}");
                if ctx.file_exists(&p) {
                    return Some(p);
                }
            }
        }
        None
    };

    if source.starts_with("package:") {
        if let Some(pkg) = ctx.dart_package.as_deref() {
            let prefix = format!("package:{pkg}/");
            if let Some(rest) = source.strip_prefix(&prefix) {
                if let Some(found) = try_prefixes(&["lib/", ""], rest) {
                    return (Some(found), false);
                }
                return (None, false);
            }
        }
        return (None, true);
    }

    if source.starts_with('.') || source.starts_with('/') {
        let dir = Path::new(file_path).parent().unwrap_or(Path::new(""));
        let dir_prefix = if dir.as_os_str().is_empty() {
            "".to_string()
        } else {
            format!("{}/", dir.to_string_lossy())
        };
        if let Some(found) = try_prefixes(&[dir_prefix.as_str()], source) {
            return (Some(found), false);
        }
        if let Some(found) = try_prefixes(&["", "lib/"], source) {
            return (Some(found), false);
        }
        return (None, false);
    }

    (None, true)
}

// ---------------------------------------------------------------------------
// Zig
// ---------------------------------------------------------------------------

fn resolve_zig(imp: &ImportInfo, file_path: &str, ctx: &ResolverContext) -> (Option<String>, bool) {
    let source = imp.source.trim();
    if source.is_empty() {
        return (None, true);
    }
    let source_rel = source.trim_start_matches("./").trim_start_matches('/');
    if source_rel == "std" {
        return (None, true);
    }

    let try_prefixes = |prefixes: &[&str]| -> Option<String> {
        let mut candidates: Vec<String> = vec![source_rel.to_string()];
        if !source_rel.ends_with(".zig") {
            candidates.push(format!("{source_rel}.zig"));
        }
        for prefix in prefixes {
            for c in candidates.iter() {
                let p = format!("{prefix}{c}");
                if ctx.file_exists(&p) {
                    return Some(p);
                }
            }
        }
        None
    };

    if source.starts_with('.') || source_rel.contains('/') || source_rel.ends_with(".zig") {
        let dir = Path::new(file_path).parent().unwrap_or(Path::new(""));
        let dir_prefix = if dir.as_os_str().is_empty() {
            "".to_string()
        } else {
            format!("{}/", dir.to_string_lossy())
        };
        if let Some(found) = try_prefixes(&[dir_prefix.as_str()]) {
            return (Some(found), false);
        }
        if let Some(found) = try_prefixes(&["", "src/"]) {
            return (Some(found), false);
        }
        return (None, false);
    }

    (None, true)
}

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
            std::path::Component::CurDir => {}
            std::path::Component::Normal(s) => {
                parts.push(s.to_str().unwrap_or(""));
            }
            _ => {}
        }
    }
    parts.join("/")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::deep_queries::{ImportInfo, ImportKind};

    fn make_ctx(files: &[&str]) -> ResolverContext {
        ResolverContext {
            project_root: PathBuf::from("/project"),
            file_paths: files.iter().map(|s| s.to_string()).collect(),
            tsconfig_paths: HashMap::new(),
            go_module: None,
            dart_package: None,
            file_set: files.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn make_import(source: &str) -> ImportInfo {
        ImportInfo {
            source: source.to_string(),
            names: Vec::new(),
            kind: ImportKind::Named,
            line: 1,
            is_type_only: false,
        }
    }

    // --- TypeScript ---

    #[test]
    fn ts_relative_import() {
        let ctx = make_ctx(&["src/components/Button.tsx", "src/utils/helpers.ts"]);
        let imp = make_import("./helpers");
        let results = resolve_imports(&[imp], "src/utils/index.ts", "ts", &ctx);
        assert_eq!(
            results[0].resolved_path.as_deref(),
            Some("src/utils/helpers.ts")
        );
        assert!(!results[0].is_external);
    }

    #[test]
    fn ts_relative_parent() {
        let ctx = make_ctx(&["src/utils.ts", "src/components/Button.tsx"]);
        let imp = make_import("../utils");
        let results = resolve_imports(&[imp], "src/components/Button.tsx", "ts", &ctx);
        assert_eq!(results[0].resolved_path.as_deref(), Some("src/utils.ts"));
    }

    #[test]
    fn ts_index_file() {
        let ctx = make_ctx(&["src/components/index.ts", "src/app.ts"]);
        let imp = make_import("./components");
        let results = resolve_imports(&[imp], "src/app.ts", "ts", &ctx);
        assert_eq!(
            results[0].resolved_path.as_deref(),
            Some("src/components/index.ts")
        );
    }

    #[test]
    fn ts_external_package() {
        let ctx = make_ctx(&["src/app.ts"]);
        let imp = make_import("react");
        let results = resolve_imports(&[imp], "src/app.ts", "ts", &ctx);
        assert!(results[0].is_external);
        assert!(results[0].resolved_path.is_none());
    }

    #[test]
    fn ts_tsconfig_paths() {
        let mut ctx = make_ctx(&["src/lib/utils/format.ts"]);
        ctx.tsconfig_paths
            .insert("@utils/*".to_string(), "src/lib/utils/*".to_string());
        let imp = make_import("@utils/format");
        let results = resolve_imports(&[imp], "src/app.ts", "ts", &ctx);
        assert_eq!(
            results[0].resolved_path.as_deref(),
            Some("src/lib/utils/format.ts")
        );
        assert!(!results[0].is_external);
    }

    // --- Rust ---

    #[test]
    fn rust_crate_import() {
        let ctx = make_ctx(&["src/core/session.rs", "src/main.rs"]);
        let imp = make_import("crate::core::session");
        let results = resolve_imports(&[imp], "src/server.rs", "rs", &ctx);
        assert_eq!(
            results[0].resolved_path.as_deref(),
            Some("src/core/session.rs")
        );
        assert!(!results[0].is_external);
    }

    #[test]
    fn rust_mod_rs() {
        let ctx = make_ctx(&["src/core/mod.rs", "src/main.rs"]);
        let imp = make_import("crate::core");
        let results = resolve_imports(&[imp], "src/main.rs", "rs", &ctx);
        assert_eq!(results[0].resolved_path.as_deref(), Some("src/core/mod.rs"));
    }

    #[test]
    fn rust_external_crate() {
        let ctx = make_ctx(&["src/main.rs"]);
        let imp = make_import("anyhow::Result");
        let results = resolve_imports(&[imp], "src/main.rs", "rs", &ctx);
        assert!(results[0].is_external);
    }

    #[test]
    fn rust_symbol_in_module() {
        let ctx = make_ctx(&["src/core/session.rs"]);
        let imp = make_import("crate::core::session::SessionState");
        let results = resolve_imports(&[imp], "src/server.rs", "rs", &ctx);
        assert_eq!(
            results[0].resolved_path.as_deref(),
            Some("src/core/session.rs")
        );
    }

    // --- Python ---

    #[test]
    fn python_absolute_import() {
        let ctx = make_ctx(&["models/user.py", "app.py"]);
        let imp = make_import("models.user");
        let results = resolve_imports(&[imp], "app.py", "py", &ctx);
        assert_eq!(results[0].resolved_path.as_deref(), Some("models/user.py"));
    }

    #[test]
    fn python_package_init() {
        let ctx = make_ctx(&["utils/__init__.py", "app.py"]);
        let imp = make_import("utils");
        let results = resolve_imports(&[imp], "app.py", "py", &ctx);
        assert_eq!(
            results[0].resolved_path.as_deref(),
            Some("utils/__init__.py")
        );
    }

    #[test]
    fn python_relative_import() {
        let ctx = make_ctx(&["pkg/utils.py", "pkg/main.py"]);
        let imp = make_import(".utils");
        let results = resolve_imports(&[imp], "pkg/main.py", "py", &ctx);
        assert_eq!(results[0].resolved_path.as_deref(), Some("pkg/utils.py"));
    }

    #[test]
    fn python_stdlib() {
        let ctx = make_ctx(&["app.py"]);
        let imp = make_import("os");
        let results = resolve_imports(&[imp], "app.py", "py", &ctx);
        assert!(results[0].is_external);
    }

    // --- Go ---

    #[test]
    fn go_internal_package() {
        let mut ctx = make_ctx(&["cmd/server/main.go", "internal/auth/auth.go"]);
        ctx.go_module = Some("github.com/org/project".to_string());
        let imp = make_import("github.com/org/project/internal/auth");
        let results = resolve_imports(&[imp], "cmd/server/main.go", "go", &ctx);
        assert_eq!(results[0].resolved_path.as_deref(), Some("internal/auth"));
        assert!(!results[0].is_external);
    }

    #[test]
    fn go_external_package() {
        let ctx = make_ctx(&["main.go"]);
        let imp = make_import("fmt");
        let results = resolve_imports(&[imp], "main.go", "go", &ctx);
        assert!(results[0].is_external);
    }

    // --- Java ---

    #[test]
    fn java_internal_class() {
        let ctx = make_ctx(&[
            "src/main/java/com/example/service/UserService.java",
            "src/main/java/com/example/model/User.java",
        ]);
        let imp = make_import("com.example.model.User");
        let results = resolve_imports(
            &[imp],
            "src/main/java/com/example/service/UserService.java",
            "java",
            &ctx,
        );
        assert_eq!(
            results[0].resolved_path.as_deref(),
            Some("src/main/java/com/example/model/User.java")
        );
        assert!(!results[0].is_external);
    }

    #[test]
    fn java_stdlib() {
        let ctx = make_ctx(&["Main.java"]);
        let imp = make_import("java.util.List");
        let results = resolve_imports(&[imp], "Main.java", "java", &ctx);
        assert!(results[0].is_external);
    }

    // --- Edge cases ---

    #[test]
    fn empty_imports() {
        let ctx = make_ctx(&["src/main.rs"]);
        let results = resolve_imports(&[], "src/main.rs", "rs", &ctx);
        assert!(results.is_empty());
    }

    #[test]
    fn unsupported_language() {
        let ctx = make_ctx(&["main.rb"]);
        let imp = make_import("some_module");
        let results = resolve_imports(&[imp], "main.rb", "rb", &ctx);
        assert!(results[0].is_external);
    }

    #[test]
    fn c_include_resolves_from_include_dir() {
        let ctx = make_ctx(&["include/foo/bar.h", "src/main.c"]);
        let imp = make_import("foo/bar.h");
        let results = resolve_imports(&[imp], "src/main.c", "c", &ctx);
        assert_eq!(
            results[0].resolved_path.as_deref(),
            Some("include/foo/bar.h")
        );
        assert!(!results[0].is_external);
    }

    #[test]
    fn ruby_require_relative_resolves() {
        let ctx = make_ctx(&["lib/utils.rb", "app.rb"]);
        let imp = make_import("./lib/utils");
        let results = resolve_imports(&[imp], "app.rb", "rb", &ctx);
        assert_eq!(results[0].resolved_path.as_deref(), Some("lib/utils.rb"));
        assert!(!results[0].is_external);
    }

    #[test]
    fn php_require_resolves() {
        let ctx = make_ctx(&["vendor/autoload.php", "index.php"]);
        let imp = make_import("./vendor/autoload.php");
        let results = resolve_imports(&[imp], "index.php", "php", &ctx);
        assert_eq!(
            results[0].resolved_path.as_deref(),
            Some("vendor/autoload.php")
        );
        assert!(!results[0].is_external);
    }

    #[test]
    fn bash_source_resolves() {
        let ctx = make_ctx(&["scripts/env.sh", "main.sh"]);
        let imp = make_import("./scripts/env.sh");
        let results = resolve_imports(&[imp], "main.sh", "sh", &ctx);
        assert_eq!(results[0].resolved_path.as_deref(), Some("scripts/env.sh"));
        assert!(!results[0].is_external);
    }

    #[test]
    fn dart_package_import_resolves_to_lib() {
        let mut ctx = make_ctx(&["lib/src/util.dart", "lib/app.dart"]);
        ctx.dart_package = Some("myapp".to_string());
        let imp = make_import("package:myapp/src/util.dart");
        let results = resolve_imports(&[imp], "lib/app.dart", "dart", &ctx);
        assert_eq!(
            results[0].resolved_path.as_deref(),
            Some("lib/src/util.dart")
        );
        assert!(!results[0].is_external);
    }
}
