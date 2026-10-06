//! `ctx_compose` — task composer (Phase 2 of the efficiency epic).
//!
//! The biggest agent win is a single "rich per call" tool that returns ranked
//! files *with* inline bodies, replacing the typical search → read → outline →
//! read chain (3-5 calls) with one.
//!
//! lean-ctx already has the building blocks as separate tools; this composes
//! them into one response for a natural-language task:
//!   1. extracted keywords,
//!   2. locally ranked files (admitted BM25),
//!   3. exact match locations (index-backed `ctx_search`),
//!   4. the body of the most relevant symbol, inline.

use std::collections::HashMap;
use std::sync::mpsc;
use std::time::Duration;

use crate::core::graph_provider;
use crate::core::tokens::count_tokens;
use crate::tools::CrpMode;

#[path = "ctx_compose_selection.rs"]
pub(crate) mod selection;

/// Wall-time budget for the semantic-ranking stage. The exact-match and symbol
/// stages are index-backed and cheap; only semantic ranking can hit a cold
/// `O(corpus)` BM25 build. We never let that block the agent loop: past the
/// budget (4s, tuned for cold-start coverage #902) we return the independently
/// admitted sections. Late ranking output is discarded; no legacy index is used
/// as a fallback. Override via `LEAN_CTX_COMPOSE_BUDGET_MS`.
const DEFAULT_SEMANTIC_BUDGET_MS: u64 = 4000;
// A timed-out worker retains its permit until it actually finishes. Repeated
// requests cannot accumulate an unbounded set of fresh corpus scans.
static RANKING_WORKERS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(4);

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

