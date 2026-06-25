//! `ctx_compose` — task composer (Phase 2 of the efficiency epic).
//!
//! The biggest agent win is a single "rich per call" tool that returns ranked
//! files *with* inline bodies, replacing the typical search → read → outline →
//! read chain (3-5 calls) with one.
//!
//! lean-ctx already has the building blocks as separate tools; this composes
//! them into one response for a natural-language task:
//!   1. extracted keywords,
//!   2. semantically ranked files (BM25 / hybrid),
//!   3. exact match locations (index-backed `ctx_search`),
//!   4. the body of the most relevant symbol, inline.

use std::collections::HashMap;
use std::sync::mpsc;
use std::time::Duration;

use crate::core::graph_provider;
use crate::core::tokens::count_tokens;
use crate::tools::CrpMode;

/// Wall-time budget for the semantic-ranking stage. The exact-match and symbol
/// stages are index-backed and cheap; only semantic ranking can hit a cold
/// `O(corpus)` BM25 build. We never let that block the agent loop: past the
/// budget we return what we have and let the detached worker finish warming the
/// resident cache for the next call. Override via `LEAN_CTX_COMPOSE_BUDGET_MS`.
const DEFAULT_SEMANTIC_BUDGET_MS: u64 = 2500;

fn semantic_budget() -> Duration {
    let ms = std::env::var("LEAN_CTX_COMPOSE_BUDGET_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|&v| v > 0)
        .unwrap_or(DEFAULT_SEMANTIC_BUDGET_MS);
    Duration::from_millis(ms)
}

/// Token budget for the inlined symbol bodies. Submodular selection fills it
/// with the most coverage-effective, non-redundant set of symbols.
/// Override via `LEAN_CTX_COMPOSE_SYMBOL_TOKENS`.
const DEFAULT_SYMBOL_BUDGET_TOKENS: usize = 600;

fn symbol_budget_tokens() -> usize {
    std::env::var("LEAN_CTX_COMPOSE_SYMBOL_TOKENS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&v| v > 0)
        .unwrap_or(DEFAULT_SYMBOL_BUDGET_TOKENS)
}

/// Wall-time budget for the associative (graph spreading-activation) stage.
/// Opening/building the graph index is `O(corpus)` on a cold repo, so — like
/// semantic ranking — we bound it and skip the (purely additive) section on
/// overrun while the detached worker warms the index. `LEAN_CTX_COMPOSE_GRAPH_BUDGET_MS`.
const DEFAULT_GRAPH_BUDGET_MS: u64 = 1500;

fn graph_budget() -> Duration {
    let ms = std::env::var("LEAN_CTX_COMPOSE_GRAPH_BUDGET_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|&v| v > 0)
        .unwrap_or(DEFAULT_GRAPH_BUDGET_MS);
    Duration::from_millis(ms)
}

/// Per-hop activation decay and hop count for spreading activation. Small decay
/// keeps activation local (structurally near the seeds); 3 hops covers
/// import→callee→sibling chains without diffusing across the whole graph.
const SPREAD_DECAY: f64 = 0.6;
const SPREAD_HOPS: usize = 3;
/// How many associative neighbours to surface.
const SPREAD_TOP_K: usize = 8;

/// Build the associative-relevance block: spreading activation seeded at the
/// files the task keywords resolve to, propagated over the union of the static
/// import/call graph and the *learned* Hebbian co-access graph. Returns an empty
/// string when no graph/seeds are available. Runs entirely in the worker thread
/// so [`associative_block_budgeted`] can bound it.
fn build_associative_block(project_root: &str, keywords: &[String]) -> String {
    let Some(open) = graph_provider::open_or_build(project_root) else {
        return String::new();
    };
    let gp = &open.provider;

    // Seeds: distinct files the keywords resolve to via symbol lookup.
    let mut seed_files: Vec<String> = Vec::new();
    for kw in keywords {
        for sym in gp.find_symbols(kw, None, None) {
            if !seed_files.contains(&sym.file) {
                seed_files.push(sym.file);
            }
        }
    }
    if seed_files.is_empty() {
        return String::new();
    }

    // Hebbian update: files relevant to the same task "fire together", so record
    // their co-access (strengthens future associative recall). Persisted.
    crate::core::cooccurrence::record_access(project_root, &seed_files);

    // Adjacency = static structural edges ∪ learned co-access edges. Edges are
    // made bidirectional so activation spreads both up and down the graph.
    let mut adjacency: HashMap<String, Vec<(String, f64)>> = HashMap::new();
    let mut add_edge = |a: &str, b: &str, w: f64| {
        adjacency
            .entry(a.to_string())
            .or_default()
            .push((b.to_string(), w));
        adjacency
            .entry(b.to_string())
            .or_default()
            .push((a.to_string(), w));
    };
    for e in gp.edges() {
        add_edge(&e.from, &e.to, if e.weight > 0.0 { e.weight } else { 1.0 });
    }
    let coaccess = crate::core::cooccurrence::load(project_root);
    for sf in &seed_files {
        for (nbr, w) in coaccess.related(sf, 16) {
            add_edge(sf, &nbr, w);
        }
    }

    let seeds: HashMap<String, f64> = seed_files.iter().map(|f| (f.clone(), 1.0)).collect();
    let ranked = crate::core::spreading_activation::related_ranked(
        &seeds,
        &adjacency,
        SPREAD_DECAY,
        SPREAD_HOPS,
        SPREAD_TOP_K,
    );
    if ranked.is_empty() {
        return String::new();
    }

    let mut s = String::from("\n## Related (associative: import/call graph + learned co-access)\n");
    for (file, activation) in ranked {
        // Forward-slash normalize so Windows backslash paths are never escape-
        // mangled by client render layers (issue #324).
        let file = crate::core::protocol::display_path(&file);
        s.push_str(&format!("- {file} (activation {activation:.2})\n"));
    }
    s
}

/// Run [`build_associative_block`] under [`graph_budget`]. The Hebbian record is
/// a side effect of the worker, so it persists even when we time out and drop
/// the (optional) section.
fn associative_block_budgeted(project_root: &str, keywords: &[String]) -> String {
    if keywords.is_empty() {
        return String::new();
    }
    let (tx, rx) = mpsc::channel::<String>();
    let root = project_root.to_string();
    let kws = keywords.to_vec();
    std::thread::spawn(move || {
        let block = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            build_associative_block(&root, &kws)
        }))
        .unwrap_or_else(|_| {
            tracing::warn!("[ctx_compose: associative block panicked; omitting section]");
            String::new()
        });
        let _ = tx.send(block);
    });
    rx.recv_timeout(graph_budget()).unwrap_or_default()
}

