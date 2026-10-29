// SPDX-License-Identifier: Apache-2.0
//! The one check through which a graph reference — a file, a symbol, an edge
//! end — leaves toward a tool or host (G04).
//!
//! A graph row names the project file it was derived from. Nothing in the row
//! changes when that file is deleted, restricted or rewritten, so every
//! reference is asked again when it is served:
//!
//! 1. **All modes:** the path must resolve inside the project root through
//!    the path jail the rooted reader uses (`pathjail::jail_path`) and be a
//!    regular file there (links are not followed). Gone or outside ⇒ not
//!    served. In Community developer mode this is the whole rule — the mode
//!    makes no protection claim, as with the originless rule.
//! 2. **Under protection** (`stored::source_authority_required`): the full
//!    rule of `bm25_index::source_gate::SourceGate` for every reference — the
//!    admitted rooted reader must return the file now, storable, under the
//!    current project policy and role, and a symbol's name must still occur
//!    in it. Same gate, same bound: over the bound the reference is refused.
//!
//! There is deliberately no shortcut under protection. The graph records a
//! content fingerprint for its own staleness check and could record the
//! gateway policy digest, but neither covers the project policy pack or the
//! role, so "unchanged since an admitted build" does not prove today's
//! rights. A staleness fingerprint is not an authorization.
//!
//! A reference that is not a project file (`scheme://…`, a cross-source node)
//! has no source authority on record: withheld under protection, served in
//! Community developer mode.
//!
//! One gate serves one opened graph, which is one request: decisions are
//! memoised per file and never outlive it.

use std::cell::{OnceCell, RefCell};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;

use rusqlite::Connection;

use super::{CodeGraph, Node};
use crate::core::bm25_index::source_gate::SourceGate;

/// Tool identity graph sources are re-read for under protection.
const GRAPH_TOOL: &str = "ctx_graph";

/// Distinct files one request can decide under protection: the content-read
/// bound of `SourceGate`. It chooses between a whole-graph and a local
/// ranking; the gate itself enforces the bound.
const PROTECTED_FILES: usize = crate::core::bm25_index::source_gate::MAX_SOURCES;

/// Files one gate decides and remembers. Over it a file the gate has not
/// decided yet is refused: not checked, not served, not remembered.
const MAX_FILES: usize = 65_536;

/// The serve-time check of one opened graph.
pub(crate) struct GraphGate {
    project_root: String,
    protected: OnceCell<bool>,
    /// Per file asked so far: whether it is a regular file inside the root.
    present: RefCell<HashMap<String, bool>>,
    sources: RefCell<SourceGate>,
}

impl GraphGate {
    pub(crate) fn for_root(project_root: &str) -> Self {
        Self {
            project_root: project_root.to_owned(),
            protected: OnceCell::new(),
            present: RefCell::new(HashMap::new()),
            sources: RefCell::new(SourceGate::new(Path::new(project_root), GRAPH_TOOL)),
        }
    }

    /// Whether the file `path` may be named now.
    pub(crate) fn file(&self, path: &str) -> bool {
        self.reference(path, "")
    }

    /// Whether the symbol `name` of `path` may be named now.
    pub(crate) fn symbol(&self, path: &str, name: &str) -> bool {
        self.reference(path, identifier(name))
    }

    /// Whether protection is active for this request (read once).
    fn protected(&self) -> bool {
        *self
            .protected
            .get_or_init(crate::core::context_admission::stored::source_authority_required)
    }

    /// Whether `text` of a node that names no project file (a commit, a
    /// knowledge fact, an issue, a CI run) may be served now. Such a node
    /// records no source binding, so it follows the originless rule of
    /// `serve_stored`: withheld while protection is active in any layer,
    /// served in Community developer mode when today's content rules leave
    /// it unchanged.
    fn originless(&self, text: &str) -> bool {
        !self.protected()
            && crate::core::context_admission::stored::serve_stored(text, None, None)
                .is_ok_and(|served| served == text)
    }

