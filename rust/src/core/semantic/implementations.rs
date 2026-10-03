//! `implements` edges: implementation file → trait/interface file.
//!
//! Tree-sitter sees `impl Store for Pg` or `class Pg implements Store`, but
//! cannot bind `Store` to its declaration across modules; the semantic
//! backend's `textDocument/implementation` can. Changing a trait then shows
//! its implementors as impacted.

use std::collections::{BTreeSet, HashMap};
use std::time::{Duration, Instant};

use crate::core::call_graph::{CallGraphInputs, SymbolSpan};
use crate::core::config::SemanticMode;
use crate::core::property_graph::{CachedResolution, CodeGraph};
use crate::lsp::router::{LiveIdentity, StartPolicy};

use super::resolve::{Access, ResolveError, live_backend_identity, resolve_implementations};

const OP_IMPLEMENTATIONS: &str = "implementations";
const MAX_LIVE_QUERIES: usize = 100;
const LIVE_BUDGET: Duration = Duration::from_secs(10);
const PER_REQUEST: Duration = Duration::from_secs(5);

/// Signature kinds whose implementors a backend can enumerate.
const ABSTRACT_KINDS: &[&str] = &["trait", "interface"];

/// `impl_file` implements the trait/interface declared in `abstract_file`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ImplementsEdge {
    pub impl_file: String,
    pub abstract_file: String,
    pub backend: String,
}

/// Result of one `implements` pass.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImplementsPass {
    /// Sorted, deduplicated edges found this pass.
    pub edges: Vec<ImplementsEdge>,
    /// Declaring files whose every trait/interface got a definitive answer.
    /// Only their earlier `implements` edges may be pruned; files left
    /// unanswered (backend unavailable, budget, cold server) keep theirs.
    pub settled: BTreeSet<String>,
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

/// Revision of the whole indexed project. Implementors can appear or vanish
/// in any file without the trait's own file changing, so a cached answer is
/// valid only for the exact project revision it was computed on.
fn project_revision(file_hashes: &HashMap<String, String>) -> String {
    let mut entries: Vec<String> = file_hashes
        .iter()
        .map(|(f, h)| format!("{f}\t{h}"))
        .collect();
    entries.sort_unstable();
    crate::core::hasher::hash_str(&entries.join("\n"))
}

/// What a cached implementor list is worth now.
#[derive(Debug, PartialEq, Eq)]
enum ImplCache {
    Reuse(Vec<String>),
    Stale,
    /// The live backend is busy and cannot vouch for the entry: leave the
    /// declaration unsettled this run, without querying.
    Defer,
}

/// Cached implementors (newline-separated in `target_file`), if the entry is
/// still valid for this project revision and no *different* backend is live.
fn implementors_from_cache(
    hit: &CachedResolution,
    revision: &str,
    live: &LiveIdentity,
) -> ImplCache {
    if hit.outcome != "resolved" || hit.context != revision {
        return ImplCache::Stale;
    }
    match live {
        LiveIdentity::Known(id) if *id != hit.backend => ImplCache::Stale,
        LiveIdentity::Busy => ImplCache::Defer,
        LiveIdentity::Known(_) | LiveIdentity::NotRunning => ImplCache::Reuse(
            hit.target_file
                .as_deref()
                .unwrap_or("")
                .lines()
                .map(str::to_string)
                .collect(),
        ),
    }
}

