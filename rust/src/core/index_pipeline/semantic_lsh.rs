//! Hyperplane-based locality-sensitive hashing (LSH) for semantic similarity.
//!
//! Each hyperplane defines a random half-space. The sign of `dot(vec, plane)`
//! gives 1 bit of the signature. For B bands × R rows, we get a B×R bit
//! signature stored in a `u64`. Two vectors that collide in at least one
//! band's bucket are candidate near-neighbors.

use std::collections::{HashMap, HashSet};
use std::fmt;

use crate::core::prng::splitmix64_f32_signed;

/// A 64-bit LSH signature produced by hyperplane dot-product.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Signature(pub u64);

/// Configuration for hyperplane LSH.
#[derive(Debug)]
pub struct LshConfig {
    dim: usize,
    bands: usize,
    rows: usize,
    /// Flat array of hyperplane coefficients: `[band][row][dim]`.
    /// Total planes = `bands * rows`.
    planes: Vec<f32>,
}

impl LshConfig {
    /// Create new config.
    ///
    /// * `dim` — vector dimension (e.g. 768)
    /// * `bands` — number of bands (e.g. 16)
    /// * `rows` — rows per band (e.g. 4)
    ///
    /// Returns `Err` if `bands * rows > 64` (won't fit in `u64` signature) or
    /// `dim == 0`.
    pub fn new(dim: usize, bands: usize, rows: usize) -> Result<Self, LshError> {
        if dim == 0 {
            return Err(LshError::InvalidParams {
                reason: "dim must be > 0",
            });
        }
        let total_planes = bands * rows;
        if total_planes > 64 {
            return Err(LshError::InvalidParams {
                reason: "bands * rows must be <= 64 (fits in u64 signature)",
            });
        }
        if total_planes == 0 {
            return Err(LshError::InvalidParams {
                reason: "bands * rows must be > 0",
            });
        }
        let planes = Self::generate_planes(dim, total_planes);
        Ok(Self {
            dim,
            bands,
            rows,
            planes,
        })
    }

    /// Generate hyperplane coefficients deterministically.
    ///
    /// Each plane is a vector of length `dim` with values ~Uniform[-0.5, 0.5].
    /// Uses splitmix64 seeded by `(plane_idx * dim + d)`.
    fn generate_planes(dim: usize, total_planes: usize) -> Vec<f32> {
        let mut planes = Vec::with_capacity(total_planes * dim);
        for plane_idx in 0..total_planes {
            for d in 0..dim {
                let seed = (plane_idx as u64) * (dim as u64) + (d as u64);
                planes.push(splitmix64_f32_signed(seed));
            }
        }
        planes
    }

    /// Compute the LSH signature for a dense vector slice.
    ///
    /// For each plane: `dot = sum(vec[d] * plane[plane_idx * dim + d])`.
    /// The sign bit is `(dot >= 0) as u64`, shifted to position `plane_idx`.
    #[must_use]
    pub fn sign(&self, vec: &[f32]) -> Signature {
        debug_assert_eq!(vec.len(), self.dim, "vector dimension mismatch");
        let mut bits: u64 = 0;
        let total_planes = self.bands * self.rows;
        for plane_idx in 0..total_planes {
            let mut dot = 0.0f32;
            let base = plane_idx * self.dim;
            for (d, &v) in vec.iter().enumerate() {
                dot += v * self.planes[base + d];
            }
            if dot >= 0.0 {
                bits |= 1u64 << plane_idx;
            }
        }
        Signature(bits)
    }

    /// Compute LSH signature for a sparse vector (position, value pairs).
    ///
    /// Only iterates over non-zero entries, making this O(nnz × `n_planes`)
    /// instead of O(dim × `n_planes`). For `RiVector` with 8 non-zeros and
    /// 64 planes, this is 96× fewer multiply-adds vs. `sign({dense})`.
    #[must_use]
    pub fn sign_sparse(&self, positions: &[usize], values: &[f32], nnz: usize) -> Signature {
        let mut bits: u64 = 0;
        let total_planes = self.bands * self.rows;
        for plane_idx in 0..total_planes {
            let mut dot = 0.0f32;
            let base = plane_idx * self.dim;
            for i in 0..nnz {
                dot += values[i] * self.planes[base + positions[i]];
            }
            if dot >= 0.0 {
                bits |= 1u64 << plane_idx;
            }
        }
        Signature(bits)
    }

