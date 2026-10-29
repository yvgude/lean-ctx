// SPDX-License-Identifier: Apache-2.0
//! The code graph an owner may publish to Hive (Hive plan v3 N1; C0 1.9
//! section 8).
//!
//! By default Hive knows a repo's code only through the anchors of its
//! facts. When the owner sets a project's `code_graph` to `full`, the
//! engine publishes the structure of its checkout: files and the symbols
//! they define, which file contains which symbol, and which file imports
//! which. Never source text, bodies or digests (a hash of a short body can
//! be guessed back).
//!
//! Every path follows the anchor path rules and passes the same admission
//! as an anchor: a file the engine may not read for knowledge, or one it
//! withholds, is not in the graph, nor are edges to it. The graph is sorted
//! and bounded so the same checkout always gives the same document.

use std::collections::{BTreeMap, BTreeSet};

use std::path::Path;

use crate::core::graph_index::ProjectIndex;
use crate::core::hive::HiveError;
use crate::core::hive::auth::Session;
use crate::core::hive::sealed;
use crate::core::hive::vault::VaultRecord;
use lean_ctx_protocol::hive::anchor::{MAX_ANCHOR_SYMBOL_CHARS, validate_anchor_path};
use lean_ctx_protocol::hive::codegraph::{
    CODE_GRAPH_PUT_SCHEMA, CODE_GRAPH_SCHEMA, CodeEdgeKind, CodeEdgeV1, CodeGraphAckV1,
    CodeGraphPutV1, CodeGraphV1, CodeNodeKind, CodeNodeV1, MAX_CODE_GRAPH_EDGES,
    MAX_CODE_GRAPH_NODES,
};

/// The exported graph; `truncated` when the checkout exceeds the limits.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CodeGraph {
    pub nodes: Vec<CodeNodeV1>,
    pub edges: Vec<CodeEdgeV1>,
    pub truncated: bool,
    /// Files left out because their path or their admission did not allow it.
    pub withheld_files: usize,
}

impl CodeGraph {
    /// The wire document (section 8), with the commit it was built from.
    pub fn into_v1(self, revision_hint: Option<String>) -> CodeGraphV1 {
        CodeGraphV1 {
            schema: CODE_GRAPH_SCHEMA.into(),
            revision_hint,
            nodes: self.nodes,
            edges: self.edges,
        }
    }
}

fn file_id(path: &str) -> String {
    format!("code:{path}")
}