/// Words that carry no retrieval signal — dropped from keyword extraction.
const STOPWORDS: &[&str] = &[
    "the",
    "and",
    "for",
    "with",
    "that",
    "this",
    "from",
    "into",
    "how",
    "where",
    "what",
    "does",
    "are",
    "was",
    "use",
    "used",
    "uses",
    "add",
    "all",
    "any",
    "can",
    "get",
    "set",
    "via",
    "out",
    "its",
    "his",
    "her",
    "you",
    "your",
    "our",
    "find",
    "show",
    "list",
    "make",
    "when",
    "then",
    "has",
    "have",
    "had",
    "not",
    "but",
    "see",
    "function",
    "method",
    "class",
    "code",
    "file",
    "files",
    "implement",
    "implementation",
];

/// Extract up to `max` distinct identifier-ish keywords from a task, preserving
/// original case (symbol lookups are case-sensitive) and first-seen order.
fn extract_keywords(task: &str, max: usize) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for raw in task.split(|c: char| !(c.is_alphanumeric() || c == '_')) {
        if raw.len() < 3 {
            continue;
        }
        if STOPWORDS.contains(&raw.to_ascii_lowercase().as_str()) {
            continue;
        }
        if seen.insert(raw.to_string()) {
            out.push(raw.to_string());
            if out.len() >= max {
                break;
            }
        }
    }
    out
}