    /// Extract band bits from a signature.
    ///
    /// Bits for band `b` are the `rows` bits starting at position `b * rows`.
    /// Returns a `u64` with those bits packed: for each row `r` in the band,
    /// if bit `(b * rows + r)` is set, adds `(1 << r)` to the result.
    #[must_use]
    pub fn band_index(&self, sig: &Signature, band: usize) -> u64 {
        let shift = band * self.rows;
        let mask = (1u64 << self.rows) - 1;
        (sig.0 >> shift) & mask
    }
}

/// Error type for LSH configuration validation.
#[derive(Debug, Clone)]
pub enum LshError {
    /// Configuration parameters are invalid.
    InvalidParams {
        /// Human-readable reason.
        reason: &'static str,
    },
}

impl fmt::Display for LshError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LshError::InvalidParams { reason } => {
                write!(f, "invalid LSH parameters: {reason}")
            }
        }
    }
}

impl std::error::Error for LshError {}

/// Bucket storage for candidate retrieval.
///
/// Maintains per-band hash maps from bucket IDs to lists of symbol indices.
pub struct CandidateTable {
    /// Per-band: `HashMap<bucket_id, Vec<symbol_index>>`
    bands: Vec<HashMap<u64, Vec<usize>>>,
}

impl CandidateTable {
    /// Create a new table with the given number of bands.
    #[must_use]
    pub fn new(bands: usize) -> Self {
        let mut bands_vec = Vec::with_capacity(bands);
        for _ in 0..bands {
            bands_vec.push(HashMap::new());
        }
        Self { bands: bands_vec }
    }

    /// Insert a symbol index into a band's bucket.
    pub fn insert(&mut self, band: usize, bucket: u64, idx: usize) {
        self.bands[band].entry(bucket).or_default().push(idx);
    }

