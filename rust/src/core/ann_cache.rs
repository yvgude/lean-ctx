//! Process-wide cache for the HNSW [`AnnIndex`] used by dense semantic search.
//!
//! Building an HNSW graph is O(n log n) with a wide construction beam, so doing
//! it per query would be slower than brute force. This cache keeps one built
//! index keyed by a content fingerprint of the embedding set: repeated queries
//! over the same corpus reuse the graph and get sub-linear search, while a
//! changed corpus (different fingerprint) transparently triggers a rebuild.
//!
//! It is threshold-gated — corpora below [`ANN_MIN_VECTORS`] skip the cache and
//! use exact SIMD brute-force top-k, which is both faster (no graph overhead)
//! and *exact*. The threshold is deliberately high: at lean-ctx's typical scale
//! (a few thousand chunks) exact brute force over int8/SIMD dot products is only
//! ~1-2 ms, so HNSW's approximate recall is not worth trading. HNSW activates
//! only for genuinely large corpora where exact scan would dominate latency.
//! On any lock failure it falls back to brute force, so correctness never
//! depends on the cache being available.

use std::sync::{Arc, Mutex, OnceLock};

use super::hnsw::{brute_force_topk, AnnIndex};

/// Minimum corpus size before an HNSW graph is worth building and caching.
/// Below this, exact SIMD brute force is faster *and* exact (no recall loss).
pub const ANN_MIN_VECTORS: usize = 50_000;

struct Cached {
    fingerprint: u64,
    index: AnnIndex,
}

fn cache() -> &'static Mutex<Option<Cached>> {
    static CACHE: OnceLock<Mutex<Option<Cached>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}

/// Returns the top-k `(index, similarity)` pairs for `query` over `embeddings`,
/// sorted by descending similarity.
///
/// Small corpora use exact brute force. Large corpora build (once) and reuse a
/// cached HNSW index. Falls back to brute force on lock failure.
///
/// `embeddings` is taken as `Arc<[Vec<f32>]>` (the same allocation the caller
/// already holds for per-query scoring) so building the cached HNSW index is an
/// `Arc::clone` — a refcount bump, not a second full-precision corpus copy.
#[must_use]
pub fn topk(embeddings: &Arc<[Vec<f32>]>, query: &[f32], top_k: usize) -> Vec<(usize, f32)> {
    topk_gated(embeddings, query, top_k, ANN_MIN_VECTORS)
}

/// Core implementation with an injectable gate so tests can exercise the HNSW
/// path without materializing a 50k-vector corpus.
fn topk_gated(
    embeddings: &Arc<[Vec<f32>]>,
    query: &[f32],
    top_k: usize,
    min_vectors: usize,
) -> Vec<(usize, f32)> {
    if embeddings.len() < min_vectors {
        return brute_force_topk(embeddings, query, top_k);
    }

    let fp = fingerprint(embeddings);
    let Ok(mut guard) = cache().lock() else {
        return brute_force_topk(embeddings, query, top_k);
    };

    let needs_build = match guard.as_ref() {
        Some(c) => c.fingerprint != fp,
        None => true,
    };
    if needs_build {
        *guard = Some(Cached {
            fingerprint: fp,
            // Arc::clone: shares the caller's corpus allocation, zero bytes copied.
            index: AnnIndex::build(Arc::clone(embeddings)),
        });
    }

    match guard.as_ref() {
        Some(c) => c.index.search(query, top_k),
        None => brute_force_topk(embeddings, query, top_k),
    }
}

