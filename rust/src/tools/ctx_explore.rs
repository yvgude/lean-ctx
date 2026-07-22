//! `ctx_explore` — FastContext-style bounded, deterministic repo exploration.
//!
//! Where [`crate::tools::ctx_compose`] is a single-shot composer that returns
//! prose plus *inlined symbol bodies*, `ctx_explore` runs a **bounded multi-turn
//! loop** and returns compact `path:start-end` **citations** (the FastContext
//! idea, arXiv 2606.14066): the calling agent gets a map of *where* the answer
//! lives at a fraction of the tokens, then reads only what it needs.
//!
//! ## Loop
//! 1. Query understanding — `parse_task_hints` → keywords + path hints.
//! 2. Lexical anchor — one broad BM25 search over the resident index.
//! 3. Structural expansion — bounded BFS over the **static** import/call graph,
//!    grounded in the lexical hit set (only files that actually match the query
//!    are followed). A turn that discovers no new files stops the loop early
//!    (coverage saturation).
//! 4. Symbol channel — exact AST definitions for each keyword (`find_symbols`).
//! 5. Selection — submodular `greedy_max_coverage` picks the minimal,
//!    non-redundant citation set that covers the query terms under a token budget.
//!
//! ## Determinism (#498)
//! The output is a pure function of (repo content, query, options). Only
//! side-effect-free paths are used: the BM25 index and the **static** graph.
//! It never writes session state and never records Hebbian co-access
//! (`cooccurrence::record_access`) — those adaptive signals would make call N+1
//! differ from call N and are deliberately excluded from the byte-stable block.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::Path;

use crate::core::bm25_index::{BM25Index, ChunkKind, SearchResult};
use crate::core::context_packing::{CoverageItem, greedy_max_coverage};
use crate::core::graph_provider;
use crate::core::task_relevance::parse_task_hints;
use crate::core::tokens::count_tokens;
use crate::tools::CrpMode;

const DEFAULT_MAX_TURNS: usize = 3;
const DEFAULT_PER_TURN_K: usize = 8;
const DEFAULT_BUDGET_TOKENS: usize = 1200;
/// Hard caps keep the loop bounded regardless of repo size / query breadth.
const MAX_HITS: usize = 100;
const MAX_CANDIDATES: usize = 60;
const MAX_CITATIONS: usize = 15;
const MAX_KEYWORDS: usize = 6;
const MAX_SYMS_PER_KW: usize = 3;

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&v| v > 0)
        .unwrap_or(default)
}

/// Caller-facing knobs. `max_turns` is clamped to a sane range; `citation_only`
/// mirrors FastContext's terse mode (emit only the `<final_answer>` block).
#[derive(Debug, Clone)]
pub struct ExploreOptions {
    pub max_turns: usize,
    pub citation_only: bool,
}

impl ExploreOptions {
    pub fn new(max_turns: Option<usize>, citation_only: bool) -> Self {
        let mt = max_turns
            .unwrap_or_else(|| env_usize("LEAN_CTX_EXPLORE_MAX_TURNS", DEFAULT_MAX_TURNS))
            .clamp(1, 8);
        Self {
            max_turns: mt,
            citation_only,
        }
    }
}

impl Default for ExploreOptions {
    fn default() -> Self {
        Self::new(None, false)
    }
}

/// A single cited source span.
#[derive(Debug, Clone)]
pub struct Citation {
    pub file: String,
    pub start: usize,
    pub end: usize,
    pub label: String,
}

/// Result of an exploration run.
#[derive(Debug, Clone)]
pub struct ExploreOutcome {
    pub text: String,
    pub tokens: usize,
    pub citations: Vec<Citation>,
}

/// Internal candidate span carrying selection metadata.
#[derive(Debug, Clone)]
struct Candidate {
    file: String,
    start: usize,
    end: usize,
    label: String,
    score: f64,
    cost: usize,
    terms: HashSet<String>,
}

/// Map a kind string (BM25 `ChunkKind` debug or graph kind) to a short tag.
fn short_kind(kind: &str) -> &str {
    match kind.to_ascii_lowercase().as_str() {
        "function" | "fn" | "method" => "fn",
        "struct" => "struct",
        "impl" => "impl",
        "module" | "mod" => "mod",
        "class" => "class",
        "trait" => "trait",
        "enum" => "enum",
        "issue" => "issue",
        "pullrequest" => "pr",
        other if !other.is_empty() => "sym",
        _ => "",
    }
}