/// A short, plain token: language and symbol kinds are hints, not text.
fn label(value: &str) -> Option<String> {
    let value = value.trim().to_ascii_lowercase().replace('-', "_");
    (!value.is_empty()
        && value.len() <= 32
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'))
    .then_some(value)
}

fn symbol_ok(name: &str) -> bool {
    !name.is_empty()
        && name.chars().count() <= MAX_ANCHOR_SYMBOL_CHARS
        && !name
            .chars()
            .any(|c| c.is_control() || c.is_whitespace() || c == '#')
}

/// The graph of `index`, keeping only files `admit` lets through.
pub fn export(index: &ProjectIndex, admit: impl Fn(&str) -> bool) -> CodeGraph {
    let mut graph = CodeGraph::default();
    let mut files: BTreeMap<&str, &str> = BTreeMap::new();
    let mut paths: Vec<&String> = index.files.keys().collect();
    paths.sort();
    for path in paths {
        if validate_anchor_path(path).is_err() || !admit(path) {
            graph.withheld_files += 1;
            continue;
        }
        files.insert(path.as_str(), index.files[path].language.as_str());
    }

    let mut symbols: BTreeMap<&str, BTreeMap<&str, &str>> = BTreeMap::new();
    for symbol in index.symbols.values() {
        if files.contains_key(symbol.file.as_str()) && symbol_ok(&symbol.name) {
            symbols
                .entry(symbol.file.as_str())
                .or_default()
                .entry(symbol.name.as_str())
                .or_insert(symbol.kind.as_str());
        }
    }

    // Files first, each with its symbols, until the node limit: a file is
    // either in the graph with all its symbols or not at all.
    let mut kept: BTreeSet<&str> = BTreeSet::new();
    for (path, lang) in &files {
        let defined = symbols.get(path).map_or(0, BTreeMap::len);
        if graph.nodes.len() + 1 + defined > MAX_CODE_GRAPH_NODES {
            graph.truncated = true;
            break;
        }
        kept.insert(path);
        graph.nodes.push(CodeNodeV1 {
            id: file_id(path),
            kind: CodeNodeKind::File,
            lang: label(lang),
            symbol_kind: None,
        });
        for (name, kind) in symbols.get(path).into_iter().flatten() {
            let id = format!("code:{path}#{name}");
            graph.edges.push(CodeEdgeV1 {
                from: file_id(path),
                to: id.clone(),
                kind: CodeEdgeKind::Contains,
            });
            graph.nodes.push(CodeNodeV1 {
                id,
                kind: CodeNodeKind::Symbol,
                lang: None,
                symbol_kind: label(kind),
            });
        }
    }

    for edge in &index.edges {
        if matches!(edge.kind.as_str(), "import" | "reexport" | "module")
            && edge.from != edge.to
            && kept.contains(edge.from.as_str())
            && kept.contains(edge.to.as_str())
        {
            graph.edges.push(CodeEdgeV1 {
                from: file_id(&edge.from),
                to: file_id(&edge.to),
                kind: CodeEdgeKind::Imports,
            });
        }
    }
    graph.nodes.sort_by(|a, b| a.id.cmp(&b.id));
    graph.nodes.dedup_by(|a, b| a.id == b.id);
    sort_edges(&mut graph.edges);
    if graph.edges.len() > MAX_CODE_GRAPH_EDGES {
        // Every kept file keeps its symbols; imports fill what is left.
        let (mut kept_edges, imports): (Vec<_>, Vec<_>) = graph
            .edges
            .drain(..)
            .partition(|e| e.kind == CodeEdgeKind::Contains);
        let room = MAX_CODE_GRAPH_EDGES.saturating_sub(kept_edges.len());
        kept_edges.extend(imports.into_iter().take(room));
        sort_edges(&mut kept_edges);
        graph.edges = kept_edges;
        graph.truncated = true;
    }
    graph
}

/// Sorted by `(from, to, type)` without repeats, as the contract wants.
fn sort_edges(edges: &mut Vec<CodeEdgeV1>) {
    edges.sort_by(|a, b| (&a.from, &a.to, a.kind).cmp(&(&b.from, &b.to, b.kind)));
    edges.dedup();
}

/// The graph of the checkout at `root`, through the same source admission
/// as a fact's anchors (a file that may not become knowledge is left out).
pub fn export_checkout(root: &str, index: &ProjectIndex) -> CodeGraph {
    export(index, |path| {
        crate::core::archive::bindings::sources_at(root, &[path], "ctx_knowledge").is_some()
    })
}

/// The commit the checkout at `root` is on (`git rev-parse HEAD`), when
/// it is a git checkout.
fn head_commit(root: &Path) -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;
    let head = String::from_utf8(out.stdout).ok()?.trim().to_owned();
    (out.status.success()
        && matches!(head.len(), 40 | 64)
        && head
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
    .then_some(head)
}

/// What a publish sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Published {
    pub nodes: usize,
    pub edges: usize,
    pub truncated: bool,
    pub withheld_files: usize,
    pub sealed: bool,
    pub revision_hint: Option<String>,
}

