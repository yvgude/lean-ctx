//! Unified Graph Enricher — indexes Git history, tests, and knowledge into the PropertyGraph.
//!
//! Three enrichment passes:
//! 1. **Git commits**: `git log` → Commit nodes + `changed_in` edges
//! 2. **Test files**: naming/annotation heuristics → Test nodes + `tested_by` edges
//! 3. **Knowledge bridge**: `ctx_knowledge` facts → Knowledge nodes + `mentioned_in` edges

use crate::core::property_graph::{CodeGraph, Edge, EdgeKind, Node};
use crate::core::semantic::{EdgeEvidence, EvidenceGrade, EvidenceOrigin};
use std::collections::{BTreeMap, HashSet};
use std::path::Path;

// ---------------------------------------------------------------------------
// Git History Indexer
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub(crate) struct CommitInfo {
    pub hash: String,
    pub short_hash: String,
    pub author: String,
    pub date: String,
    pub message: String,
    pub files_changed: Vec<String>,
}

pub(crate) fn index_git_history(
    graph: &CodeGraph,
    project_root: &Path,
    max_commits: usize,
) -> anyhow::Result<EnrichmentStats> {
    let mut stats = EnrichmentStats::default();

    let output = std::process::Command::new("git")
        .args([
            "log",
            &format!("-{max_commits}"),
            "--format=%H%n%h%n%an%n%ai%n%s",
            "--name-only",
        ])
        .current_dir(project_root)
        .output();

    let output = match output {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).to_string(),
        _ => return Ok(stats),
    };

    let commits = parse_git_log(&output);
    for commit in &commits {
        let commit_node =
            Node::commit(&commit.short_hash, &commit.message).with_metadata(&format!(
                "{{\"author\":\"{}\",\"date\":\"{}\",\"hash\":\"{}\"}}",
                commit.author, commit.date, commit.hash
            ));

        let commit_id = graph.upsert_node(&commit_node)?;
        stats.commits_indexed += 1;

        for file in &commit.files_changed {
            if let Some(file_node) = graph.get_node_by_path(file)?
                && let Some(file_id) = file_node.id
            {
                graph.upsert_edge(&Edge::new(file_id, commit_id, EdgeKind::ChangedIn))?;
                stats.edges_created += 1;
            }
        }
    }

    Ok(stats)
}

fn parse_git_log(output: &str) -> Vec<CommitInfo> {
    let mut commits = Vec::new();
    let mut lines = output.lines().peekable();

    while lines.peek().is_some() {
        let hash = match lines.next() {
            Some(h) if !h.is_empty() && h.len() >= 7 => h.to_string(),
            _ => {
                lines.next();
                continue;
            }
        };

        let short_hash = match lines.next() {
            Some(s) => s.to_string(),
            None => break,
        };
        let author = match lines.next() {
            Some(a) => a.to_string(),
            None => break,
        };
        let date = match lines.next() {
            Some(d) => d.to_string(),
            None => break,
        };
        let message = match lines.next() {
            Some(m) => m.to_string(),
            None => break,
        };

        let mut files_changed = Vec::new();
        while let Some(line) = lines.peek() {
            if line.is_empty() {
                lines.next();
                break;
            }
            files_changed.push(line.to_string());
            lines.next();
        }

        commits.push(CommitInfo {
            hash,
            short_hash,
            author,
            date,
            message,
            files_changed,
        });
    }

    commits
}

// ---------------------------------------------------------------------------
// Test Indexer
// ---------------------------------------------------------------------------

const TEST_PATTERNS: &[&str] = &[
    "_test.",
    "test_",
    ".test.",
    ".spec.",
    "_spec.",
    "tests/",
    "__tests__/",
];

