//! Phase 1: Build the structural hierarchy (Project → Folder → File nodes).
//!
//! Creates the node/edge skeleton for the index: a Project node, a root Folder
//! node, nested Folder nodes for each subdirectory, and one File node per
//! discovered file. All edges are `CONTAINS_FOLDER` (folder→folder) or
//! `CONTAINS_FILE` (folder→file).
//!
//! # Determinism
//!
//! Sorting by absolute path plus dedup guarantees that for a given set of
//! files, the exact same node and edge set is produced every time.
//!
//! # C reference
//!
//! Maps to `pass_structure()` + `create_folder_chain()` in
//! `/tmp/codebase-memory-mcp/src/pipeline/pipeline.c:249-382`.

use std::collections::HashMap;
use std::path::Path;

use crate::core::graph_buffer::GraphBuffer;
use crate::core::index_pipeline::discovery::DiscoveredFile;
use crate::core::index_types::NodeId;

/// Build the structural hierarchy (Project → Folder → File nodes) in `gbuf`.
///
/// Called once at the start of the pipeline, before extraction.
///
/// # Panics
///
/// Panics if `project_root` is empty (needs a non-empty string for the
/// Project-node qualified name).
pub fn build_structure(project_root: &str, files: &[DiscoveredFile], gbuf: &mut GraphBuffer) {
    assert!(!project_root.is_empty(), "project_root must not be empty");

    // 1. Create the Project node.
    let project_id = gbuf.upsert_node(
        "Project",
        project_root,
        project_root,
        "",
        0,
        0,
        HashMap::new(),
    );

    // 2. Create the root Folder node (empty string represents root).
    let root_folder_qn = format!("{project_root}::");
    let root_folder_id = gbuf.upsert_node("Folder", "", &root_folder_qn, "", 0, 0, HashMap::new());

    // 3. Edge: Project ──CONTAINS_FOLDER──→ root Folder.
    gbuf.insert_edge(
        project_id,
        root_folder_id,
        "CONTAINS_FOLDER",
        HashMap::new(),
    );

    // 4. Process every file: build folder chain and create File nodes.
    for file in files {
        let rel_path = &file.rel_path;

        // Determine the parent directory of this file.
        let dir = Path::new(rel_path)
            .parent()
            .and_then(|p| {
                let s = p.to_str()?;
                if s.is_empty() { None } else { Some(s) }
            })
            .unwrap_or("");

        // Ensure the folder chain exists up to this directory.
        let parent_folder_id = if dir.is_empty() {
            root_folder_id
        } else {
            ensure_folder_chain(dir, project_root, &root_folder_qn, gbuf)
        };

        // Create the File node.
        let file_id = gbuf.upsert_node("File", rel_path, rel_path, rel_path, 0, 0, HashMap::new());

        // Edge: parent Folder ──CONTAINS_FILE──→ File.
        gbuf.insert_edge(parent_folder_id, file_id, "CONTAINS_FILE", HashMap::new());
    }
}