/// Publish the code graph of the checkout at `root` for `record` (PUT
/// replaces the stored one). Hive takes it only when the repo's owner set
/// `code_graph` to `full`; a sealed vault gets it sealed with the repo key.
pub fn publish(
    session: &Session,
    record: &VaultRecord,
    root: &Path,
) -> Result<Published, HiveError> {
    let root_str = root.to_string_lossy();
    let index =
        ProjectIndex::load(&root_str).unwrap_or_else(|| crate::core::graph_index::scan(&root_str));
    let exported = export_checkout(&root_str, &index);
    let (truncated, withheld_files) = (exported.truncated, exported.withheld_files);
    let revision_hint = head_commit(root);
    let graph = exported.into_v1(revision_hint.clone());
    graph
        .validate()
        .map_err(|e| HiveError::Local(format!("code graph: {e}")))?;
    let (nodes, edges) = (graph.nodes.len(), graph.edges.len());
    let put = if record.sealed {
        // A repo without a key yet (key_epoch 0) and without knowledge gets
        // its first key from the first engine that has something to seal.
        let base = super::load_base(session.store.data_dir(), &record.vault_id).revision;
        let key = match sealed::sealing_key(session, &record.vault_id, base)? {
            sealed::SealingKey::Ready(epoch, key) => (epoch, key),
            sealed::SealingKey::Waiting => {
                return Err(HiveError::Config(
                    "this sealed vault's key has not reached this machine yet".into(),
                ));
            }
            sealed::SealingKey::Rotating { .. } => {
                return Err(HiveError::Config(
                    "this sealed vault's key is being rotated; try again shortly".into(),
                ));
            }
        };
        let doc = serde_json::to_value(&graph).map_err(|e| HiveError::Local(e.to_string()))?;
        let plaintext = lean_ctx_protocol::hive::canonical_json(&doc)
            .map_err(|e| HiveError::Local(e.to_string()))?;
        CodeGraphPutV1 {
            schema: CODE_GRAPH_PUT_SCHEMA.into(),
            graph: None,
            sealed: Some(sealed::seal(
                &key.1,
                &record.vault_id,
                0,
                key.0,
                CODE_GRAPH_SCHEMA,
                &plaintext,
            )?),
        }
    } else {
        CodeGraphPutV1 {
            schema: CODE_GRAPH_PUT_SCHEMA.into(),
            graph: Some(graph),
            sealed: None,
        }
    };
    put.validate()
        .map_err(|e| HiveError::Local(format!("code graph: {e}")))?;
    let body = serde_json::to_value(&put).map_err(|e| HiveError::Local(e.to_string()))?;
    let path = format!("/vaults/{}/code-graph", record.vault_id);
    let answer = session
        .with_client(|c| c.send("PUT", &path, Some(&body), &[], 64 * 1024))
        .map_err(|e| match e {
            HiveError::Server { status: 409, .. } => HiveError::Config(
                if record.sealed {
                    "Hive did not take the graph: the vault keeps only anchors (its owner turns on the full code graph on the web), or its key moved to a newer epoch (run `lean-ctx hive sync`, then publish again)"
                } else {
                    "Hive keeps only anchors for this vault; its owner turns on the full code graph on the web"
                }
                .into(),
            ),
            other => other,
        })?;
    let _ack: CodeGraphAckV1 = serde_json::from_value(answer.body)
        .map_err(|e| HiveError::Protocol(format!("code graph: {e}")))?;
    Ok(Published {
        nodes,
        edges,
        truncated,
        withheld_files,
        sealed: record.sealed,
        revision_hint,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::graph_index::{FileEntry, IndexEdge, SymbolEntry};

    fn index() -> ProjectIndex {
        let mut index = ProjectIndex::new("/w");
        for (path, lang) in [
            ("src/lib.rs", "rust"),
            ("src/hook.rs", "rust"),
            (".env", "dotenv"),
            ("../escape.rs", "rust"),
        ] {
            index.files.insert(
                path.into(),
                FileEntry {
                    path: path.into(),
                    hash: "h".into(),
                    language: lang.into(),
                    line_count: 1,
                    token_count: 1,
                    exports: Vec::new(),
                    summary: "fn secret() { token }".into(),
                },
            );
        }
        for (file, name, kind) in [
            ("src/hook.rs", "retry", "fn"),
            ("src/hook.rs", "Hook", "struct"),
            ("src/lib.rs", "run", "fn"),
            (".env", "API_KEY", "var"),
            ("src/hook.rs", "bad name", "fn"),
        ] {
            index.symbols.insert(
                format!("{file}::{name}"),
                SymbolEntry {
                    file: file.into(),
                    name: name.into(),
                    kind: kind.into(),
                    start_line: 1,
                    end_line: 2,
                    is_exported: true,
                },
            );
        }
        for (from, to, kind) in [
            ("src/lib.rs", "src/hook.rs", "import"),
            ("src/lib.rs", ".env", "import"),
            ("src/lib.rs", "src/hook.rs", "cochange"),
        ] {
            index.edges.push(IndexEdge {
                from: from.into(),
                to: to.into(),
                kind: kind.into(),
                weight: 1.0,
            });
        }
        index
    }

    #[test]
    fn the_graph_holds_structure_only_for_admitted_files() {
        let graph = export(&index(), |path| path != ".env");
        let ids: Vec<&str> = graph.nodes.iter().map(|n| n.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "code:src/hook.rs",
                "code:src/hook.rs#Hook",
                "code:src/hook.rs#retry",
                "code:src/lib.rs",
                "code:src/lib.rs#run",
            ]
        );
        assert_eq!(
            graph.withheld_files, 2,
            ".env withheld, ../escape.rs malformed"
        );
        let edges: Vec<(&str, &str, CodeEdgeKind)> = graph
            .edges
            .iter()
            .map(|e| (e.from.as_str(), e.to.as_str(), e.kind))
            .collect();
        assert_eq!(
            edges,
            vec![
                (
                    "code:src/hook.rs",
                    "code:src/hook.rs#Hook",
                    CodeEdgeKind::Contains
                ),
                (
                    "code:src/hook.rs",
                    "code:src/hook.rs#retry",
                    CodeEdgeKind::Contains
                ),
                ("code:src/lib.rs", "code:src/hook.rs", CodeEdgeKind::Imports),
                (
                    "code:src/lib.rs",
                    "code:src/lib.rs#run",
                    CodeEdgeKind::Contains
                ),
            ]
        );
        let wire = serde_json::to_string(&graph.nodes).unwrap()
            + &serde_json::to_string(&graph.edges).unwrap();
        for leak in ["secret", "token", "API_KEY", ".env", "\"h\""] {
            assert!(!wire.contains(leak), "{leak} must not leave: {wire}");
        }
        assert!(!graph.truncated);
        graph
            .clone()
            .into_v1(Some("a".repeat(40)))
            .validate()
            .expect("the export is a valid section 8 graph");
        assert_eq!(
            graph,
            export(&index(), |path| path != ".env"),
            "deterministic"
        );
    }

    #[test]
    fn a_large_checkout_is_cut_by_whole_files() {
        let mut index = ProjectIndex::new("/w");
        for i in 0..(MAX_CODE_GRAPH_NODES / 2 + 10) {
            let path = format!("src/f{i:05}.rs");
            index.files.insert(
                path.clone(),
                FileEntry {
                    path: path.clone(),
                    hash: String::new(),
                    language: "rust".into(),
                    line_count: 1,
                    token_count: 1,
                    exports: Vec::new(),
                    summary: String::new(),
                },
            );
            index.symbols.insert(
                format!("{path}::f"),
                SymbolEntry {
                    file: path,
                    name: "f".into(),
                    kind: "fn".into(),
                    start_line: 1,
                    end_line: 1,
                    is_exported: true,
                },
            );
        }
        let graph = export(&index, |_| true);
        assert!(graph.truncated);
        assert!(graph.nodes.len() <= MAX_CODE_GRAPH_NODES);
        assert_eq!(graph.nodes.len() % 2, 0, "every kept file keeps its symbol");
    }
}