pub(crate) fn index_tests(
    graph: &CodeGraph,
    project_root: &Path,
) -> anyhow::Result<EnrichmentStats> {
    let mut stats = EnrichmentStats::default();

    let output = std::process::Command::new("git")
        .args(["ls-files"])
        .current_dir(project_root)
        .output();

    let files: Vec<String> = match output {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .lines()
            .map(ToString::to_string)
            .collect(),
        _ => return Ok(stats),
    };

    for file in &files {
        if !is_test_file(file) {
            continue;
        }

        let test_node = Node::test(file, file);
        let test_id = graph.upsert_node(&test_node)?;
        stats.tests_indexed += 1;

        let tested_file = infer_tested_file(file);
        if let Some(ref tested) = tested_file
            && files.contains(tested)
        {
            let target_node = graph.get_node_by_path(tested)?;
            if let Some(target) = target_node {
                if let Some(target_id) = target.id {
                    graph.upsert_edge(&Edge::new(target_id, test_id, EdgeKind::TestedBy))?;
                    stats.edges_created += 1;
                }
            } else {
                let file_id = graph.upsert_node(&Node::file(tested))?;
                graph.upsert_edge(&Edge::new(file_id, test_id, EdgeKind::TestedBy))?;
                stats.edges_created += 1;
            }
        }
    }

    Ok(stats)
}

fn is_test_file(path: &str) -> bool {
    let lower = path.to_lowercase();
    TEST_PATTERNS.iter().any(|p| lower.contains(p))
}

fn infer_tested_file(test_path: &str) -> Option<String> {
    let name = Path::new(test_path).file_name()?.to_str()?;

    for pattern in &["_test.", ".test.", "_spec.", ".spec."] {
        if let Some(pos) = name.find(pattern) {
            let base = &name[..pos];
            let ext = &name[pos + pattern.len() - 1..];
            let parent = Path::new(test_path).parent()?;

            let candidate = parent.join(format!("{base}{ext}"));
            if let Some(s) = candidate.to_str() {
                return Some(s.replace('\\', "/"));
            }

            if let Some(pp) = parent.parent() {
                let src_candidate = pp.join("src").join(format!("{base}{ext}"));
                if let Some(s) = src_candidate.to_str() {
                    return Some(s.replace('\\', "/"));
                }
            }
        }
    }

    if let Some(base) = name.strip_prefix("test_") {
        let parent = Path::new(test_path).parent()?;
        let candidate = parent.join(base);
        return candidate.to_str().map(|s| s.replace('\\', "/"));
    }

    None
}

// ---------------------------------------------------------------------------
// Knowledge Bridge
// ---------------------------------------------------------------------------

pub(crate) fn index_knowledge(
    graph: &CodeGraph,
    project_root: &str,
) -> anyhow::Result<EnrichmentStats> {
    let mut stats = EnrichmentStats::default();

    let knowledge = crate::core::knowledge::ProjectKnowledge::load(project_root);
    let Some(knowledge) = knowledge else {
        return Ok(stats);
    };

    let mut mentioned_files: HashSet<String> = HashSet::new();

    for fact in &knowledge.facts {
        let node = Node::knowledge(&fact.key, &format!("[{}] {}", fact.category, fact.value));
        let knowledge_id = graph.upsert_node(&node)?;
        stats.knowledge_indexed += 1;

        for file_ref in extract_file_refs(&fact.value) {
            if mentioned_files.insert(format!("{}:{}", fact.key, file_ref))
                && let Some(file_node) = graph.get_node_by_path(&file_ref)?
                && let Some(file_id) = file_node.id
            {
                graph.upsert_edge(&Edge::new(file_id, knowledge_id, EdgeKind::MentionedIn))?;
                stats.edges_created += 1;
            }
        }
    }

    Ok(stats)
}

fn extract_file_refs(text: &str) -> Vec<String> {
    let mut refs = Vec::new();
    for word in text.split_whitespace() {
        let cleaned = word.trim_matches(|c: char| c == '`' || c == '\'' || c == '"' || c == ',');
        if looks_like_file_path(cleaned) {
            refs.push(cleaned.to_string());
        }
    }
    refs
}