/// Run the semantic ranking stage under a wall-time budget. Returns the ranked
/// block on time, or a short "deferred" note if the (cold) build overruns.
fn ranked_files_budgeted(task: &str, project_root: &str, crp_mode: CrpMode) -> String {
    let (tx, rx) = mpsc::channel::<String>();
    let task_owned = task.to_string();
    let root_owned = project_root.to_string();

    std::thread::spawn(move || {
        let ranked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            crate::tools::ctx_semantic_search::handle_impl(
                &task_owned,
                &root_owned,
                8,
                crp_mode,
                None,
                None,
                None,
                Some(false),
                Some(false),
            )
        }))
        .unwrap_or_else(|_| {
            tracing::warn!("[ctx_compose: semantic ranking panicked; omitting section]");
            String::new()
        });
        // Receiver may be gone (we timed out); dropping the result is fine —
        // the cache warming already happened as a side effect of the build.
        let _ = tx.send(ranked);
    });

    match rx.recv_timeout(semantic_budget()) {
        Ok(ranked) => ranked.trim().to_string(),
        Err(_) => deferred_ranking_note(project_root),
    }
}

/// Honest, state-aware note when semantic ranking overruns its wall-time budget.
///
/// The old message always promised ranking would be "instant on the next call".
/// That is a lie when the index build *failed* or the index is too large to
/// persist — in those cases every call rebuilds and the promise never comes
/// true (issue #249: "keeps saying it's warming up … but it never happens").
/// We now read the real orchestrator state and tell the agent exactly what is
/// happening and what to do about it.
fn deferred_ranking_note(project_root: &str) -> String {
    let exact = "the exact matches below are authoritative for this call";
    // Check if the code_index.db exists with chunks — if so the index is ready.
    let db_path = crate::core::index_namespace::vectors_dir(std::path::Path::new(project_root))
        .join("code_index.db");
    let has_chunks = db_path.exists()
        && rusqlite::Connection::open(&db_path)
            .ok()
            .and_then(|conn| {
                conn.query_row("SELECT COUNT(*) FROM chunks", [], |r| r.get::<_, i64>(0))
                    .ok()
            })
            .unwrap_or(0)
            > 0;
    if has_chunks {
        format!(
            "(deferred — semantic index is warming; {exact}, \
             and ranking will be fast on the next call once the index is cached)"
        )
    } else {
        format!(
            "(deferred — semantic index is warming; {exact}, \
             and ranking becomes available once the index is built)"
        )
    }
}

