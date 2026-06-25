//! Embedding-based scoring and budgeted MMR selection for extractive prose
//! compression.
//!
//! Two scoring modes:
//! * **Centrality** (query-free, universally safe): each segment scores as the
//!   mean cosine similarity to every other segment — LexRank-style degree
//!   centrality. Keeps the most representative sentences, drops the peripheral
//!   and the redundant. Safe even for system prompts because nothing is judged
//!   "irrelevant", only "less central".
//! * **Query**: each segment scores as cosine similarity to an anchor embedding
//!   (the task / most-recent user message). Used on RAG / research / tool paths.
//!
//! Selection is greedy by quantized score with an original-index tiebreak and a
//! Maximal-Marginal-Relevance redundancy gate (cosine ≥ [`REDUNDANCY_COSINE`] to
//! an already-kept segment ⇒ skip), reusing [`ScoringCtx`]. Protected segments
//! are always kept first. The returned indices are sorted, so the caller emits
//! kept segments in their ORIGINAL order. Deterministic by construction.

use super::segment::Segment;
use crate::core::embeddings::cosine_similarity;
use crate::core::surprise::ScoringCtx;

/// Cosine at or above which two segments are treated as semantic duplicates and
/// the later (lower-ranked) one is dropped. Matches the redundancy threshold the
/// entropy read path already uses (`core::surprise`).
const REDUNDANCY_COSINE: f64 = 0.92;

/// Which signal drives segment scoring.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RankMode {
    /// Query-free mean-cosine centrality. The default; never drops a segment for
    /// being "off-topic", so it is safe on system/user instructions.
    Centrality,
    /// Cosine to an anchor embedding (task / recent user query).
    Query,
}

/// Fixed-precision score quantization. Tiny floating-point jitter must never be
/// able to reorder two near-equal scores, or the output would not be
/// byte-stable (#498). `1e-4` is finer than any meaningful cosine gap yet coarse
/// enough to absorb residual FP noise from batched inference.
fn quantize(score: f32) -> i64 {
    (f64::from(score) * 10_000.0).round() as i64
}

/// Mean cosine of each embedding to every OTHER embedding (degree centrality).
/// `O(n²·d)`; the caller caps `n` (see [`super::MAX_SEGMENTS`]).
pub(super) fn centrality_scores(embs: &[Vec<f32>]) -> Vec<f32> {
    let n = embs.len();
    if n <= 1 {
        return vec![0.0; n];
    }
    let mut scores = vec![0.0f32; n];
    for i in 0..n {
        let mut sum = 0.0f32;
        for j in 0..n {
            if i != j {
                sum += cosine_similarity(&embs[i], &embs[j]);
            }
        }
        scores[i] = sum / (n as f32 - 1.0);
    }
    scores
}

/// Cosine of each embedding to the query anchor.
pub(super) fn query_scores(embs: &[Vec<f32>], anchor: &[f32]) -> Vec<f32> {
    embs.iter().map(|e| cosine_similarity(e, anchor)).collect()
}

/// Per-segment char cost, including the one separator char it adds on re-emit.
fn cost_of(seg: &Segment) -> usize {
    seg.text.len() + 1
}

