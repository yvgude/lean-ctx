//! Unit + determinism (#498) tests and the daemon-free quality benchmark for
//! the extractive ranker. Split out of `mod.rs` so the module file stays focused
//! on the public API.

use super::*;

/// Deterministic hash-bag pseudo-embedding so the determinism guard needs no
/// ONNX engine. Identical sentences map to identical vectors (so the MMR gate
/// fires), and `DefaultHasher::new()` is fixed-seed, so it is stable across
/// runs and processes.
fn synth_embed(s: &str) -> Vec<f32> {
    let mut v = vec![0.0f32; 16];
    for w in s.to_lowercase().split_whitespace() {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        std::hash::Hash::hash(&w, &mut h);
        v[(std::hash::Hasher::finish(&h) % 16) as usize] += 1.0;
    }
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in &mut v {
            *x /= norm;
        }
    }
    v
}

#[test]
fn pipeline_is_byte_identical_across_runs() {
    // #498 guard: segment → score → quantize → MMR select → reassemble is a
    // pure function. Real embeddings are run-to-run stable on a host, so the
    // byte-stability of the full path reduces to this offline check.
    let text = "Cache eviction uses an LRU policy. The proxy compresses prose. \
                Cache eviction uses an LRU policy. Logs are redacted at the edge. \
                The proxy compresses prose deterministically. Some unrelated chatter.";
    let segs = segment::segment(text);
    let embs: Vec<Vec<f32>> = segs.iter().map(|s| synth_embed(&s.text)).collect();
    let run = || {
        let kept = ranker::select(&segs, &embs, RankMode::Centrality, None, 120);
        let sel: Vec<&segment::Segment> = kept.iter().map(|&i| &segs[i]).collect();
        segment::reassemble(&sel)
    };
    let first = run();
    for _ in 0..10 {
        assert_eq!(run(), first, "extractive pipeline must be byte-identical");
    }
    assert!(first.len() < text.len(), "must actually compress");
    // The duplicated sentence survives at most once (MMR).
    assert_eq!(
        first.matches("LRU policy").count(),
        1,
        "near-duplicate sentence deduplicated"
    );
}

/// Cosine of two equal-length vectors; `0.0` for a degenerate (zero) vector.
#[cfg(feature = "embeddings")]
fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na * nb)
    }
}

/// Coverage of the full document by a compressed string: for every full-doc
/// sentence, the cosine to its *nearest kept sentence*, averaged. `1.0` = the
/// kept set has a close match for every original sentence.
///
/// This is the fair extractive-quality proxy. Unlike a centroid cosine — which
/// rewards redundancy (a prefix dominated by the most frequent theme tracks the
/// whole-doc centroid, while MMR de-duplication diversifies away from it) —
/// coverage rewards spreading selection across the whole document and penalizes
/// dropping the second half wholesale (which prefix truncation always does).
/// `None` when the output has no embeddable segments.
#[cfg(feature = "embeddings")]
fn coverage(
    engine: &crate::core::embeddings::EmbeddingEngine,
    full_embs: &[Vec<f32>],
    out: &str,
) -> Option<f32> {
    let segs = segment::segment(out);
    if segs.is_empty() || full_embs.is_empty() {
        return None;
    }
    let texts: Vec<&str> = segs.iter().map(|s| s.text.as_str()).collect();
    let kept = engine.embed_batch(&texts).ok()?;
    if kept.is_empty() {
        return None;
    }
    let mut sum = 0.0f32;
    for f in full_embs {
        let best = kept.iter().map(|k| cosine(f, k)).fold(f32::MIN, f32::max);
        sum += best;
    }
    #[allow(clippy::cast_precision_loss)]
    Some(sum / full_embs.len() as f32)
}