fn looks_like_file_path(s: &str) -> bool {
    if s.len() < 4 || s.len() > 200 {
        return false;
    }
    let path = Path::new(s);
    let has_sep = s.contains('/') || s.contains('\\');
    match path.extension().and_then(|e| e.to_str()) {
        Some(ext) => {
            let ext_lower = ext.to_ascii_lowercase();
            has_sep
                || matches!(
                    ext_lower.as_str(),
                    "rs" | "ts"
                        | "py"
                        | "js"
                        | "go"
                        | "java"
                        | "tsx"
                        | "jsx"
                        | "rb"
                        | "c"
                        | "cpp"
                        | "h"
                        | "cs"
                        | "swift"
                        | "kt"
                )
        }
        None => false,
    }
}

// ---------------------------------------------------------------------------
// Full enrichment pipeline
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
pub(crate) struct EnrichmentStats {
    pub commits_indexed: usize,
    pub tests_indexed: usize,
    pub knowledge_indexed: usize,
    pub edges_created: usize,
    /// Semantic escalation of structurally uncertain call edges.
    pub semantic: crate::core::semantic::EscalationStats,
}

impl EnrichmentStats {
    pub(crate) fn merge(&mut self, other: &Self) {
        self.commits_indexed += other.commits_indexed;
        self.tests_indexed += other.tests_indexed;
        self.knowledge_indexed += other.knowledge_indexed;
        self.edges_created += other.edges_created;
        let (s, o) = (&mut self.semantic, &other.semantic);
        s.candidates += o.candidates;
        s.cache_hits += o.cache_hits;
        s.live_queries += o.live_queries;
        s.verified += o.verified;
        s.not_in_project += o.not_in_project;
        s.unresolved += o.unresolved;
    }

    pub(crate) fn format_summary(&self) -> String {
        let mut out = format!(
            "Graph enriched: {} commits, {} tests, {} knowledge entries, {} edges",
            self.commits_indexed, self.tests_indexed, self.knowledge_indexed, self.edges_created
        );
        let s = &self.semantic;
        if s.candidates > 0 {
            out.push_str(&format!(
                "\nSemantic: {} uncertain call sites → {} verified, {} outside project, {} unresolved ({} cached, {} live queries)",
                s.candidates, s.verified, s.not_in_project, s.unresolved, s.cache_hits, s.live_queries
            ));
        }
        out
    }
}

pub(crate) fn enrich_graph(
    graph: &CodeGraph,
    project_root: &Path,
    max_commits: usize,
) -> anyhow::Result<EnrichmentStats> {
    let mut total = EnrichmentStats::default();

    let git_stats = index_git_history(graph, project_root, max_commits)?;
    total.merge(&git_stats);

    let test_stats = index_tests(graph, project_root)?;
    total.merge(&test_stats);

    if let Some(root_str) = project_root.to_str() {
        let knowledge_stats = index_knowledge(graph, root_str)?;
        total.merge(&knowledge_stats);

        let callgraph_stats = consolidate_callgraph(graph, root_str)?;
        total.merge(&callgraph_stats);
    }

    Ok(total)
}

/// Background semantic pass after a graph build: refreshes evidence-graded
/// call and `implements` edges — but only when it can add something.
/// `off` never runs; `auto` runs only while a semantic backend is already
/// live for the project (warm server or open IDE), so an idle machine pays
/// nothing; `eager` always runs. All live work is budget-bounded.
pub(crate) fn refresh_semantic_edges_in_background(project_root: &str) {
    use crate::core::config::SemanticMode;
    let mode = SemanticMode::for_project(project_root);
    let worthwhile = match mode {
        SemanticMode::Off => false,
        SemanticMode::Auto => crate::lsp::router::has_live_backend(project_root),
        SemanticMode::Eager => true,
    };
    if !worthwhile {
        return;
    }
    match CodeGraph::open(project_root) {
        Ok(graph) => {
            if let Err(e) = consolidate_callgraph(&graph, project_root) {
                tracing::warn!("[semantic] call-edge refresh failed for {project_root}: {e}");
            }
        }
        Err(e) => tracing::debug!("[semantic] no property graph for {project_root}: {e}"),
    }
}

