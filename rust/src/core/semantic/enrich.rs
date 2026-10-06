// SPDX-License-Identifier: Apache-2.0
//! Semantic escalation for call edges structure cannot settle.
//!
//! Only edges whose callee tree-sitter could not bind to the caller's scope
//! are escalated: an ambiguous name (several project definitions), a name that
//! is merely unique project-wide, or a path call (`db::save`). Scope-bound
//! edges (same file, unique import) and unknown names (no project definition)
//! are never queried.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::time::{Duration, Instant};

use crate::core::call_graph::{
    CallEdge, CallGraphInputs, ScopeMatch, StructuralTarget, callee_segment,
};
use crate::core::config::SemanticMode;
use crate::core::property_graph::{CachedResolution, CodeGraph};
use crate::lsp::router::{LiveIdentity, StartPolicy};

use super::resolve::{Access, Resolution, ResolveError, live_backend_identity, resolve_definition};

const OP_DEFINITION: &str = "definition";

/// Limits for one escalation run. Cached answers are free; only live
/// backend queries count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EscalationBudget {
    pub max_live_queries: usize,
    /// Wall-clock budget for all live queries of the run.
    pub wall: Duration,
    /// Cap per request, so one slow answer cannot hold the backend long.
    pub per_request: Duration,
}

impl EscalationBudget {
    /// Background enrichment (after a graph build, or while a backend is in
    /// use). Measured ~65 ms per warm rust-analyzer lookup on a 2.4k-file
    /// crate, so one pass decides ~1000 file-pair groups in about a minute,
    /// off every interactive path.
    pub const BACKGROUND: Self = Self {
        max_live_queries: 1000,
        wall: Duration::from_mins(1),
        per_request: Duration::from_secs(5),
    };
    /// An interactive tool call (`ctx_callgraph`): must answer promptly.
    pub const INTERACTIVE: Self = Self {
        max_live_queries: 20,
        wall: Duration::from_secs(4),
        per_request: Duration::from_secs(2),
    };
}

/// What the semantic backend says about one call edge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SemanticVerdict {
    /// Verified callee file (and backend identity).
    Verified { file: String, backend: String },
    /// The backend located the callee's definition outside the project: a
    /// structural name-match guess for this edge is wrong. ("No definition
    /// found" is *not* this — a server that is still indexing answers that.)
    NotInProject,
}

/// Counters for status reporting and tests.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EscalationStats {
    pub candidates: usize,
    pub cache_hits: usize,
    pub live_queries: usize,
    pub verified: usize,
    pub not_in_project: usize,
    /// Candidates left unresolved (no backend, budget, ambiguity, errors).
    pub unresolved: usize,
}

/// Result of one escalation run, index-aligned with the input edges.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Escalation {
    /// `None` = no semantic information; keep the structural answer.
    pub verdicts: Vec<Option<SemanticVerdict>>,
    /// `true` = a candidate that got no *definitive* answer this run (backend
    /// unavailable or busy, budget exhausted, timeout, "no result"). Absence
    /// of an answer is not evidence: callers must not treat these sites'
    /// earlier semantic edges as disproved.
    pub unsettled: Vec<bool>,
    pub stats: EscalationStats,
}

fn needs_escalation(edge: &CallEdge, structural: &StructuralTarget) -> bool {
    let uncertain = matches!(
        structural,
        StructuralTarget::Ambiguous
            | StructuralTarget::Resolved {
                via: ScopeMatch::UniqueInProject,
                ..
            }
    );
    uncertain
        && edge.callee_pos.is_some()
        && std::path::Path::new(&edge.caller_file)
            .extension()
            .and_then(|e| e.to_str())
            .and_then(crate::lsp::config::language_for_extension)
            .is_some()
}