pub(crate) fn kernel_supplement_budget(project_root: &str) -> usize {
    let config = crate::core::context_kernel::activation::load_config(project_root);
    (symbol_budget_tokens() / 5).min(crate::core::context_kernel::activation::supplement_budget(
        &config,
    ))
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

/// Order `keywords` from most to least specific using the resident BM25 index's
/// per-token document frequency (how many chunks contain the token). Rarer =
/// more specific = better as the "exact matches" seed. A token absent from the
/// corpus (df 0) sinks to the end — grepping it yields nothing useful.
///
/// Non-blocking and best-effort: if the resident index isn't warm yet we return
/// the keywords in their original first-seen order (the previous behaviour), so
/// this can only improve the seed, never stall the call to build an index.
fn order_by_specificity(keywords: &[String], project_root: &str) -> Vec<String> {
    let Some(index) = resident_index(project_root) else {
        return keywords.to_vec();
    };
    rank_by_doc_freq(keywords, &index.doc_freqs)
}

/// Pure ranking core: choose the exact-match seed keyword.
///
/// The seed feeds a case-sensitive regex grep, so raw rarity is the wrong sort:
/// the rarest task token is often a lowercase prose word (`measurand`) that the
/// index counts case-insensitively but the grep then misses against `Measurand`
/// — 0 hits, worse than before. Instead prefer *code identifiers* (camelCase or
/// snake_case: `GetMaxCurrent`, `CurrentGetter`), which grep straight to code,
/// over acronyms (`OCPP`) and prose (`current`) that also match READMEs. Within
/// each class, rarer (lower document frequency) wins; absent tokens (df 0) sink.
/// Keys are lowercased in `doc_freqs`; a stable sort keeps first-seen order on
/// ties, so a task with no identifiers degrades to the previous rarity order.
fn rank_by_doc_freq(
    keywords: &[String],
    doc_freqs: &std::collections::HashMap<String, usize>,
) -> Vec<String> {
    let df = |kw: &String| match doc_freqs.get(&kw.to_ascii_lowercase()) {
        Some(&n) if n > 0 => n,
        _ => usize::MAX,
    };
    // Class 0 = code identifier (grep-friendly), class 1 = acronym/prose.
    let rank_key = |kw: &String| (u8::from(!is_code_identifier(kw)), df(kw));
    let mut ranked = keywords.to_vec();
    ranked.sort_by_key(rank_key);
    ranked
}

/// True for tokens that read as code identifiers — snake_case (`get_max_current`)
/// or camelCase/PascalCase with an internal capital (`GetMaxCurrent`). A leading
/// capital alone (`Current`) or an all-caps acronym (`OCPP`) does not qualify:
/// those match prose and file boilerplate as readily as code.
fn is_code_identifier(kw: &str) -> bool {
    if kw.contains('_') {
        return true;
    }
    let has_lower = kw.chars().any(|c| c.is_ascii_lowercase());
    let internal_upper = kw.chars().skip(1).any(|c| c.is_ascii_uppercase());
    has_lower && internal_upper
}

/// Fetch the already-resident BM25 index for `project_root` without triggering a
/// build. Returns `None` when nothing is cached yet (cold start).
fn resident_index(
    project_root: &str,
) -> Option<std::sync::Arc<crate::core::bm25_index::BM25Index>> {
    let cache = crate::tools::ctx_semantic_search::get_thread_cache()?;
    crate::core::bm25_cache::get_or_background(&cache, std::path::Path::new(project_root))
}

/// Run admitted local ranking under the caller's source authority and deadline.
/// The inherited view closes with the owning request, so a late worker cannot
/// switch to an unprotected policy or role while finishing.
fn ranked_files_budgeted(task: &str, project_root: &str, crp_mode: CrpMode) -> String {
    ranked_files_with_slots(task, project_root, crp_mode, &RANKING_WORKERS)
}

fn ranked_files_with_slots(
    task: &str,
    project_root: &str,
    crp_mode: CrpMode,
    slots: &'static tokio::sync::Semaphore,
) -> String {
    // Admit in the caller's scope: a role that denies ctx_search must not
    // obtain ranked source through compose, whatever the worker inherits.
    let role = crate::core::roles::active_role();
    if role
        .tools
        .denied
        .iter()
        .any(|denied| denied == "ctx_search")
    {
        return "ERR: source search is not authorized".into();
    }
    let Ok(slot) = slots.try_acquire() else {
        return "(local BM25 ranking busy: retry after active requests finish. Other sections are independently admitted.)".into();
    };
    let (tx, rx) = mpsc::channel::<String>();
    let task_owned = task.to_string();
    let root_owned = project_root.to_string();

    crate::core::task_spine::TaskSpine::spawn_thread(move || {
        let _slot = slot;
        let ranked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            crate::tools::ctx_semantic_search::handle_for_tool(
                "ctx_compose",
                &task_owned,
                &root_owned,
                8,
                crp_mode,
                None,
                None,
                Some("bm25"),
                Some(false),
                Some(false),
            )
        }))
        .unwrap_or_else(|_| {
            tracing::warn!("[ctx_compose: semantic ranking panicked; omitting section]");
            String::new()
        });
        // A timed-out receiver drops the result. This fresh view is never
        // persisted or substituted with a legacy shared index.
        let _ = tx.send(ranked);
    });

    match rx.recv_timeout(semantic_budget()) {
        Ok(ranked) => ranked.trim().to_string(),
        Err(_) => deferred_ranking_note().to_string(),
    }
}

/// Do not replay unadmitted index diagnostics or promise a warmed cache: each
/// local ranking view is freshly admitted, including after a timeout.
fn deferred_ranking_note() -> &'static str {
    "(local BM25 ranking deferred: source admission did not finish within this call's budget; \
     late ranking output is discarded. Other sections are independently admitted.)"
}

/// Append IB intent-specific query terms to `keywords` when basic science is on.
///
/// Additive only — never removes existing keywords. Panics and other failures
/// fall back to the original keyword list.
fn enrich_keywords_with_ib_intent(task: &str, keywords: Vec<String>) -> Vec<String> {
    if !crate::core::cognitive_gate::basic_science_enabled() {
        return keywords;
    }

    let fallback = keywords.clone();
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        use crate::core::ib::{classify_intent, intent_query_terms};
        use crate::core::session::{SessionState, TaskInfo};

        let mut session = SessionState::new();
        session.task = Some(TaskInfo {
            description: task.to_owned(),
            intent: None,
            progress_pct: None,
        });
        let intent = classify_intent(&session);
        let mut enriched = keywords;
        for term in intent_query_terms(&intent) {
            let term = term.to_string();
            if !enriched
                .iter()
                .any(|keyword| keyword.eq_ignore_ascii_case(&term))
            {
                enriched.push(term);
            }
        }
        enriched
    }))
    .unwrap_or_else(|_| {
        tracing::warn!("[ctx_compose: IB intent enrichment failed; using original keywords]");
        fallback
    })
}

