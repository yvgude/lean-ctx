//! Phase 3B: Serial registry build.
//!
//! Builds a symbol [`Registry`] from cached [`ExtractedFile`] results, then
//! emits `DEFINES`, `DEFINES_METHOD`, and `IMPORTS` edges into the
//! [`GraphBuffer`].
//!
//! ## Algorithm
//!
//! 1. **Pass 1**: Register all definitions in a name→qualified-names index.
//! 2. **Pass 2**: Create `DEFINES` edges (File node → Def node) and
//!    `DEFINES_METHOD` edges (Class node → Method node).
//! 3. **Pass 3**: Create `IMPORTS` edges by resolving `import.local_name`
//!    against the registry.
//!
//! ## C reference
//!
//! Maps to `cbm_build_registry_from_cache` in
//! `/tmp/codebase-memory-mcp/src/pipeline/pass_parallel.c:922-970`.
//!
//! ## Thread safety
//!
//! Single-threaded — the registry uses `&mut self` on all mutation paths.
//! No parallel access is attempted or supported.

use std::collections::HashMap;

use crate::core::graph_buffer::GraphBuffer;
use crate::core::index_types::{Definition, ExtractedFile};

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

/// Symbol registry that maps short names to qualified names and stores
/// the full definition records.
///
/// Used for name resolution during the edge-creation passes and as a
/// queryable index for downstream phases (Phase 4 resolution).
#[derive(Debug, Clone)]
pub struct Registry {
    /// Short name → list of qualified names (multiple defs can share a name).
    name_index: HashMap<String, Vec<String>>,
    /// Qualified name → full Definition record.
    defs: HashMap<String, Definition>,
}

impl Registry {
    /// Create an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            name_index: HashMap::new(),
            defs: HashMap::new(),
        }
    }

    /// Register a definition by its `qualified_name`.
    ///
    /// Only definitions whose label is one of `Function`, `Method`, `Class`,
    /// `Interface`, `Variable`, or `Field` are indexed in the name lookup
    /// (matching C's `register_and_link_def` at
    /// `/tmp/codebase-memory-mcp/src/pipeline/pass_parallel.c:825-827`).
    /// All definitions are stored in the full `defs` map regardless of label.
    pub fn register(&mut self, def: &Definition) {
        // Store full definition keyed by qualified_name.
        self.defs.insert(def.qualified_name.clone(), def.clone());

        // Index by short name for lookups — only for callable/useable labels.
        match def.label.as_str() {
            "Function" | "Method" | "Class" | "Interface" | "Variable" | "Field" => {
                self.name_index
                    .entry(def.name.clone())
                    .or_default()
                    .push(def.qualified_name.clone());
            }
            _ => {}
        }
    }

    /// Look up all qualified names that share the given short name.
    #[must_use]
    pub fn lookup(&self, name: &str) -> Option<&Vec<String>> {
        self.name_index.get(name)
    }

    /// Look up a definition by its exact qualified name.
    #[must_use]
    pub fn lookup_qn(&self, qn: &str) -> Option<&Definition> {
        self.defs.get(qn)
    }

    /// Check whether a qualified name exists in the registry.
    #[must_use]
    pub fn contains(&self, qn: &str) -> bool {
        self.defs.contains_key(qn)
    }

    /// Return the number of registered definitions.
    #[must_use]
    pub fn len(&self) -> usize {
        self.defs.len()
    }

    /// Returns `true` when no definitions are registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.defs.is_empty()
    }
}