/// Minimum spacing of backend-triggered refreshes per project: a burst of
/// `ctx_refactor` calls warming several languages yields one pass.
const BACKEND_REFRESH_INTERVAL: std::time::Duration = std::time::Duration::from_mins(5);
/// Retry delay when a pass could not run (graph not ready, backend kept busy).
const BACKEND_REFRESH_RETRY: std::time::Duration = std::time::Duration::from_secs(30);

/// A semantic backend is in use *in this process* (a language server started
/// for `ctx_refactor`, an attached IDE). Graph builds may run in another
/// process (the daemon) that cannot see this backend, so `auto` mode would
/// otherwise never use it: schedule a bounded refresh here, where the backend
/// lives. It waits until the current call is done (never competing with
/// interactive use) and runs only on a current, populated property graph.
/// At most one pass per project every [`BACKEND_REFRESH_INTERVAL`]; a pass
/// that could not run is retried on a use after [`BACKEND_REFRESH_RETRY`].
pub(crate) fn schedule_semantic_refresh(project_root: &str) {
    use std::collections::HashMap;
    use std::sync::{LazyLock, Mutex, PoisonError};
    use std::time::{Duration, Instant};
    /// Earliest next pass per project root.
    static NEXT: LazyLock<Mutex<HashMap<String, Instant>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));
    let defer = |root: &str, by: Duration| {
        NEXT.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(root.to_string(), Instant::now() + by);
    };

    // Unit tests seed fake backends for fake roots; never refresh those.
    if cfg!(test) {
        return;
    }
    let root = crate::core::index_paths::normalize_project_root(project_root);
    {
        let now = Instant::now();
        let mut next = NEXT.lock().unwrap_or_else(PoisonError::into_inner);
        if next.get(&root).is_some_and(|t| now < *t) {
            return;
        }
        // Expired entries carry no state; the map stays bounded by the
        // projects with a pass due or running.
        next.retain(|_, t| now < *t);
        // Reserved while the pass is pending.
        next.insert(root.clone(), now + BACKEND_REFRESH_INTERVAL);
    }
    let worker_root = root.clone();
    let spawned = std::thread::Builder::new()
        .name("leanctx-semantic-refresh".into())
        .spawn(move || {
            let root = worker_root;
            let give_up = Instant::now() + Duration::from_mins(2);
            loop {
                std::thread::sleep(Duration::from_secs(2));
                if !crate::lsp::router::backend_busy(&root) {
                    break;
                }
                if Instant::now() > give_up {
                    return defer(&root, BACKEND_REFRESH_RETRY);
                }
            }
            let ready = !crate::core::property_graph::engine_outdated(&root)
                && CodeGraph::open(&root).is_ok_and(|g| g.node_count().unwrap_or(0) > 0);
            if !ready {
                return defer(&root, BACKEND_REFRESH_RETRY);
            }
            refresh_semantic_edges_in_background(&root);
            defer(&root, BACKEND_REFRESH_INTERVAL);
        });
    if spawned.is_err() {
        NEXT.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&root);
    }
}

fn consolidate_callgraph(graph: &CodeGraph, project_root: &str) -> anyhow::Result<EnrichmentStats> {
    use crate::core::call_graph::{CallGraph, CallGraphInputs, resolve_edge_callee_targets};
    use crate::core::semantic::{escalate_calls, implementations::resolve_implements_edges};

    let inputs = CallGraphInputs::open(project_root);
    let call_graph = CallGraph::load_or_build(project_root, &inputs);
    let structural = resolve_edge_callee_targets(&inputs, &call_graph.edges);
    let mode = crate::core::config::SemanticMode::for_project(project_root);
    let escalation = escalate_calls(
        graph,
        project_root,
        &inputs,
        &call_graph.edges,
        &structural,
        &call_graph.file_hashes,
        mode,
        crate::core::semantic::EscalationBudget::BACKGROUND,
    );
    let mut stats = consolidate_call_edges(graph, &call_graph.edges, &structural, &escalation)?;
    stats.semantic = escalation.stats;

    let implements =
        resolve_implements_edges(graph, project_root, &inputs, &call_graph.file_hashes, mode);
    stats.merge(&apply_implements_edges(graph, &implements)?);
    Ok(stats)
}