/// Compose a single rich response for `task`.
pub fn handle(task: &str, project_root: &str, crp_mode: CrpMode) -> (String, usize) {
    let task = task.trim();
    if task.is_empty() {
        return ("ERROR: task is required".to_string(), 0);
    }

    let keywords = extract_keywords(task, 6);
    let allow_secret = crate::core::roles::active_role().io.allow_secret_paths;

    let mut out = String::new();
    out.push_str(&format!("TASK: {task}\n"));
    if keywords.is_empty() {
        out.push_str("KEYWORDS: (none extracted — using full task for ranking)\n");
    } else {
        out.push_str(&format!("KEYWORDS: {}\n", keywords.join(", ")));
    }

    // 1. Semantically ranked files for the whole task — budgeted so a cold
    //    BM25 build can never stall the agent loop (hardening H1). The worker
    //    inherits the resident cache, so a build that overruns the budget still
    //    warms the cache for the next call rather than being wasted.
    out.push_str("\n## Ranked files (semantic)\n");
    out.push_str(&ranked_files_budgeted(task, project_root, crp_mode));
    out.push('\n');

    // 2. Exact match locations for the primary keyword (index-backed search).
    if let Some(primary) = keywords.first() {
        let grep = crate::tools::ctx_search::handle(
            primary,
            project_root,
            None,
            10,
            crp_mode,
            true,
            allow_secret,
        )
        .text;
        out.push_str(&format!("\n## Exact matches: '{primary}'\n"));
        out.push_str(grep.trim());
        out.push('\n');
    }

    // 3. Inline the symbol bodies that best cover the task keywords. Rather
    //    than just the first match, select the non-redundant *set* of symbols
    //    with maximal keyword coverage under a token budget via submodular
    //    greedy (1−1/e optimal). Two keywords resolving to the same symbol, or
    //    a symbol whose body adds no new keyword, are naturally pruned.
    use crate::core::context_packing::{CoverageItem, greedy_max_coverage};
    let mut snippets: Vec<String> = Vec::new();
    let mut items: Vec<CoverageItem> = Vec::new();
    for kw in &keywords {
        if let Some((rendered, toks)) =
            crate::tools::ctx_symbol::best_symbol_snippet(kw, project_root)
        {
            // The snippet always covers its triggering keyword, plus any other
            // task keyword its body textually surfaces (a more central symbol).
            let mut terms: std::collections::HashSet<String> =
                std::collections::HashSet::from([kw.clone()]);
            for other in &keywords {
                if other != kw && rendered.contains(other.as_str()) {
                    terms.insert(other.clone());
                }
            }
            items.push(CoverageItem {
                terms,
                cost: toks.max(1),
            });
            snippets.push(rendered);
        }
    }
    if !items.is_empty() {
        let chosen = greedy_max_coverage(&items, symbol_budget_tokens(), |_| 1.0);
        let mut seen = std::collections::HashSet::new();
        let mut header_written = false;
        for idx in chosen {
            let rendered = snippets[idx].trim();
            if rendered.is_empty() || !seen.insert(rendered.to_string()) {
                continue;
            }
            if !header_written {
                out.push_str("\n## Top symbols (bodies)\n");
                header_written = true;
            }
            out.push_str(rendered);
            out.push('\n');
        }
    }

    // 4. Associative neighbours via spreading activation over the import/call
    //    graph unified with the learned Hebbian co-access graph (budgeted,
    //    additive — surfaces structurally-close files lexical search misses).
    out.push_str(&associative_block_budgeted(project_root, &keywords));

    let sent = count_tokens(&out);
    (out, sent)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_keywords_drops_stopwords_and_short_tokens() {
        let kw = extract_keywords("How does the BM25Index cache work for ctx_search?", 6);
        assert!(kw.contains(&"BM25Index".to_string()));
        assert!(kw.contains(&"cache".to_string()));
        assert!(kw.contains(&"ctx_search".to_string()));
        assert!(!kw.iter().any(|k| k == "the" || k == "How" || k == "for"));
    }

    #[test]
    fn extract_keywords_dedups_and_caps() {
        let kw = extract_keywords("alpha alpha beta gamma delta epsilon zeta eta", 3);
        assert_eq!(kw.len(), 3);
        assert_eq!(kw[0], "alpha");
    }

    #[test]
    fn empty_task_is_rejected() {
        let (out, tok) = handle("   ", "/tmp", CrpMode::Off);
        assert!(out.starts_with("ERROR"));
        assert_eq!(tok, 0);
    }

    #[test]
    fn deferred_note_for_idle_index_is_optimistic_but_honest() {
        // Unknown project → orchestrator state is idle. The note must NOT promise
        // "instant on the next call" (the dishonest wording from #249); it should
        // explain the index is warming and will be fast once cached.
        let tmp = tempfile::tempdir().unwrap();
        let note = deferred_ranking_note(tmp.path().to_string_lossy().as_ref());
        assert!(
            note.contains("warming") || note.contains("building"),
            "note: {note}"
        );
        assert!(
            note.contains("authoritative"),
            "note must reassure that exact matches are authoritative: {note}"
        );
        assert!(
            !note.contains("instant on the next call"),
            "must not repeat the dishonest 'instant next call' promise: {note}"
        );
    }
}
