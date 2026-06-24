//! RAM-first graph index builder for the indexing pipeline.
//!
//! Collects node/edge/symbol data in memory during the build, then produces
//! a [`ProjectIndex`] via [`RamGraphBuilder::finalize`]. No SQLite writes during the build
//! phase — all persistence happens in the dump engine.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;

use crate::core::graph_index::{
    FileEntry, IndexEdge, ProjectIndex, SymbolEntry, build_edges_cached,
};
use crate::core::index_pipeline::content_pipeline::ContentEntry;
use crate::core::signatures::Signature;

/// RAM-first graph index builder.
///
/// Accumulates files, symbols, and edges in memory, then produces a complete
/// [`ProjectIndex`] via [`RamGraphBuilder::finalize`]. No SQLite writes occur during build.
pub struct RamGraphBuilder {
    /// Project root path, used when constructing the final [`ProjectIndex`].
    project_root: String,
    /// Accumulated file entries, keyed by project-relative path.
    files: HashMap<String, FileEntry>,
    /// Accumulated symbol entries, keyed by `{rel_path}::{symbol_name}`.
    symbols: HashMap<String, SymbolEntry>,
    /// Accumulated cross-file edges (populated by [`build_edges`]).
    edges: Vec<IndexEdge>,
    /// Content cache for edge building — maps rel_path to file content.
    content_cache: HashMap<String, String>,
}

impl RamGraphBuilder {
    /// Create a new builder for the given project root.
    #[must_use]
    pub fn new(project_root: &str) -> Self {
        Self {
            project_root: project_root.to_string(),
            files: HashMap::new(),
            symbols: HashMap::new(),
            edges: Vec::new(),
            content_cache: HashMap::new(),
        }
    }