    /// The files of `files` that may enter a traversal or ranking over the
    /// whole graph. `edges` are the file pairs of that graph, `seeds` the
    /// files the request is about.
    ///
    /// Community developer mode, or a graph small enough to be decided in
    /// full: every file is asked. Under protection with more files than one
    /// request can decide ([`PROTECTED_FILES`]): the ranking is local — the
    /// seeds are asked first, then their neighbours, frontier by frontier and
    /// only through files that are served, until the bound is spent. Without
    /// seeds that is the empty set: no ranking rather than one over files
    /// nobody decided.
    pub(crate) fn ranking_files(
        &self,
        mut files: Vec<String>,
        edges: &[(String, String)],
        seeds: &[String],
    ) -> HashSet<String> {
        files.sort();
        files.dedup();
        if !self.protected() || files.len() <= PROTECTED_FILES {
            return files.into_iter().filter(|file| self.file(file)).collect();
        }
        let known: HashSet<&str> = files.iter().map(String::as_str).collect();
        let mut asked: HashSet<&str> = HashSet::new();
        let mut served: HashSet<String> = HashSet::new();
        let mut frontier: BTreeSet<&str> = seeds
            .iter()
            .map(String::as_str)
            .filter(|seed| known.contains(*seed))
            .collect();
        while !frontier.is_empty() && asked.len() < PROTECTED_FILES {
            let mut next: BTreeSet<&str> = BTreeSet::new();
            for file in frontier {
                if asked.len() >= PROTECTED_FILES {
                    break;
                }
                if !asked.insert(file) || !self.file(file) {
                    continue;
                }
                served.insert(file.to_owned());
                for (from, to) in edges {
                    if from == file {
                        next.insert(to.as_str());
                    } else if to == file {
                        next.insert(from.as_str());
                    }
                }
            }
            next.retain(|file| known.contains(*file) && !asked.contains(*file));
            frontier = next;
        }
        served
    }

    fn reference(&self, path: &str, needle: &str) -> bool {
        let protected = self.protected();
        if path.contains("://") {
            return !protected;
        }
        self.present(path) && (!protected || self.sources.borrow_mut().serves(path, needle))
    }

    fn present(&self, path: &str) -> bool {
        let known = self.present.borrow().get(path).copied();
        if let Some(known) = known {
            return known;
        }
        if self.present.borrow().len() >= MAX_FILES {
            return false;
        }
        let root = Path::new(&self.project_root);
        let present = crate::core::pathjail::jail_path(&root.join(path), root)
            .is_ok_and(|jailed| std::fs::symlink_metadata(jailed).is_ok_and(|meta| meta.is_file()));
        self.present.borrow_mut().insert(path.to_owned(), present);
        present
    }
}

/// The last identifier of a symbol name: `Config::load` → `load`,
/// `fmt::Display for Wrapper` → `Wrapper`. That is what must still occur in
/// the admitted text.
fn identifier(name: &str) -> &str {
    name.rsplit(|c: char| !(c.is_alphanumeric() || c == '_'))
        .find(|part| !part.is_empty())
        .unwrap_or("")
}

impl CodeGraph {
    /// This graph, gated for `project_root`: for an in-memory copy of a
    /// project's graph that is read back through the serving surface.
    #[must_use]
    pub(crate) fn gated_for(mut self, project_root: &str) -> Self {
        self.gate = Some(GraphGate::for_root(project_root));
        self
    }

    /// Whether a reference to the file `path` of this graph may be served
    /// now. A graph without a project root (in memory) has no source on
    /// record to ask and is not gated.
    pub fn serves_file(&self, path: &str) -> bool {
        self.gate.as_ref().is_none_or(|gate| gate.file(path))
    }

    /// Whether a reference to the symbol `name` of `path` may be served now.
    pub fn serves_symbol(&self, path: &str, name: &str) -> bool {
        self.gate
            .as_ref()
            .is_none_or(|gate| gate.symbol(path, name))
    }

