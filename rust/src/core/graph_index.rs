use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::core::import_resolver;
use crate::core::signatures;

const INDEX_VERSION: u32 = 6;

#[derive(Debug, Serialize, Deserialize)]
pub struct ProjectIndex {
    pub version: u32,
    pub project_root: String,
    pub last_scan: String,
    pub files: HashMap<String, FileEntry>,
    pub edges: Vec<IndexEdge>,
    pub symbols: HashMap<String, SymbolEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntry {
    pub path: String,
    pub hash: String,
    pub language: String,
    pub line_count: usize,
    pub token_count: usize,
    pub exports: Vec<String>,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymbolEntry {
    pub file: String,
    pub name: String,
    pub kind: String,
    pub start_line: usize,
    pub end_line: usize,
    pub is_exported: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexEdge {
    pub from: String,
    pub to: String,
    pub kind: String,
}

impl ProjectIndex {
    pub fn new(project_root: &str) -> Self {
        Self {
            version: INDEX_VERSION,
            project_root: normalize_project_root(project_root),
            last_scan: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
            files: HashMap::new(),
            edges: Vec::new(),
            symbols: HashMap::new(),
        }
    }

    pub fn index_dir(project_root: &str) -> Option<std::path::PathBuf> {
        let hash = short_hash(&normalize_project_root(project_root));
        crate::core::data_dir::lean_ctx_data_dir()
            .ok()
            .map(|d| d.join("graphs").join(hash))
    }

    pub fn load(project_root: &str) -> Option<Self> {
        let dir = Self::index_dir(project_root)?;
        let path = dir.join("index.json");
        let content = std::fs::read_to_string(path).ok()?;
        let index: Self = serde_json::from_str(&content).ok()?;
        if index.version != INDEX_VERSION {
            return None;
        }
        Some(index)
    }

    pub fn save(&self) -> Result<(), String> {
        let dir = Self::index_dir(&self.project_root)
            .ok_or_else(|| "Cannot determine data directory".to_string())?;
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(dir.join("index.json"), json).map_err(|e| e.to_string())
    }

    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    pub fn symbol_count(&self) -> usize {
        self.symbols.len()
    }

    pub fn edge_count(&self) -> usize {
        self.edges.len()
    }

    pub fn get_symbol(&self, key: &str) -> Option<&SymbolEntry> {
        self.symbols.get(key)
    }

    pub fn get_reverse_deps(&self, path: &str, depth: usize) -> Vec<String> {
        let mut result = Vec::new();
        let mut visited = std::collections::HashSet::new();
        let mut queue: Vec<(String, usize)> = vec![(path.to_string(), 0)];

        while let Some((current, d)) = queue.pop() {
            if d > depth || visited.contains(&current) {
                continue;
            }
            visited.insert(current.clone());
            if current != path {
                result.push(current.clone());
            }

            for edge in &self.edges {
                if edge.to == current && edge.kind == "import" && !visited.contains(&edge.from) {
                    queue.push((edge.from.clone(), d + 1));
                }
            }
        }
        result
    }

    pub fn get_related(&self, path: &str, depth: usize) -> Vec<String> {
        let mut result = Vec::new();
        let mut visited = std::collections::HashSet::new();
        let mut queue: Vec<(String, usize)> = vec![(path.to_string(), 0)];

        while let Some((current, d)) = queue.pop() {
            if d > depth || visited.contains(&current) {
                continue;
            }
            visited.insert(current.clone());
            if current != path {
                result.push(current.clone());
            }

            for edge in &self.edges {
                if edge.from == current && !visited.contains(&edge.to) {
                    queue.push((edge.to.clone(), d + 1));
                }
                if edge.to == current && !visited.contains(&edge.from) {
                    queue.push((edge.from.clone(), d + 1));
                }
            }
        }
        result
    }
}

/// Load the best available graph index, trying multiple root path variants.
/// If no valid index exists, automatically scans the project to build one.
/// This is the primary entry point — ensures zero-config usage.
pub fn load_or_build(project_root: &str) -> ProjectIndex {
    // Prefer stable absolute roots. Using "." as a cache key is fragile because
    // it depends on the process cwd and can accidentally load the wrong project.
    let root_abs = if project_root.trim().is_empty() || project_root == "." {
        std::env::current_dir().ok().map_or_else(
            || ".".to_string(),
            |p| normalize_project_root(&p.to_string_lossy()),
        )
    } else {
        normalize_project_root(project_root)
    };

    // Try the absolute/root-normalized path first.
    if let Some(idx) = ProjectIndex::load(&root_abs) {
        if !idx.files.is_empty() {
            if index_looks_stale(&idx, &root_abs) {
                tracing::warn!("[graph_index: stale index detected for {root_abs}; rebuilding]");
                return scan(&root_abs);
            }
            return idx;
        }
    }

    // Legacy: older builds may have cached the index under ".". Only accept it if it
    // actually refers to the current cwd project, then migrate it to `root_abs`.
    if let Some(idx) = ProjectIndex::load(".") {
        if !idx.files.is_empty() {
            let mut migrated = idx;
            migrated.project_root.clone_from(&root_abs);
            let _ = migrated.save();
            if index_looks_stale(&migrated, &root_abs) {
                tracing::warn!(
                    "[graph_index: stale legacy index detected for {root_abs}; rebuilding]"
                );
                return scan(&root_abs);
            }
            return migrated;
        }
    }

    // Try absolute cwd
    if let Ok(cwd) = std::env::current_dir() {
        let cwd_str = normalize_project_root(&cwd.to_string_lossy());
        if cwd_str != root_abs {
            if let Some(idx) = ProjectIndex::load(&cwd_str) {
                if !idx.files.is_empty() {
                    if index_looks_stale(&idx, &cwd_str) {
                        tracing::warn!(
                            "[graph_index: stale index detected for {cwd_str}; rebuilding]"
                        );
                        return scan(&cwd_str);
                    }
                    return idx;
                }
            }
        }
    }

    // No existing index found anywhere — auto-build
    scan(&root_abs)
}

fn index_looks_stale(index: &ProjectIndex, root_abs: &str) -> bool {
    if index.files.is_empty() {
        return true;
    }

    let root_path = Path::new(root_abs);
    for rel in index.files.keys() {
        let rel = rel.trim_start_matches(['/', '\\']);
        if rel.is_empty() {
            continue;
        }
        let abs = root_path.join(rel);
        if !abs.exists() {
            return true;
        }
    }

    false
}

pub fn scan(project_root: &str) -> ProjectIndex {
    let project_root = normalize_project_root(project_root);
    let existing = ProjectIndex::load(&project_root);
    let mut index = ProjectIndex::new(&project_root);

    let old_files: HashMap<String, (String, Vec<(String, SymbolEntry)>)> =
        if let Some(ref prev) = existing {
            prev.files
                .iter()
                .map(|(path, entry)| {
                    let syms: Vec<(String, SymbolEntry)> = prev
                        .symbols
                        .iter()
                        .filter(|(_, s)| s.file == *path)
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect();
                    (path.clone(), (entry.hash.clone(), syms))
                })
                .collect()
        } else {
            HashMap::new()
        };

    let walker = ignore::WalkBuilder::new(&project_root)
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .max_depth(Some(10))
        .build();

    let cfg = crate::core::config::Config::load();
    let extra_ignores: Vec<glob::Pattern> = cfg
        .extra_ignore_patterns
        .iter()
        .filter_map(|p| glob::Pattern::new(p).ok())
        .collect();

    let mut scanned = 0usize;
    let mut reused = 0usize;
    let max_files = 2000;

    for entry in walker.filter_map(std::result::Result::ok) {
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        let file_path = normalize_absolute_path(&entry.path().to_string_lossy());
        let ext = Path::new(&file_path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");

        if !is_indexable_ext(ext) {
            continue;
        }

        let rel = make_relative(&file_path, &project_root);
        if extra_ignores.iter().any(|p| p.matches(&rel)) {
            continue;
        }

        if index.files.len() >= max_files {
            break;
        }

        let Ok(content) = std::fs::read_to_string(&file_path) else {
            continue;
        };

        let hash = compute_hash(&content);
        let rel_path = make_relative(&file_path, &project_root);

        if let Some((old_hash, old_syms)) = old_files.get(&rel_path) {
            if *old_hash == hash {
                if let Some(old_entry) = existing.as_ref().and_then(|p| p.files.get(&rel_path)) {
                    index.files.insert(rel_path.clone(), old_entry.clone());
                    for (key, sym) in old_syms {
                        index.symbols.insert(key.clone(), sym.clone());
                    }
                    reused += 1;
                    continue;
                }
            }
        }

        let sigs = signatures::extract_signatures(&content, ext);
        let line_count = content.lines().count();
        let token_count = crate::core::tokens::count_tokens(&content);
        let summary = extract_summary(&content);

        let exports: Vec<String> = sigs
            .iter()
            .filter(|s| s.is_exported)
            .map(|s| s.name.clone())
            .collect();

        index.files.insert(
            rel_path.clone(),
            FileEntry {
                path: rel_path.clone(),
                hash,
                language: ext.to_string(),
                line_count,
                token_count,
                exports,
                summary,
            },
        );

        for sig in &sigs {
            let (start, end) = sig
                .start_line
                .zip(sig.end_line)
                .unwrap_or_else(|| find_symbol_range(&content, sig));
            let key = format!("{}::{}", rel_path, sig.name);
            index.symbols.insert(
                key,
                SymbolEntry {
                    file: rel_path.clone(),
                    name: sig.name.clone(),
                    kind: sig.kind.to_string(),
                    start_line: start,
                    end_line: end,
                    is_exported: sig.is_exported,
                },
            );
        }

        scanned += 1;
    }

    build_edges(&mut index);

    if let Err(e) = index.save() {
        tracing::warn!("could not save graph index: {e}");
    }

    tracing::warn!(
        "[graph_index: {} files ({} scanned, {} reused), {} symbols, {} edges]",
        index.file_count(),
        scanned,
        reused,
        index.symbol_count(),
        index.edge_count()
    );

    index
}

fn build_edges(index: &mut ProjectIndex) {
    index.edges.clear();

    let root = normalize_project_root(&index.project_root);
    let root_path = Path::new(&root);

    let mut file_paths: Vec<String> = index.files.keys().cloned().collect();
    file_paths.sort();

    let resolver_ctx = import_resolver::ResolverContext::new(root_path, file_paths.clone());

    for rel_path in &file_paths {
        let abs_path = root_path.join(rel_path.trim_start_matches(['/', '\\']));
        let Ok(content) = std::fs::read_to_string(&abs_path) else {
            continue;
        };

        let ext = Path::new(rel_path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");

        let resolve_ext = match ext {
            "vue" | "svelte" => "ts",
            _ => ext,
        };

        let imports = crate::core::deep_queries::analyze(&content, resolve_ext).imports;
        if imports.is_empty() {
            continue;
        }

        let resolved =
            import_resolver::resolve_imports(&imports, rel_path, resolve_ext, &resolver_ctx);
        for r in resolved {
            if r.is_external {
                continue;
            }
            if let Some(to) = r.resolved_path {
                index.edges.push(IndexEdge {
                    from: rel_path.clone(),
                    to,
                    kind: "import".to_string(),
                });
            }
        }
    }

    index.edges.sort_by(|a, b| {
        a.from
            .cmp(&b.from)
            .then_with(|| a.to.cmp(&b.to))
            .then_with(|| a.kind.cmp(&b.kind))
    });
    index
        .edges
        .dedup_by(|a, b| a.from == b.from && a.to == b.to && a.kind == b.kind);
}

fn find_symbol_range(content: &str, sig: &signatures::Signature) -> (usize, usize) {
    let lines: Vec<&str> = content.lines().collect();
    let mut start = 0;

    for (i, line) in lines.iter().enumerate() {
        if line.contains(&sig.name) {
            let trimmed = line.trim();
            let is_def = trimmed.starts_with("fn ")
                || trimmed.starts_with("pub fn ")
                || trimmed.starts_with("pub(crate) fn ")
                || trimmed.starts_with("async fn ")
                || trimmed.starts_with("pub async fn ")
                || trimmed.starts_with("struct ")
                || trimmed.starts_with("pub struct ")
                || trimmed.starts_with("enum ")
                || trimmed.starts_with("pub enum ")
                || trimmed.starts_with("trait ")
                || trimmed.starts_with("pub trait ")
                || trimmed.starts_with("impl ")
                || trimmed.starts_with("class ")
                || trimmed.starts_with("export class ")
                || trimmed.starts_with("export function ")
                || trimmed.starts_with("export async function ")
                || trimmed.starts_with("function ")
                || trimmed.starts_with("async function ")
                || trimmed.starts_with("def ")
                || trimmed.starts_with("async def ")
                || trimmed.starts_with("func ")
                || trimmed.starts_with("interface ")
                || trimmed.starts_with("export interface ")
                || trimmed.starts_with("type ")
                || trimmed.starts_with("export type ")
                || trimmed.starts_with("const ")
                || trimmed.starts_with("export const ")
                || trimmed.starts_with("fun ")
                || trimmed.starts_with("private fun ")
                || trimmed.starts_with("public fun ")
                || trimmed.starts_with("internal fun ")
                || trimmed.starts_with("class ")
                || trimmed.starts_with("data class ")
                || trimmed.starts_with("sealed class ")
                || trimmed.starts_with("sealed interface ")
                || trimmed.starts_with("enum class ")
                || trimmed.starts_with("object ")
                || trimmed.starts_with("private object ")
                || trimmed.starts_with("interface ")
                || trimmed.starts_with("typealias ")
                || trimmed.starts_with("private typealias ");
            if is_def {
                start = i + 1;
                break;
            }
        }
    }

    if start == 0 {
        return (1, lines.len().min(20));
    }

    let base_indent = lines
        .get(start - 1)
        .map_or(0, |l| l.len() - l.trim_start().len());

    let mut end = start;
    let mut brace_depth: i32 = 0;
    let mut found_open = false;

    for (i, line) in lines.iter().enumerate().skip(start - 1) {
        for ch in line.chars() {
            if ch == '{' {
                brace_depth += 1;
                found_open = true;
            } else if ch == '}' {
                brace_depth -= 1;
            }
        }

        end = i + 1;

        if found_open && brace_depth <= 0 {
            break;
        }

        if !found_open && i > start {
            let indent = line.len() - line.trim_start().len();
            if indent <= base_indent && !line.trim().is_empty() && i > start {
                end = i;
                break;
            }
        }

        if end - start > 200 {
            break;
        }
    }

    (start, end)
}

fn extract_summary(content: &str) -> String {
    for line in content.lines().take(20) {
        let trimmed = line.trim();
        if trimmed.is_empty()
            || trimmed.starts_with("//")
            || trimmed.starts_with('#')
            || trimmed.starts_with("/*")
            || trimmed.starts_with('*')
            || trimmed.starts_with("use ")
            || trimmed.starts_with("import ")
            || trimmed.starts_with("from ")
            || trimmed.starts_with("require(")
            || trimmed.starts_with("package ")
        {
            continue;
        }
        return trimmed.chars().take(120).collect();
    }
    String::new()
}

fn compute_hash(content: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();
    content.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn short_hash(input: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();
    input.hash(&mut hasher);
    format!("{:08x}", hasher.finish() & 0xFFFF_FFFF)
}

fn normalize_absolute_path(path: &str) -> String {
    if let Ok(canon) = crate::core::pathutil::safe_canonicalize(std::path::Path::new(path)) {
        return canon.to_string_lossy().to_string();
    }

    let mut normalized = path.to_string();
    while normalized.ends_with("\\.") || normalized.ends_with("/.") {
        normalized.truncate(normalized.len() - 2);
    }
    while normalized.len() > 1
        && (normalized.ends_with('\\') || normalized.ends_with('/'))
        && !normalized.ends_with(":\\")
        && !normalized.ends_with(":/")
        && normalized != "\\"
        && normalized != "/"
    {
        normalized.pop();
    }
    normalized
}

pub fn normalize_project_root(path: &str) -> String {
    normalize_absolute_path(path)
}

pub fn graph_match_key(path: &str) -> String {
    let stripped =
        crate::core::pathutil::strip_verbatim_str(path).unwrap_or_else(|| path.replace('\\', "/"));
    stripped.trim_start_matches('/').to_string()
}

pub fn graph_relative_key(path: &str, root: &str) -> String {
    let root_norm = normalize_project_root(root);
    let path_norm = normalize_absolute_path(path);
    let root_path = Path::new(&root_norm);
    let path_path = Path::new(&path_norm);

    if let Ok(rel) = path_path.strip_prefix(root_path) {
        let rel = rel.to_string_lossy().to_string();
        return rel.trim_start_matches(['/', '\\']).to_string();
    }

    path.trim_start_matches(['/', '\\'])
        .replace('/', std::path::MAIN_SEPARATOR_STR)
}

fn make_relative(path: &str, root: &str) -> String {
    graph_relative_key(path, root)
}

fn is_indexable_ext(ext: &str) -> bool {
    crate::core::language_capabilities::is_indexable_ext(ext)
}

#[cfg(test)]
fn kotlin_package_name(content: &str) -> Option<String> {
    content.lines().map(str::trim).find_map(|line| {
        line.strip_prefix("package ")
            .map(|rest| rest.trim().trim_end_matches(';').to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_short_hash_deterministic() {
        let h1 = short_hash("/Users/test/project");
        let h2 = short_hash("/Users/test/project");
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 8);
    }

    #[test]
    fn test_make_relative() {
        assert_eq!(
            make_relative("/foo/bar/src/main.rs", "/foo/bar"),
            graph_relative_key("/foo/bar/src/main.rs", "/foo/bar")
        );
        assert_eq!(
            make_relative("src/main.rs", "/foo/bar"),
            graph_relative_key("src/main.rs", "/foo/bar")
        );
        assert_eq!(
            make_relative("C:\\repo\\src\\main\\kotlin\\Example.kt", "C:\\repo"),
            graph_relative_key("C:\\repo\\src\\main\\kotlin\\Example.kt", "C:\\repo")
        );
        assert_eq!(
            make_relative("//?/C:/repo/src/main/kotlin/Example.kt", "//?/C:/repo"),
            graph_relative_key("//?/C:/repo/src/main/kotlin/Example.kt", "//?/C:/repo")
        );
    }

    #[test]
    fn test_normalize_project_root() {
        assert_eq!(normalize_project_root("C:\\repo\\"), "C:\\repo");
        assert_eq!(normalize_project_root("C:\\repo\\."), "C:\\repo");
        assert_eq!(normalize_project_root("//?/C:/repo/"), "//?/C:/repo");
    }

    #[test]
    fn test_graph_match_key_normalizes_windows_forms() {
        assert_eq!(
            graph_match_key(r"C:\repo\src\main.rs"),
            "C:/repo/src/main.rs"
        );
        assert_eq!(
            graph_match_key(r"\\?\C:\repo\src\main.rs"),
            "C:/repo/src/main.rs"
        );
        assert_eq!(graph_match_key(r"\src\main.rs"), "src/main.rs");
    }

    #[test]
    fn test_extract_summary() {
        let content = "// comment\nuse std::io;\n\npub fn main() {\n    println!(\"hello\");\n}";
        let summary = extract_summary(content);
        assert_eq!(summary, "pub fn main() {");
    }

    #[test]
    fn test_compute_hash_deterministic() {
        let h1 = compute_hash("hello world");
        let h2 = compute_hash("hello world");
        assert_eq!(h1, h2);
        assert_ne!(h1, compute_hash("hello world!"));
    }

    #[test]
    fn test_project_index_new() {
        let idx = ProjectIndex::new("/test");
        assert_eq!(idx.version, INDEX_VERSION);
        assert_eq!(idx.project_root, "/test");
        assert!(idx.files.is_empty());
    }

    fn fe(path: &str, content: &str, language: &str) -> FileEntry {
        FileEntry {
            path: path.to_string(),
            hash: compute_hash(content),
            language: language.to_string(),
            line_count: content.lines().count(),
            token_count: crate::core::tokens::count_tokens(content),
            exports: Vec::new(),
            summary: extract_summary(content),
        }
    }

    #[test]
    fn test_index_looks_stale_when_any_file_missing() {
        let td = tempdir().expect("tempdir");
        let root = td.path();
        std::fs::write(root.join("a.rs"), "pub fn a() {}\n").expect("write a.rs");

        let root_s = normalize_project_root(&root.to_string_lossy());
        let mut idx = ProjectIndex::new(&root_s);
        idx.files
            .insert("a.rs".to_string(), fe("a.rs", "pub fn a() {}\n", "rs"));
        idx.files.insert(
            "missing.rs".to_string(),
            fe("missing.rs", "pub fn m() {}\n", "rs"),
        );

        assert!(index_looks_stale(&idx, &root_s));
    }

    #[test]
    fn test_index_looks_fresh_when_all_files_exist() {
        let td = tempdir().expect("tempdir");
        let root = td.path();
        std::fs::write(root.join("a.rs"), "pub fn a() {}\n").expect("write a.rs");

        let root_s = normalize_project_root(&root.to_string_lossy());
        let mut idx = ProjectIndex::new(&root_s);
        idx.files
            .insert("a.rs".to_string(), fe("a.rs", "pub fn a() {}\n", "rs"));

        assert!(!index_looks_stale(&idx, &root_s));
    }

    #[test]
    fn test_reverse_deps() {
        let mut idx = ProjectIndex::new("/test");
        idx.edges.push(IndexEdge {
            from: "a.rs".to_string(),
            to: "b.rs".to_string(),
            kind: "import".to_string(),
        });
        idx.edges.push(IndexEdge {
            from: "c.rs".to_string(),
            to: "b.rs".to_string(),
            kind: "import".to_string(),
        });

        let deps = idx.get_reverse_deps("b.rs", 1);
        assert_eq!(deps.len(), 2);
        assert!(deps.contains(&"a.rs".to_string()));
        assert!(deps.contains(&"c.rs".to_string()));
    }

    #[test]
    fn test_find_symbol_range_kotlin_function() {
        let content = r#"
package com.example

class UserService {
    fun greet(name: String): String {
        return "hi $name"
    }
}
"#;
        let sig = signatures::Signature {
            kind: "method",
            name: "greet".to_string(),
            params: "name:String".to_string(),
            return_type: "String".to_string(),
            is_async: false,
            is_exported: true,
            indent: 2,
            ..signatures::Signature::no_span()
        };
        let (start, end) = find_symbol_range(content, &sig);
        assert_eq!(start, 5);
        assert!(end >= start);
    }

    #[test]
    fn test_signature_spans_override_fallback_range() {
        let sig = signatures::Signature {
            kind: "method",
            name: "release".to_string(),
            params: "id:String".to_string(),
            return_type: "Boolean".to_string(),
            is_async: true,
            is_exported: true,
            indent: 2,
            start_line: Some(42),
            end_line: Some(43),
        };

        let (start, end) = sig
            .start_line
            .zip(sig.end_line)
            .unwrap_or_else(|| find_symbol_range("ignored", &sig));
        assert_eq!((start, end), (42, 43));
    }

    #[test]
    fn test_parse_stale_index_version() {
        let json = format!(
            r#"{{"version":{},"project_root":"/test","last_scan":"now","files":{{}},"edges":[],"symbols":{{}}}}"#,
            INDEX_VERSION - 1
        );
        let parsed: ProjectIndex = serde_json::from_str(&json).unwrap();
        assert_ne!(parsed.version, INDEX_VERSION);
    }

    #[test]
    fn test_kotlin_package_name() {
        let content = "package com.example.feature\n\nclass UserService";
        assert_eq!(
            kotlin_package_name(content).as_deref(),
            Some("com.example.feature")
        );
    }
}