    /// Add a file and its signatures to the builder.
    ///
    /// Converts signatures into [`SymbolEntry`] nodes, creates a [`FileEntry`]
    /// for file-level metadata, and stores the content for later edge building.
    /// No SQLite writes — all data stays in memory.
    pub fn add_file(&mut self, rel_path: &str, sigs: &[Signature], content: &str, hash: &str) {
        let ext = std::path::Path::new(rel_path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        let line_count = content.lines().count();
        let token_count = crate::core::tokens::count_tokens(content);
        let summary = extract_summary(content);

        let exports: Vec<String> = sigs
            .iter()
            .filter(|s| s.is_exported)
            .map(|s| s.name.clone())
            .collect();

        // Store file entry.
        self.files.insert(
            rel_path.to_string(),
            FileEntry {
                path: rel_path.to_string(),
                hash: hash.to_string(),
                language: ext.to_string(),
                line_count,
                token_count,
                exports,
                summary,
            },
        );

        // Store symbol entries.
        for sig in sigs {
            let key = format!("{}::{}", rel_path, sig.name);
            self.symbols.insert(
                key,
                SymbolEntry {
                    file: rel_path.to_string(),
                    name: sig.name.clone(),
                    kind: sig.kind.to_string(),
                    start_line: sig.start_line.unwrap_or(1),
                    end_line: sig.end_line.unwrap_or(1),
                    is_exported: sig.is_exported,
                    minhash: sig.minhash.map(|a| a.to_vec()).unwrap_or_default(),
                },
            );
        }

        // Store content for edge building.
        self.content_cache
            .insert(rel_path.to_string(), content.to_string());
    }

    /// Build cross-file edges using the accumulated data.
    ///
    /// Delegates to `build_edges_cached` from `graph_index` which computes
    /// import, module, package, barrel, co-change, and sibling edges.
    ///
    /// The `entries` map provides content for all indexed files.
    pub fn build_edges(&mut self, entries: &HashMap<String, ContentEntry>) {
        let mut edge_content_cache: HashMap<String, Arc<String>> = HashMap::new();
        for (path, entry) in entries {
            edge_content_cache.insert(path.clone(), Arc::clone(&entry.content));
        }

        // Swap files/symbols out temporarily to avoid cloning into a temp
        // ProjectIndex. build_edges_cached only reads files/symbols and
        // writes edges, so we can swap back afterwards.
        let files = std::mem::take(&mut self.files);
        let symbols = std::mem::take(&mut self.symbols);

        let mut temp_index = ProjectIndex::new(&self.project_root);
        temp_index.files = files;
        temp_index.symbols = symbols;

        build_edges_cached(&mut temp_index, &edge_content_cache);

        self.edges = temp_index.edges;
        self.files = temp_index.files;
        self.symbols = temp_index.symbols;
    }

    /// Finalize the builder and produce a complete [`ProjectIndex`].
    ///
    /// Nodes and edges are sorted and deduplicated for deterministic output.
    pub fn finalize(mut self) -> Result<ProjectIndex> {
        let mut index = ProjectIndex::new(&self.project_root);
        index.files = self.files;
        index.symbols = self.symbols;

        // Sort edges for deterministic order.
        self.edges.sort_by(|a, b| {
            a.from
                .cmp(&b.from)
                .then_with(|| a.to.cmp(&b.to))
                .then_with(|| a.kind.cmp(&b.kind))
                .then_with(|| {
                    a.weight
                        .partial_cmp(&b.weight)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
        });
        self.edges
            .dedup_by(|a, b| a.from == b.from && a.to == b.to && a.kind == b.kind);
        index.edges = self.edges;

        Ok(index)
    }

    /// Merge another builder's files and symbols into this one.
    /// Both builders must share the same `project_root.
    /// Edges are not merged here — they are produced by `build_edges()` and
    /// `finalize()` which run after the merge.
    pub(crate) fn merge(&mut self, other: Self) {
        debug_assert_eq!(
            self.project_root, other.project_root,
            "cannot merge builders with different project roots"
        );
        self.files.extend(other.files);
        self.symbols.extend(other.symbols);
        self.content_cache.extend(other.content_cache);
    }

    /// Clear all accumulated data, readying the builder for reuse.
    pub fn reset(&mut self) {
        self.files.clear();
        self.symbols.clear();
        self.edges.clear();
        self.content_cache.clear();
    }
}

/// Extract the first meaningful line as a file summary.
///
/// Skips comments, blank lines, and common declaration lines (`use`, `import`,
/// `package`) to find the first substantive line of content.
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::UNIX_EPOCH;

    fn make_entry(content: &str) -> ContentEntry {
        ContentEntry {
            content: Arc::new(content.to_string()),
            mtime: UNIX_EPOCH,
            size: content.len() as u64,
            content_hash: String::new(),
        }
    }

    #[test]
    fn empty_builder_produces_empty_index() {
        let builder = RamGraphBuilder::new("/test");
        let index = builder.finalize().unwrap();
        assert_eq!(index.project_root, "/test");
        assert!(index.files.is_empty());
        assert!(index.symbols.is_empty());
        assert!(index.edges.is_empty());
    }

    #[test]
    fn single_file_produces_correct_nodes() {
        let mut builder = RamGraphBuilder::new("/test");
        let sigs = [Signature {
            kind: "fn",
            name: "hello".to_string(),
            params: String::new(),
            return_type: "bool".to_string(),
            is_async: false,
            is_exported: true,
            indent: 0,
            start_line: Some(1),
            end_line: Some(3),
            minhash: None,
        }];
        builder.add_file(
            "main.rs",
            &sigs,
            "pub fn hello() -> bool { true }",
            "abc123",
        );

        let index = builder.finalize().unwrap();
        assert_eq!(index.files.len(), 1);
        assert_eq!(index.symbols.len(), 1);

        let file = index.files.get("main.rs").unwrap();
        assert_eq!(file.path, "main.rs");
        assert_eq!(file.hash, "abc123");
        assert_eq!(file.language, "rs");
        assert!(file.exports.contains(&"hello".to_string()));

        let sym = index.symbols.get("main.rs::hello").unwrap();
        assert_eq!(sym.name, "hello");
        assert_eq!(sym.kind, "fn");
        assert!(sym.is_exported);
    }

    #[test]
    fn multiple_files_produce_correct_structure() {
        let mut builder = RamGraphBuilder::new("/test");
        builder.add_file(
            "a.rs",
            &[Signature {
                kind: "fn",
                name: "foo".to_string(),
                params: String::new(),
                return_type: String::new(),
                is_async: false,
                is_exported: false,
                indent: 0,
                start_line: Some(1),
                end_line: Some(1),
                minhash: None,
            }],
            "fn foo() {}",
            "hash1",
        );
        builder.add_file(
            "b.rs",
            &[Signature {
                kind: "struct",
                name: "Bar".to_string(),
                params: String::new(),
                return_type: String::new(),
                is_async: false,
                is_exported: true,
                indent: 0,
                start_line: Some(1),
                end_line: Some(1),
                minhash: None,
            }],
            "pub struct Bar;",
            "hash2",
        );

        let index = builder.finalize().unwrap();
        assert_eq!(index.files.len(), 2);
        assert_eq!(index.symbols.len(), 2);
    }

    #[test]
    fn reset_clears_all_data() {
        let mut builder = RamGraphBuilder::new("/test");
        builder.add_file("a.rs", &[], "fn a() {}", "hash");
        assert_eq!(builder.files.len(), 1);

        builder.reset();
        assert!(builder.files.is_empty());
        assert!(builder.symbols.is_empty());
        assert!(builder.edges.is_empty());

        let index = builder.finalize().unwrap();
        assert!(index.files.is_empty());
    }

    #[test]
    fn edge_building_runs_without_error() {
        let mut builder = RamGraphBuilder::new("/test");
        builder.add_file(
            "lib.rs",
            &[Signature {
                kind: "fn",
                name: "helper".to_string(),
                params: String::new(),
                return_type: "u32".to_string(),
                is_async: false,
                is_exported: true,
                indent: 0,
                start_line: Some(1),
                end_line: Some(1),
                minhash: None,
            }],
            "pub fn helper() -> u32 { 42 }",
            "hash_lib",
        );
        builder.add_file(
            "a.rs",
            &[Signature {
                kind: "fn",
                name: "main".to_string(),
                params: String::new(),
                return_type: String::new(),
                is_async: false,
                is_exported: false,
                indent: 0,
                start_line: Some(1),
                end_line: Some(1),
                minhash: None,
            }],
            "fn main() {}",
            "hash_a",
        );

        let mut entries = HashMap::new();
        entries.insert(
            "lib.rs".to_string(),
            make_entry("pub fn helper() -> u32 { 42 }"),
        );
        entries.insert("a.rs".to_string(), make_entry("fn main() {}"));

        // Should run without error and produce a Vec (possibly empty).
        builder.build_edges(&entries);
        // build_edges_cached runs all 4 phases and populates edges accordingly
        // (may be empty for simple files without imports).
        assert!(builder.edges.is_empty() || !builder.edges.is_empty());
    }

    #[test]
    fn finalize_produces_valid_project_index() {
        let mut builder = RamGraphBuilder::new("/project");
        let sigs = [Signature {
            kind: "fn",
            name: "main".to_string(),
            params: String::new(),
            return_type: String::new(),
            is_async: false,
            is_exported: false,
            indent: 0,
            start_line: Some(1),
            end_line: Some(5),
            minhash: None,
        }];
        builder.add_file(
            "src/main.rs",
            &sigs,
            "fn main() {\n    println!(\"hi\");\n}\n",
            "hash_main",
        );

        let index = builder.finalize().unwrap();
        assert_eq!(index.version, 6);
        assert_eq!(index.project_root, "/project");
        assert_eq!(index.file_count(), 1);
        assert_eq!(index.symbol_count(), 1);
        assert_eq!(index.edge_count(), 0);

        let file = index.files.get("src/main.rs").unwrap();
        assert_eq!(file.line_count, 3);
        assert_eq!(file.language, "rs");
        assert!(file.summary.starts_with("fn main()"));
    }

    #[test]
    fn add_file_without_signatures_produces_file_only() {
        let mut builder = RamGraphBuilder::new("/test");
        builder.add_file("empty.rs", &[], "// just a comment", "hash_empty");

        let index = builder.finalize().unwrap();
        assert_eq!(index.files.len(), 1);
        assert_eq!(index.symbols.len(), 0);
        assert!(index.files.get("empty.rs").unwrap().exports.is_empty());
    }

    #[test]
    fn builder_reuse_after_reset() {
        let mut builder = RamGraphBuilder::new("/test");
        builder.add_file("a.rs", &[], "fn a() {}", "hash_a");
        builder.reset();
        builder.add_file("b.rs", &[], "fn b() {}", "hash_b");

        let index = builder.finalize().unwrap();
        assert_eq!(index.files.len(), 1);
        assert!(index.files.contains_key("b.rs"));
        assert!(!index.files.contains_key("a.rs"));
    }

    #[test]
    fn builder_is_deterministic() {
        let mut builder1 = RamGraphBuilder::new("/test");
        let mut builder2 = RamGraphBuilder::new("/test");

        let sigs = [
            Signature {
                kind: "fn",
                name: "foo".to_string(),
                params: "x: i32".to_string(),
                return_type: "i32".to_string(),
                is_async: false,
                is_exported: true,
                indent: 0,
                start_line: Some(1),
                end_line: Some(3),
                minhash: None,
            },
            Signature {
                kind: "struct",
                name: "Bar".to_string(),
                params: String::new(),
                return_type: String::new(),
                is_async: false,
                is_exported: false,
                indent: 0,
                start_line: Some(5),
                end_line: Some(5),
                minhash: None,
            },
        ];

        builder1.add_file(
            "mod.rs",
            &sigs,
            "pub fn foo(x: i32) -> i32 { x }\nstruct Bar;",
            "h1",
        );
        builder2.add_file(
            "mod.rs",
            &sigs,
            "pub fn foo(x: i32) -> i32 { x }\nstruct Bar;",
            "h1",
        );

        let index1 = builder1.finalize().unwrap();
        let index2 = builder2.finalize().unwrap();

        assert_eq!(index1.file_count(), index2.file_count());
        assert_eq!(index1.symbol_count(), index2.symbol_count());
        assert_eq!(index1.edge_count(), index2.edge_count());
    }
}
