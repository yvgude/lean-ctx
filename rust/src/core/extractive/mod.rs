//! Extractive prose compression: keep the most informative sentences within a
//! char budget instead of truncating to the prefix.
//!
//! This is the premium replacement for [`crate::core::web::distill::squeeze_prose`]'s
//! FIFO prefix truncation. It reuses the embedding model lean-ctx already ships
//! (all-MiniLM-L6-v2, 384d, the `embeddings` feature) — **no new model, no new
//! heavy dependency**.
//!
//! ## Determinism (#498)
//!
//! For a fixed `(text, budget, mode, anchor, model_version)` the output is
//! byte-identical. Guaranteed by:
//! 1. pure, allocation-only segmentation (the `segment` module);
//! 2. embeddings that are run-to-run stable on a given build/host;
//! 3. fixed-precision score quantization with an original-index tiebreak
//!    (the `ranker` module); and
//! 4. re-emitting kept segments in their ORIGINAL order.
//!
//! A regression test asserts the byte-stability empirically.
//!
//! ## Graceful fallback
//!
//! When the embedding engine is unavailable — `embeddings` feature off, model
//! not yet loaded, or `memory_profile=low` — [`rank_and_squeeze`] returns
//! `None`, and callers fall back to the deterministic truncating squeeze. No
//! build or OS regresses.

mod ranker;
mod segment;

pub use ranker::RankMode;

/// Too few units to rank meaningfully → let the caller fall back.
#[cfg(feature = "embeddings")]
const MIN_SEGMENTS: usize = 3;
/// Centrality is `O(n²·d)`; above this many segments the cost is not worth it
/// for a prose block, so we fall back to the linear truncating squeeze.
#[cfg(feature = "embeddings")]
const MAX_SEGMENTS: usize = 512;

/// Rank the sentences of `text` and keep the highest-value ones within
/// `budget_chars`, emitted in original order.
///
/// Returns `Some(compressed)` only when the embedding engine is available AND
/// the result is actually smaller than the input (anti-inflation). Returns
/// `None` otherwise — the caller then falls back to a truncating squeeze or
/// leaves the text verbatim.
///
/// `anchor` is required for [`RankMode::Query`] (the task / recent user query)
/// and ignored for [`RankMode::Centrality`].
#[cfg(feature = "embeddings")]
#[must_use]
pub fn rank_and_squeeze(
    text: &str,
    budget_chars: usize,
    mode: RankMode,
    anchor: Option<&str>,
) -> Option<String> {
    let engine = crate::tools::ctx_knowledge::embeddings::embedding_engine_nonblocking()?;

    let segs = segment::segment(text);
    if segs.len() < MIN_SEGMENTS || segs.len() > MAX_SEGMENTS {
        return None;
    }

    let texts: Vec<&str> = segs.iter().map(|s| s.text.as_str()).collect();
    let embs = engine.embed_batch(&texts).ok()?;
    if embs.len() != segs.len() {
        return None;
    }

    let anchor_emb = match mode {
        RankMode::Query => Some(engine.embed_query(anchor?).ok()?),
        RankMode::Centrality => None,
    };

    let kept = ranker::select(&segs, &embs, mode, anchor_emb.as_deref(), budget_chars);
    if kept.is_empty() {
        return None;
    }

    let selected: Vec<&segment::Segment> = kept.iter().map(|&i| &segs[i]).collect();
    let out = segment::reassemble(&selected);

    (out.len() < text.len()).then_some(out)
}

/// Stub when the `embeddings` feature is disabled: always falls back.
#[cfg(not(feature = "embeddings"))]
#[must_use]
pub fn rank_and_squeeze(
    _text: &str,
    _budget_chars: usize,
    _mode: RankMode,
    _anchor: Option<&str>,
) -> Option<String> {
    None
}

#[cfg(test)]
mod tests;