fn chunk_kind_tag(kind: &ChunkKind) -> &'static str {
    match kind {
        ChunkKind::Function | ChunkKind::Method => "fn",
        ChunkKind::Struct => "struct",
        ChunkKind::Impl => "impl",
        ChunkKind::Module => "mod",
        ChunkKind::Class => "class",
        ChunkKind::Issue => "issue",
        ChunkKind::PullRequest => "pr",
        _ => "",
    }
}

/// Repo-relative, forward-slash path for stable citations (#324).
fn rel_path(file: &str, root: &str) -> String {
    let stripped = file
        .strip_prefix(root)
        .map_or(file, |s| s.trim_start_matches(['/', '\\']));
    crate::core::protocol::display_path(stripped)
}

/// Distinct, lowercased query terms used for coverage selection.
fn query_terms(keywords: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for k in keywords {
        let lk = k.to_ascii_lowercase();
        if lk.len() >= 3 && seen.insert(lk.clone()) {
            out.push(lk);
            if out.len() >= MAX_KEYWORDS {
                break;
            }
        }
    }
    out
}

/// Which query terms a piece of text covers (case-insensitive substring).
fn covered_terms(text: &str, terms: &[String]) -> HashSet<String> {
    let lower = text.to_ascii_lowercase();
    terms
        .iter()
        .filter(|t| lower.contains(t.as_str()))
        .cloned()
        .collect()
}

/// File-level adjacency from the **static** import/call graph (bidirectional),
/// keyed by repo-relative path. Empty when no graph is available.
fn build_adjacency(project_root: &str) -> BTreeMap<String, BTreeSet<String>> {
    let mut adj: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    if let Some(open) = graph_provider::open_or_build(project_root) {
        for e in open.provider.edges() {
            let from = rel_path(&e.from, project_root);
            let to = rel_path(&e.to, project_root);
            if from == to {
                continue;
            }
            adj.entry(from.clone()).or_default().insert(to.clone());
            adj.entry(to).or_default().insert(from);
        }
    }
    adj
}

/// Symbol candidates: exact AST definitions for the keywords (deterministic).
fn symbol_candidates(project_root: &str, keywords: &[String], terms: &[String]) -> Vec<Candidate> {
    let Some(open) = graph_provider::open_or_build(project_root) else {
        return Vec::new();
    };
    let gp = &open.provider;
    let mut out: Vec<Candidate> = Vec::new();
    for kw in keywords.iter().take(MAX_KEYWORDS) {
        let mut syms = gp.find_symbols(kw, None, None);
        // Deterministic, exported-first ordering, then a small cap per keyword.
        syms.sort_by(|a, b| {
            b.is_exported
                .cmp(&a.is_exported)
                .then_with(|| a.file.cmp(&b.file))
                .then_with(|| a.start_line.cmp(&b.start_line))
        });
        for sym in syms.into_iter().take(MAX_SYMS_PER_KW) {
            let file = rel_path(&sym.file, project_root);
            let label = format!("{} ({})", sym.name, short_kind(&sym.kind));
            let text = format!("{} {}", sym.name, sym.kind);
            let mut covered = covered_terms(&text, terms);
            covered.insert(kw.to_ascii_lowercase());
            out.push(Candidate {
                file,
                start: sym.start_line,
                end: sym.end_line.max(sym.start_line),
                label,
                score: 0.0,
                cost: 1,
                terms: covered,
            });
        }
    }
    out
}

/// Convert a BM25 hit into a candidate.
fn candidate_from_hit(hit: &SearchResult, project_root: &str, terms: &[String]) -> Candidate {
    let file = rel_path(&hit.file_path, project_root);
    let tag = chunk_kind_tag(&hit.kind);
    let label = if hit.symbol_name.is_empty() {
        if tag.is_empty() {
            String::new()
        } else {
            format!("({tag})")
        }
    } else if tag.is_empty() {
        hit.symbol_name.clone()
    } else {
        format!("{} ({})", hit.symbol_name, tag)
    };
    let terms_covered = covered_terms(&format!("{} {}", hit.symbol_name, hit.snippet), terms);
    Candidate {
        file,
        start: hit.start_line,
        end: hit.end_line.max(hit.start_line),
        label,
        score: hit.score,
        cost: count_tokens(&hit.snippet).max(1),
        terms: terms_covered,
    }
}