    /// Whether `node` may be named now — the one entry point for a node of
    /// any kind. See [`Self::serves_node_parts`].
    pub fn serves_node(&self, node: &Node) -> bool {
        self.serves_node_parts(
            node.kind.as_str(),
            &node.file_path,
            &node.name,
            node.metadata.as_deref(),
        )
    }

    /// [`Self::serves_node`] for a row read as columns (`nodes.kind`, the
    /// path, `nodes.name`, `nodes.metadata`).
    ///
    /// - A node of a project file: a `symbol`, or a `test` named other than
    ///   its file, by the symbol rule; every other kind by the file rule.
    /// - A node without a file on record (commit, knowledge, issue, CI run):
    ///   no node records a source binding that could be re-checked, so the
    ///   originless rule decides its name and, when given, its metadata —
    ///   withheld under protection, served in Community developer mode while
    ///   today's content rules leave the text unchanged. Pass the metadata
    ///   whenever it is shown or exported.
    pub fn serves_node_parts(
        &self,
        kind: &str,
        file_path: &str,
        name: &str,
        metadata: Option<&str>,
    ) -> bool {
        let Some(gate) = self.gate.as_ref() else {
            return true;
        };
        if file_path.is_empty() {
            return gate.originless(name) && metadata.is_none_or(|text| gate.originless(text));
        }
        match kind {
            "symbol" | "test" if name != file_path => gate.symbol(file_path, name),
            _ => gate.file(file_path),
        }
    }

    /// `neighbours` of the file `anchor`: nothing when the anchor itself may
    /// not be served, otherwise the neighbours that may.
    pub(super) fn served_neighbours(&self, anchor: &str, neighbours: Vec<String>) -> Vec<String> {
        if !self.serves_file(anchor) {
            return Vec::new();
        }
        neighbours
            .into_iter()
            .filter(|path| self.serves_file(path))
            .collect()
    }
}

/// A graph store as a whole-graph computation reads it — a ranking, a
/// community detection, a rule scan: the rows, and the serve-time source
/// check that decides which of them exist for the computation (G04).
///
/// [`CodeGraph`] answers with the gate of the request that opened it. A bare
/// [`Connection`] has no project root on record and admits everything; it is
/// the form of a graph that is not served from a project (in memory).
pub trait GraphSource {
    /// The connection the rows are read from.
    fn store(&self) -> &Connection;

    /// Whether this source asks current sources at all. `false` for a store
    /// without a project root: a caller that would name several files in one
    /// row must then leave that row to a check that can decide them.
    fn checks_sources(&self) -> bool;

    /// Whether the file `path` may be used now.
    fn file_served(&self, path: &str) -> bool;

    /// Whether the symbol `name` of `path` may be used now.
    fn symbol_served(&self, path: &str, name: &str) -> bool;

    /// The files of `files` that may enter a computation over the whole
    /// graph (`GraphGate::ranking_files`).
    fn rankable_files(
        &self,
        files: Vec<String>,
        edges: &[(String, String)],
        seeds: &[String],
    ) -> HashSet<String>;
}

/// A reference to a source is that source (`&graph` where `graph` is already
/// a `&CodeGraph`).
impl<T: GraphSource + ?Sized> GraphSource for &T {
    fn store(&self) -> &Connection {
        (**self).store()
    }

    fn checks_sources(&self) -> bool {
        (**self).checks_sources()
    }

    fn file_served(&self, path: &str) -> bool {
        (**self).file_served(path)
    }

    fn symbol_served(&self, path: &str, name: &str) -> bool {
        (**self).symbol_served(path, name)
    }

    fn rankable_files(
        &self,
        files: Vec<String>,
        edges: &[(String, String)],
        seeds: &[String],
    ) -> HashSet<String> {
        (**self).rankable_files(files, edges, seeds)
    }
}

impl GraphSource for Connection {
    fn store(&self) -> &Connection {
        self
    }

    fn checks_sources(&self) -> bool {
        false
    }