/// Fingerprint of the files defining `name` *and their contents* — the part
/// of the project (besides the caller itself) a definition lookup depends on.
/// Adding, moving or removing a same-named definition, or any edit inside a
/// defining file (a new overload, a changed export), changes it.
fn definitions_fingerprint(
    defs: &BTreeMap<&str, BTreeSet<&str>>,
    name: &str,
    file_hashes: &HashMap<String, String>,
    dependency_revision: &str,
) -> String {
    let mut entries: Vec<String> = defs
        .get(name)
        .into_iter()
        .flatten()
        .map(|f| format!("{f}\t{}", file_hashes.get(*f).map_or("", String::as_str)))
        .collect();
    entries.push(dependency_revision.to_string());
    crate::core::hasher::hash_str(&entries.join("\n"))
}

/// Manifests and lockfiles whose change can re-route a call between the
/// project and a dependency (a glob import gaining a symbol) without any
/// indexed source file changing.
const DEPENDENCY_FILES: &[&str] = &[
    "Cargo.toml",
    "Cargo.lock",
    "package.json",
    "package-lock.json",
    "pnpm-lock.yaml",
    "yarn.lock",
    "go.mod",
    "go.sum",
    "pyproject.toml",
    "poetry.lock",
    "uv.lock",
    "requirements.txt",
];

/// Revision of the project's dependency manifests at its root. Nested
/// workspace manifests are covered through their root lockfile.
fn dependency_revision(project_root: &str) -> String {
    let entries: Vec<String> = DEPENDENCY_FILES
        .iter()
        .filter_map(|name| {
            let content = std::fs::read(std::path::Path::new(project_root).join(name)).ok()?;
            Some(format!("{name}\t{}", blake3::hash(&content).to_hex()))
        })
        .collect();
    crate::core::hasher::hash_str(&entries.join("\n"))
}

/// What a cached answer is worth now.
#[derive(Debug, PartialEq, Eq)]
enum CacheUse {
    /// Still valid; `None` = a definitive "ambiguous" with nothing to add.
    Reuse(Option<SemanticVerdict>),
    /// Must be re-resolved.
    Stale,
    /// Cannot be checked right now (the live backend is busy): neither reuse
    /// it nor query — the site stays unsettled for this run.
    Defer,
}

/// Validates a cached answer: same definition context, no *different* live
/// backend, and — for a resolved target — the recorded definition line still
/// lies inside a symbol of the callee's name.
fn verdict_from_cache(
    hit: &CachedResolution,
    inputs: &CallGraphInputs,
    callee: &str,
    context: &str,
    live: &LiveIdentity,
) -> CacheUse {
    if hit.context != context {
        return CacheUse::Stale;
    }
    match live {
        LiveIdentity::Known(id) if *id != hit.backend => return CacheUse::Stale,
        LiveIdentity::Busy => return CacheUse::Defer,
        LiveIdentity::Known(_) | LiveIdentity::NotRunning => {}
    }
    match hit.outcome.as_str() {
        "resolved" => {
            let (Some(file), Some(line)) = (hit.target_file.clone(), hit.target_line) else {
                return CacheUse::Stale;
            };
            let name = callee_segment(callee);
            let still_there = inputs.symbols.iter().any(|s| {
                s.file == file && s.name == name && s.start_line <= line && line <= s.end_line
            });
            if still_there {
                CacheUse::Reuse(Some(SemanticVerdict::Verified {
                    file,
                    backend: hit.backend.clone(),
                }))
            } else {
                CacheUse::Stale
            }
        }
        "external" => CacheUse::Reuse(Some(SemanticVerdict::NotInProject)),
        "ambiguous" => CacheUse::Reuse(None),
        _ => CacheUse::Stale,
    }
}

/// Cache record for a definitive answer. `NoResult` is not definitive — a
/// cold server answers it while indexing — so it is never cached (`None`).
fn to_cache(resolution: &Resolution, backend: &str, context: &str) -> Option<CachedResolution> {
    let (outcome, target_file, target_line, target_symbol) = match resolution {
        Resolution::Resolved { file, line, symbol } => {
            ("resolved", Some(file.clone()), Some(*line), symbol.clone())
        }
        Resolution::External => ("external", None, None, None),
        Resolution::Ambiguous => ("ambiguous", None, None, None),
        Resolution::NoResult => return None,
    };
    Some(CachedResolution {
        backend: backend.to_string(),
        outcome: outcome.to_string(),
        target_file,
        target_line,
        target_symbol,
        context: context.to_string(),
    })
}

