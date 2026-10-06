// SPDX-License-Identifier: Apache-2.0
//! Symbol-level relations from a semantic backend, lifted to file edges:
//!
//! - `implements`: implementation file → trait/interface file
//!   (`textDocument/implementation` on the trait/interface),
//! - `extends`: subtype file → supertype file (`typeHierarchy/supertypes`),
//! - `references`: referencing file → declaring file (`textDocument/references`).
//!
//! Tree-sitter sees `impl Store for Pg`, `class Pg extends Base` or a use of
//! `Store`, but cannot bind the name to its declaration across modules; the
//! backend can. `implements` and `extends` run in the background pass;
//! `references` costs one request per symbol and runs on demand for the one
//! file `ctx_impact` analyses.

use std::collections::{BTreeSet, HashMap};
use std::time::{Duration, Instant};

use crate::core::call_graph::{CallGraphInputs, SymbolSpan};
use crate::core::config::SemanticMode;
use crate::core::property_graph::{CachedResolution, CodeGraph, EdgeKind};
use crate::lsp::router::{LiveIdentity, StartPolicy};

use super::resolve::{
    Access, RelatedFiles, ResolveError, live_backend_identity, resolve_implementations,
    resolve_references, resolve_supertypes,
};

/// A symbol-level relation a backend can enumerate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Relation {
    Implements,
    Extends,
    References,
}

impl Relation {
    /// Cache operation key (`semantic_resolutions.op`).
    fn op(self) -> &'static str {
        match self {
            Self::Implements => "implementations",
            Self::Extends => "supertypes",
            Self::References => "references",
        }
    }

    /// Symbol kinds the relation is asked for.
    fn kinds(self) -> &'static [&'static str] {
        match self {
            Self::Implements => &["trait", "interface"],
            Self::Extends => &["class", "struct", "interface", "trait"],
            Self::References => &[
                "fn",
                "method",
                "class",
                "struct",
                "enum",
                "interface",
                "trait",
                "type",
                "const",
                "macro",
            ],
        }
    }

    /// Graph edge kind the relation is stored as.
    pub fn edge_kind(self) -> EdgeKind {
        match self {
            Self::Implements => EdgeKind::Implements,
            Self::Extends => EdgeKind::Extends,
            Self::References => EdgeKind::References,
        }
    }

    /// `true`: edges point from the declaring file to the found files
    /// (subtype → supertype); otherwise from the found files to it.
    fn outgoing(self) -> bool {
        self == Self::Extends
    }
}

/// How much one pass may ask a backend.
#[derive(Debug, Clone, Copy)]
pub struct RelationBudget {
    pub max_live_queries: usize,
    pub wall: Duration,
    pub per_request: Duration,
}

impl RelationBudget {
    /// Background graph pass.
    pub const BACKGROUND: Self = Self {
        max_live_queries: 100,
        wall: Duration::from_secs(10),
        per_request: Duration::from_secs(5),
    };
    /// One file, while a tool call waits (`ctx_impact`).
    pub const INTERACTIVE: Self = Self {
        max_live_queries: 40,
        wall: Duration::from_secs(8),
        per_request: Duration::from_secs(3),
    };
}

/// A file-level relation edge, oriented as stored in the graph.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct RelationEdge {
    pub from: String,
    pub to: String,
    pub backend: String,
}

/// Result of one relation pass.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RelationPass {
    /// Sorted, deduplicated edges found this pass.
    pub edges: Vec<RelationEdge>,
    /// Declaring files whose every queried symbol got a definitive answer.
    /// Only their earlier edges may be pruned; files left unanswered
    /// (backend unavailable, budget, cold server) keep theirs.
    pub settled: BTreeSet<String>,
    /// Symbols asked live this pass, and answered from the cache.
    pub live_queries: usize,
    pub cache_hits: usize,
    /// Symbols that got any answer (live or cached, complete or not). Zero
    /// means the pass learned nothing — e.g. a file declaring no symbol of
    /// the relation's kinds is settled without a single question.
    pub answered: usize,
}