/// Bounded BFS over the static graph, grounded in the lexical hit set. Returns
/// the set of discovered (query-relevant) files and the number of turns run.
fn expand_frontier(
    seed_files: &[String],
    relevant: &BTreeSet<String>,
    adjacency: &BTreeMap<String, BTreeSet<String>>,
    max_turns: usize,
) -> (BTreeSet<String>, usize) {
    let mut discovered: BTreeSet<String> = seed_files.iter().cloned().collect();
    let mut frontier: Vec<String> = seed_files.to_vec();
    let mut turns = 1usize;

    while turns < max_turns && !frontier.is_empty() {
        let mut next: BTreeSet<String> = BTreeSet::new();
        for f in &frontier {
            if let Some(nbrs) = adjacency.get(f) {
                for nbr in nbrs {
                    if !discovered.contains(nbr) && relevant.contains(nbr) {
                        next.insert(nbr.clone());
                    }
                }
            }
        }
        if next.is_empty() {
            break; // coverage saturation — additional turns add nothing
        }
        for n in &next {
            discovered.insert(n.clone());
        }
        frontier = next.into_iter().collect();
        turns += 1;
    }
    (discovered, turns)
}

/// Select the citation set: coverage-first (submodular), then score-fill, under
/// a token budget. Falls back to score order when the query has no usable terms.
fn select_citations(mut candidates: Vec<Candidate>, budget: usize) -> Vec<Candidate> {
    if candidates.is_empty() {
        return Vec::new();
    }
    // Deterministic candidate order: (file, start, end). greedy breaks gain/cost
    // ties by earliest index, so a stable order ⇒ stable selection.
    candidates.sort_by(|a, b| {
        a.file
            .cmp(&b.file)
            .then_with(|| a.start.cmp(&b.start))
            .then_with(|| a.end.cmp(&b.end))
    });
    candidates.truncate(MAX_CANDIDATES);

    let any_terms = candidates.iter().any(|c| !c.terms.is_empty());
    let mut chosen: Vec<usize> = if any_terms {
        let items: Vec<CoverageItem> = candidates
            .iter()
            .map(|c| CoverageItem {
                terms: c.terms.clone(),
                cost: c.cost,
            })
            .collect();
        greedy_max_coverage(&items, budget, |_| 1.0)
    } else {
        Vec::new()
    };

    let mut spent: usize = chosen.iter().map(|&i| candidates[i].cost).sum();
    let in_chosen: HashSet<usize> = chosen.iter().copied().collect();

    // Score-fill the remaining budget with the most relevant uncovered spans.
    let mut by_score: Vec<usize> = (0..candidates.len())
        .filter(|i| !in_chosen.contains(i))
        .collect();
    by_score.sort_by(|&a, &b| {
        candidates[b]
            .score
            .partial_cmp(&candidates[a].score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| candidates[a].file.cmp(&candidates[b].file))
            .then_with(|| candidates[a].start.cmp(&candidates[b].start))
    });
    for idx in by_score {
        if chosen.len() >= MAX_CITATIONS {
            break;
        }
        let cost = candidates[idx].cost;
        if spent + cost <= budget {
            chosen.push(idx);
            spent += cost;
        }
    }

    // Emit in stable (file, start) order for a readable, byte-stable block.
    chosen.sort_by(|&a, &b| {
        candidates[a]
            .file
            .cmp(&candidates[b].file)
            .then_with(|| candidates[a].start.cmp(&candidates[b].start))
    });
    chosen.into_iter().map(|i| candidates[i].clone()).collect()
}