/// Select the segment indices to keep, within `budget_chars`, applying the MMR
/// redundancy gate. Returns indices sorted ascending (original order).
///
/// `embs[i]` MUST align with `segs[i]`. Protected segments are always kept.
pub(super) fn select(
    segs: &[Segment],
    embs: &[Vec<f32>],
    mode: RankMode,
    anchor: Option<&[f32]>,
    budget_chars: usize,
) -> Vec<usize> {
    debug_assert_eq!(segs.len(), embs.len());
    let n = segs.len();

    let scores = match mode {
        RankMode::Centrality => centrality_scores(embs),
        RankMode::Query => {
            let Some(a) = anchor else {
                return Vec::new();
            };
            query_scores(embs, a)
        }
    };

    let mut kept: Vec<usize> = Vec::new();
    let mut ctx = ScoringCtx::new();
    let mut used = 0usize;

    // 1) Protected segments are kept verbatim and seed the redundancy window.
    for (i, seg) in segs.iter().enumerate() {
        if seg.protected {
            kept.push(i);
            used += cost_of(seg);
            ctx.push_kept(embs[i].clone());
        }
    }

    // 2) Rank the rest by quantized score (desc) with an original-index tiebreak.
    let mut ranked: Vec<usize> = (0..n).filter(|&i| !segs[i].protected).collect();
    ranked.sort_by(|&a, &b| {
        quantize(scores[b])
            .cmp(&quantize(scores[a]))
            .then(a.cmp(&b))
    });

    // 3) Greedily add under budget, skipping near-duplicates (MMR). Lower-ranked
    //    but shorter segments may still fit after a long one is skipped.
    for i in ranked {
        let cost = cost_of(&segs[i]);
        if used + cost > budget_chars && !kept.is_empty() {
            continue;
        }
        if ctx.max_cosine(&embs[i]) >= REDUNDANCY_COSINE {
            continue;
        }
        kept.push(i);
        used += cost;
        ctx.push_kept(embs[i].clone());
    }

    kept.sort_unstable();
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a tiny unit-norm embedding so cosine math is exact and the tests
    /// need no ONNX engine.
    fn unit(v: [f32; 3]) -> Vec<f32> {
        let norm = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
        vec![v[0] / norm, v[1] / norm, v[2] / norm]
    }

    fn seg(idx: usize, text: &str, protected: bool) -> Segment {
        Segment {
            idx,
            para: 0,
            text: text.to_string(),
            protected,
        }
    }

    #[test]
    fn centrality_ranks_the_outlier_lowest() {
        // Three near-identical vectors + one orthogonal outlier.
        let embs = vec![
            unit([1.0, 0.0, 0.0]),
            unit([0.98, 0.02, 0.0]),
            unit([0.97, 0.0, 0.03]),
            unit([0.0, 0.0, 1.0]),
        ];
        let scores = centrality_scores(&embs);
        let outlier = scores
            .iter()
            .enumerate()
            .min_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .unwrap()
            .0;
        assert_eq!(outlier, 3, "the orthogonal vector is least central");
    }

    #[test]
    fn query_mode_ranks_by_anchor() {
        let embs = vec![unit([1.0, 0.0, 0.0]), unit([0.0, 1.0, 0.0])];
        let anchor = unit([0.9, 0.1, 0.0]);
        let scores = query_scores(&embs, &anchor);
        assert!(scores[0] > scores[1], "segment aligned with anchor wins");
    }

    #[test]
    fn select_keeps_protected_and_central_drops_peripheral() {
        // idx0 protected; idx1/idx2 central; idx3 peripheral. Budget fits 3.
        let segs = vec![
            seg(0, "PROTECTED", true),
            seg(1, "central a", false),
            seg(2, "central b", false),
            seg(3, "peripheral", false),
        ];
        // idx1/idx2 sit near the mass (high centrality) but are NOT duplicates
        // (cos = -0.28 < REDUNDANCY_COSINE); idx3 is orthogonal (peripheral).
        let embs = vec![
            unit([1.0, 0.0, 0.0]),
            unit([0.6, 0.8, 0.0]),
            unit([0.6, -0.8, 0.0]),
            unit([0.0, 0.0, 1.0]),
        ];
        let budget = "PROTECTED".len() + "central a".len() + "central b".len() + 3;
        let kept = select(&segs, &embs, RankMode::Centrality, None, budget);
        assert!(kept.contains(&0), "protected always kept");
        assert!(kept.contains(&1) && kept.contains(&2), "central kept");
        assert!(!kept.contains(&3), "peripheral dropped under budget");
        // Sorted ascending → original order on re-emit.
        let mut sorted = kept.clone();
        sorted.sort_unstable();
        assert_eq!(kept, sorted);
    }

    #[test]
    fn mmr_drops_near_duplicate_segment() {
        let segs = vec![
            seg(0, "unique sentence one", false),
            seg(1, "duplicate", false),
            seg(2, "duplicate copy", false),
        ];
        // idx1 and idx2 are identical vectors → one must be dropped.
        let embs = vec![
            unit([1.0, 0.0, 0.0]),
            unit([0.0, 1.0, 0.0]),
            unit([0.0, 1.0, 0.0]),
        ];
        let kept = select(&segs, &embs, RankMode::Centrality, None, 10_000);
        let dups = [1usize, 2].iter().filter(|i| kept.contains(i)).count();
        assert_eq!(dups, 1, "MMR keeps only one of the duplicates");
    }

    #[test]
    fn select_is_deterministic() {
        let segs = vec![
            seg(0, "alpha", false),
            seg(1, "beta", false),
            seg(2, "gamma", false),
        ];
        let embs = vec![
            unit([1.0, 0.1, 0.0]),
            unit([0.9, 0.2, 0.0]),
            unit([0.0, 0.0, 1.0]),
        ];
        let a = select(&segs, &embs, RankMode::Centrality, None, 12);
        let b = select(&segs, &embs, RankMode::Centrality, None, 12);
        assert_eq!(a, b);
    }
}