/// Recall of a query against a compressed string: the cosine of `query_emb`
/// to its *nearest kept sentence*. `1.0` = the answer survived compression.
/// This is the canonical RAG/research signal: when the answer lives in the
/// back half of a document, prefix truncation drops it (low recall) while
/// query-aware extraction keeps it (high recall). `None` when the output has
/// no embeddable segments.
#[cfg(feature = "embeddings")]
fn query_recall(
    engine: &crate::core::embeddings::EmbeddingEngine,
    query_emb: &[f32],
    out: &str,
) -> Option<f32> {
    let segs = segment::segment(out);
    if segs.is_empty() {
        return None;
    }
    let texts: Vec<&str> = segs.iter().map(|s| s.text.as_str()).collect();
    let kept = engine.embed_batch(&texts).ok()?;
    kept.iter()
        .map(|k| cosine(query_emb, k))
        .fold(None, |acc: Option<f32>, c| {
            Some(acc.map_or(c, |a| a.max(c)))
        })
}

/// Deterministically pick a real query from the *back half* of a document:
/// the longest sentence at or after the midpoint (tiebreak: earliest index).
/// Long, late sentences carry the specific detail a RAG question targets and
/// are exactly what prefix truncation discards — a fair, real query, not a
/// fabricated one.
#[cfg(feature = "embeddings")]
fn back_half_query(doc: &str) -> Option<String> {
    let segs = segment::segment(doc);
    if segs.len() < 4 {
        return None;
    }
    let mid = segs.len() / 2;
    segs[mid..]
        .iter()
        .max_by_key(|s| s.text.chars().count())
        .map(|s| s.text.clone())
}

