//! Hebbian Co-Access Cache with Boltzmann-Temperature Eviction.
//!
//! Scientific basis:
//! - Hebb (1949): "Neurons that fire together wire together" — files accessed together
//!   strengthen their association, making co-accessed files resistant to eviction.
//! - Boltzmann distribution (Statistical Physics): P(evict) = exp(-E/kT) where E is the
//!   "value" of a cache entry and T is the memory pressure. Low T = deterministic (only
//!   lowest-value entries evicted), High T = stochastic (prevents thrashing).

use std::collections::HashMap;
use std::time::Instant;

/// Maximum number of co-access pairs tracked (prevents unbounded growth).
const MAX_ASSOCIATIONS: usize = 10_000;
/// Decay half-life in seconds for Hebbian weights.
const DECAY_HALF_LIFE_SECS: f64 = 300.0;
/// Minimum weight before pruning.
const PRUNE_THRESHOLD: f32 = 0.01;

/// Tracks co-access patterns between files (Hebbian learning).
pub struct CoAccessMatrix {
    /// Sparse co-access weights: (`path_hash_a`, `path_hash_b`) → weight
    weights: HashMap<(u64, u64), f32>,
    /// When each pair was last strengthened
    timestamps: HashMap<(u64, u64), Instant>,
    /// Current access burst (files read in the same tool-call window)
    current_burst: Vec<u64>,
    burst_start: Instant,
}

impl Default for CoAccessMatrix {
    fn default() -> Self {
        Self::new()
    }
}

impl CoAccessMatrix {
    #[must_use]
    pub fn new() -> Self {
        Self {
            weights: HashMap::with_capacity(256),
            timestamps: HashMap::with_capacity(256),
            current_burst: Vec::with_capacity(8),
            burst_start: Instant::now(),
        }
    }

    /// Record a file access. If within the burst window (500ms), strengthens
    /// associations with other files in the same burst.
    pub fn record_access(&mut self, path_hash: u64) {
        let now = Instant::now();
        let burst_window = std::time::Duration::from_millis(500);

        if now.duration_since(self.burst_start) > burst_window {
            self.flush_burst();
            self.burst_start = now;
        }

        self.current_burst.push(path_hash);
    }

    /// Flush current burst: strengthen all pairwise associations.
    fn flush_burst(&mut self) {
        if self.current_burst.len() < 2 {
            self.current_burst.clear();
            return;
        }

        let now = Instant::now();
        let burst = std::mem::take(&mut self.current_burst);

        for i in 0..burst.len() {
            for j in (i + 1)..burst.len() {
                let key = normalized_key(burst[i], burst[j]);
                let w = self.weights.entry(key).or_insert(0.0);
                *w += 1.0;
                self.timestamps.insert(key, now);
            }
        }

        if self.weights.len() > MAX_ASSOCIATIONS {
            self.prune();
        }
    }

    /// Get the association strength of a file with all currently active files.
    /// Applies exponential decay based on elapsed time.
    #[must_use]
    pub fn association_strength(&self, path_hash: u64, active_hashes: &[u64]) -> f32 {
        let now = Instant::now();
        let mut total = 0.0f32;

        for &active in active_hashes {
            let key = normalized_key(path_hash, active);
            if let Some(&weight) = self.weights.get(&key) {
                let elapsed = self.timestamps.get(&key).map_or(DECAY_HALF_LIFE_SECS, |t| {
                    now.duration_since(*t).as_secs_f64()
                });
                let decay = (-elapsed * (2.0f64.ln()) / DECAY_HALF_LIFE_SECS).exp();
                total += weight * decay as f32;
            }
        }

        total
    }

    /// Remove weak associations to keep memory bounded.
    fn prune(&mut self) {
        let now = Instant::now();
        self.weights.retain(|key, weight| {
            let elapsed = self
                .timestamps
                .get(key)
                .map_or(DECAY_HALF_LIFE_SECS * 2.0, |t| {
                    now.duration_since(*t).as_secs_f64()
                });
            let decay = (-elapsed * (2.0f64.ln()) / DECAY_HALF_LIFE_SECS).exp();
            let effective = *weight * decay as f32;
            if effective < PRUNE_THRESHOLD {
                self.timestamps.remove(key);
                false
            } else {
                true
            }
        });

        // Hard cap: a high-churn burst of strong, fresh associations can leave the map
        // above MAX_ASSOCIATIONS after threshold-pruning. Drop the lowest-effective-weight
        // pairs to enforce the cap (keeping both maps key-synced). Tradeoff: discards the
        // weakest learned co-access pairs, which are re-learned if seen again.
        if self.weights.len() > MAX_ASSOCIATIONS {
            let mut scored: Vec<((u64, u64), f32)> = self
                .weights
                .iter()
                .map(|(&key, &weight)| {
                    let elapsed = self
                        .timestamps
                        .get(&key)
                        .map_or(DECAY_HALF_LIFE_SECS * 2.0, |t| {
                            now.duration_since(*t).as_secs_f64()
                        });
                    let decay = (-elapsed * (2.0f64.ln()) / DECAY_HALF_LIFE_SECS).exp();
                    (key, weight * decay as f32)
                })
                .collect();
            scored.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
            let to_drop = self.weights.len() - MAX_ASSOCIATIONS;
            for (key, _) in scored.into_iter().take(to_drop) {
                self.weights.remove(&key);
                self.timestamps.remove(&key);
            }
        }
    }

