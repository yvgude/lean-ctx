//! Lightweight HNSW (Hierarchical Navigable Small World) index for approximate nearest neighbors.
//!
//! Scientific basis: Malkov & Yashunin, "Efficient and Robust Approximate Nearest Neighbor
//! using Hierarchical Navigable Small World Graphs" (IEEE TPAMI 2018).
//!
//! This is a minimal implementation optimized for lean-ctx's embedding dimensions (384-d).
//! For indices under BRUTE_FORCE_THRESHOLD chunks, falls back to exact linear scan
//! with binary-heap top-k selection (O(n log k) instead of O(n log n)).

use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::sync::Arc;

const BRUTE_FORCE_THRESHOLD: usize = 1000;
const M: usize = 16; // max connections per node per layer
const EF_CONSTRUCTION: usize = 200; // search width during build
const EF_SEARCH: usize = 64; // search width during query
                             // ML = 1/ln(M) = 1/ln(16) ≈ 0.3607
const ML: f64 = 0.360_674_0;

/// A scored item for the min-heap (lowest similarity first for top-k pruning).
#[derive(Clone, PartialEq)]
struct Candidate {
    idx: usize,
    sim: f32,
}

impl Eq for Candidate {}

impl PartialOrd for Candidate {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Candidate {
    fn cmp(&self, other: &Self) -> Ordering {
        // Min-heap: lower similarity should be popped first
        other.sim.partial_cmp(&self.sim).unwrap_or(Ordering::Equal)
    }
}

/// Max-heap variant for HNSW traversal.
#[derive(Clone, PartialEq)]
struct MaxCandidate {
    idx: usize,
    sim: f32,
}

impl Eq for MaxCandidate {}

impl PartialOrd for MaxCandidate {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for MaxCandidate {
    fn cmp(&self, other: &Self) -> Ordering {
        self.sim.partial_cmp(&other.sim).unwrap_or(Ordering::Equal)
    }
}

/// HNSW index node.
struct Node {
    connections: Vec<Vec<usize>>, // connections[layer] = list of neighbor indices
}

/// Approximate nearest neighbor index using HNSW for large datasets,
/// with brute-force fallback for small ones.
pub struct AnnIndex {
    vectors: Arc<[Vec<f32>]>,
    nodes: Vec<Node>,
    entry_point: usize,
    max_level: usize,
}

impl AnnIndex {
    /// Build the index from a shared set of vectors.
    ///
    /// The corpus is taken as `Arc<[Vec<f32>]>` so the cached index shares the
    /// *same* full-precision allocation as the per-query aligned corpus: build
    /// performs an `Arc::clone` (a refcount bump, zero element bytes copied)
    /// rather than duplicating the whole `Vec<Vec<f32>>`.
    pub fn build(vectors: Arc<[Vec<f32>]>) -> Self {
        let n = vectors.len();
        if n == 0 {
            return Self {
                vectors,
                nodes: Vec::new(),
                entry_point: 0,
                max_level: 0,
            };
        }

        if n < BRUTE_FORCE_THRESHOLD {
            return Self {
                vectors,
                nodes: Vec::new(),
                entry_point: 0,
                max_level: 0,
            };
        }

        // HNSW graph path: the vectors slice is shared up front (Arc::clone, no
        // element copy) and the insert loop reads from it by index. `insert`
        // only mutates `nodes`, deriving each new id from `nodes.len()`.
        let mut index = Self {
            vectors,
            nodes: Vec::with_capacity(n),
            entry_point: 0,
            max_level: 0,
        };

        for i in 0..n {
            index.insert(i);
        }

        index
    }