/// Ensure that Folder nodes exist for every directory component of `dir_path`.
///
/// Walks **top-down** from the root, creating missing Folder nodes and
/// `CONTAINS_FOLDER` edges as it goes. Returns the `NodeId` of the deepest
/// (most specific) folder — the direct parent of whichever file triggered the
/// call.
///
/// Because `upsert_node` and `insert_edge` are both idempotent, calling this
/// for a directory that has already been processed is a no-op (aside from a
/// handful of HashMap lookups).
///
/// # Panics
///
/// Panics if `root_folder_qn` does not reference an existing node (caller must
/// have inserted the root folder before calling this).
fn ensure_folder_chain(
    dir_path: &str,
    project_root: &str,
    root_folder_qn: &str,
    gbuf: &mut GraphBuffer,
) -> NodeId {
    // Root folder must already exist.
    let _root = gbuf
        .find_by_qn(root_folder_qn)
        .expect("root folder must exist before ensure_folder_chain");

    let mut parent_qn = root_folder_qn.to_string();
    let mut accumulated = String::new();

    for segment in dir_path.split('/') {
        if segment.is_empty() {
            continue;
        }
        if !accumulated.is_empty() {
            accumulated.push('/');
        }
        accumulated.push_str(segment);

        let folder_qn = format!("{project_root}::{accumulated}");
        let folder_id = gbuf.upsert_node(
            "Folder",
            segment,
            &folder_qn,
            &accumulated,
            0,
            0,
            HashMap::new(),
        );

        // Connect parent → child via CONTAINS_FOLDER.
        // `insert_edge` is deduplicated, so this is safe to call repeatedly.
        let parent_node = gbuf
            .find_by_qn(&parent_qn)
            .expect("parent folder must exist when walking top-down");
        gbuf.insert_edge(parent_node.id, folder_id, "CONTAINS_FOLDER", HashMap::new());

        parent_qn = folder_qn;
    }

    // Return the deepest folder's NodeId.
    let deepest_qn = format!("{project_root}::{dir_path}");
    gbuf.find_by_qn(&deepest_qn)
        .expect("deepest folder must exist after ensure_folder_chain")
        .id
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;

    /// Minimal helper: build a `DiscoveredFile` slice from a list of relative
    /// paths.
    fn make_files(paths: &[&str]) -> Vec<DiscoveredFile> {
        paths
            .iter()
            .map(|p| DiscoveredFile {
                path: std::path::PathBuf::from(p),
                rel_path: p.to_string(),
                ext: p.rsplit('.').next().unwrap_or("").to_string(),
                size: 100,
                mtime: SystemTime::UNIX_EPOCH,
            })
            .collect()
    }

    // ------------------------------------------------------------------
    // 3 files → 3 File nodes + 1 Project node + folder nodes
    // ------------------------------------------------------------------

    #[test]
    fn basic_hierarchy() {
        let project_root = "/test/project";
        let files = make_files(&["src/main.rs", "src/lib.rs", "README.md"]);
        let mut gbuf = GraphBuffer::new(project_root);
        build_structure(project_root, &files, &mut gbuf);

        // Project node.
        let project = gbuf.find_by_qn(project_root).expect("Project node");
        assert_eq!(project.label, "Project");

        // Root folder.
        let root_qn = format!("{project_root}::");
        let root = gbuf.find_by_qn(&root_qn).expect("Root folder");
        assert_eq!(root.label, "Folder");
        assert_eq!(root.name, "");

        // 3 File nodes (rel_path used as QN).
        for path in &["src/main.rs", "src/lib.rs", "README.md"] {
            let f = gbuf
                .find_by_qn(path)
                .unwrap_or_else(|| panic!("File node {path}"));
            assert_eq!(f.label, "File", "{path} should be File");
            assert_eq!(f.file_path, *path, "{path} file_path mismatch");
        }

        // Total node count: 1 Project + 1 root Folder + 1 src Folder + 3 Files = 6
        assert_eq!(gbuf.node_count(), 6, "expected 6 nodes");

        // Edge: root folder contains README.md (root-level file).
        let readme = gbuf.find_by_qn("README.md").unwrap();
        assert!(
            gbuf.edge_dedup_key(root.id, readme.id, "CONTAINS_FILE"),
            "root→README.md edge missing"
        );
    }

    // ------------------------------------------------------------------
    // Nested path creates correct folder chain
    // ------------------------------------------------------------------

    #[test]
    fn nested_folder_chain() {
        let project_root = "/test/project";
        let files = make_files(&["src/core/index_pipeline/mod.rs"]);
        let mut gbuf = GraphBuffer::new(project_root);
        build_structure(project_root, &files, &mut gbuf);

        // Verify folder chain exists.
        let folders = [
            ("", format!("{project_root}::")),             // root
            ("src", format!("{project_root}::src")),       // src
            ("core", format!("{project_root}::src/core")), // core
            (
                "index_pipeline",
                format!("{project_root}::src/core/index_pipeline"),
            ),
        ];

        for (name, qn) in &folders {
            let node = gbuf
                .find_by_qn(qn)
                .unwrap_or_else(|| panic!("Folder {name} not found"));
            assert_eq!(node.label, "Folder");
            assert_eq!(node.name, *name);
        }

        // Verify CONTAINS_FOLDER edges: Project → root → src → core → index_pipeline
        let project = gbuf.find_by_qn(project_root).unwrap();
        let root = gbuf.find_by_qn(&folders[0].1).unwrap();
        let src = gbuf.find_by_qn(&folders[1].1).unwrap();
        let core = gbuf.find_by_qn(&folders[2].1).unwrap();
        let ip = gbuf.find_by_qn(&folders[3].1).unwrap();

        assert!(
            gbuf.edge_dedup_key(project.id, root.id, "CONTAINS_FOLDER"),
            "Project→root edge missing"
        );
        assert!(
            gbuf.edge_dedup_key(root.id, src.id, "CONTAINS_FOLDER"),
            "root→src edge missing"
        );
        assert!(
            gbuf.edge_dedup_key(src.id, core.id, "CONTAINS_FOLDER"),
            "src→core edge missing"
        );
        assert!(
            gbuf.edge_dedup_key(core.id, ip.id, "CONTAINS_FOLDER"),
            "core→index_pipeline edge missing"
        );

        // Verify CONTAINS_FILE edge: index_pipeline → mod.rs
        let file = gbuf.find_by_qn("src/core/index_pipeline/mod.rs").unwrap();
        assert!(
            gbuf.edge_dedup_key(ip.id, file.id, "CONTAINS_FILE"),
            "index_pipeline→file edge missing"
        );
    }

    // ------------------------------------------------------------------
    // Deterministic output: same input → same node/edge set
    // ------------------------------------------------------------------

    #[test]
    fn deterministic_output() {
        let project_root = "/det/proj";
        let files = make_files(&["a.rs", "b/c.rs", "b/d/e.rs"]);

        let mut gbuf1 = GraphBuffer::new(project_root);
        build_structure(project_root, &files, &mut gbuf1);

        let mut gbuf2 = GraphBuffer::new(project_root);
        build_structure(project_root, &files, &mut gbuf2);

        assert_eq!(
            gbuf1.node_count(),
            gbuf2.node_count(),
            "deterministic node count"
        );
        assert_eq!(
            gbuf1.edge_count(),
            gbuf2.edge_count(),
            "deterministic edge count"
        );

        // Every node in gbuf1 must exist in gbuf2 with matching fields.
        gbuf1.foreach_node(&mut |n1| {
            let n2 = gbuf2
                .find_by_qn(&n1.qualified_name)
                .unwrap_or_else(|| panic!("node {} not in gbuf2", n1.qualified_name));
            assert_eq!(
                n1.label, n2.label,
                "label mismatch for {}",
                n1.qualified_name
            );
            assert_eq!(n1.name, n2.name, "name mismatch for {}", n1.qualified_name);
        });
    }
}