fn ext_of(file: &str) -> String {
    std::path::Path::new(file)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_string()
}

/// Order in which uncertain sites are asked: a budget-limited run must decide
/// as many file-level edges as possible, so it takes one site of every
/// `(caller file, structural target)` group before a second site of any —
/// guessed edges (a name match that may be false) before ambiguous calls
/// (an edge that is missing). Deterministic: ties break by file and position.
fn escalation_order(
    edges: &[CallEdge],
    structural: &[StructuralTarget],
    candidates: Vec<usize>,
) -> Vec<usize> {
    let group = |i: usize| match &structural[i] {
        StructuralTarget::Resolved { file, .. } => (0u8, file.as_str()),
        _ => (1u8, edges[i].callee_name.as_str()),
    };
    let mut by_site = candidates;
    by_site.sort_by(|&a, &b| {
        (&edges[a].caller_file, edges[a].callee_pos)
            .cmp(&(&edges[b].caller_file, edges[b].callee_pos))
    });
    let mut seen: HashMap<(&str, (u8, &str)), usize> = HashMap::new();
    let mut ranked: Vec<(usize, u8, usize)> = by_site
        .iter()
        .map(|&i| {
            let (class, key) = group(i);
            let rank = seen
                .entry((edges[i].caller_file.as_str(), (class, key)))
                .or_insert(0);
            *rank += 1;
            (*rank, class, i)
        })
        .collect();
    // Stable: equal (rank, class) keep the file/position order.
    ranked.sort_by_key(|&(rank, class, _)| (rank, class));
    ranked.into_iter().map(|(_, _, i)| i).collect()
}