impl Default for Registry {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// build_registry — main entry point
// ---------------------------------------------------------------------------

/// Build the registry from extracted files, then emit `DEFINES`,
/// `DEFINES_METHOD`, and `IMPORTS` edges into `gbuf`.
///
/// # Arguments
///
/// * `extracted_files` — Per-file extraction results (output of Phase 3A).
/// * `gbuf` — Graph buffer containing File nodes (from Phase 1) and
///   Definition nodes (from Phase 3A).
///
/// # Returns
///
/// The populated `Registry`, which can be passed to Phase 4 (resolution)
/// for name-resolution queries.
///
/// # Panics
///
/// Should not panic under normal operation. If a node referenced by QN
/// is missing from `gbuf` (e.g. because Phase 1 or Phase 3A was skipped),
/// the corresponding edge is silently skipped.
///
/// # Algorithm
///
/// 1. **Pass 1**: Register all definitions in the symbol index.
/// 2. **Pass 2**: For each definition, create a `DEFINES` edge from its
///    containing File node. If the definition has a `parent_class` and is
///    a Method, also create a `DEFINES_METHOD` edge from the class node.
/// 3. **Pass 3**: For each import, resolve `import.local_name` against the
///    registry and create `IMPORTS` edges from the file to each matching
///    target node.
///
/// ## C reference
///
/// Maps to `cbm_build_registry_from_cache` in
/// `/tmp/codebase-memory-mcp/src/pipeline/pass_parallel.c:922-970`.
pub fn build_registry(extracted_files: &[ExtractedFile], gbuf: &mut GraphBuffer) -> Registry {
    let mut registry = Registry::new();

    // ── Pass 1: Register all definitions ──────────────────────────
    for ef in extracted_files {
        for def in &ef.defs {
            registry.register(def);
        }
    }

    // ── Pass 2: Create DEFINES / DEFINES_METHOD edges ─────────────
    for ef in extracted_files {
        // Extract Copy-able NodeId before any mutable calls.
        let file_id = gbuf.find_by_qn(&ef.file_path).map(|n| n.id);
        for def in &ef.defs {
            // DEFINES edge: File node → Definition node
            if let Some(fid) = file_id
                && let Some(def_id) = gbuf.find_by_qn(&def.qualified_name).map(|n| n.id)
            {
                gbuf.insert_edge(fid, def_id, "DEFINES", HashMap::new());
            }

            // DEFINES_METHOD edge: Class node → Method node
            if let Some(parent_class) = &def.parent_class
                && def.label == "Method"
                && let Some(class_id) = gbuf.find_by_qn(parent_class).map(|n| n.id)
                && let Some(def_id) = gbuf.find_by_qn(&def.qualified_name).map(|n| n.id)
            {
                gbuf.insert_edge(class_id, def_id, "DEFINES_METHOD", HashMap::new());
            }
        }
    }

    // ── Pass 3: Create IMPORTS edges ──────────────────────────────
    for ef in extracted_files {
        let file_id = gbuf.find_by_qn(&ef.file_path).map(|n| n.id);
        for import in &ef.imports {
            if let Some(fid) = file_id
                && let Some(target_qns) = registry.lookup(&import.local_name)
            {
                for target_qn in target_qns {
                    if let Some(target_id) = gbuf.find_by_qn(target_qn).map(|n| n.id) {
                        let mut props = HashMap::new();
                        props.insert("local_name".to_string(), import.local_name.clone());
                        gbuf.insert_edge(fid, target_id, "IMPORTS", props);
                    }
                }
            }
        }
    }

    registry
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::index_types::DefKind;

    // ── Test helpers ──────────────────────────────────────────────

    /// Build a minimal `Definition` for testing.
    fn make_def(name: &str, qn: &str, label: &str, parent_class: Option<&str>) -> Definition {
        Definition {
            name: name.to_string(),
            qualified_name: qn.to_string(),
            kind: DefKind::Function,
            label: label.to_string(),
            file_path: "src/lib.rs".to_string(),
            start_line: 1,
            end_line: 10,
            signature: Some(format!("fn {name}()")),
            return_type: None,
            receiver: None,
            docstring: None,
            parent_class: parent_class.map(String::from),
            decorators: Vec::new(),
            base_classes: Vec::new(),
            param_names: Vec::new(),
            param_types: Vec::new(),
            is_async: false,
            is_exported: false,
            is_abstract: false,
            is_test: false,
            is_entry_point: false,
            complexity: 0,
            cognitive: 0,
            loop_count: 0,
            loop_depth: 0,
            is_recursive: false,
            param_count: 0,
            minhash: None,
            body_tokens: None,
        }
    }

    /// Build a minimal `ExtractedFile` with defs (no imports).
    fn make_ef(file_path: &str, defs: Vec<Definition>) -> ExtractedFile {
        ExtractedFile {
            file_path: file_path.to_string(),
            module_qn: None,
            defs,
            calls: Vec::new(),
            imports: Vec::new(),
            usages: Vec::new(),
            throws: Vec::new(),
            channels: Vec::new(),
            chunks: Vec::new(),
            content_hash: "abcdef".to_string(),
            is_test_file: false,
            has_parse_error: false,
        }
    }

    /// Build a minimal `ExtractedFile` with defs and imports.
    ///
    /// `imports` is a vec of `(local_name, module_path)` pairs.
    fn make_ef_with_imports(
        file_path: &str,
        defs: Vec<Definition>,
        imports: Vec<(&str, &str)>,
    ) -> ExtractedFile {
        use crate::core::index_types::Import;
        ExtractedFile {
            imports: imports
                .into_iter()
                .map(|(local_name, module_path)| Import {
                    local_name: local_name.to_string(),
                    module_path: module_path.to_string(),
                })
                .collect(),
            ..make_ef(file_path, defs)
        }
    }

    // ── Registry unit tests ───────────────────────────────────────

    #[test]
    fn empty_registry_returns_none() {
        let reg = Registry::new();
        assert_eq!(reg.len(), 0);
        assert!(reg.lookup("anything").is_none());
        assert!(reg.lookup_qn("anything").is_none());
        assert!(!reg.contains("anything"));
    }

    #[test]
    fn register_def_lookup_by_name() {
        let mut reg = Registry::new();
        reg.register(&make_def("foo", "src/lib.rs::foo", "Function", None));

        let results = reg.lookup("foo").expect("should find 'foo'");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0], "src/lib.rs::foo");
    }

    #[test]
    fn register_def_lookup_by_qn() {
        let mut reg = Registry::new();
        reg.register(&make_def("bar", "src/utils.rs::bar", "Function", None));

        let found = reg
            .lookup_qn("src/utils.rs::bar")
            .expect("should find by QN");
        assert_eq!(found.name, "bar");
    }

    #[test]
    fn same_name_different_qns() {
        let mut reg = Registry::new();
        reg.register(&make_def("fn1", "a.rs::fn1", "Function", None));
        reg.register(&make_def("fn1", "b.rs::fn1", "Function", None));

        let results = reg.lookup("fn1").expect("should find 'fn1'");
        assert_eq!(results.len(), 2);
        assert!(results.contains(&"a.rs::fn1".to_string()));
        assert!(results.contains(&"b.rs::fn1".to_string()));
    }

    #[test]
    fn register_skips_non_callable_labels() {
        let mut reg = Registry::new();
        reg.register(&make_def("ignored", "m.rs::ignored", "Struct", None));

        // The def is stored in the full index...
        assert!(reg.contains("m.rs::ignored"));
        assert_eq!(reg.len(), 1);
        // ... but not in the name-index for name lookups.
        assert!(reg.lookup("ignored").is_none());
    }

    #[test]
    fn contains_and_len() {
        let mut reg = Registry::new();
        reg.register(&make_def("a", "a.rs::a", "Function", None));
        reg.register(&make_def("b", "b.rs::b", "Method", None));

        assert_eq!(reg.len(), 2);
        assert!(reg.contains("a.rs::a"));
        assert!(reg.contains("b.rs::b"));
        assert!(!reg.contains("nonexistent"));
    }

    #[test]
    fn register_same_qn_overwrites() {
        let mut reg = Registry::new();
        let def1 = Definition {
            start_line: 1,
            ..make_def("dup", "pkg.rs::dup", "Function", None)
        };
        let def2 = Definition {
            start_line: 99,
            ..make_def("dup", "pkg.rs::dup", "Function", None)
        };
        reg.register(&def1);
        reg.register(&def2);

        // Same QN → second overwrites in defs map.
        assert_eq!(reg.len(), 1);
        let stored = reg.lookup_qn("pkg.rs::dup").unwrap();
        assert_eq!(stored.start_line, 99);

        // Name index: two calls both push the same QN.
        // This is an edge case — the name list contains duplicates.
        // The registry is honest; dedup on insert is the caller's choice.
        let names = reg.lookup("dup").unwrap();
        assert_eq!(names.len(), 2);
    }

    // ── build_registry integration tests ──────────────────────────

    #[test]
    fn empty_files_produces_empty_registry() {
        let mut gbuf = GraphBuffer::new("test");
        let reg = build_registry(&[], &mut gbuf);
        assert_eq!(reg.len(), 0);
        assert_eq!(gbuf.edge_count(), 0);
    }

    #[test]
    fn registry_contains_all_defs_from_all_files() {
        let mut gbuf = GraphBuffer::new("test");

        // Pre-populate nodes (as structure_pass + parallel_extract would).
        gbuf.upsert_node("File", "a.rs", "a.rs", "a.rs", 0, 0, HashMap::new());
        gbuf.upsert_node("File", "b.rs", "b.rs", "b.rs", 0, 0, HashMap::new());
        gbuf.upsert_node("Function", "foo", "a.rs::foo", "a.rs", 1, 5, HashMap::new());
        gbuf.upsert_node("Function", "bar", "b.rs::bar", "b.rs", 1, 5, HashMap::new());

        let ef_a = make_ef("a.rs", vec![make_def("foo", "a.rs::foo", "Function", None)]);
        let ef_b = make_ef("b.rs", vec![make_def("bar", "b.rs::bar", "Function", None)]);

        let reg = build_registry(&[ef_a, ef_b], &mut gbuf);
        assert_eq!(reg.len(), 2);
        assert!(reg.contains("a.rs::foo"));
        assert!(reg.contains("b.rs::bar"));
    }

    #[test]
    fn defines_edge_from_file_to_def() {
        let mut gbuf = GraphBuffer::new("test");
        gbuf.upsert_node(
            "File",
            "test.rs",
            "test.rs",
            "test.rs",
            0,
            0,
            HashMap::new(),
        );
        gbuf.upsert_node(
            "Function",
            "hello",
            "test.rs::hello",
            "test.rs",
            1,
            5,
            HashMap::new(),
        );

        let ef = make_ef(
            "test.rs",
            vec![make_def("hello", "test.rs::hello", "Function", None)],
        );
        let _reg = build_registry(&[ef], &mut gbuf);

        assert_eq!(gbuf.edge_count(), 1);

        let file_node = gbuf.find_by_qn("test.rs").unwrap();
        let def_node = gbuf.find_by_qn("test.rs::hello").unwrap();
        assert!(gbuf.edge_dedup_key(file_node.id, def_node.id, "DEFINES"));
    }

    #[test]
    fn missing_file_node_skips_defines_edge() {
        // Def exists but File node is missing — edge is silently skipped.
        let mut gbuf = GraphBuffer::new("test");
        gbuf.upsert_node(
            "Function",
            "orphan",
            "ghost.rs::orphan",
            "ghost.rs",
            1,
            5,
            HashMap::new(),
        );

        let ef = make_ef(
            "ghost.rs",
            vec![make_def("orphan", "ghost.rs::orphan", "Function", None)],
        );
        let _reg = build_registry(&[ef], &mut gbuf);

        // No File node → no edge.
        assert_eq!(gbuf.edge_count(), 0);
    }

    #[test]
    fn defines_method_edge_for_method_with_parent_class() {
        let mut gbuf = GraphBuffer::new("test");
        gbuf.upsert_node(
            "File",
            "test.rs",
            "test.rs",
            "test.rs",
            0,
            0,
            HashMap::new(),
        );
        gbuf.upsert_node(
            "Class",
            "MyClass",
            "test.rs::MyClass",
            "test.rs",
            1,
            10,
            HashMap::new(),
        );
        gbuf.upsert_node(
            "Method",
            "doSomething",
            "test.rs::MyClass::doSomething",
            "test.rs",
            12,
            20,
            HashMap::new(),
        );

        let def = make_def(
            "doSomething",
            "test.rs::MyClass::doSomething",
            "Method",
            Some("test.rs::MyClass"),
        );
        let ef = make_ef("test.rs", vec![def]);
        let _reg = build_registry(&[ef], &mut gbuf);

        // DEFINES + DEFINES_METHOD
        assert_eq!(gbuf.edge_count(), 2);

        let class_node = gbuf.find_by_qn("test.rs::MyClass").unwrap();
        let method_node = gbuf.find_by_qn("test.rs::MyClass::doSomething").unwrap();
        assert!(gbuf.edge_dedup_key(class_node.id, method_node.id, "DEFINES_METHOD"));
    }

    #[test]
    fn method_without_parent_class_no_defines_method() {
        let mut gbuf = GraphBuffer::new("test");
        gbuf.upsert_node(
            "File",
            "test.rs",
            "test.rs",
            "test.rs",
            0,
            0,
            HashMap::new(),
        );
        gbuf.upsert_node(
            "Method",
            "standalone",
            "test.rs::standalone",
            "test.rs",
            1,
            5,
            HashMap::new(),
        );

        let def = make_def("standalone", "test.rs::standalone", "Method", None);
        let ef = make_ef("test.rs", vec![def]);
        let _reg = build_registry(&[ef], &mut gbuf);

        // Only DEFINES, no DEFINES_METHOD (parent_class is None).
        assert_eq!(gbuf.edge_count(), 1);
    }

    #[test]
    fn non_method_label_with_parent_class_no_defines_method() {
        let mut gbuf = GraphBuffer::new("test");
        gbuf.upsert_node(
            "File",
            "test.rs",
            "test.rs",
            "test.rs",
            0,
            0,
            HashMap::new(),
        );
        gbuf.upsert_node(
            "Function",
            "helper",
            "test.rs::MyClass::helper",
            "test.rs",
            1,
            5,
            HashMap::new(),
        );

        // A Function with a parent_class set but label is "Function" not "Method".
        let def = make_def(
            "helper",
            "test.rs::MyClass::helper",
            "Function",
            Some("test.rs::MyClass"),
        );
        let ef = make_ef("test.rs", vec![def]);
        let _reg = build_registry(&[ef], &mut gbuf);

        // Only DEFINES (no DEFINES_METHOD because label != "Method").
        assert_eq!(gbuf.edge_count(), 1);
    }

    #[test]
    fn imports_edge_resolved_via_registry() {
        let mut gbuf = GraphBuffer::new("test");
        // File node for the importing file.
        gbuf.upsert_node(
            "File",
            "main.rs",
            "main.rs",
            "main.rs",
            0,
            0,
            HashMap::new(),
        );
        // File node for the target (needed for DEFINES edge in Pass 2).
        gbuf.upsert_node(
            "File",
            "std/collections.rs",
            "std/collections.rs",
            "std/collections.rs",
            0,
            0,
            HashMap::new(),
        );
        gbuf.upsert_node(
            "Function",
            "HashMap",
            "std::collections::HashMap",
            "std/collections.rs",
            1,
            100,
            HashMap::new(),
        );

        let target_def = make_def("HashMap", "std::collections::HashMap", "Function", None);
        let target_ef = ExtractedFile {
            file_path: "std/collections.rs".to_string(),
            ..make_ef("std/collections.rs", vec![target_def])
        };

        let source_ef =
            make_ef_with_imports("main.rs", vec![], vec![("HashMap", "std::collections")]);

        let _reg = build_registry(&[target_ef, source_ef], &mut gbuf);

        // DEFINES (for HashMap) + IMPORTS
        assert_eq!(gbuf.edge_count(), 2);

        let file_node = gbuf.find_by_qn("main.rs").unwrap();
        let target_node = gbuf.find_by_qn("std::collections::HashMap").unwrap();
        assert!(gbuf.edge_dedup_key(file_node.id, target_node.id, "IMPORTS"));
    }

    #[test]
    fn imports_has_local_name_property() {
        let mut gbuf = GraphBuffer::new("test");
        gbuf.upsert_node(
            "File",
            "main.rs",
            "main.rs",
            "main.rs",
            0,
            0,
            HashMap::new(),
        );
        gbuf.upsert_node(
            "File",
            "std/collections.rs",
            "std/collections.rs",
            "std/collections.rs",
            0,
            0,
            HashMap::new(),
        );
        gbuf.upsert_node(
            "Function",
            "HashMap",
            "std::collections::HashMap",
            "std/collections.rs",
            1,
            100,
            HashMap::new(),
        );

        let target_def = make_def("HashMap", "std::collections::HashMap", "Function", None);
        let target_ef = ExtractedFile {
            file_path: "std/collections.rs".to_string(),
            ..make_ef("std/collections.rs", vec![target_def])
        };

        let source_ef =
            make_ef_with_imports("main.rs", vec![], vec![("HashMap", "std::collections")]);

        let _reg = build_registry(&[target_ef, source_ef], &mut gbuf);

        // Verify the IMPORTS edge has the local_name property.
        let file_node = gbuf.find_by_qn("main.rs").unwrap();
        let _target_node = gbuf.find_by_qn("std::collections::HashMap").unwrap();
        let edges = gbuf.find_edges_by_source_type(file_node.id, "IMPORTS");
        assert_eq!(edges.len(), 1);

        let edge = edges[0];
        assert_eq!(
            edge.properties.get("local_name").unwrap(),
            "HashMap",
            "IMPORTS edge should carry local_name property"
        );
    }

    #[test]
    fn import_unresolved_name_no_edge() {
        let mut gbuf = GraphBuffer::new("test");
        gbuf.upsert_node(
            "File",
            "main.rs",
            "main.rs",
            "main.rs",
            0,
            0,
            HashMap::new(),
        );

        // Import for "NonexistentType" which is not in the registry.
        let ef = make_ef_with_imports("main.rs", vec![], vec![("NonexistentType", "unknown")]);
        let _reg = build_registry(&[ef], &mut gbuf);

        assert_eq!(gbuf.edge_count(), 0);
    }

    #[test]
    fn no_file_node_for_import_skips_edge() {
        let mut gbuf = GraphBuffer::new("test");
        // Source file node deliberately omitted (ghost.rs).
        // Target file node for HashMap must exist for its DEFINES edge.
        gbuf.upsert_node(
            "File",
            "std/collections.rs",
            "std/collections.rs",
            "std/collections.rs",
            0,
            0,
            HashMap::new(),
        );
        gbuf.upsert_node(
            "Function",
            "HashMap",
            "std::collections::HashMap",
            "std/collections.rs",
            1,
            100,
            HashMap::new(),
        );

        let target_def = make_def("HashMap", "std::collections::HashMap", "Function", None);
        let target_ef = ExtractedFile {
            file_path: "std/collections.rs".to_string(),
            ..make_ef("std/collections.rs", vec![target_def])
        };

        let source_ef =
            make_ef_with_imports("ghost.rs", vec![], vec![("HashMap", "std::collections")]);
        let _reg = build_registry(&[target_ef, source_ef], &mut gbuf);

        // Only DEFINES (HashMap), IMPORTS skipped because source file node missing.
        assert_eq!(gbuf.edge_count(), 1);
    }

    #[test]
    fn multiple_files_multiple_edges() {
        let mut gbuf = GraphBuffer::new("test");

        // File 1
        gbuf.upsert_node("File", "a.rs", "a.rs", "a.rs", 0, 0, HashMap::new());
        gbuf.upsert_node("Function", "foo", "a.rs::foo", "a.rs", 1, 5, HashMap::new());
        gbuf.upsert_node(
            "Function",
            "bar",
            "a.rs::bar",
            "a.rs",
            10,
            15,
            HashMap::new(),
        );

        // File 2
        gbuf.upsert_node("File", "b.rs", "b.rs", "b.rs", 0, 0, HashMap::new());
        gbuf.upsert_node(
            "Class",
            "MyClass",
            "b.rs::MyClass",
            "b.rs",
            1,
            20,
            HashMap::new(),
        );
        gbuf.upsert_node(
            "Method",
            "method1",
            "b.rs::MyClass::method1",
            "b.rs",
            5,
            10,
            HashMap::new(),
        );

        let ef_a = make_ef(
            "a.rs",
            vec![
                make_def("foo", "a.rs::foo", "Function", None),
                make_def("bar", "a.rs::bar", "Function", None),
            ],
        );

        let ef_b = make_ef(
            "b.rs",
            vec![
                make_def("MyClass", "b.rs::MyClass", "Class", None),
                make_def(
                    "method1",
                    "b.rs::MyClass::method1",
                    "Method",
                    Some("b.rs::MyClass"),
                ),
            ],
        );

        let _reg = build_registry(&[ef_a, ef_b], &mut gbuf);

        // Expected edges:
        //   a.rs: 2 × DEFINES
        //   b.rs: 2 × DEFINES + 1 × DEFINES_METHOD
        assert_eq!(gbuf.edge_count(), 5);
    }

    #[test]
    fn duplicate_defines_edge_is_deduplicated() {
        let mut gbuf = GraphBuffer::new("test");
        gbuf.upsert_node(
            "File",
            "test.rs",
            "test.rs",
            "test.rs",
            0,
            0,
            HashMap::new(),
        );
        gbuf.upsert_node(
            "Function",
            "dup",
            "test.rs::dup",
            "test.rs",
            1,
            5,
            HashMap::new(),
        );

        // Same def appears twice in the same file (edge case from re-parse).
        let def = make_def("dup", "test.rs::dup", "Function", None);
        let ef = make_ef("test.rs", vec![def.clone(), def]);
        let _reg = build_registry(&[ef], &mut gbuf);

        // Only 1 edge despite 2 defs (dedup by source, target, type).
        assert_eq!(gbuf.edge_count(), 1);
    }
}