/// Per file pair: strongest evidence grade, the backend behind it (smallest
/// identity on ties, for determinism) and the number of supporting sites.
type PairEvidence = (EvidenceGrade, Option<String>, u32);

/// Lifts call edges to file-level `Calls` edges with typed evidence.
///
/// Per edge, a semantic verdict wins over structure: `Verified` binds the
/// callee file; `NotInProject` vetoes a structural name match. Otherwise the
/// callee is resolved in the caller's own scope (same file → unique import →
/// unique project-wide); an ambiguous name yields no edge rather than an
/// arbitrary one. Pairs are visited in sorted order (deterministic graph).
///
/// This producer then withdraws its contribution from edges it no longer
/// derives — but only for callers whose candidates were all settled this
/// pass: an unanswered site (backend unavailable or busy, budget, cold
/// server) is not evidence that an earlier verified edge is wrong.
fn consolidate_call_edges(
    graph: &CodeGraph,
    edges: &[crate::core::call_graph::CallEdge],
    structural: &[crate::core::call_graph::StructuralTarget],
    escalation: &crate::core::semantic::Escalation,
) -> anyhow::Result<EnrichmentStats> {
    use crate::core::call_graph::StructuralTarget;
    use crate::core::semantic::SemanticVerdict;

    let mut stats = EnrichmentStats::default();
    let mut pairs: BTreeMap<(&str, String), PairEvidence> = BTreeMap::new();
    let unsettled_callers: std::collections::BTreeSet<&str> = edges
        .iter()
        .zip(&escalation.unsettled)
        .filter(|(_, unsettled)| **unsettled)
        .map(|(e, _)| e.caller_file.as_str())
        .collect();
    for ((edge, target), verdict) in edges.iter().zip(structural).zip(&escalation.verdicts) {
        let resolved = match verdict {
            Some(SemanticVerdict::Verified { file, backend }) => Some((
                file.clone(),
                EvidenceGrade::VerifiedSemantic,
                Some(backend.clone()),
            )),
            Some(SemanticVerdict::NotInProject) => None,
            None => match target {
                StructuralTarget::Resolved { file, via } => {
                    Some((file.clone(), EvidenceGrade::from_scope(*via), None))
                }
                StructuralTarget::Ambiguous | StructuralTarget::Unknown => None,
            },
        };
        let Some((to, grade, backend)) = resolved else {
            continue;
        };
        if to == edge.caller_file {
            continue;
        }
        let slot =
            pairs
                .entry((edge.caller_file.as_str(), to))
                .or_insert((grade, backend.clone(), 0));
        slot.2 += 1;
        if grade > slot.0 || (grade == slot.0 && backend < slot.1 && backend.is_some()) {
            slot.0 = grade;
            slot.1 = backend;
        }
    }

    for ((from_file, to_file), (grade, backend, sites)) in &pairs {
        let evidence =
            EdgeEvidence::new(*grade, EvidenceOrigin::Enrichment, backend.clone(), *sites);
        if let (Some(from_id), Some(to_id)) = (
            file_node_id(graph, from_file)?,
            file_node_id(graph, to_file)?,
        ) {
            graph.upsert_edge_with_evidence(from_id, to_id, &EdgeKind::Calls, &evidence)?;
            stats.edges_created += 1;
        }
    }

    withdraw_own_file_edges(graph, &EdgeKind::Calls, |s, t| {
        pairs.contains_key(&(s, t.to_string())) || unsettled_callers.contains(s)
    })?;
    Ok(stats)
}