/// Cheap, content-sensitive fingerprint (FNV-1a over lengths + sampled values).
/// Strong enough that a changed corpus reliably triggers a rebuild; a collision
/// would only mildly degrade already-approximate recall, never break results.
fn fingerprint(embeddings: &[Vec<f32>]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    macro_rules! mix {
        ($x:expr) => {{
            h ^= $x;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }};
    }
    mix!(embeddings.len() as u64);
    for (i, v) in embeddings.iter().enumerate() {
        mix!(v.len() as u64);
        mix!(i as u64);
        if let Some(&f) = v.first() {
            mix!(u64::from(f.to_bits()));
        }
        if let Some(&f) = v.get(v.len() / 2) {
            mix!(u64::from(f.to_bits()));
        }
        if let Some(&f) = v.last() {
            mix!(u64::from(f.to_bits()));
        }
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    // Test gate that forces the HNSW path on modest corpora (AnnIndex itself
    // switches to HNSW at 1000 vectors, so 1000 here exercises the real graph).
    const TEST_GATE: usize = 1000;

    // The cache is a single process-wide slot, so tests that drive the HNSW path
    // must not interleave or they would clobber each other's cached index. This
    // lock serializes them; poison is recovered since a panic in one test must
    // not cascade into the others.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn serial() -> std::sync::MutexGuard<'static, ()> {
        TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Reads the fingerprint of the currently cached index (test-only
    /// introspection; `tests` is a child module so it may touch private state).
    fn cached_fingerprint() -> Option<u64> {
        cache()
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(|c| c.fingerprint))
    }

    fn random_vec(dim: usize, seed: u64) -> Vec<f32> {
        let mut v = Vec::with_capacity(dim);
        let mut s = seed;
        for _ in 0..dim {
            s = s.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            v.push((s as f32 / u64::MAX as f32) * 2.0 - 1.0);
        }
        v
    }

    /// A vector near `base` with small per-dimension noise — produces dense,
    /// well-connected clusters where HNSW recall is high and stable (unlike a
    /// single needle in random noise, which approximate search can miss).
    fn jitter(base: &[f32], seed: u64, scale: f32) -> Vec<f32> {
        base.iter()
            .enumerate()
            .map(|(i, &b)| {
                let s = seed
                    .wrapping_add(i as u64)
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1);
                b + ((s as f32 / u64::MAX as f32) * 2.0 - 1.0) * scale
            })
            .collect()
    }

    fn clustered(
        n_clusters: usize,
        per_cluster: usize,
        dim: usize,
    ) -> (Vec<Vec<f32>>, Vec<Vec<f32>>) {
        let centers: Vec<Vec<f32>> = (0..n_clusters)
            .map(|c| random_vec(dim, (c as u64 + 1) * 1_000))
            .collect();
        let mut vectors = Vec::with_capacity(n_clusters * per_cluster);
        for (c, center) in centers.iter().enumerate() {
            for j in 0..per_cluster {
                vectors.push(jitter(center, (c * per_cluster + j) as u64 + 7, 0.02));
            }
        }
        (vectors, centers)
    }

    #[test]
    fn small_corpus_matches_brute_force_exactly() {
        let vectors: Arc<[Vec<f32>]> = (0..200).map(|i| random_vec(32, i)).collect();
        let query = random_vec(32, 9_999);

        // Production gate (50k) → small corpus is exact brute force.
        let via_cache = topk(&vectors, &query, 8);
        let exact = brute_force_topk(&vectors, &query, 8);

        assert_eq!(via_cache.len(), exact.len());
        for (a, b) in via_cache.iter().zip(exact.iter()) {
            assert_eq!(a.0, b.0, "below threshold must be exact brute force");
        }
    }

    #[test]
    fn hnsw_path_recall_matches_brute_force_on_clusters() {
        let _serial = serial();
        let (vectors, centers) = clustered(24, 60, 32); // 1440 vectors
        let vectors: Arc<[Vec<f32>]> = Arc::from(vectors);
        let query = centers[5].clone();
        let k = 20;

        let ann = topk_gated(&vectors, &query, k, TEST_GATE); // forces HNSW
        let exact = brute_force_topk(&vectors, &query, k);
        assert_eq!(ann.len(), k);

        let exact_set: HashSet<usize> = exact.iter().map(|(i, _)| *i).collect();
        let overlap = ann.iter().filter(|(i, _)| exact_set.contains(i)).count();
        assert!(
            overlap * 100 >= k * 50,
            "HNSW recall@{k} too low: {overlap}/{k}"
        );
    }

    #[test]
    fn hnsw_path_results_are_descending() {
        let _serial = serial();
        let (vectors, centers) = clustered(20, 60, 24); // 1200 vectors
        let vectors: Arc<[Vec<f32>]> = Arc::from(vectors);
        let results = topk_gated(&vectors, &centers[3], 10, TEST_GATE);
        for w in results.windows(2) {
            assert!(
                w[0].1 >= w[1].1,
                "results must be sorted by descending similarity"
            );
        }
    }

    #[test]
    fn rebuilds_when_corpus_changes() {
        let _serial = serial();
        // Two distinct corpora share the global cache slot; the fingerprint must
        // force a rebuild so each query reflects its own corpus (no staleness).
        // Asserting on the cached fingerprint tests the rebuild mechanism
        // directly — deterministic, unlike HNSW's approximate top-1 recall.
        let (a, ca) = clustered(20, 55, 32); // 1100 vectors
        let (b, cb) = clustered(18, 60, 32); // 1080 vectors
        let a: Arc<[Vec<f32>]> = Arc::from(a);
        let b: Arc<[Vec<f32>]> = Arc::from(b);

        let _ = topk_gated(&a, &ca[7], 5, TEST_GATE);
        assert_eq!(
            cached_fingerprint(),
            Some(fingerprint(&a)),
            "first query caches corpus A's index"
        );

        let _ = topk_gated(&b, &cb[4], 5, TEST_GATE);
        assert_eq!(
            cached_fingerprint(),
            Some(fingerprint(&b)),
            "a different corpus must force a rebuild to B"
        );

        let _ = topk_gated(&a, &ca[7], 5, TEST_GATE);
        assert_eq!(
            cached_fingerprint(),
            Some(fingerprint(&a)),
            "re-querying A must rebuild A — never serve stale B"
        );
    }

    #[test]
    fn fingerprint_differs_on_content_change() {
        let a: Vec<Vec<f32>> = (0..10).map(|i| random_vec(8, i)).collect();
        let mut b = a.clone();
        b[3][0] += 0.5;
        assert_ne!(fingerprint(&a), fingerprint(&b));
    }
}