/// Head-to-head: extractive (centrality) vs truncation over a real prose
/// corpus, reporting token savings AND a meaning-retention fidelity for each.
/// `#[ignore]`d (needs the embedding model + is slow); reproduce with:
/// `cargo test -p lean-ctx --lib --features embeddings \
///  core::extractive::tests::bench_extractive_vs_truncation -- --ignored --nocapture`.
#[cfg(feature = "embeddings")]
#[test]
#[ignore = "benchmark; needs the embedding model; run with --ignored --nocapture"]
#[allow(clippy::cast_precision_loss)]
fn bench_extractive_vs_truncation() {
    use crate::core::tokens::count_tokens;
    use crate::core::web::distill::squeeze_prose;
    use std::path::Path;
    use std::time::Instant;

    // Force a blocking engine load (the non-blocking path is suppressed under
    // cargo test). If the model is absent (e.g. CI without download), report
    // honestly and skip rather than fabricate a result.
    let Some(engine) = crate::core::embeddings::shared_engine() else {
        println!(
            "{{\"available\": false, \"reason\": \"embedding model not loaded; run after `lean-ctx` has the model\"}}"
        );
        return;
    };

    let corpus = Path::new(env!("CARGO_MANIFEST_DIR")).join("../docs/reference");
    let mut docs: Vec<String> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&corpus) {
        let mut paths: Vec<_> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("md"))
            .collect();
        paths.sort();
        for path in paths.into_iter().take(20) {
            if let Ok(text) = std::fs::read_to_string(&path) {
                // Cap to keep the O(n²) centrality affordable per doc.
                let capped: String = text.chars().take(8_000).collect();
                if capped.trim().len() > 800 {
                    docs.push(capped);
                }
            }
        }
    }
    assert!(!docs.is_empty(), "no corpus files found at {corpus:?}");

    let (mut n, mut sum_ratio_t, mut sum_ratio_e) = (0u32, 0.0f64, 0.0f64);
    let (mut sum_cov_t, mut sum_cov_e, mut cov_n) = (0.0f64, 0.0f64, 0u32);
    let (mut sum_rec_t, mut sum_rec_e, mut rec_n) = (0.0f64, 0.0f64, 0u32);
    let (mut t_ms, mut e_ms) = (0.0f64, 0.0f64);

    for doc in &docs {
        let budget = doc.len() / 2; // shrink to ~50% so both must drop content

        let s_t = Instant::now();
        let trunc = squeeze_prose(doc, budget);
        t_ms += s_t.elapsed().as_secs_f64() * 1000.0;

        let s_e = Instant::now();
        let extr = rank_and_squeeze(doc, budget, RankMode::Centrality, None);
        e_ms += s_e.elapsed().as_secs_f64() * 1000.0;
        // No engine output (too few/many segments) → fall back so the row is
        // still comparable to truncation (this is the production behaviour).
        let extr = extr.unwrap_or_else(|| trunc.clone());

        let orig_tok = count_tokens(doc).max(1);
        sum_ratio_t += 1.0 - count_tokens(&trunc) as f64 / orig_tok as f64;
        sum_ratio_e += 1.0 - count_tokens(&extr) as f64 / orig_tok as f64;
        n += 1;

        // Coverage of the full document by each method's kept sentences
        // (query-free centrality vs truncation).
        let full_segs = segment::segment(doc);
        let full_texts: Vec<&str> = full_segs.iter().map(|s| s.text.as_str()).collect();
        if let Ok(full_embs) = engine.embed_batch(&full_texts)
            && let (Some(ct), Some(ce)) = (
                coverage(engine, &full_embs, &trunc),
                coverage(engine, &full_embs, &extr),
            )
        {
            sum_cov_t += f64::from(ct);
            sum_cov_e += f64::from(ce);
            cov_n += 1;
        }

        // RAG scenario: a real back-half sentence as the query. Truncation
        // keeps only the prefix, so a back-half answer is lost; query-aware
        // extraction is designed to retain it. This is the documented
        // highest-value path (research/tool-result prose).
        if let Some(query) = back_half_query(doc)
            && let Ok(query_emb) = engine.embed_query(&query)
        {
            let extr_q = rank_and_squeeze(doc, budget, RankMode::Query, Some(&query))
                .unwrap_or_else(|| trunc.clone());
            if let (Some(rt), Some(re)) = (
                query_recall(engine, &query_emb, &trunc),
                query_recall(engine, &query_emb, &extr_q),
            ) {
                sum_rec_t += f64::from(rt);
                sum_rec_e += f64::from(re);
                rec_n += 1;
            }
        }
    }

    let avg = |s: f64, d: u32| {
        if d == 0 {
            0.0
        } else {
            (s / f64::from(d) * 1e4).round() / 1e4
        }
    };
    let report = serde_json::json!({
        "available": true,
        "corpus": corpus.to_string_lossy(),
        "docs": n,
        "budget": "50% of chars",
        "truncate": {
            "avg_saved_ratio": avg(sum_ratio_t, n),
            "avg_coverage": avg(sum_cov_t, cov_n),
            "total_ms": (t_ms * 100.0).round() / 100.0,
        },
        "extractive_centrality": {
            "avg_saved_ratio": avg(sum_ratio_e, n),
            "avg_coverage": avg(sum_cov_e, cov_n),
            "total_ms": (e_ms * 100.0).round() / 100.0,
        },
        "coverage_docs_scored": cov_n,
        "rag_query_recall": {
            "truncate": avg(sum_rec_t, rec_n),
            "extractive_query": avg(sum_rec_e, rec_n),
            "docs_scored": rec_n,
            "query": "longest real sentence in the document's back half",
        },
        "note": "avg_coverage = mean over full-doc sentences of cosine to nearest kept sentence (query-free); rag_query_recall = cosine of a real back-half query to its nearest kept sentence (higher = answer survived). Truncation is a strong baseline on front-loaded structured docs; query-aware extraction wins when the answer is not in the prefix.",
    });
    println!("{}", serde_json::to_string_pretty(&report).unwrap());
}

#[test]
fn deterministic_and_never_inflates() {
    // In `cargo test` the ONNX engine is intentionally never loaded
    // (`background_load_allowed` is false), so this exercises the graceful
    // no-engine path: both calls return None and are byte-equal. On a host
    // with a warm engine the same equality assertion enforces #498.
    let text = "Alpha covers the cache layer. Beta is unrelated chatter. \
                Alpha also covers eviction. Gamma is more chatter."
        .repeat(4);
    let a = rank_and_squeeze(&text, 64, RankMode::Centrality, None);
    let b = rank_and_squeeze(&text, 64, RankMode::Centrality, None);
    assert_eq!(a, b, "rank_and_squeeze must be a pure function of inputs");
    if let Some(out) = a {
        assert!(out.len() < text.len(), "anti-inflation: never larger");
    }
}