impl RelationPass {
    /// Whether this pass may withdraw the earlier edge `from → to`: only
    /// when the declaring file was settled.
    pub fn may_prune(&self, relation: Relation, from: &str, to: &str) -> bool {
        let declaring = if relation.outgoing() { from } else { to };
        self.settled.contains(declaring)
    }
}

/// 0-based byte column of `name` as a whole identifier in `line`.
fn name_column(line: &str, name: &str) -> Option<usize> {
    let is_ident = |c: char| c.is_alphanumeric() || c == '_';
    line.match_indices(name).map(|(i, _)| i).find(|&i| {
        let before = line[..i].chars().next_back();
        let after = line[i + name.len()..].chars().next();
        !before.is_some_and(is_ident) && !after.is_some_and(is_ident)
    })
}

fn declaration_position(project_root: &str, sym: &SymbolSpan) -> Option<(usize, usize)> {
    let content =
        std::fs::read_to_string(std::path::Path::new(project_root).join(&sym.file)).ok()?;
    let line = content.lines().nth(sym.start_line.checked_sub(1)?)?;
    Some((sym.start_line, name_column(line, &sym.name)?))
}

/// Revision of the whole indexed project. Related files can appear or vanish
/// anywhere without the declaring file changing, so a cached answer is valid
/// only for the exact project revision it was computed on.
fn project_revision(file_hashes: &HashMap<String, String>) -> String {
    let mut entries: Vec<String> = file_hashes
        .iter()
        .map(|(f, h)| format!("{f}\t{h}"))
        .collect();
    entries.sort_unstable();
    crate::core::hasher::hash_str(&entries.join("\n"))
}

/// What a cached related-file list is worth now.
#[derive(Debug, PartialEq, Eq)]
enum RelCache {
    Reuse(Vec<String>),
    Stale,
    /// The live backend is busy and cannot vouch for the entry: leave the
    /// symbol unsettled this run, without querying.
    Defer,
}

/// Cached related files (newline-separated in `target_file`), if the entry is
/// still valid for this project revision and no *different* backend is live.
fn related_from_cache(hit: &CachedResolution, revision: &str, live: &LiveIdentity) -> RelCache {
    if hit.outcome != "resolved" || hit.context != revision {
        return RelCache::Stale;
    }
    match live {
        LiveIdentity::Known(id) if *id != hit.backend => RelCache::Stale,
        LiveIdentity::Busy => RelCache::Defer,
        LiveIdentity::Known(_) | LiveIdentity::NotRunning => RelCache::Reuse(
            hit.target_file
                .as_deref()
                .unwrap_or("")
                .lines()
                .map(str::to_string)
                .collect(),
        ),
    }
}

/// One live question. A non-definitive answer (capped, or nothing from a
/// server still indexing) still contributes the files it names — each is a
/// verified relation — but is neither cached nor allowed to settle the file.
fn query(
    relation: Relation,
    project_root: &str,
    file: &str,
    pos: (usize, usize),
    access: Access,
    inputs: &CallGraphInputs,
) -> Result<(String, RelatedFiles), ResolveError> {
    match relation {
        Relation::Implements => resolve_implementations(project_root, file, pos, access, inputs),
        Relation::Extends => resolve_supertypes(project_root, file, pos, access, inputs),
        Relation::References => resolve_references(project_root, file, pos, access, inputs),
    }
}

