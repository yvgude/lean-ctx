//! Per-language import resolvers.
//!
//! Each `resolve_*` maps a language's import specifier to a project file path and
//! is dispatched from [`super::resolve_one`]. Language-private helpers (path
//! probing, stdlib checks) stay private to this module.

#[allow(clippy::wildcard_imports)]
use super::*;

// ---------------------------------------------------------------------------
// TypeScript / JavaScript
// ---------------------------------------------------------------------------

pub(super) fn resolve_ts(
    imp: &ImportInfo,
    file_path: &str,
    ctx: &ResolverContext,
) -> (Option<String>, bool) {
    let source = &imp.source;

    if source.starts_with('.') {
        let dir = Path::new(file_path).parent().unwrap_or(Path::new(""));
        let resolved = dir.join(source);
        let normalized = normalize_path(&resolved);

        if let Some(found) = try_ts_with_js_remap(&normalized, ctx) {
            return (Some(found), false);
        }
        return (None, false);
    }

    if let Some(mapped) = resolve_tsconfig_path(source, ctx) {
        return (Some(mapped), false);
    }

    (None, true)
}

/// Resolve a TS/JS import path, handling the TypeScript convention where
/// `.js` specifiers in `.ts` files resolve to `.ts` sources.
/// See: <https://www.typescriptlang.org/docs/handbook/modules/reference.html#relative-file-path-resolution>
fn try_ts_with_js_remap(base: &str, ctx: &ResolverContext) -> Option<String> {
    const JS_TO_TS: &[(&str, &[&str])] = &[
        (".js", &[".ts", ".tsx"]),
        (".jsx", &[".tsx", ".ts"]),
        (".mjs", &[".mts"]),
        (".cjs", &[".cts"]),
    ];

    for &(js_ext, ts_exts) in JS_TO_TS {
        if let Some(stem) = base.strip_suffix(js_ext) {
            for ts_ext in ts_exts {
                let candidate = format!("{stem}{ts_ext}");
                if ctx.file_exists(&candidate) {
                    return Some(candidate);
                }
            }
        }
    }

    try_ts_extensions(base, ctx)
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
            if let Some(found) = try_ts_with_js_remap(&candidate, ctx) {
                return Some(found);
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Rust
// ---------------------------------------------------------------------------

pub(super) fn resolve_rust(
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
        && !source.starts_with("super")
        && !source.starts_with("self")
        && !ctx.file_paths.iter().any(|f| {
            let stem = Path::new(f)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("");
            stem == parts[0] || f.contains(&format!("/{}/", parts[0]))
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

pub(super) fn resolve_python(
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

pub(super) fn resolve_go(imp: &ImportInfo, ctx: &ResolverContext) -> (Option<String>, bool) {
    let source = &imp.source;

    if let Some(ref go_mod) = ctx.go_module
        && source.starts_with(go_mod.as_str())
    {
        let relative = source.strip_prefix(go_mod.as_str()).unwrap_or(source);
        let relative = relative.trim_start_matches('/');

        if let Some(found) = try_go_package(relative, ctx) {
            return (Some(found), false);
        }
        return (None, false);
    }

    if let Some(found) = try_go_package(source, ctx) {
        return (Some(found), false);
    }

    (None, true)
}

fn try_go_package(pkg_path: &str, ctx: &ResolverContext) -> Option<String> {
    for file in &ctx.file_paths {
        let p = Path::new(file.as_str());
        if p.extension().and_then(|e| e.to_str()) != Some("go") {
            continue;
        }
        if file.ends_with("_test.go") {
            continue;
        }
        let dir = p.parent()?.to_string_lossy();
        if dir == pkg_path || dir.ends_with(pkg_path) {
            return Some(file.clone());
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Java
// ---------------------------------------------------------------------------

pub(super) fn resolve_java(imp: &ImportInfo, ctx: &ResolverContext) -> (Option<String>, bool) {
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
// Kotlin
// ---------------------------------------------------------------------------

pub(super) fn resolve_kotlin(imp: &ImportInfo, ctx: &ResolverContext) -> (Option<String>, bool) {
    let source = &imp.source;

    if source.starts_with("java.")
        || source.starts_with("javax.")
        || source.starts_with("kotlin.")
        || source.starts_with("kotlinx.")
        || source.starts_with("android.")
        || source.starts_with("androidx.")
        || source.starts_with("org.junit.")
        || source.starts_with("org.jetbrains.")
    {
        return (None, true);
    }

    let parts: Vec<&str> = source.rsplitn(2, '.').collect();
    if parts.len() < 2 {
        return (None, true);
    }

    let class_name = parts[0];
    let package_path = parts[1].replace('.', "/");

    let search_roots = [
        "",
        "src/main/kotlin/",
        "src/main/java/",
        "src/",
        "app/src/main/kotlin/",
        "app/src/main/java/",
        "src/commonMain/kotlin/",
    ];

    for root in &search_roots {
        for ext in &["kt", "kts", "java"] {
            let candidate = format!("{root}{package_path}/{class_name}.{ext}");
            if ctx.file_exists(&candidate) {
                return (Some(candidate), false);
            }
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

pub(super) fn resolve_c_like(
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
            for c in &candidates {
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
            String::new()
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

pub(super) fn resolve_ruby(
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
        if !Path::new(source_rel)
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("rb"))
        {
            candidates.push(format!("{source_rel}.rb"));
        }
        for prefix in prefixes {
            for c in &candidates {
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
            String::new()
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

pub(super) fn resolve_php(
    imp: &ImportInfo,
    file_path: &str,
    ctx: &ResolverContext,
) -> (Option<String>, bool) {
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
        if !Path::new(source_rel)
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("php"))
        {
            candidates.push(format!("{source_rel}.php"));
        }
        for prefix in prefixes {
            for c in &candidates {
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
            String::new()
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

pub(super) fn resolve_bash(
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
        if !Path::new(source_rel)
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("sh"))
        {
            candidates.push(format!("{source_rel}.sh"));
        }
        for prefix in prefixes {
            for c in &candidates {
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
            String::new()
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

pub(super) fn resolve_dart(
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
        if !Path::new(rel)
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("dart"))
        {
            candidates.push(format!("{rel}.dart"));
        }
        for prefix in prefixes {
            for c in &candidates {
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
            String::new()
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
// GDScript (Godot)
// ---------------------------------------------------------------------------

/// Resolves `GDScript` `extends "res://…"` and `preload`/`load` resource paths.
/// `res://` is anchored at the project root; `user://` is a runtime data path
/// (never a source file); other paths are resolved relative to the importer.
pub(super) fn resolve_gd(
    imp: &ImportInfo,
    file_path: &str,
    ctx: &ResolverContext,
) -> (Option<String>, bool) {
    let source = imp.source.trim();
    if source.is_empty() {
        return (None, true);
    }

    // Probe the verbatim path first (the common `preload("…/Foo.tscn")` form
    // already carries its extension), then Godot resource suffixes for imports
    // that omit one (`extends "res://actors/Player"`). #315
    let try_paths = |rel: &str| -> Option<String> {
        let rel = rel.trim();
        if ctx.file_exists(rel) {
            return Some(rel.to_string());
        }
        if Path::new(rel).extension().is_none() {
            for ext in ["gd", "tscn", "tres"] {
                let candidate = format!("{rel}.{ext}");
                if ctx.file_exists(&candidate) {
                    return Some(candidate);
                }
            }
        }
        None
    };

    if let Some(rest) = source.strip_prefix("res://") {
        let rel = rest.trim_start_matches('/');
        if let Some(found) = try_paths(rel) {
            return (Some(found), false);
        }
        // `res://` always names an intra-project resource. When the target is a
        // concrete file (carries a resource extension) that simply isn't indexed
        // yet — e.g. a `.tscn` scene before scene indexing exists (#316) — still
        // emit the edge to the declared path so scene references survive. #315
        if !rel.is_empty() && Path::new(rel).extension().is_some() {
            return (Some(rel.to_string()), false);
        }
        return (None, false);
    }

    // Runtime user data path — not a project source file.
    if source.starts_with("user://") {
        return (None, true);
    }

    if source.starts_with('.') || source.contains('/') {
        let stripped = source.trim_start_matches("./");
        let dir = Path::new(file_path).parent().unwrap_or(Path::new(""));
        let joined = if dir.as_os_str().is_empty() {
            stripped.to_string()
        } else {
            format!("{}/{stripped}", dir.to_string_lossy())
        };
        if let Some(found) = try_paths(&joined) {
            return (Some(found), false);
        }
        return (try_paths(stripped), false);
    }

    (None, true)
}

// ---------------------------------------------------------------------------
// Lua / Luau
// ---------------------------------------------------------------------------

/// Resolves Lua `require("a.b.c")` (dotted module path, package.path style) and
/// Luau `require("a/b")` / `require("./a")` (slash/relative paths) to a project
/// file. Dotted specifiers map `.` -> `/`; pathy specifiers are used verbatim.
/// Candidates: `<rel>.lua`, `<rel>/init.lua` (and `.luau`), probed relative to
/// the importer first, then the project root and common source roots.
pub(super) fn resolve_lua(
    imp: &ImportInfo,
    file_path: &str,
    ctx: &ResolverContext,
) -> (Option<String>, bool) {
    let source = imp.source.trim();
    if source.is_empty() {
        return (None, true);
    }

    // Pathy specifiers (Luau relative/slash form) are used as-is; pure dotted
    // module names (`a.b.c`) map dots to directory separators (package.path).
    let is_pathy = source.contains('/') || source.starts_with('.');
    let rel = if is_pathy {
        source
            .trim_start_matches("./")
            .trim_start_matches('/')
            .to_string()
    } else {
        source.replace('.', "/")
    };
    if rel.is_empty() {
        return (None, true);
    }

    let try_paths = |base: &str| -> Option<String> {
        let base = base.trim_start_matches('/');
        if ctx.file_exists(base) {
            return Some(base.to_string());
        }
        for ext in ["lua", "luau"] {
            let file = format!("{base}.{ext}");
            if ctx.file_exists(&file) {
                return Some(file);
            }
            let init = format!("{base}/init.{ext}");
            if ctx.file_exists(&init) {
                return Some(init);
            }
        }
        None
    };

    let dir = Path::new(file_path).parent().unwrap_or(Path::new(""));
    let relative_to_importer = || -> Option<String> {
        if dir.as_os_str().is_empty() {
            return None;
        }
        try_paths(&format!("{}/{rel}", dir.to_string_lossy()))
    };
    // Project root + common Lua source roots (package.path style).
    let from_roots = || -> Option<String> {
        try_paths(&rel).or_else(|| {
            ["src/", "lua/", "lib/"]
                .iter()
                .find_map(|prefix| try_paths(&format!("{prefix}{rel}")))
        })
    };

    // Luau slash/relative requires are importer-relative first; standard Lua
    // dotted module names resolve from the project/source roots first.
    let found = if is_pathy {
        relative_to_importer().or_else(from_roots)
    } else {
        from_roots().or_else(relative_to_importer)
    };

    match found {
        Some(path) => (Some(path), false),
        None => (None, true),
    }
}

// ---------------------------------------------------------------------------
// Zig
// ---------------------------------------------------------------------------

pub(super) fn resolve_zig(
    imp: &ImportInfo,
    file_path: &str,
    ctx: &ResolverContext,
) -> (Option<String>, bool) {
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
        if !Path::new(source_rel)
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("zig"))
        {
            candidates.push(format!("{source_rel}.zig"));
        }
        for prefix in prefixes {
            for c in &candidates {
                let p = format!("{prefix}{c}");
                if ctx.file_exists(&p) {
                    return Some(p);
                }
            }
        }
        None
    };

    if source.starts_with('.')
        || source_rel.contains('/')
        || Path::new(source_rel)
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("zig"))
    {
        let dir = Path::new(file_path).parent().unwrap_or(Path::new(""));
        let dir_prefix = if dir.as_os_str().is_empty() {
            String::new()
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
// C#
// ---------------------------------------------------------------------------

pub(super) fn resolve_csharp(imp: &ImportInfo, ctx: &ResolverContext) -> (Option<String>, bool) {
    let ns = imp.source.trim();
    if ns.is_empty() {
        return (None, true);
    }

    // 1) .NET BCL / common NuGet roots are external — resolved first so a `using`
    //    such as `System.Text` or `System.IO` can never fall through to a local
    //    folder that happens to share the trailing segment (`Text/`, `IO/`).
    if is_csharp_external_namespace(ns) {
        return (None, true);
    }

    let segs: Vec<&str> = ns.split('.').filter(|s| !s.is_empty()).collect();
    if segs.is_empty() {
        return (None, true);
    }

    // 2) `using A.B.C` names a *namespace*. The C# root namespace (the assembly's
    //    default namespace, possibly multi-segment) is usually NOT a folder, so we
    //    probe every trailing folder-suffix, longest (most specific) match first.
    if let Some(file) = probe_csharp_namespace(&segs, ctx) {
        return (Some(file), false);
    }

    // 3) `using A.B.C` may instead import the *type* `C` from namespace `A.B`:
    //    drop the final segment and probe the parent namespace's folder.
    if segs.len() >= 2
        && let Some(file) = probe_csharp_namespace(&segs[..segs.len() - 1], ctx)
    {
        return (Some(file), false);
    }

    // 4) Unresolved: treat as external so it neither creates a phantom edge nor
    //    is miscounted as a missing local dependency.
    (None, true)
}

/// Probe trailing folder-suffixes of a namespace path against the C# namespace
/// index, returning the representative file of the longest (most specific) match.
/// `["MyApp", "Models"]` probes `MyApp/Models` then `Models`, so a root namespace
/// that isn't mirrored as a folder (the common case) still resolves.
fn probe_csharp_namespace(segs: &[&str], ctx: &ResolverContext) -> Option<String> {
    (0..segs.len())
        .map(|start| segs[start..].join("/"))
        .find_map(|key| ctx.csharp_namespace_file(&key).map(str::to_string))
}

/// Roots of the .NET BCL and common `NuGet` packages. A `using` whose first
/// segment matches one of these is an external dependency (no local edge).
fn is_csharp_external_namespace(ns: &str) -> bool {
    const EXTERNAL_ROOTS: &[&str] = &[
        "System",
        "Microsoft",
        "Windows",
        "Mono",
        "Newtonsoft",
        "NUnit",
        "Xunit",
        "Moq",
        "Serilog",
        "AutoMapper",
        "MediatR",
        "FluentValidation",
        "Polly",
        "Castle",
        "Dapper",
        "RestSharp",
        "Google",
        "Amazon",
        "Azure",
        "Grpc",
        "Nito",
        "StackExchange",
    ];
    let root = ns.split('.').next().unwrap_or(ns);
    EXTERNAL_ROOTS.contains(&root)
}

// ---------------------------------------------------------------------------
// Swift
// ---------------------------------------------------------------------------

pub(super) fn resolve_swift(
    imp: &ImportInfo,
    file_path: &str,
    ctx: &ResolverContext,
) -> (Option<String>, bool) {
    let source = &imp.source;

    if matches!(
        source.as_str(),
        "Foundation" | "UIKit" | "SwiftUI" | "Combine" | "CoreData" | "Darwin"
    ) {
        return (None, true);
    }

    let dir = Path::new(file_path)
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();

    let candidate = if dir.is_empty() {
        format!("{source}.swift")
    } else {
        format!("{dir}/{source}.swift")
    };
    if ctx.file_exists(&candidate) {
        return (Some(candidate), false);
    }
    (None, false)
}

// ---------------------------------------------------------------------------
// Scala
// ---------------------------------------------------------------------------

pub(super) fn resolve_scala(imp: &ImportInfo, ctx: &ResolverContext) -> (Option<String>, bool) {
    let source = &imp.source;

    if source.starts_with("scala.") || source.starts_with("java.") {
        return (None, true);
    }

    let parts: Vec<&str> = source.rsplitn(2, '.').collect();
    if parts.len() < 2 {
        return (None, true);
    }

    let name = parts[0];
    let package_path = parts[1].replace('.', "/");

    for ext in ["scala", "sc"] {
        let candidate = format!("{package_path}/{name}.{ext}");
        if ctx.file_exists(&candidate) {
            return (Some(candidate), false);
        }
    }
    (None, false)
}

// ---------------------------------------------------------------------------
// Elixir
// ---------------------------------------------------------------------------

pub(super) fn resolve_elixir(
    imp: &ImportInfo,
    file_path: &str,
    ctx: &ResolverContext,
) -> (Option<String>, bool) {
    let source = &imp.source;

    if source.starts_with("Kernel") || source.starts_with("Enum") || source.starts_with("IO") {
        return (None, true);
    }

    let snake =
        source
            .replace('.', "/")
            .chars()
            .enumerate()
            .fold(String::new(), |mut acc, (i, c)| {
                if c.is_uppercase() && i > 0 && !acc.ends_with('/') {
                    acc.push('_');
                }
                acc.push(c.to_ascii_lowercase());
                acc
            });

    let dir = Path::new(file_path)
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();

    for ext in ["ex", "exs"] {
        let candidate = format!("lib/{snake}.{ext}");
        if ctx.file_exists(&candidate) {
            return (Some(candidate), false);
        }
        if !dir.is_empty() {
            let local = format!("{dir}/{snake}.{ext}");
            if ctx.file_exists(&local) {
                return (Some(local), false);
            }
        }
    }
    (None, false)
}