/// Escalates every uncertain call edge, in `escalation_order` so a
/// budget-limited run always covers the same, most decisive subset.
pub fn escalate_calls(
    graph: &CodeGraph,
    project_root: &str,
    inputs: &CallGraphInputs,
    edges: &[CallEdge],
    structural: &[StructuralTarget],
    file_hashes: &HashMap<String, String>,
    mode: SemanticMode,
    budget: EscalationBudget,
) -> Escalation {
    let mut out = Escalation {
        verdicts: vec![None; edges.len()],
        unsettled: vec![false; edges.len()],
        stats: EscalationStats::default(),
    };
    let candidates: Vec<usize> = (0..edges.len())
        .filter(|&i| needs_escalation(&edges[i], &structural[i]))
        .collect();
    out.stats.candidates = candidates.len();
    if mode == SemanticMode::Off {
        // Off is a deliberate choice, not missing evidence: nothing pending.
        return out;
    }
    let order = escalation_order(edges, structural, candidates);

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
    let mut defs: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for s in &inputs.symbols {
        defs.entry(s.name.as_str())
            .or_default()
            .insert(s.file.as_str());
    }

    // Stale cache rows are pruned by the background pass, never here: an
    // interactive call must not queue for the write lock, and lookups are
    // keyed by the caller's content hash anyway.
    let deps = dependency_revision(project_root);
    let started = Instant::now();
    // Per language: identity of the live backend (a cheap registry peek,
    // remembered unless the backend was busy) and whether the backend is
    // unavailable for the rest of this run.
    let mut live_identity: HashMap<String, LiveIdentity> = HashMap::new();
    let mut unavailable: BTreeSet<String> = BTreeSet::new();

    for i in order {
        let edge = &edges[i];
        out.unsettled[i] = true;
        let (Some((line, col)), Some(hash)) = (edge.callee_pos, file_hashes.get(&edge.caller_file))
        else {
            continue;
        };
        let site = (edge.caller_file.as_str(), line, col, OP_DEFINITION);
        let context =
            definitions_fingerprint(&defs, callee_segment(&edge.callee_name), file_hashes, &deps);
        let ext = ext_of(&edge.caller_file);

        if let Ok(Some(hit)) = graph.semantic_lookup(site, hash) {
            if live_identity
                .get(&ext)
                .is_none_or(|l| *l == LiveIdentity::Busy)
            {
                live_identity.insert(
                    ext.clone(),
                    live_backend_identity(project_root, &edge.caller_file),
                );
            }
            let live = live_identity.get(&ext).unwrap_or(&LiveIdentity::NotRunning);
            match verdict_from_cache(&hit, inputs, &edge.callee_name, &context, live) {
                CacheUse::Reuse(v) => {
                    out.stats.cache_hits += 1;
                    out.verdicts[i] = v;
                    out.unsettled[i] = false;
                    continue;
                }
                CacheUse::Defer => continue,
                CacheUse::Stale => {}
            }
        }

        let remaining = budget.wall.saturating_sub(started.elapsed());
        if unavailable.contains(&ext)
            || out.stats.live_queries >= budget.max_live_queries
            || remaining.is_zero()
        {
            continue;
        }

        out.stats.live_queries += 1;
        let access = Access {
            deadline: Some(started + budget.wall),
            ..access
        };
        match resolve_definition(project_root, &edge.caller_file, (line, col), access, inputs) {
            Ok(answer) => {
                if let Some(record) = to_cache(&answer.resolution, &answer.backend, &context) {
                    let _ = graph.semantic_store(site, hash, &record);
                }
                live_identity.insert(ext, LiveIdentity::Known(answer.backend.clone()));
                (out.verdicts[i], out.unsettled[i]) = match answer.resolution {
                    Resolution::Resolved { file, .. } => (
                        Some(SemanticVerdict::Verified {
                            file,
                            backend: answer.backend,
                        }),
                        false,
                    ),
                    Resolution::External => (Some(SemanticVerdict::NotInProject), false),
                    Resolution::Ambiguous => (None, false),
                    Resolution::NoResult => (None, true),
                };
            }
            // Never cached: retried next run. An unavailable or busy backend
            // is the same for every remaining site of this language.
            Err(ResolveError::Unavailable(_)) => {
                unavailable.insert(ext);
            }
            Err(ResolveError::Site(_)) => {}
        }
    }

    for v in out.verdicts.iter().flatten() {
        match v {
            SemanticVerdict::Verified { .. } => out.stats.verified += 1,
            SemanticVerdict::NotInProject => out.stats.not_in_project += 1,
        }
    }
    out.stats.unresolved = out.stats.candidates - out.stats.verified - out.stats.not_in_project;
    tracing::debug!(
        target: "lean_ctx::semantic",
        "escalate: {} candidates, {} cached, {} live, {} verified, {} external in {} ms",
        out.stats.candidates,
        out.stats.cache_hits,
        out.stats.live_queries,
        out.stats.verified,
        out.stats.not_in_project,
        started.elapsed().as_millis()
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::call_graph::SymbolSpan;

    fn edge(file: &str, callee: &str, pos: Option<(usize, usize)>) -> CallEdge {
        CallEdge {
            caller_file: file.into(),
            caller_symbol: "f".into(),
            caller_line: 1,
            callee_name: callee.into(),
            callee_pos: pos,
            ..Default::default()
        }
    }

    #[test]
    fn only_scope_unbound_edges_with_a_position_are_escalated() {
        let resolved = |via| StructuralTarget::Resolved {
            file: "b.rs".into(),
            via,
        };
        let e = edge("a.rs", "save", Some((1, 4)));
        assert!(needs_escalation(&e, &StructuralTarget::Ambiguous));
        assert!(needs_escalation(&e, &resolved(ScopeMatch::UniqueInProject)));
        assert!(!needs_escalation(&e, &resolved(ScopeMatch::SameFile)));
        assert!(!needs_escalation(&e, &resolved(ScopeMatch::UniqueImport)));
        assert!(!needs_escalation(&e, &StructuralTarget::Unknown));
        assert!(!needs_escalation(
            &edge("a.rs", "save", None),
            &StructuralTarget::Ambiguous
        ));
        assert!(
            !needs_escalation(
                &edge("a.md", "save", Some((1, 0))),
                &StructuralTarget::Ambiguous
            ),
            "no semantic backend for the language"
        );
    }

    #[test]
    fn cached_answers_are_reused_only_while_their_context_holds() {
        let inputs = CallGraphInputs {
            symbols: vec![SymbolSpan {
                file: "repo.rs".into(),
                name: "save".into(),
                start_line: 5,
                end_line: 9,
                ..Default::default()
            }],
            ..Default::default()
        };
        let hit = |file: &str, line: usize| CachedResolution {
            backend: "lsp:ra@1".into(),
            outcome: "resolved".into(),
            target_file: Some(file.into()),
            target_line: Some(line),
            target_symbol: None,
            context: "ctx".into(),
        };
        let verified = CacheUse::Reuse(Some(SemanticVerdict::Verified {
            file: "repo.rs".into(),
            backend: "lsp:ra@1".into(),
        }));
        let none = LiveIdentity::NotRunning;
        let check = |h: &CachedResolution, ctx: &str, live: &LiveIdentity| {
            verdict_from_cache(h, &inputs, "save", ctx, live)
        };

        assert_eq!(check(&hit("repo.rs", 6), "ctx", &none), verified);
        assert_eq!(
            check(
                &hit("repo.rs", 6),
                "ctx",
                &LiveIdentity::Known("lsp:ra@1".into())
            ),
            verified
        );
        assert_eq!(
            check(
                &hit("repo.rs", 6),
                "ctx",
                &LiveIdentity::Known("lsp:ra@2".into())
            ),
            CacheUse::Stale,
            "another server version answers anew"
        );
        assert_eq!(
            check(&hit("repo.rs", 6), "ctx", &LiveIdentity::Busy),
            CacheUse::Defer,
            "a busy backend cannot vouch for the old answer"
        );
        assert_eq!(
            check(&hit("repo.rs", 6), "other-ctx", &none),
            CacheUse::Stale,
            "the `save` definitions (set or content) changed"
        );
        assert_eq!(
            check(&hit("repo.rs", 2), "ctx", &none),
            CacheUse::Stale,
            "the recorded definition line no longer holds a `save`"
        );
        assert_eq!(check(&hit("moved.rs", 6), "ctx", &none), CacheUse::Stale);
        let ambiguous = CachedResolution {
            outcome: "ambiguous".into(),
            target_file: None,
            ..hit("repo.rs", 6)
        };
        assert_eq!(
            check(&ambiguous, "ctx", &none),
            CacheUse::Reuse(None),
            "a definitive ambiguous answer is reused, not re-asked"
        );
    }

    #[test]
    fn unanswered_candidates_are_reported_unsettled_but_off_mode_is_settled() {
        let g = CodeGraph::open_in_memory().unwrap();
        let edges = [edge("a.rs", "save", Some((1, 4)))];
        let hashes = HashMap::from([("a.rs".to_string(), "h".to_string())]);
        let run = |mode| {
            escalate_calls(
                &g,
                "/nonexistent/leanctx-escalation",
                &CallGraphInputs::default(),
                &edges,
                &[StructuralTarget::Ambiguous],
                &hashes,
                mode,
                EscalationBudget::INTERACTIVE,
            )
        };

        let off = run(SemanticMode::Off);
        assert_eq!((off.verdicts, off.unsettled), (vec![None], vec![false]));

        // No backend can serve this root: no verdict, and the site stays
        // unsettled so earlier semantic edges are not pruned on its account.
        let auto = run(SemanticMode::Auto);
        assert_eq!((auto.verdicts, auto.unsettled), (vec![None], vec![true]));
    }
}