/// Writes `implements` edges and withdraws the ones this producer no longer
/// derives — for settled declaring files only.
fn apply_implements_edges(
    graph: &CodeGraph,
    pass: &crate::core::semantic::implementations::ImplementsPass,
) -> anyhow::Result<EnrichmentStats> {
    let mut stats = EnrichmentStats::default();
    for e in &pass.edges {
        let evidence = EdgeEvidence::new(
            EvidenceGrade::VerifiedSemantic,
            EvidenceOrigin::Enrichment,
            Some(e.backend.clone()),
            1,
        );
        if let (Some(from_id), Some(to_id)) = (
            file_node_id(graph, &e.impl_file)?,
            file_node_id(graph, &e.abstract_file)?,
        ) {
            graph.upsert_edge_with_evidence(from_id, to_id, &EdgeKind::Implements, &evidence)?;
            stats.edges_created += 1;
        }
    }
    let live: std::collections::BTreeSet<(&str, &str)> = pass
        .edges
        .iter()
        .map(|e| (e.impl_file.as_str(), e.abstract_file.as_str()))
        .collect();
    withdraw_own_file_edges(graph, &EdgeKind::Implements, |s, t| {
        live.contains(&(s, t)) || !pass.settled.contains(t)
    })?;
    Ok(stats)
}

fn file_node_id(graph: &CodeGraph, path: &str) -> anyhow::Result<Option<i64>> {
    Ok(graph.get_node_by_path(path)?.and_then(|n| n.id))
}