    fn insert(&mut self, new_id: usize) {
        let level = Self::level_for(new_id);

        self.nodes.push(Node {
            connections: vec![Vec::new(); level + 1],
        });

        if self.nodes.len() == 1 {
            self.entry_point = 0;
            self.max_level = level;
            return;
        }

        let mut ep = self.entry_point;

        // Traverse from top layer down to level+1 (greedy)
        for lc in (level + 1..=self.max_level).rev() {
            ep = self.search_layer_single(&self.vectors[new_id], ep, lc);
        }

        // Insert into layers [min(level, max_level) .. 0]
        let insert_levels = level.min(self.max_level);
        for lc in (0..=insert_levels).rev() {
            let neighbors = self.search_layer(&self.vectors[new_id], ep, EF_CONSTRUCTION, lc);
            let selected = Self::select_neighbors(&neighbors, M);

            if lc < self.nodes[new_id].connections.len() {
                self.nodes[new_id].connections[lc].clone_from(&selected);
            }

            for &neighbor in &selected {
                if lc < self.nodes[neighbor].connections.len() {
                    self.nodes[neighbor].connections[lc].push(new_id);
                    if self.nodes[neighbor].connections[lc].len() > M * 2 {
                        let nv = &self.vectors[neighbor];
                        let mut scored: Vec<(usize, f32)> = self.nodes[neighbor].connections[lc]
                            .iter()
                            .map(|&n| (n, cosine_sim(nv, &self.vectors[n])))
                            .collect();
                        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(Ordering::Equal));
                        scored.truncate(M);
                        self.nodes[neighbor].connections[lc] =
                            scored.into_iter().map(|(id, _)| id).collect();
                    }
                }
            }

            if !neighbors.is_empty() {
                ep = neighbors[0].0;
            }
        }

        if level > self.max_level {
            self.max_level = level;
            self.entry_point = new_id;
        }
    }

    fn search_layer_single(&self, query: &[f32], ep: usize, _layer: usize) -> usize {
        let mut current = ep;
        let mut best_sim = cosine_sim(query, &self.vectors[ep]);

        loop {
            let mut improved = false;
            let conns = &self.nodes[current].connections;
            let layer_conns = if _layer < conns.len() {
                &conns[_layer]
            } else {
                break;
            };

            for &neighbor in layer_conns {
                let sim = cosine_sim(query, &self.vectors[neighbor]);
                if sim > best_sim {
                    best_sim = sim;
                    current = neighbor;
                    improved = true;
                }
            }
            if !improved {
                break;
            }
        }
        current
    }

    fn search_layer(&self, query: &[f32], ep: usize, ef: usize, layer: usize) -> Vec<(usize, f32)> {
        let mut visited = vec![false; self.vectors.len()];
        let mut candidates = BinaryHeap::<MaxCandidate>::new();
        let mut results = BinaryHeap::<Candidate>::new();

        let sim = cosine_sim(query, &self.vectors[ep]);
        visited[ep] = true;
        candidates.push(MaxCandidate { idx: ep, sim });
        results.push(Candidate { idx: ep, sim });

        while let Some(MaxCandidate { idx: c, sim: _ }) = candidates.pop() {
            let worst_result = results.peek().map_or(f32::MIN, |r| r.sim);
            if cosine_sim(query, &self.vectors[c]) < worst_result && results.len() >= ef {
                break;
            }

            let conns = &self.nodes[c].connections;
            let layer_conns = if layer < conns.len() {
                &conns[layer]
            } else {
                continue;
            };

            for &neighbor in layer_conns {
                if visited[neighbor] {
                    continue;
                }
                visited[neighbor] = true;

                let n_sim = cosine_sim(query, &self.vectors[neighbor]);
                let worst = results.peek().map_or(f32::MIN, |r| r.sim);

                if results.len() < ef || n_sim > worst {
                    candidates.push(MaxCandidate {
                        idx: neighbor,
                        sim: n_sim,
                    });
                    results.push(Candidate {
                        idx: neighbor,
                        sim: n_sim,
                    });
                    if results.len() > ef {
                        results.pop();
                    }
                }
            }
        }

        let mut out: Vec<(usize, f32)> = results.into_iter().map(|c| (c.idx, c.sim)).collect();
        out.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(Ordering::Equal));
        out
    }

    fn select_neighbors(candidates: &[(usize, f32)], max_count: usize) -> Vec<usize> {
        candidates
            .iter()
            .take(max_count)
            .map(|&(idx, _)| idx)
            .collect()
    }