    /// Collect candidate indices from all bands, deduplicated, up to `max`.
    #[must_use]
    pub fn candidates(&self, lsh: &LshConfig, sig: &Signature, max: usize) -> Vec<usize> {
        let mut seen = HashSet::new();
        let mut result = Vec::new();
        for band in 0..lsh.bands {
            let bucket = lsh.band_index(sig, band);
            if let Some(indices) = self.bands[band].get(&bucket) {
                for &idx in indices {
                    if seen.insert(idx) {
                        result.push(idx);
                        if result.len() >= max {
                            return result;
                        }
                    }
                }
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_newtype() {
        let s1 = Signature(42);
        let s2 = Signature(42);
        let s3 = Signature(99);
        assert_eq!(s1, s2);
        assert_ne!(s1, s3);
        assert_eq!(s1.0, 42);
    }

    #[test]
    fn lsh_config_new_ok() {
        let cfg = LshConfig::new(768, 16, 4).unwrap();
        assert_eq!(cfg.bands, 16);
        assert_eq!(cfg.rows, 4);
        assert_eq!(cfg.dim, 768);
    }

    #[test]
    fn lsh_config_new_zero_dim() {
        let err = LshConfig::new(0, 16, 4).unwrap_err();
        assert!(matches!(err, LshError::InvalidParams { .. }));
    }

    #[test]
    fn lsh_config_new_too_many_planes() {
        let err = LshConfig::new(768, 17, 4).unwrap_err();
        assert!(matches!(err, LshError::InvalidParams { .. }));
    }

    #[test]
    fn lsh_config_new_zero_planes() {
        let err = LshConfig::new(768, 0, 0).unwrap_err();
        assert!(matches!(err, LshError::InvalidParams { .. }));
    }

    #[test]
    fn generate_planes_deterministic() {
        let cfg = LshConfig::new(8, 2, 2).unwrap();
        let cfg2 = LshConfig::new(8, 2, 2).unwrap();
        assert_eq!(cfg.planes, cfg2.planes);
    }

    #[test]
    fn sign_produces_deterministic_signature() {
        let cfg = LshConfig::new(8, 2, 2).unwrap();
        let vec = vec![0.1, -0.2, 0.3, -0.4, 0.5, -0.6, 0.7, -0.8];
        let sig1 = cfg.sign(&vec);
        let sig2 = cfg.sign(&vec);
        assert_eq!(sig1, sig2);
    }

    #[test]
    fn band_index_round_trip() {
        let cfg = LshConfig::new(8, 2, 4).unwrap();
        // signature with all bits set
        let sig = Signature(u64::MAX);
        // each band of 4 bits should yield 0b1111 = 15
        for b in 0..2 {
            assert_eq!(cfg.band_index(&sig, b), 0b1111);
        }
    }

    #[test]
    fn candidate_table_basic() {
        let lsh = LshConfig::new(8, 2, 2).unwrap();
        let vec = vec![0.1, -0.2, 0.3, -0.4, 0.5, -0.6, 0.7, -0.8];
        let sig = lsh.sign(&vec);
        let mut table = CandidateTable::new(lsh.bands);
        table.insert(0, lsh.band_index(&sig, 0), 42);
        let candidates = table.candidates(&lsh, &sig, 100);
        assert_eq!(candidates, vec![42]);
    }

    #[test]
    fn candidate_table_deduplicates() {
        let lsh = LshConfig::new(8, 2, 2).unwrap();
        let vec = vec![0.1, -0.2, 0.3, -0.4, 0.5, -0.6, 0.7, -0.8];
        let sig = lsh.sign(&vec);
        let mut table = CandidateTable::new(lsh.bands);
        // Insert same index in both bands
        table.insert(0, lsh.band_index(&sig, 0), 42);
        table.insert(1, lsh.band_index(&sig, 1), 42);
        let candidates = table.candidates(&lsh, &sig, 100);
        assert_eq!(candidates, vec![42]); // deduplicated
    }

    #[test]
    fn display_lsh_error() {
        let err = LshError::InvalidParams {
            reason: "test error",
        };
        let msg = format!("{err}");
        assert!(msg.contains("test error"));
    }

    #[test]
    fn candidate_table_max_limit() {
        let lsh = LshConfig::new(8, 2, 2).unwrap();
        let vec = vec![0.1, -0.2, 0.3, -0.4, 0.5, -0.6, 0.7, -0.8];
        let sig = lsh.sign(&vec);
        let mut table = CandidateTable::new(lsh.bands);
        // Insert 3 different indices in the same band+bucket
        table.insert(0, lsh.band_index(&sig, 0), 10);
        table.insert(0, lsh.band_index(&sig, 0), 20);
        table.insert(0, lsh.band_index(&sig, 0), 30);
        // Cap at 2 — should return only 2 results
        let candidates = table.candidates(&lsh, &sig, 2);
        assert_eq!(candidates.len(), 2);
    }

    #[test]
    fn candidate_table_empty_when_no_match() {
        let lsh = LshConfig::new(8, 2, 2).unwrap();
        let sig_a = lsh.sign(&[0.1, -0.2, 0.3, -0.4, 0.5, -0.6, 0.7, -0.8]);
        let sig_b = lsh.sign(&[-0.1, 0.2, -0.3, 0.4, -0.5, 0.6, -0.7, 0.8]);
        let mut table = CandidateTable::new(lsh.bands);
        // Insert sig_a but query with sig_b → no common bucket
        table.insert(0, lsh.band_index(&sig_a, 0), 42);
        let candidates = table.candidates(&lsh, &sig_b, 100);
        assert!(candidates.is_empty());
    }

    #[test]
    #[should_panic(expected = "index out of bounds")]
    fn candidate_table_new_zero_bands_panics() {
        // CandidateTable with 0 bands panics because candidates() iterates
        // `0..lsh.bands` and indexes into self.bands[band] without bounds check.
        let lsh = LshConfig::new(8, 2, 2).unwrap();
        let sig = lsh.sign(&[0.1, -0.2, 0.3, -0.4, 0.5, -0.6, 0.7, -0.8]);
        let table = CandidateTable::new(0);
        table.candidates(&lsh, &sig, 100);
    }

    #[test]
    fn signature_hash_in_set() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(Signature(1));
        set.insert(Signature(2));
        set.insert(Signature(1)); // duplicate
        assert_eq!(set.len(), 2);
        assert!(set.contains(&Signature(1)));
        assert!(set.contains(&Signature(2)));
    }

    #[test]
    fn sign_different_inputs_different_signatures() {
        let cfg = LshConfig::new(8, 2, 2).unwrap();
        let sig_a = cfg.sign(&[0.1, -0.2, 0.3, -0.4, 0.5, -0.6, 0.7, -0.8]);
        let sig_b = cfg.sign(&[-0.1, 0.2, -0.3, 0.4, -0.5, 0.6, -0.7, 0.8]);
        // Different inputs should produce different signatures
        // (vanishingly unlikely to collide with random hyperplanes)
        assert_ne!(sig_a, sig_b);
        // Both should be deterministic
        assert_eq!(
            sig_a,
            cfg.sign(&[0.1, -0.2, 0.3, -0.4, 0.5, -0.6, 0.7, -0.8])
        );
        assert_eq!(
            sig_b,
            cfg.sign(&[-0.1, 0.2, -0.3, 0.4, -0.5, 0.6, -0.7, 0.8])
        );
    }
}