/// Compose a single rich response for `task`.
pub fn handle(task: &str, project_root: &str, crp_mode: CrpMode) -> (String, usize) {
    crate::core::policy::runtime::with_project_source_view(project_root, || {
        handle_in_view(task, project_root, crp_mode)
    })
    .unwrap_or_else(|_| {
        (
            "ERROR: context withheld: source authority changed or could not be verified"
                .to_string(),
            0,
        )
    })
}

fn handle_in_view(task: &str, project_root: &str, crp_mode: CrpMode) -> (String, usize) {
    let task = task.trim();
    if task.is_empty() {
        return ("ERROR: task is required".to_string(), 0);
    }

    let protected = crate::core::policy::runtime::is_active();
    let keywords = if protected {
        extract_keywords(task, 6)
    } else {
        enrich_keywords_with_ib_intent(task, extract_keywords(task, 6))
    };
    let allow_secret = crate::core::roles::active_role().io.allow_secret_paths;

    let mut out = String::new();
    out.push_str(&format!("TASK: {task}\n"));
    if keywords.is_empty() {
        out.push_str("KEYWORDS: (none extracted — using full task for ranking)\n");
    } else {
        out.push_str(&format!("KEYWORDS: {}\n", keywords.join(", ")));
    }

    // 1. Fresh source admission precedes ranking under the same pinned view as
    //    the rest of this response, including in the bounded worker.
    out.push_str("\n## Ranked files (local BM25)\n");
    out.push_str(&ranked_files_budgeted(task, project_root, crp_mode));
    if protected {
        // These legacy stores do not retain enough original-source provenance
        // to reauthorize derived records after a policy or rights change.
        out.push_str("\nStored graph and memory enrichment withheld: source authorization is unavailable. Fresh exact matches and symbols follow.");
    }
    out.push('\n');

    // 2. Exact match locations for the most specific identifier-shaped keyword.
    // Broad prose words and acronyms create repository-wide README/Dockerfile
    // noise. Within identifiers, the resident index ranks the rarest one first.
    let ranked_keywords = if protected {
        keywords.clone()
    } else {
        order_by_specificity(&keywords, project_root)
    };
    if let Some(primary) = ranked_keywords
        .iter()
        .find(|keyword| is_code_identifier(keyword))
    {
        let grep = crate::tools::ctx_search::handle(
            primary,
            project_root,
            None,
            10,
            crp_mode,
            true,
            allow_secret,
            false,
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
    use crate::core::context_packing::CoverageItem;
    let mut snippets: Vec<String> = Vec::new();
    let mut items: Vec<CoverageItem> = Vec::new();
    for kw in &keywords {
        if let Some((rendered, toks)) =
            crate::tools::ctx_symbol::best_symbol_snippet_for_task(kw, task, project_root)
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
            if let Some(index) = snippets.iter().position(|snippet| snippet == &rendered) {
                items[index].terms.extend(terms);
                continue;
            }
            items.push(CoverageItem {
                terms,
                cost: toks.max(1),
            });
            snippets.push(rendered);
        }
    }
    if !items.is_empty() {
        let selected = crate::core::context_packing::greedy_max_coverage(
            &items,
            symbol_budget_tokens(),
            |_| 1.0,
        );
        let mut seen = std::collections::HashSet::new();
        let mut header_written = false;
        for idx in selected {
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
    if !protected {
        out.push_str(&associative_block_budgeted(project_root, &keywords));
    }

    // 5. Context Kernel enrichment — cross-store context from Knowledge,
    //    Episodic, and Procedural memory that the lexical pipeline misses.
    //    Budget: 20% of symbol budget. Graceful no-op if kernel returns None.
    if !protected {
        use crate::core::context_kernel::context_dedup::dedup_kernel_blocks;

        let budget = kernel_supplement_budget(project_root);
        if let Some(enrichment) =
            crate::core::context_kernel::bridge::kernel_enrich(task, project_root, budget)
                .filter(|enrichment| !enrichment.blocks.is_empty())
        {
            let blocks =
                dedup_kernel_blocks(&enrichment.blocks, &mut std::collections::HashSet::new());
            if !blocks.is_empty() {
                out.push_str("\n## Context Kernel\n");
                out.push_str(&blocks);
                out.push('\n');
            }
        }
    }

    let sent = count_tokens(&out);
    (out, sent)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranking_worker_keeps_original_source_policy_and_full_role() {
        let _env = crate::core::data_dir::test_env_lock();
        let policy = crate::core::policy::load(
            "name = \"compose-worker\"\nversion = \"1.0.0\"\ndescription = \"test\"\n\
             [filters]\nclassification = \"block\"\nblocked_labels = [\"CONFIDENTIAL\"]\n\
             [redaction]\ncustomer = \"ACC-1234\"\n",
        )
        .unwrap();
        let _policy = crate::core::policy::runtime::TestPolicyOverride::set(Some(policy));
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("public.rs"),
            "fn authenticate() { let customer = \"ACC-1234\"; }\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("private.rs"),
            "// CONFIDENTIAL\nfn authenticate_private() { let hidden = \"CLASSIFIED_CANARY\"; }\n",
        )
        .unwrap();
        let root = dir.path().to_str().unwrap();
        let rank = || {
            crate::core::policy::runtime::with_project_source_view(root, || {
                ranked_files_budgeted("authenticate", root, CrpMode::Off)
            })
            .unwrap()
        };
        let role = crate::core::roles::load_role("coder").unwrap();
        crate::core::roles::with_test_active_role(role.clone(), || {
            let ranked = rank();
            assert!(ranked.contains("public.rs"), "{ranked}");
            assert!(ranked.contains("REDACTED"), "{ranked}");
            assert!(!ranked.contains("ACC-1234"));
            assert!(!ranked.contains("private.rs"));
            assert!(!ranked.contains("CLASSIFIED_CANARY"));
            static ONE_SLOT: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);
            let occupied = ONE_SLOT.try_acquire().unwrap();
            let rank_with_limit = || {
                crate::core::policy::runtime::with_project_source_view(root, || {
                    ranked_files_with_slots("authenticate", root, CrpMode::Off, &ONE_SLOT)
                })
                .unwrap()
            };
            let busy = rank_with_limit();
            assert!(busy.contains("ranking busy"));
            assert!(!busy.contains("public.rs"));
            drop(occupied);
            let recovered = rank_with_limit();
            assert!(recovered.contains("public.rs"));
            assert!(recovered.contains("REDACTED"));
            assert!(!recovered.contains("CLASSIFIED_CANARY"));
        });
        let mut denied = role;
        denied.tools.denied.push("ctx_search".into());
        crate::core::roles::with_test_active_role(denied, || {
            let ranked = rank();
            assert!(ranked.starts_with("ERR:"), "{ranked}");
            assert!(!ranked.contains("public.rs"));
        });
    }

    #[test]
    fn rank_by_doc_freq_puts_rare_identifier_first() {
        // The evcc/#993 shape: an "OCPP … GetMaxCurrent" task. `Current` and
        // `OCPP` are common tokens; `GetMaxCurrent` is rare. The rare one must
        // seed the exact-match grep so it lands on code, not README/Dockerfile.
        let keywords = vec![
            "OCPP".to_string(),
            "GetMaxCurrent".to_string(),
            "Current".to_string(),
        ];
        let doc_freqs = std::collections::HashMap::from([
            ("ocpp".to_string(), 120),
            ("current".to_string(), 400),
            ("getmaxcurrent".to_string(), 3),
        ]);
        let ranked = rank_by_doc_freq(&keywords, &doc_freqs);
        assert_eq!(ranked.first().unwrap(), "GetMaxCurrent");
        assert_eq!(ranked.last().unwrap(), "Current");
    }

    #[test]
    fn rank_by_doc_freq_sinks_absent_tokens_and_is_stable() {
        // A token absent from the corpus (df 0) is useless as a grep seed and
        // must sort last; equal-df tokens keep their original order.
        let keywords = vec![
            "absent".to_string(),
            "alpha".to_string(),
            "beta".to_string(),
        ];
        let doc_freqs =
            std::collections::HashMap::from([("alpha".to_string(), 5), ("beta".to_string(), 5)]);
        let ranked = rank_by_doc_freq(&keywords, &doc_freqs);
        assert_eq!(ranked, vec!["alpha", "beta", "absent"]);
    }

    #[test]
    fn rank_prefers_code_identifier_over_rarer_prose_word() {
        // The regression the case-sensitive grep exposed: a rarer lowercase prose
        // token (`measurand`, df 4) must NOT beat a camelCase identifier
        // (`GetMaxCurrent`, df 30) as the seed — the identifier greps to code,
        // the prose word whiffs against `Measurand`.
        let keywords = vec!["measurand".to_string(), "GetMaxCurrent".to_string()];
        let doc_freqs = std::collections::HashMap::from([
            ("measurand".to_string(), 4),
            ("getmaxcurrent".to_string(), 30),
        ]);
        let ranked = rank_by_doc_freq(&keywords, &doc_freqs);
        assert_eq!(ranked.first().unwrap(), "GetMaxCurrent");
    }

    #[test]
    fn is_code_identifier_classifies_camel_snake_vs_prose_and_acronym() {
        assert!(is_code_identifier("GetMaxCurrent"));
        assert!(is_code_identifier("CurrentGetter"));
        assert!(is_code_identifier("get_max_current"));
        // Leading-cap word and all-caps acronym are not code identifiers.
        assert!(!is_code_identifier("Current"));
        assert!(!is_code_identifier("OCPP"));
        assert!(!is_code_identifier("charger"));
    }

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
    fn exact_matches_choose_specific_identifier_not_first_broad_keyword() {
        let keywords = extract_keywords(
            "OCPP charger GetMaxCurrent Current.Offered measurand CurrentGetter",
            6,
        );
        assert!(keywords.iter().any(|keyword| keyword == "GetMaxCurrent"));
        assert!(keywords.iter().any(|keyword| is_code_identifier(keyword)));
        assert!(!is_code_identifier("OCPP"));

        let prose = extract_keywords("Fix semantic ranking exact matches", 6);
        assert!(prose.iter().all(|keyword| !is_code_identifier(keyword)));
    }

    #[test]
    fn empty_task_is_rejected() {
        let (out, tok) = handle("   ", "/tmp", CrpMode::Off);
        assert!(out.starts_with("ERROR"));
        assert_eq!(tok, 0);
    }

    #[test]
    fn handle_withholds_context_when_project_authority_cannot_be_verified() {
        let root = tempfile::tempdir().unwrap();
        let missing = root.path().join("missing-project");
        let (output, tokens) = handle(
            "find authentication bugs",
            missing.to_str().unwrap(),
            CrpMode::Tdd,
        );
        assert_eq!(tokens, 0);
        assert!(output.starts_with("ERROR"));
        assert!(output.contains("source authority"));
        assert!(!output.contains("TASK:"));
    }

    #[test]
    fn deferred_ranking_note_is_deterministic_and_has_no_timing() {
        // Issue #498 / #1366: elapsed_ms must never appear in the note — it
        // varies between calls and defeats provider prompt caching.
        let a = deferred_ranking_note();
        let b = deferred_ranking_note();
        assert_eq!(a, b, "deferred note must be byte-stable across calls");
        assert!(
            !a.contains("elapsed"),
            "deferred note must not embed timing data: {a}"
        );
    }

    #[test]
    fn deferred_note_does_not_promise_cached_or_late_delivery() {
        let note = deferred_ranking_note();
        assert!(note.contains("late ranking output is discarded"));
        assert!(note.contains("independently admitted"));
        assert!(!note.contains("warming"));
        assert!(!note.contains("next call"));
    }
}