    /// Deterministic geometric level draw seeded by the node's insertion index.
    ///
    /// HNSW only requires the per-node level to follow a geometric distribution
    /// (mean `ML`); it does not require OS entropy. Deriving the draw from the
    /// node id via splitmix64 keeps that distribution while making index
    /// construction **fully reproducible** — the same corpus always yields the
    /// same graph and therefore the same search results. The previous
    /// `getrandom`-seeded draw rebuilt a different graph on every run, which made
    /// approximate recall (and the recall tests) non-deterministic and flaky.
    fn level_for(node_id: usize) -> usize {
        // splitmix64: a single id → a well-distributed u64.
        let mut z = (node_id as u64).wrapping_add(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        // Map the top 53 bits to (0,1); +1 keeps r > 0 so -ln(r) stays finite.
        let r = (((z >> 11) as f64) + 1.0) / ((1u64 << 53) as f64 + 1.0);
        (-r.ln() * ML).floor() as usize
    }

    /// Search for the top-k nearest neighbors of a query vector.
    /// Returns (index, similarity) pairs sorted by descending similarity.
    pub fn search(&self, query: &[f32], top_k: usize) -> Vec<(usize, f32)> {
        if self.vectors.is_empty() {
            return Vec::new();
        }

        // Brute-force for small indices (faster due to no graph overhead)
        if self.nodes.is_empty() || self.vectors.len() < BRUTE_FORCE_THRESHOLD {
            return brute_force_topk(&self.vectors, query, top_k);
        }

        // HNSW search
        let mut ep = self.entry_point;
        for lc in (1..=self.max_level).rev() {
            ep = self.search_layer_single(query, ep, lc);
        }

        let mut results = self.search_layer(query, ep, EF_SEARCH.max(top_k), 0);
        results.truncate(top_k);
        results
    }
}

/// O(n log k) brute-force top-k selection using a min-heap.
pub fn brute_force_topk(vectors: &[Vec<f32>], query: &[f32], top_k: usize) -> Vec<(usize, f32)> {
    let mut heap = BinaryHeap::<Candidate>::with_capacity(top_k + 1);

    for (i, vec) in vectors.iter().enumerate() {
        let sim = cosine_sim(query, vec);
        if heap.len() < top_k {
            heap.push(Candidate { idx: i, sim });
        } else if let Some(worst) = heap.peek() {
            if sim > worst.sim {
                heap.pop();
                heap.push(Candidate { idx: i, sim });
            }
        }
    }

    let mut results: Vec<(usize, f32)> = heap.into_iter().map(|c| (c.idx, c.sim)).collect();
    results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(Ordering::Equal));
    results
}

/// Cosine similarity via the shared SIMD-friendly dot product (turbovec-derived,
/// autovectorized chunked accumulators) rather than a scalar triple-accumulate
/// loop. This is the hot path for every brute-force and HNSW comparison, so the
/// vectorized kernel matters: each query touches the distance fn O(n) (brute) or
/// O(ef·log n) (HNSW) times.
#[inline]
fn cosine_sim(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return 0.0;
    }
    crate::core::embeddings::cosine_similarity_raw(a, b)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn random_vec(dim: usize, seed: u64) -> Vec<f32> {
        let mut v = Vec::with_capacity(dim);
        let mut s = seed;
        for _ in 0..dim {
            s = s.wrapping_mul(6364136223846793005).wrapping_add(1);
            v.push((s as f32 / u64::MAX as f32) * 2.0 - 1.0);
        }
        v
    }

    #[test]
    fn brute_force_topk_correctness() {
        let vectors: Vec<Vec<f32>> = (0..100).map(|i| random_vec(16, i)).collect();
        let query = random_vec(16, 999);

        let results = brute_force_topk(&vectors, &query, 5);
        assert_eq!(results.len(), 5);

        // Results should be in descending similarity order
        for w in results.windows(2) {
            assert!(w[0].1 >= w[1].1);
        }
    }

    #[test]
    fn brute_force_topk_matches_exhaustive() {
        let vectors: Vec<Vec<f32>> = (0..50).map(|i| random_vec(8, i + 42)).collect();
        let query = random_vec(8, 123);

        let top5 = brute_force_topk(&vectors, &query, 5);

        // Exhaustive comparison
        let mut all: Vec<(usize, f32)> = vectors
            .iter()
            .enumerate()
            .map(|(i, v)| (i, cosine_sim(&query, v)))
            .collect();
        all.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        all.truncate(5);

        for (heap_r, exact_r) in top5.iter().zip(all.iter()) {
            assert_eq!(heap_r.0, exact_r.0);
            assert!((heap_r.1 - exact_r.1).abs() < 1e-6);
        }
    }

    #[test]
    fn empty_index_returns_empty() {
        let index = AnnIndex::build(Arc::from(Vec::new()));
        assert!(index.search(&[1.0, 0.0], 5).is_empty());
    }

    #[test]
    fn small_index_uses_brute_force() {
        let vectors: Vec<Vec<f32>> = (0..50).map(|i| random_vec(4, i)).collect();
        let index = AnnIndex::build(Arc::from(vectors));
        assert!(index.nodes.is_empty()); // no HNSW graph built
        let results = index.search(&random_vec(4, 999), 3);
        assert_eq!(results.len(), 3);
    }
}
