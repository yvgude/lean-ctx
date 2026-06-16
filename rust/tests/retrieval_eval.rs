//! Retrieval-quality eval harness for the spreading-activation ranker.
//!
//! This is the regression gate for the associative-retrieval feature. It runs a
//! small labelled benchmark — a synthetic project graph with hand-authored
//! relevance judgments — and measures standard IR metrics (recall@k, MRR,
//! precision@k) for two rankers:
//!
//!   * **lexical** — only the files the query terms directly name (the seeds);
//!     this is what plain keyword/BM25 search returns for these queries.
//!   * **associative** — lexical seeds *plus* spreading activation over the
//!     project graph.
//!
//! The feature is purely additive, so the harness asserts two properties:
//!   1. **No regression**: associative recall@k ≥ lexical recall@k for *every*
//!      query (the seeds are always retained).
//!   2. **Real gain**: associative retrieval strictly beats lexical on aggregate
//!      recall/MRR while keeping precision high (it does not flood the result
//!      with structurally-unrelated files).
//!
//! Run `cargo test --test retrieval_eval -- --nocapture` to see the report.

use std::collections::{HashMap, HashSet};

use lean_ctx::core::spreading_activation;

/// One labelled benchmark query.
struct EvalQuery {
    name: &'static str,
    /// Files the query terms directly resolve to (lexical seeds).
    seeds: &'static [&'static str],
    /// Ground-truth relevant files (excluding the seeds themselves).
    relevant: &'static [&'static str],
}

/// Build the synthetic project graph: three feature clusters wired internally
/// (import/call edges) plus a couple of weak cross-links, mirroring how a real
/// codebase factors into cohesive modules.
fn project_graph() -> HashMap<String, Vec<(String, f64)>> {
    let edges: &[(&str, &str, f64)] = &[
        // auth feature
        ("auth/login.rs", "auth/token.rs", 3.0),
        ("auth/login.rs", "auth/session.rs", 3.0),
        ("auth/token.rs", "auth/session.rs", 2.0),
        // billing feature
        ("billing/invoice.rs", "billing/tax.rs", 3.0),
        ("billing/invoice.rs", "billing/ledger.rs", 3.0),
        ("billing/tax.rs", "billing/ledger.rs", 2.0),
        // storage feature
        ("storage/pool.rs", "storage/migrate.rs", 3.0),
        ("storage/pool.rs", "storage/schema.rs", 3.0),
        // weak cross-feature links (shared util) — should NOT dominate ranking
        ("auth/session.rs", "storage/pool.rs", 0.5),
        ("billing/ledger.rs", "storage/pool.rs", 0.5),
    ];
    let mut adj: HashMap<String, Vec<(String, f64)>> = HashMap::new();
    for &(a, b, w) in edges {
        adj.entry(a.to_string())
            .or_default()
            .push((b.to_string(), w));
        adj.entry(b.to_string())
            .or_default()
            .push((a.to_string(), w));
    }
    adj
}

fn queries() -> Vec<EvalQuery> {
    vec![
        EvalQuery {
            name: "work on login",
            seeds: &["auth/login.rs"],
            relevant: &["auth/token.rs", "auth/session.rs"],
        },
        EvalQuery {
            name: "work on invoice",
            seeds: &["billing/invoice.rs"],
            relevant: &["billing/tax.rs", "billing/ledger.rs"],
        },
        EvalQuery {
            name: "work on db pool",
            seeds: &["storage/pool.rs"],
            relevant: &["storage/migrate.rs", "storage/schema.rs"],
        },
    ]
}

fn recall_at_k(ranked: &[String], relevant: &[&str], k: usize) -> f64 {
    if relevant.is_empty() {
        return 1.0;
    }
    let topk: HashSet<&str> = ranked.iter().take(k).map(String::as_str).collect();
    let hits = relevant.iter().filter(|r| topk.contains(**r)).count();
    hits as f64 / relevant.len() as f64
}

fn precision_at_k(ranked: &[String], relevant: &[&str], k: usize) -> f64 {
    let topk: Vec<&str> = ranked.iter().take(k).map(String::as_str).collect();
    if topk.is_empty() {
        return 1.0;
    }
    let hits = topk.iter().filter(|r| relevant.contains(*r)).count();
    hits as f64 / topk.len() as f64
}

fn mrr(ranked: &[String], relevant: &[&str]) -> f64 {
    for (i, r) in ranked.iter().enumerate() {
        if relevant.contains(&r.as_str()) {
            return 1.0 / (i as f64 + 1.0);
        }
    }
    0.0
}