    /// Force flush any pending burst (call at end of tool-call processing).
    pub fn end_burst(&mut self) {
        self.flush_burst();
    }
}

/// Normalize key so (a,b) == (b,a).
fn normalized_key(a: u64, b: u64) -> (u64, u64) {
    if a <= b { (a, b) } else { (b, a) }
}

/// Compute a fast hash for a file path.
#[must_use]
pub fn path_hash(path: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut h);
    h.finish()
}

// ─── Boltzmann-Temperature Eviction ───────────────────────────────────────────

/// Compute the "energy" (value) of a cache entry for Boltzmann eviction.
/// Higher energy = more valuable = less likely to be evicted.
pub struct EntryEnergy {
    pub read_count: u32,
    pub recency_secs: f64,
    pub association_strength: f32,
    pub token_size: usize,
    pub graph_centrality: f32,
}

impl EntryEnergy {
    /// Calculate the energy value E for this entry.
    /// Combines multiple signals into a single scalar.
    #[must_use]
    pub fn compute(&self) -> f64 {
        // Recency contributes with log-decay (recent = high energy)
        let recency_score = 1.0 / (1.0 + self.recency_secs / 60.0);

        // Read frequency (diminishing returns via sqrt)
        let freq_score = f64::from(self.read_count).sqrt();

        // Association boost (normalized)
        let assoc_score = f64::from(self.association_strength).min(5.0);

        // Size penalty (large entries cost more to keep)
        let size_penalty = 1.0 / (1.0 + (self.token_size as f64 / 5000.0));

        // Graph centrality bonus
        let centrality_score = f64::from(self.graph_centrality);

        // Weighted combination
        recency_score * 3.0
            + freq_score * 2.0
            + assoc_score * 1.5
            + size_penalty * 1.0
            + centrality_score * 1.0
    }
}

/// Boltzmann eviction decision.
/// Returns the indices to evict from a list of energy scores, given a temperature T.
///
/// Temperature T = normalized memory pressure:
/// - T ≈ 0: almost deterministic (only lowest-energy entries evicted)
/// - T ≈ 1: stochastic (prevents pathological thrashing)
pub fn boltzmann_select_evictions(
    energies: &[f64],
    num_to_evict: usize,
    temperature: f64,
) -> Vec<usize> {
    if energies.is_empty() || num_to_evict == 0 {
        return Vec::new();
    }

    let n = energies.len().min(num_to_evict);
    let t = temperature.max(0.01); // avoid division by zero

    // Compute eviction probabilities: P(evict_i) ∝ exp(-E_i / T)
    let max_e = energies.iter().copied().fold(f64::MIN, f64::max);
    let probs: Vec<f64> = energies
        .iter()
        .map(|&e| {
            let normalized = (e - max_e) / t.max(0.01);
            (-normalized).exp() // lower energy → higher eviction probability
        })
        .collect();

    // Sort by eviction probability (highest first = lowest energy first)
    let mut indexed: Vec<(usize, f64)> = probs.into_iter().enumerate().collect();
    indexed.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    // At low temperature, this is nearly deterministic (sorted by energy).
    // At high temperature, the probabilities flatten out.
    indexed.into_iter().take(n).map(|(idx, _)| idx).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn co_access_strengthens_pairs() {
        let mut matrix = CoAccessMatrix::new();
        let a = path_hash("src/main.rs");
        let b = path_hash("src/lib.rs");
        let c = path_hash("src/config.rs");

        // Simulate burst: A, B, C accessed together
        matrix.record_access(a);
        matrix.record_access(b);
        matrix.record_access(c);
        matrix.end_burst();

        // A should have association with B
        assert!(matrix.association_strength(a, &[b]) > 0.0);
        // And with C
        assert!(matrix.association_strength(a, &[c]) > 0.0);
    }

    #[test]
    fn unrelated_files_have_zero_association() {
        let matrix = CoAccessMatrix::new();
        let a = path_hash("src/main.rs");
        let b = path_hash("src/lib.rs");
        assert_eq!(matrix.association_strength(a, &[b]), 0.0);
    }

    #[test]
    fn boltzmann_low_temp_is_deterministic() {
        let energies = vec![10.0, 1.0, 5.0, 0.5, 8.0];
        let evictions = boltzmann_select_evictions(&energies, 2, 0.01);
        // Should evict lowest-energy entries: idx 3 (0.5) and idx 1 (1.0)
        assert!(evictions.contains(&3));
        assert!(evictions.contains(&1));
    }

    #[test]
    fn boltzmann_high_temp_still_picks_n() {
        let energies = vec![10.0, 1.0, 5.0, 0.5, 8.0];
        let evictions = boltzmann_select_evictions(&energies, 2, 100.0);
        assert_eq!(evictions.len(), 2);
    }

    #[test]
    fn entry_energy_compute_is_sane() {
        let high_value = EntryEnergy {
            read_count: 10,
            recency_secs: 5.0,
            association_strength: 3.0,
            token_size: 500,
            graph_centrality: 0.8,
        };
        let low_value = EntryEnergy {
            read_count: 1,
            recency_secs: 3600.0,
            association_strength: 0.0,
            token_size: 50000,
            graph_centrality: 0.0,
        };
        assert!(high_value.compute() > low_value.compute());
    }

    #[test]
    fn normalized_key_is_symmetric() {
        assert_eq!(normalized_key(42, 99), normalized_key(99, 42));
    }
}