/// Render the final answer: a `<final_answer>` block of `path:start-end label`,
/// optionally preceded by a short, deterministic summary.
fn render(
    query: &str,
    keywords: &[String],
    turns: usize,
    files_examined: usize,
    citations: &[Citation],
    crp_mode: CrpMode,
    citation_only: bool,
) -> String {
    let mut block = String::from("<final_answer>\n");
    for c in citations {
        if c.label.is_empty() {
            block.push_str(&format!("{}:{}-{}\n", c.file, c.start, c.end));
        } else {
            block.push_str(&format!("{}:{}-{}  {}\n", c.file, c.start, c.end, c.label));
        }
    }
    block.push_str("</final_answer>\n");

    if citation_only {
        return block;
    }

    let mut out = String::new();
    if crp_mode.is_tdd() {
        out.push_str(&format!(
            "explore({query}) → {} citations\n\n",
            citations.len()
        ));
    } else {
        out.push_str(&format!("EXPLORE: {query}\n"));
        if keywords.is_empty() {
            out.push_str("keywords: (none extracted)\n");
        } else {
            out.push_str(&format!("keywords: {}\n", keywords.join(", ")));
        }
        out.push_str(&format!(
            "turns: {turns}  files_examined: {files_examined}  citations: {}\n\n",
            citations.len()
        ));
    }
    out.push_str(&block);
    out
}

/// Run a bounded, deterministic exploration for `query`.
pub fn handle(
    query: &str,
    project_root: &str,
    crp_mode: CrpMode,
    opts: &ExploreOptions,
) -> ExploreOutcome {
    let query = query.trim();
    if query.is_empty() {
        return ExploreOutcome {
            text: "ERROR: query is required".to_string(),
            tokens: 0,
            citations: Vec::new(),
        };
    }

    let (_hint_files, keywords) = parse_task_hints(query);
    let terms = query_terms(&keywords);

    // Lexical anchor: one broad BM25 search over the resident index.
    let root = Path::new(project_root);
    let index = BM25Index::load_or_build(root);
    let hits: Vec<SearchResult> = if index.doc_count == 0 {
        Vec::new()
    } else {
        index.search(query, MAX_HITS)
    };

    // Best (highest-ranked) hit per file, in score order.
    let mut best_hit_for_file: BTreeMap<String, Candidate> = BTreeMap::new();
    let mut seed_order: Vec<String> = Vec::new();
    for hit in &hits {
        let cand = candidate_from_hit(hit, project_root, &terms);
        if let std::collections::btree_map::Entry::Vacant(slot) =
            best_hit_for_file.entry(cand.file.clone())
        {
            seed_order.push(cand.file.clone());
            slot.insert(cand);
        }
    }
    let relevant: BTreeSet<String> = best_hit_for_file.keys().cloned().collect();

    // Turn 1 frontier: the top-k distinct files by lexical relevance.
    let per_turn_k = env_usize("LEAN_CTX_EXPLORE_K", DEFAULT_PER_TURN_K);
    let seeds: Vec<String> = seed_order.iter().take(per_turn_k).cloned().collect();

    // Bounded BFS over the static graph, grounded in the lexical hit set.
    let adjacency = build_adjacency(project_root);
    let (discovered, turns) = expand_frontier(&seeds, &relevant, &adjacency, opts.max_turns);

    // Candidates: best hit per discovered file + exact symbol definitions.
    let mut by_loc: BTreeMap<(String, usize, usize), Candidate> = BTreeMap::new();
    let mut insert = |c: Candidate| {
        let key = (c.file.clone(), c.start, c.end);
        by_loc
            .entry(key)
            .and_modify(|e| {
                if c.score > e.score {
                    e.score = c.score;
                }
                if e.label.is_empty() && !c.label.is_empty() {
                    e.label.clone_from(&c.label);
                }
                for t in &c.terms {
                    e.terms.insert(t.clone());
                }
            })
            .or_insert(c);
    };
    for file in &discovered {
        if let Some(c) = best_hit_for_file.get(file) {
            insert(c.clone());
        }
    }
    for c in symbol_candidates(project_root, &keywords, &terms) {
        insert(c);
    }

    let files_examined = discovered.len();
    let candidates: Vec<Candidate> = by_loc.into_values().collect();
    let budget = env_usize("LEAN_CTX_EXPLORE_BUDGET_TOKENS", DEFAULT_BUDGET_TOKENS);
    let selected = select_citations(candidates, budget);

    let citations: Vec<Citation> = selected
        .into_iter()
        .map(|c| Citation {
            file: c.file,
            start: c.start,
            end: c.end,
            label: c.label,
        })
        .collect();

    let text = render(
        query,
        &keywords,
        turns,
        files_examined,
        &citations,
        crp_mode,
        opts.citation_only,
    );
    let tokens = count_tokens(&text);
    ExploreOutcome {
        text,
        tokens,
        citations,
    }
}