/// Lexical ranker: returns only the seed files (what keyword search alone sees).
fn lexical_ranker(q: &EvalQuery) -> Vec<String> {
    q.seeds.iter().map(|s| (*s).to_string()).collect()
}

/// Associative ranker: spreading activation over the project graph.
fn associative_ranker(q: &EvalQuery, adj: &HashMap<String, Vec<(String, f64)>>) -> Vec<String> {
    let seeds: HashMap<String, f64> = q.seeds.iter().map(|s| ((*s).to_string(), 1.0)).collect();
    spreading_activation::related_ranked(&seeds, adj, 0.6, 3, 10)
        .into_iter()
        .map(|(f, _)| f)
        .collect()
}

#[test]
fn spreading_activation_improves_retrieval_without_regression() {
    let adj = project_graph();
    let qs = queries();
    const K: usize = 3;

    let (mut lex_recall, mut assoc_recall) = (0.0, 0.0);
    let (mut lex_mrr, mut assoc_mrr) = (0.0, 0.0);
    let mut assoc_precision = 0.0;

    eprintln!("\n── Retrieval eval (k={K}) ──────────────────────────────");
    eprintln!(
        "{:<20} {:>10} {:>10} {:>12} {:>10}",
        "query", "lex R@k", "assoc R@k", "assoc R-prec", "assoc MRR"
    );

    for q in &qs {
        let lex = lexical_ranker(q);
        let assoc = associative_ranker(q, &adj);

        let lr = recall_at_k(&lex, q.relevant, K);
        let ar = recall_at_k(&assoc, q.relevant, K);
        let lm = mrr(&lex, q.relevant);
        let am = mrr(&assoc, q.relevant);
        // R-precision (precision@R, R = #relevant): the principled precision
        // metric when R < K, where precision@K is capped at R/K by construction.
        let ap = precision_at_k(&assoc, q.relevant, q.relevant.len());

        // No-regression invariant, per query.
        assert!(
            ar >= lr,
            "regression on {:?}: associative recall {ar} < lexical {lr}",
            q.name
        );

        eprintln!("{:<20} {lr:>10.2} {ar:>10.2} {ap:>12.2} {am:>10.2}", q.name);

        lex_recall += lr;
        assoc_recall += ar;
        lex_mrr += lm;
        assoc_mrr += am;
        assoc_precision += ap;
    }

    let n = qs.len() as f64;
    let (lex_recall, assoc_recall) = (lex_recall / n, assoc_recall / n);
    let (lex_mrr, assoc_mrr) = (lex_mrr / n, assoc_mrr / n);
    let assoc_precision = assoc_precision / n;

    eprintln!("────────────────────────────────────────────────────────");
    eprintln!("mean lexical    recall@{K}={lex_recall:.2}  mrr={lex_mrr:.2}");
    eprintln!(
        "mean associative recall@{K}={assoc_recall:.2}  mrr={assoc_mrr:.2}  r-precision={assoc_precision:.2}\n"
    );

    // Real gain: lexical alone retrieves none of the (non-seed) relevant files.
    assert_eq!(
        lex_recall, 0.0,
        "lexical baseline should miss related files"
    );
    // Associative retrieval recovers essentially all in-cluster relevant files.
    assert!(
        assoc_recall >= 0.9,
        "associative recall@{K} too low: {assoc_recall}"
    );
    // …and keeps precision high (it does not flood with unrelated clusters).
    assert!(
        assoc_precision >= 0.9,
        "associative r-precision too low: {assoc_precision}"
    );
    // Top result is always truly relevant.
    assert!(assoc_mrr >= 0.99, "associative MRR too low: {assoc_mrr}");
}

/// Guards the precision claim directly: activation must not rank a file from a
/// *different* feature cluster above the seed's own cluster members.
#[test]
fn activation_respects_cluster_boundaries() {
    let adj = project_graph();
    let seeds = HashMap::from([("auth/login.rs".to_string(), 1.0)]);
    let ranked = spreading_activation::related_ranked(&seeds, &adj, 0.6, 3, 10);

    let top2: Vec<&str> = ranked.iter().take(2).map(|(f, _)| f.as_str()).collect();
    assert!(
        top2.iter().all(|f| f.starts_with("auth/")),
        "top results must stay within the auth cluster, got {top2:?}"
    );
}