/// Withdraws enrichment's contribution from file→file edges of `kind` that
/// `keep` no longer confirms. An edge disappears only when no other producer
/// still derives it; edges without typed evidence are never touched.
fn withdraw_own_file_edges(
    graph: &CodeGraph,
    kind: &EdgeKind,
    keep: impl Fn(&str, &str) -> bool,
) -> anyhow::Result<()> {
    for (source, target, metadata) in graph.file_edges_of_kind(kind)? {
        let own = EdgeEvidence::from_metadata(metadata.as_deref())
            .is_some_and(|e| e.has(EvidenceOrigin::Enrichment));
        if own && !keep(&source, &target) {
            graph.withdraw_file_edge(&source, &target, kind, EvidenceOrigin::Enrichment)?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::property_graph::NodeKind;

    #[test]
    fn parse_git_log_basic() {
        let log = "abc1234567890abcdef1234567890abcdef12345678\nabc1234\nJohn Doe\n2026-04-28 12:00:00 +0200\nfeat: add feature\nsrc/main.rs\nsrc/lib.rs\n\n";
        let commits = parse_git_log(log);
        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].short_hash, "abc1234");
        assert_eq!(commits[0].author, "John Doe");
        assert_eq!(commits[0].files_changed.len(), 2);
    }

    #[test]
    fn parse_git_log_multiple() {
        let log = "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2\na1b2c3d\nAlice\n2026-04-27\nfirst\nfile1.rs\n\nf6e5d4c3b2a1f6e5d4c3b2a1f6e5d4c3b2a1f6e5\nf6e5d4c\nBob\n2026-04-28\nsecond\nfile2.rs\nfile3.rs\n\n";
        let commits = parse_git_log(log);
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[1].files_changed.len(), 2);
    }

    #[test]
    fn is_test_file_detection() {
        assert!(is_test_file("src/utils_test.rs"));
        assert!(is_test_file("tests/integration.rs"));
        assert!(is_test_file("src/component.test.ts"));
        assert!(is_test_file("src/component.spec.js"));
        assert!(is_test_file("__tests__/app.js"));
        assert!(!is_test_file("src/main.rs"));
        assert!(!is_test_file("src/utils.rs"));
    }

    #[test]
    fn infer_tested_file_from_test() {
        assert_eq!(
            infer_tested_file("src/utils_test.rs"),
            Some("src/utils.rs".to_string())
        );
        assert_eq!(
            infer_tested_file("src/component.test.ts"),
            Some("src/component.ts".to_string())
        );
        assert_eq!(
            infer_tested_file("src/app.spec.js"),
            Some("src/app.js".to_string())
        );
    }

    #[test]
    fn infer_tested_file_prefix() {
        assert_eq!(
            infer_tested_file("tests/test_parser.py"),
            Some("tests/parser.py".to_string())
        );
    }

    #[test]
    fn looks_like_file_path_detection() {
        assert!(looks_like_file_path("src/main.rs"));
        assert!(looks_like_file_path("core/utils.ts"));
        assert!(looks_like_file_path("main.py"));
        assert!(!looks_like_file_path("hello"));
        assert!(!looks_like_file_path("a.b"));
        assert!(!looks_like_file_path(".hidden"));
    }

    #[test]
    fn extract_file_refs_from_text() {
        let text = "Changed `src/main.rs` and core/utils.ts for the fix";
        let refs = extract_file_refs(text);
        assert!(refs.contains(&"src/main.rs".to_string()));
        assert!(refs.contains(&"core/utils.ts".to_string()));
    }

    #[test]
    fn enrichment_stats_merge() {
        let mut a = EnrichmentStats {
            commits_indexed: 5,
            tests_indexed: 3,
            knowledge_indexed: 2,
            edges_created: 10,
            ..Default::default()
        };
        let b = EnrichmentStats {
            commits_indexed: 2,
            tests_indexed: 1,
            knowledge_indexed: 0,
            edges_created: 4,
            ..Default::default()
        };
        a.merge(&b);
        assert_eq!(a.commits_indexed, 7);
        assert_eq!(a.edges_created, 14);
    }

    #[test]
    fn enrichment_stats_format() {
        let s = EnrichmentStats {
            commits_indexed: 10,
            tests_indexed: 5,
            knowledge_indexed: 3,
            edges_created: 20,
            ..Default::default()
        };
        let fmt = s.format_summary();
        assert!(fmt.contains("10 commits"));
        assert!(fmt.contains("5 tests"));
    }

    #[test]
    fn commit_node_construction() {
        let node = Node::commit("abc1234", "feat: add feature");
        assert_eq!(node.kind, NodeKind::Commit);
        assert_eq!(node.name, "abc1234");
    }

    #[test]
    fn test_node_construction() {
        let node = Node::test("src/utils_test.rs", "src/utils_test.rs");
        assert_eq!(node.kind, NodeKind::Test);
        assert_eq!(node.file_path, "src/utils_test.rs");
    }

    #[test]
    fn knowledge_node_construction() {
        let node = Node::knowledge("k1", "Database uses PostgreSQL");
        assert_eq!(node.kind, NodeKind::Knowledge);
        assert!(node.metadata.unwrap().contains("PostgreSQL"));
    }

    #[test]
    fn graph_commit_and_edge() {
        let g = CodeGraph::open_in_memory().unwrap();
        let file_id = g.upsert_node(&Node::file("src/main.rs")).unwrap();
        let commit_id = g.upsert_node(&Node::commit("abc1234", "fix bug")).unwrap();
        g.upsert_edge(&Edge::new(file_id, commit_id, EdgeKind::ChangedIn))
            .unwrap();

        let edges = g.edges_from(file_id).unwrap();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].kind, EdgeKind::ChangedIn);
    }

    #[test]
    fn graph_test_edge() {
        let g = CodeGraph::open_in_memory().unwrap();
        let code_id = g.upsert_node(&Node::file("src/utils.rs")).unwrap();
        let test_id = g
            .upsert_node(&Node::test("src/utils_test.rs", "test_parse"))
            .unwrap();
        g.upsert_edge(&Edge::new(code_id, test_id, EdgeKind::TestedBy))
            .unwrap();

        let edges = g.edges_from(code_id).unwrap();
        assert_eq!(edges[0].kind, EdgeKind::TestedBy);
    }

    #[test]
    fn graph_knowledge_edge() {
        let g = CodeGraph::open_in_memory().unwrap();
        let file_id = g.upsert_node(&Node::file("src/db.rs")).unwrap();
        let k_id = g
            .upsert_node(&Node::knowledge("db_type", "Uses PostgreSQL"))
            .unwrap();
        g.upsert_edge(&Edge::new(file_id, k_id, EdgeKind::MentionedIn))
            .unwrap();

        let edges = g.edges_from(file_id).unwrap();
        assert_eq!(edges[0].kind, EdgeKind::MentionedIn);
    }

    /// Regression: a name→file map (last symbol wins) attributed every `save()`
    /// call to whichever `save` was listed last, inventing false `Calls` edges.
    #[test]
    fn consolidate_calls_resolves_in_caller_scope_and_skips_ambiguous() {
        use crate::core::call_graph::{CallEdge, CallGraphInputs, SymbolSpan};
        let sym = |file: &str| SymbolSpan {
            file: file.into(),
            name: "save".into(),
            start_line: 1,
            end_line: 3,
            ..Default::default()
        };
        let call = |file: &str| CallEdge {
            caller_file: file.into(),
            caller_symbol: "f".into(),
            caller_line: 1,
            callee_name: "save".into(),
            ..Default::default()
        };
        let inputs = CallGraphInputs {
            project_root: "/p".into(),
            symbols: vec![sym("b.rs"), sym("a.rs")],
            import_edges: vec![("c.rs".into(), "b.rs".into())],
            ..Default::default()
        };

        let g = CodeGraph::open_in_memory().unwrap();
        let ids: Vec<i64> = ["a.rs", "b.rs", "c.rs", "d.rs"]
            .iter()
            .map(|f| g.upsert_node(&Node::file(f)).unwrap())
            .collect();

        // c.rs imports b.rs → b.rs; d.rs has no scope hint → ambiguous → no edge.
        use crate::core::semantic::{Escalation, SemanticVerdict};
        let run = |verdicts: Vec<Option<SemanticVerdict>>, unsettled: Vec<bool>| Escalation {
            verdicts,
            unsettled,
            ..Default::default()
        };
        let edges = [call("c.rs"), call("d.rs")];
        let structural = crate::core::call_graph::resolve_edge_callee_targets(&inputs, &edges);
        consolidate_call_edges(
            &g,
            &edges,
            &structural,
            &run(vec![None, None], vec![false; 2]),
        )
        .unwrap();

        let from_c = g.edges_from(ids[2]).unwrap();
        assert_eq!(from_c.len(), 1);
        assert_eq!(from_c[0].target_id, ids[1]);
        assert_eq!(
            EdgeEvidence::from_metadata(from_c[0].metadata.as_deref()).map(|e| e.grade),
            Some(EvidenceGrade::ResolvedStructural)
        );
        assert!(g.edges_from(ids[3]).unwrap().is_empty());

        // Semantic verdicts: the backend binds d.rs's ambiguous call to a.rs
        // and places c.rs's callee outside the project — the structural c→b
        // edge this producer wrote before must disappear.
        let verdicts = vec![
            Some(SemanticVerdict::NotInProject),
            Some(SemanticVerdict::Verified {
                file: "a.rs".into(),
                backend: "lsp:ra@1".into(),
            }),
        ];
        consolidate_call_edges(&g, &edges, &structural, &run(verdicts, vec![false; 2])).unwrap();

        assert!(
            g.edges_from(ids[2]).unwrap().is_empty(),
            "vetoed edge pruned"
        );
        let from_d = g.edges_from(ids[3]).unwrap();
        assert_eq!(from_d.len(), 1);
        assert_eq!(from_d[0].target_id, ids[0]);
        let evidence = EdgeEvidence::from_metadata(from_d[0].metadata.as_deref()).unwrap();
        assert_eq!(evidence.grade, EvidenceGrade::VerifiedSemantic);
        assert_eq!(
            evidence.primary().and_then(|c| c.backend.as_deref()),
            Some("lsp:ra@1")
        );

        // Regression (review): d.rs changed and the backend is cold — its
        // site is unanswered. That is not evidence against d→a: it stays.
        consolidate_call_edges(
            &g,
            &edges,
            &structural,
            &run(vec![None, None], vec![false, true]),
        )
        .unwrap();
        assert_eq!(
            g.edges_from(ids[3]).unwrap().len(),
            1,
            "unsettled caller kept"
        );
    }
}