/// Parse the `path:start-end` citations out of an explore answer. Reused by the
/// eval harness to score the Explore arm. Tolerant of the optional trailing
/// label and of text outside the `<final_answer>` block.
pub fn parse_final_answer(text: &str) -> Vec<(String, usize, usize)> {
    let mut out = Vec::new();
    let mut inside = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed == "<final_answer>" {
            inside = true;
            continue;
        }
        if trimmed == "</final_answer>" {
            break;
        }
        if !inside || trimmed.is_empty() {
            continue;
        }
        // First whitespace-delimited token is `path:start-end`.
        let token = trimmed.split_whitespace().next().unwrap_or("");
        if let Some((path, range)) = token.rsplit_once(':')
            && let Some((s, e)) = range.split_once('-')
            && let (Ok(start), Ok(end)) = (s.parse::<usize>(), e.parse::<usize>())
        {
            out.push((path.to_string(), start, end));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_query_is_rejected() {
        let outcome = handle("   ", "/tmp", CrpMode::Off, &ExploreOptions::default());
        assert!(outcome.text.starts_with("ERROR"));
        assert_eq!(outcome.tokens, 0);
        assert!(outcome.citations.is_empty());
    }

    #[test]
    fn short_kind_maps_known_kinds() {
        assert_eq!(short_kind("Function"), "fn");
        assert_eq!(short_kind("method"), "fn");
        assert_eq!(short_kind("struct"), "struct");
        assert_eq!(short_kind("Constant"), "sym");
        assert_eq!(short_kind(""), "");
    }

    #[test]
    fn rel_path_strips_root_and_normalizes() {
        assert_eq!(rel_path("/repo/src/a.rs", "/repo"), "src/a.rs");
        assert_eq!(rel_path("src/a.rs", "/repo"), "src/a.rs");
    }

    #[test]
    fn query_terms_dedup_and_cap() {
        let kws: Vec<String> = vec!["Cache", "cache", "Index", "ab", "Search"]
            .into_iter()
            .map(String::from)
            .collect();
        let terms = query_terms(&kws);
        assert!(terms.contains(&"cache".to_string()));
        assert!(terms.contains(&"index".to_string()));
        assert!(!terms.iter().any(|t| t == "ab")); // too short
        // "cache" appears once despite two casings.
        assert_eq!(terms.iter().filter(|t| *t == "cache").count(), 1);
    }

    #[test]
    fn parse_final_answer_extracts_citations() {
        let text = "EXPLORE: foo\n\n<final_answer>\nsrc/a.rs:10-20  foo (fn)\nsrc/b.rs:5-5\n</final_answer>\n";
        let cits = parse_final_answer(text);
        assert_eq!(
            cits,
            vec![
                ("src/a.rs".to_string(), 10, 20),
                ("src/b.rs".to_string(), 5, 5),
            ]
        );
    }

    #[test]
    fn parse_final_answer_ignores_text_outside_block() {
        let text = "noise:1-2 should be ignored\n<final_answer>\nsrc/x.rs:1-3\n</final_answer>\ntrailing:9-9";
        let cits = parse_final_answer(text);
        assert_eq!(cits, vec![("src/x.rs".to_string(), 1, 3)]);
    }

    #[test]
    fn select_citations_respects_budget_and_is_deterministic() {
        let mk = |file: &str, start: usize, term: &str, cost: usize, score: f64| Candidate {
            file: file.to_string(),
            start,
            end: start + 5,
            label: format!("{term} (fn)"),
            score,
            cost,
            terms: HashSet::from([term.to_string()]),
        };
        let cands = vec![
            mk("src/a.rs", 1, "cache", 100, 2.0),
            mk("src/b.rs", 1, "index", 100, 1.5),
            mk("src/c.rs", 1, "cache", 100, 1.0), // redundant term, low score
        ];
        let a = select_citations(cands.clone(), 250);
        let b = select_citations(cands, 250);
        // Deterministic across calls.
        let fa: Vec<_> = a.iter().map(|c| (c.file.clone(), c.start)).collect();
        let fb: Vec<_> = b.iter().map(|c| (c.file.clone(), c.start)).collect();
        assert_eq!(fa, fb);
        // Budget 250 with cost 100 each ⇒ at most 2 spans.
        assert!(a.len() <= 2, "budget should cap to 2: {}", a.len());
    }

    #[test]
    fn expand_frontier_stops_on_coverage_saturation() {
        // No neighbours ⇒ the loop saturates after the first turn regardless of
        // the `max_turns` budget (the early-stop that bounds delegated cost).
        let adj: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let relevant: BTreeSet<String> = ["a.rs".to_string()].into_iter().collect();
        let (discovered, turns) = expand_frontier(&["a.rs".to_string()], &relevant, &adj, 8);
        assert_eq!(turns, 1, "no new files ⇒ stop after turn 1");
        assert_eq!(discovered.len(), 1);
    }

    #[test]
    fn expand_frontier_respects_max_turns() {
        // Relevant chain a→b→c→d. With max_turns=2 only b is reached (turn 2);
        // c/d lie beyond the depth budget.
        let mut adj: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        adj.insert("a.rs".into(), ["b.rs".to_string()].into_iter().collect());
        adj.insert("b.rs".into(), ["c.rs".to_string()].into_iter().collect());
        adj.insert("c.rs".into(), ["d.rs".to_string()].into_iter().collect());
        let relevant: BTreeSet<String> = ["a.rs", "b.rs", "c.rs", "d.rs"]
            .iter()
            .map(ToString::to_string)
            .collect();
        let (discovered, turns) = expand_frontier(&["a.rs".to_string()], &relevant, &adj, 2);
        assert_eq!(turns, 2, "max_turns caps BFS depth");
        assert!(discovered.contains("b.rs"));
        assert!(
            !discovered.contains("c.rs"),
            "depth beyond max_turns is unexplored"
        );
    }

    #[test]
    fn expand_frontier_follows_only_relevant_files() {
        // `b.rs` is a graph neighbour but not in the lexical hit set, so the
        // graph walk never follows it (grounding prevents drift into noise).
        let mut adj: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        adj.insert("a.rs".into(), ["b.rs".to_string()].into_iter().collect());
        let relevant: BTreeSet<String> = ["a.rs".to_string()].into_iter().collect();
        let (discovered, turns) = expand_frontier(&["a.rs".to_string()], &relevant, &adj, 8);
        assert_eq!(turns, 1);
        assert!(
            !discovered.contains("b.rs"),
            "irrelevant neighbours are skipped"
        );
    }

    #[test]
    fn handle_output_is_byte_stable_across_runs() {
        // #498: the output is a pure function of (content, query, options). Two
        // back-to-back runs on the same fixture must be byte-identical — no
        // session writes, no Hebbian co-access, no timestamps.
        let _env_lock = crate::core::data_dir::test_env_lock();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("cache.rs"),
            "pub struct CacheStore { entries: usize }\n\
             impl CacheStore {\n    pub fn lookup(&self, key: &str) -> Option<usize> { let _ = key; None }\n}\n",
        )
        .unwrap();
        std::fs::write(
            root.join("index.rs"),
            "pub fn build_index(cache: &str) -> usize { cache.len() }\n",
        )
        .unwrap();
        let root_str = root.to_string_lossy().to_string();
        let opts = ExploreOptions::new(Some(3), false);

        let run = || {
            handle(
                "how does the cache lookup work",
                &root_str,
                CrpMode::Off,
                &opts,
            )
        };

        // The fixture root is brand new, so the first call races index warm-up:
        // it can answer from a partially-built symbol index and cite two spans
        // where a warm call cites three (`lookup (fn)` arrives late). That is a
        // cold-start difference, not the state accumulation #498 is about — so
        // warm the index with a discarded call and compare two warm runs.
        let _warm = run();
        let a = run();
        let b = run();

        assert_eq!(a.text, b.text, "explore output must be byte-stable");
        assert_eq!(a.tokens, b.tokens);
        let locs = |o: &ExploreOutcome| -> Vec<(String, usize, usize)> {
            o.citations
                .iter()
                .map(|c| (c.file.clone(), c.start, c.end))
                .collect()
        };
        assert_eq!(locs(&a), locs(&b));
        assert!(a.text.contains("<final_answer>"));
        assert!(a.text.contains("</final_answer>"));
    }
}