/// Resolves `implements` edges for every trait/interface in the index, in
/// sorted `(file, line, name)` order under a per-run budget.
pub fn resolve_implements_edges(
    graph: &CodeGraph,
    project_root: &str,
    inputs: &CallGraphInputs,
    file_hashes: &HashMap<String, String>,
    mode: SemanticMode,
) -> ImplementsPass {
    let mut abstracts: Vec<&SymbolSpan> = inputs
        .symbols
        .iter()
        .filter(|s| ABSTRACT_KINDS.contains(&s.kind.as_str()))
        .filter(|s| {
            std::path::Path::new(&s.file)
                .extension()
                .and_then(|e| e.to_str())
                .and_then(crate::lsp::config::language_for_extension)
                .is_some()
        })
        .collect();
    // Every indexed file starts settled — including one that no longer
    // declares any trait/interface, so its stale incoming edges are withdrawn.
    // Declarations that get no definitive answer below unsettle their file.
    let indexed: BTreeSet<String> = inputs.file_paths.iter().cloned().collect();
    if mode == SemanticMode::Off {
        // A deliberate choice, not missing evidence: everything is settled.
        return ImplementsPass {
            edges: Vec::new(),
            settled: indexed,
        };
    }
    abstracts
        .sort_by(|a, b| (&a.file, a.start_line, &a.name).cmp(&(&b.file, b.start_line, &b.name)));

    let access = Access {
        policy: if mode == SemanticMode::Eager {
            StartPolicy::Lazy
        } else {
            StartPolicy::ReuseOnly
        },
        wait: false,
        timeout: Some(PER_REQUEST),
        deadline: None,
    };
    let revision = project_revision(file_hashes);
    let started = Instant::now();
    let mut live = 0usize;
    let mut live_identity: HashMap<String, LiveIdentity> = HashMap::new();
    let mut unavailable: BTreeSet<String> = BTreeSet::new();
    let mut edges: BTreeSet<ImplementsEdge> = BTreeSet::new();
    let mut unsettled: BTreeSet<String> = BTreeSet::new();

    for sym in abstracts {
        let (Some(hash), Some(pos)) = (
            file_hashes.get(&sym.file),
            declaration_position(project_root, sym),
        ) else {
            unsettled.insert(sym.file.clone());
            continue;
        };
        let site = (sym.file.as_str(), pos.0, pos.1, OP_IMPLEMENTATIONS);
        let ext = std::path::Path::new(&sym.file)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_string();

        let mut cache = ImplCache::Stale;
        let mut cached_backend = String::new();
        if let Ok(Some(hit)) = graph.semantic_lookup(site, hash) {
            if live_identity
                .get(&ext)
                .is_none_or(|l| *l == LiveIdentity::Busy)
            {
                live_identity.insert(ext.clone(), live_backend_identity(project_root, &sym.file));
            }
            let live_id = live_identity.get(&ext).unwrap_or(&LiveIdentity::NotRunning);
            cache = implementors_from_cache(&hit, &revision, live_id);
            cached_backend = hit.backend;
        }
        let found = match cache {
            ImplCache::Reuse(files) => Some((cached_backend, files)),
            ImplCache::Defer => None,
            ImplCache::Stale => {
                let remaining = LIVE_BUDGET.saturating_sub(started.elapsed());
                if unavailable.contains(&ext) || live >= MAX_LIVE_QUERIES || remaining.is_zero() {
                    None
                } else {
                    live += 1;
                    let access = Access {
                        deadline: Some(started + LIVE_BUDGET),
                        ..access
                    };
                    match resolve_implementations(project_root, &sym.file, pos, access, inputs) {
                        // An empty answer is not definitive (a cold server
                        // reports none while indexing): neither cached nor
                        // used to settle the file.
                        Ok((_, files)) if files.is_empty() => None,
                        Ok((backend, files)) => {
                            let _ = graph.semantic_store(
                                site,
                                hash,
                                &CachedResolution {
                                    backend: backend.clone(),
                                    outcome: "resolved".into(),
                                    target_file: Some(files.join("\n")),
                                    target_line: None,
                                    target_symbol: Some(sym.name.clone()),
                                    context: revision.clone(),
                                },
                            );
                            live_identity.insert(ext, LiveIdentity::Known(backend.clone()));
                            Some((backend, files))
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

        match found {
            Some((backend, files)) => {
                for impl_file in files {
                    edges.insert(ImplementsEdge {
                        impl_file,
                        abstract_file: sym.file.clone(),
                        backend: backend.clone(),
                    });
                }
            }
            None => {
                unsettled.insert(sym.file.clone());
            }
        }
    }
    ImplementsPass {
        edges: edges.into_iter().collect(),
        settled: indexed.difference(&unsettled).cloned().collect(),
    }
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

    /// Implementors can change anywhere: a cached answer is bound to the
    /// project revision and to the backend that produced it.
    #[test]
    fn cached_implementors_are_valid_only_for_their_revision_and_backend() {
        let hit = CachedResolution {
            backend: "lsp:ra@1".into(),
            outcome: "resolved".into(),
            target_file: Some("mem.rs\npg.rs".into()),
            target_line: None,
            target_symbol: Some("Store".into()),
            context: "rev-1".into(),
        };
        let files = ImplCache::Reuse(vec!["mem.rs".to_string(), "pg.rs".to_string()]);
        let known = |id: &str| LiveIdentity::Known(id.into());
        let none = LiveIdentity::NotRunning;
        assert_eq!(implementors_from_cache(&hit, "rev-1", &none), files);
        assert_eq!(
            implementors_from_cache(&hit, "rev-1", &known("lsp:ra@1")),
            files
        );
        assert_eq!(
            implementors_from_cache(&hit, "rev-2", &none),
            ImplCache::Stale,
            "any file changed"
        );
        assert_eq!(
            implementors_from_cache(&hit, "rev-1", &known("lsp:ra@2")),
            ImplCache::Stale
        );
        assert_eq!(
            implementors_from_cache(&hit, "rev-1", &LiveIdentity::Busy),
            ImplCache::Defer
        );

        let a = HashMap::from([("a.rs".to_string(), "1".to_string())]);
        let b = HashMap::from([("a.rs".to_string(), "2".to_string())]);
        assert_ne!(project_revision(&a), project_revision(&b));
    }
}