/// Resolves `relation` for the indexed symbols of the relation's kinds — all
/// files, or only `only_file` — in sorted `(file, line, name)` order under
/// `budget`.
#[allow(clippy::too_many_arguments)] // one pass = graph, project, mode, scope, budget
pub fn resolve_relation_edges(
    graph: &CodeGraph,
    project_root: &str,
    inputs: &CallGraphInputs,
    file_hashes: &HashMap<String, String>,
    mode: SemanticMode,
    relation: Relation,
    only_file: Option<&str>,
    budget: RelationBudget,
) -> RelationPass {
    let mut symbols: Vec<&SymbolSpan> = inputs
        .symbols
        .iter()
        .filter(|s| relation.kinds().contains(&s.kind.as_str()))
        .filter(|s| only_file.is_none_or(|f| s.file == f))
        .filter(|s| {
            std::path::Path::new(&s.file)
                .extension()
                .and_then(|e| e.to_str())
                .and_then(crate::lsp::config::language_for_extension)
                .is_some()
        })
        .collect();
    // Every file in scope starts settled — including one that no longer
    // declares any such symbol, so its stale edges are withdrawn. Symbols
    // that get no definitive answer below unsettle their file.
    let scope: BTreeSet<String> = match only_file {
        Some(f) => inputs
            .file_paths
            .iter()
            .filter(|p| *p == f)
            .cloned()
            .collect(),
        None => inputs.file_paths.iter().cloned().collect(),
    };
    if mode == SemanticMode::Off {
        // A deliberate choice, not missing evidence: everything is settled.
        return RelationPass {
            settled: scope,
            ..RelationPass::default()
        };
    }
    symbols.sort_by(|a, b| (&a.file, a.start_line, &a.name).cmp(&(&b.file, b.start_line, &b.name)));

    let access = Access {
        policy: if mode == SemanticMode::Eager {
            StartPolicy::Lazy
        } else {
            StartPolicy::ReuseOnly
        },
        wait: false,
        timeout: Some(budget.per_request),
        deadline: None,
    };
    let revision = project_revision(file_hashes);
    let started = Instant::now();
    let mut pass = RelationPass::default();
    let mut live_identity: HashMap<String, LiveIdentity> = HashMap::new();
    let mut unavailable: BTreeSet<String> = BTreeSet::new();
    let mut edges: BTreeSet<RelationEdge> = BTreeSet::new();
    let mut unsettled: BTreeSet<String> = BTreeSet::new();

    for sym in symbols {
        let (Some(hash), Some(pos)) = (
            file_hashes.get(&sym.file),
            declaration_position(project_root, sym),
        ) else {
            unsettled.insert(sym.file.clone());
            continue;
        };
        let site = (sym.file.as_str(), pos.0, pos.1, relation.op());
        let ext = std::path::Path::new(&sym.file)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_string();

        let mut cache = RelCache::Stale;
        let mut cached_backend = String::new();
        if let Ok(Some(hit)) = graph.semantic_lookup(site, hash) {
            if live_identity
                .get(&ext)
                .is_none_or(|l| *l == LiveIdentity::Busy)
            {
                live_identity.insert(ext.clone(), live_backend_identity(project_root, &sym.file));
            }
            let live_id = live_identity.get(&ext).unwrap_or(&LiveIdentity::NotRunning);
            cache = related_from_cache(&hit, &revision, live_id);
            cached_backend = hit.backend;
        }
        // `(backend, files, definitive)`; `None` = no answer at all.
        let found = match cache {
            RelCache::Reuse(files) => {
                pass.cache_hits += 1;
                Some((cached_backend, files, true))
            }
            RelCache::Defer => None,
            RelCache::Stale => {
                let remaining = budget.wall.saturating_sub(started.elapsed());
                if unavailable.contains(&ext)
                    || pass.live_queries >= budget.max_live_queries
                    || remaining.is_zero()
                {
                    None
                } else {
                    pass.live_queries += 1;
                    let access = Access {
                        deadline: Some(started + budget.wall),
                        ..access
                    };
                    match query(relation, project_root, &sym.file, pos, access, inputs) {
                        Ok((backend, related)) => {
                            if related.definitive {
                                let _ = graph.semantic_store(
                                    site,
                                    hash,
                                    &CachedResolution {
                                        backend: backend.clone(),
                                        outcome: "resolved".into(),
                                        target_file: Some(related.files.join("\n")),
                                        target_line: None,
                                        target_symbol: Some(sym.name.clone()),
                                        context: revision.clone(),
                                    },
                                );
                            }
                            live_identity.insert(ext, LiveIdentity::Known(backend.clone()));
                            Some((backend, related.files, related.definitive))
                        }
                        Err(ResolveError::Unavailable(_)) => {
                            unavailable.insert(ext);
                            None
                        }
                        Err(ResolveError::Site(_)) => None,
                    }
                }
            }
        };

        let Some((backend, files, definitive)) = found else {
            unsettled.insert(sym.file.clone());
            continue;
        };
        pass.answered += 1;
        if !definitive {
            unsettled.insert(sym.file.clone());
        }
        // Every named file is a verified relation, even from a partial answer.
        for other in files.into_iter().filter(|f| *f != sym.file) {
            let (from, to) = if relation.outgoing() {
                (sym.file.clone(), other)
            } else {
                (other, sym.file.clone())
            };
            edges.insert(RelationEdge {
                from,
                to,
                backend: backend.clone(),
            });
        }
    }
    pass.edges = edges.into_iter().collect();
    pass.settled = scope.difference(&unsettled).cloned().collect();
    tracing::debug!(
        target: "lean_ctx::semantic",
        "{}: {} live, {} cached, {} edges, {}/{} files settled in {} ms",
        relation.op(),
        pass.live_queries,
        pass.cache_hits,
        pass.edges.len(),
        pass.settled.len(),
        scope.len(),
        started.elapsed().as_millis()
    );
    pass
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declaration_name_is_found_as_a_whole_identifier() {
        assert_eq!(
            name_column("pub trait Store: StoreBase {", "Store"),
            Some(10)
        );
        assert_eq!(
            name_column("interface IStore extends Store {", "Store"),
            Some(25)
        );
        assert_eq!(name_column("trait Storage {", "Store"), None);
    }

    /// Related files can change anywhere: a cached answer is bound to the
    /// project revision and to the backend that produced it.
    #[test]
    fn cached_relations_are_valid_only_for_their_revision_and_backend() {
        let hit = CachedResolution {
            backend: "lsp:ra@1".into(),
            outcome: "resolved".into(),
            target_file: Some("mem.rs\npg.rs".into()),
            target_line: None,
            target_symbol: Some("Store".into()),
            context: "rev-1".into(),
        };
        let files = RelCache::Reuse(vec!["mem.rs".to_string(), "pg.rs".to_string()]);
        let known = |id: &str| LiveIdentity::Known(id.into());
        let none = LiveIdentity::NotRunning;
        assert_eq!(related_from_cache(&hit, "rev-1", &none), files);
        assert_eq!(related_from_cache(&hit, "rev-1", &known("lsp:ra@1")), files);
        assert_eq!(
            related_from_cache(&hit, "rev-2", &none),
            RelCache::Stale,
            "any file changed"
        );
        assert_eq!(
            related_from_cache(&hit, "rev-1", &known("lsp:ra@2")),
            RelCache::Stale
        );
        assert_eq!(
            related_from_cache(&hit, "rev-1", &LiveIdentity::Busy),
            RelCache::Defer
        );

        let a = HashMap::from([("a.rs".to_string(), "1".to_string())]);
        let b = HashMap::from([("a.rs".to_string(), "2".to_string())]);
        assert_ne!(project_revision(&a), project_revision(&b));
    }

    /// Only the declaring side of an edge decides whether it may be pruned:
    /// the trait/interface for `implements`/`references`, the subtype for
    /// `extends`.
    #[test]
    fn pruning_is_decided_by_the_declaring_file() {
        let pass = RelationPass {
            settled: BTreeSet::from(["decl.rs".to_string()]),
            ..RelationPass::default()
        };
        assert!(pass.may_prune(Relation::Implements, "impl.rs", "decl.rs"));
        assert!(pass.may_prune(Relation::References, "user.rs", "decl.rs"));
        assert!(!pass.may_prune(Relation::References, "decl.rs", "other.rs"));
        assert!(pass.may_prune(Relation::Extends, "decl.rs", "base.rs"));
        assert!(!pass.may_prune(Relation::Extends, "sub.rs", "decl.rs"));
    }
}