    fn file_served(&self, _path: &str) -> bool {
        true
    }

    fn symbol_served(&self, _path: &str, _name: &str) -> bool {
        true
    }

    fn rankable_files(
        &self,
        files: Vec<String>,
        _edges: &[(String, String)],
        _seeds: &[String],
    ) -> HashSet<String> {
        files.into_iter().collect()
    }
}

impl GraphSource for CodeGraph {
    fn store(&self) -> &Connection {
        self.connection()
    }

    fn checks_sources(&self) -> bool {
        self.gate.is_some()
    }

    fn file_served(&self, path: &str) -> bool {
        self.serves_file(path)
    }

    fn symbol_served(&self, path: &str, name: &str) -> bool {
        self.serves_symbol(path, name)
    }

    fn rankable_files(
        &self,
        files: Vec<String>,
        edges: &[(String, String)],
        seeds: &[String],
    ) -> HashSet<String> {
        match self.gate.as_ref() {
            Some(gate) => gate.ranking_files(files, edges, seeds),
            None => files.into_iter().collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::core::graph_index::{FileEntry, ProjectIndex, SymbolEntry};
    use crate::core::graph_provider::GraphProvider;
    use crate::core::policy::runtime::TestPolicyOverride;

    /// A file and its symbol are served while the file exists and are
    /// withheld — path, entry and symbol name — once it is removed.
    #[test]
    fn a_graph_reference_is_withheld_once_its_source_file_is_gone() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = TestPolicyOverride::set(None);
        let project = tempfile::tempdir().expect("project");
        let root = project.path().to_string_lossy().into_owned();
        std::fs::write(project.path().join("auth.rs"), "pub fn verify() {}\n").expect("source");

        let mut index = ProjectIndex::new(&root);
        index.files.insert(
            "auth.rs".to_owned(),
            FileEntry {
                path: "auth.rs".to_owned(),
                hash: String::new(),
                language: "rust".to_owned(),
                line_count: 1,
                token_count: 4,
                exports: vec!["verify".to_owned()],
                summary: "pub fn verify() {}".to_owned(),
            },
        );
        index.symbols.insert(
            "auth.rs::verify".to_owned(),
            SymbolEntry {
                file: "auth.rs".to_owned(),
                name: "verify".to_owned(),
                kind: "fn".to_owned(),
                start_line: 1,
                end_line: 1,
                is_exported: true,
            },
        );
        let provider = GraphProvider::GraphIndex(index);

        assert_eq!(provider.file_paths(), vec!["auth.rs".to_owned()]);
        assert_eq!(provider.all_symbols().len(), 1);
        assert!(provider.get_file_entry("auth.rs").is_some());

        std::fs::remove_file(project.path().join("auth.rs")).expect("remove");
        assert!(provider.file_paths().is_empty());
        assert!(provider.all_symbols().is_empty());
        assert!(provider.get_symbol("auth.rs::verify").is_none());
        assert!(provider.get_file_entry("auth.rs").is_none());
        assert!(provider.materialize_project_index(&root).files.is_empty());
    }

    /// Under protection a graph with more files than one request can decide
    /// is ranked locally: from the seeds outwards, never more files than the
    /// bound, never one the gate did not decide — and not at all without
    /// seeds.
    #[test]
    fn a_ranking_under_protection_uses_only_files_decided_from_its_seeds() {
        use super::{GraphGate, PROTECTED_FILES};

        let _data = crate::core::data_dir::isolated_data_dir();
        let pack = crate::core::policy::parse(
            "name = \"graph-ranking-test\"\nversion = \"1.0.0\"\ndescription = \"test\"\n\
             [redaction]\ncustomer = 'K-[0-9]{6}'",
        )
        .expect("valid policy");
        let _protected = TestPolicyOverride::set(Some(
            crate::core::policy::resolve(&pack).expect("resolved policy"),
        ));
        let project = tempfile::tempdir().expect("project");
        let root = project.path().to_string_lossy().into_owned();
        // A chain f000 → f001 → … two files longer than the bound.
        let files: Vec<String> = (0..PROTECTED_FILES + 2)
            .map(|n| format!("f{n:03}.rs"))
            .collect();
        for file in &files {
            std::fs::write(project.path().join(file), "pub fn item() {}\n").expect("source");
        }
        let edges: Vec<(String, String)> = files
            .windows(2)
            .map(|pair| (pair[0].clone(), pair[1].clone()))
            .collect();

        let gate = GraphGate::for_root(&root);
        let local = gate.ranking_files(files.clone(), &edges, &files[..1]);
        assert!(local.contains(&files[0]), "the seed is decided first");
        assert!(local.len() <= PROTECTED_FILES);
        assert!(
            !local.contains(&files[PROTECTED_FILES + 1]),
            "beyond the bound"
        );
        // Whatever the ranking uses, this gate decided and serves.
        assert!(local.iter().all(|file| gate.file(file)));
        // The far end was never decided; the spent gate refuses it.
        assert!(!gate.file(&files[PROTECTED_FILES + 1]));

        let unseeded = GraphGate::for_root(&root).ranking_files(files, &edges, &[]);
        assert!(unseeded.is_empty());
    }

    /// A withdrawn file is no step of a walk: with `a → w → b` and `a → n`,
    /// `b` is reached from `a` (and `a` from `b`) only while `w` is served.
    /// Once `w` is gone nothing is reached through it, its edges are not
    /// counted, and `a`'s neighbour that still exists is still found.
    #[test]
    fn a_traversal_does_not_pass_through_a_withdrawn_file() {
        use crate::core::property_graph::{CodeGraph, Edge, EdgeKind, Node};

        let _data = crate::core::data_dir::isolated_data_dir();
        let _community = TestPolicyOverride::set(None);
        let project = tempfile::tempdir().expect("project");
        let root = project.path().to_string_lossy().into_owned();
        for file in ["a.rs", "w.rs", "b.rs", "n.rs"] {
            std::fs::write(project.path().join(file), "pub fn item() {}\n").expect("source");
        }

        let graph = CodeGraph::open(&root).expect("graph");
        let node = |file: &str| graph.upsert_node(&Node::file(file)).expect("node");
        let (a, w, b, n) = (node("a.rs"), node("w.rs"), node("b.rs"), node("n.rs"));
        for (from, to) in [(a, w), (w, b), (a, n)] {
            graph
                .upsert_edge(&Edge::new(from, to, EdgeKind::Imports))
                .expect("edge");
        }

        let chain = graph.dependency_chain("a.rs", "b.rs").expect("chain");
        assert_eq!(
            chain.map(|chain| chain.path),
            Some(vec![
                "a.rs".to_owned(),
                "w.rs".to_owned(),
                "b.rs".to_owned()
            ])
        );
        let impact = graph.impact_analysis("b.rs", 5).expect("impact");
        assert_eq!(impact.affected_files, ["a.rs", "w.rs"]);
        assert_eq!(impact.edges_traversed, 2);

        std::fs::remove_file(project.path().join("w.rs")).expect("withdraw");
        // A new request: a gate decides a file once.
        let graph = CodeGraph::open(&root).expect("graph");
        assert!(
            graph
                .dependency_chain("a.rs", "b.rs")
                .expect("chain")
                .is_none()
        );
        let impact = graph.impact_analysis("b.rs", 5).expect("impact");
        assert!(impact.affected_files.is_empty(), "{impact:?}");
        assert_eq!(impact.edges_traversed, 0);
        assert_eq!(impact.max_depth_reached, 0);
        let near = graph.dependency_chain("a.rs", "n.rs").expect("chain");
        assert_eq!(near.map(|chain| chain.depth), Some(1));
        let related: Vec<String> = graph
            .related_files("a.rs", 10)
            .expect("related")
            .into_iter()
            .map(|(path, _)| path)
            .collect();
        assert_eq!(related, ["n.rs"]);
    }
}
