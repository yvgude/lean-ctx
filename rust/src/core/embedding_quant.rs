//! int8 scalar quantization + SIMD-friendly scoring for embedding vectors.
//!
//! Adapted from the `TurboQuant` approach (RyanCodrai/turbovec, ICLR 2026): a
//! data-oblivious, training-free, single-pass quantizer. At lean-ctx's scale
//! (hundreds of facts × 384-dim `MiniLM` vectors) the win is twofold:
//!   1. **4× smaller** on-disk knowledge index (`i8` codes vs `f32`).
//!   2. **Faster scoring** — the query is rotated once into the codebook domain
//!      (the per-vector `scale`) and accumulated directly over `i8` codes, so we
//!      never reconstruct the full `f32` document vector (turbovec's core idea).
//!
//! No heavy SIMD crate is pulled in: the chunked-lane accumulators below are
//! shaped so the autovectorizer emits NEON/AVX automatically, with a scalar tail
//! that is always correct on every target.

use serde::{Deserialize, Serialize};

/// Largest magnitude representable by a symmetric `i8` code (−127..=127; −128 is
/// excluded to keep the mapping symmetric and avoid an asymmetric overflow edge).
const I8_ABS_MAX: f32 = 127.0;

/// Lane width for the chunked accumulators. 8 maps cleanly onto a 256-bit AVX2
/// f32 register and two 128-bit NEON registers; the scalar tail handles any
/// remainder (e.g. 384 % 8 == 0, but odd dimensions stay correct).
const LANES: usize = 8;

/// A vector stored as int8 codes plus the per-vector scale needed to reconstruct
/// approximate values: `value[i] ≈ code[i] · scale`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct QuantizedVector {
    pub code: Vec<i8>,
    pub scale: f32,
}

impl QuantizedVector {
    #[must_use]
    pub fn dim(&self) -> usize {
        self.code.len()
    }

    /// Reconstruct the approximate `f32` vector. Only needed for diagnostics /
    /// migration; the hot path scores against the codes directly via [`dot_quant`].
    #[must_use]
    pub fn dequantize(&self) -> Vec<f32> {
        self.code
            .iter()
            .map(|&c| f32::from(c) * self.scale)
            .collect()
    }
}

/// Symmetric, per-vector quantization: `scale = max|x| / 127`, `code = round(x / scale)`.
///
/// Data-oblivious (no codebook training) and single-pass. A zero vector maps to
/// all-zero codes with `scale = 0.0`, which [`dot_quant`] treats as a zero result.
#[must_use]
pub fn quantize(v: &[f32]) -> QuantizedVector {
    let max_abs = v.iter().fold(0.0f32, |m, &x| m.max(x.abs()));
    if max_abs == 0.0 {
        return QuantizedVector {
            code: vec![0; v.len()],
            scale: 0.0,
        };
    }
    let scale = max_abs / I8_ABS_MAX;
    let inv = 1.0 / scale;
    let code = v
        .iter()
        .map(|&x| {
            // round-half-away then clamp into the symmetric range before the cast.
            let q = (x * inv).round().clamp(-I8_ABS_MAX, I8_ABS_MAX);
            q as i8
        })
        .collect();
    QuantizedVector { code, scale }
}

/// Asymmetric dot product: full-precision `query` · quantized `doc`.
///
/// Computes `Σ query[i] · code[i] · scale` without ever reconstructing the doc
/// vector. For L2-normalized inputs this approximates cosine similarity; the
/// quantization error is well within embedding-ranking tolerance.
#[must_use]
pub fn dot_quant(query: &[f32], doc: &QuantizedVector) -> f32 {
    debug_assert_eq!(query.len(), doc.code.len(), "dim mismatch");
    if doc.scale == 0.0 {
        return 0.0;
    }

    let mut lanes = [0.0f32; LANES];
    let mut q_chunks = query.chunks_exact(LANES);
    let mut c_chunks = doc.code.chunks_exact(LANES);

    for (q, c) in q_chunks.by_ref().zip(c_chunks.by_ref()) {
        for i in 0..LANES {
            lanes[i] += q[i] * f32::from(c[i]);
        }
    }

    let mut tail = 0.0f32;
    for (q, c) in q_chunks.remainder().iter().zip(c_chunks.remainder()) {
        tail += q * f32::from(*c);
    }

    (lanes.iter().sum::<f32>() + tail) * doc.scale
}

/// SIMD-friendly `f32` dot product with chunked lane accumulators.
///
/// Numerically a hair different from a naïve left-fold (float add is
/// non-associative) but far within similarity tolerance, and materially faster
/// on the 384-dim vectors used for semantic recall.
#[must_use]
pub fn dot_f32(a: &[f32], b: &[f32]) -> f32 {
    debug_assert_eq!(a.len(), b.len(), "dim mismatch");

    let mut lanes = [0.0f32; LANES];
    let mut a_chunks = a.chunks_exact(LANES);
    let mut b_chunks = b.chunks_exact(LANES);

    for (x, y) in a_chunks.by_ref().zip(b_chunks.by_ref()) {
        for i in 0..LANES {
            lanes[i] += x[i] * y[i];
        }
    }

    let mut tail = 0.0f32;
    for (x, y) in a_chunks.remainder().iter().zip(b_chunks.remainder()) {
        tail += x * y;
    }

    lanes.iter().sum::<f32>() + tail
}

#[cfg(test)]
mod tests {
    use super::*;

    fn naive_dot(a: &[f32], b: &[f32]) -> f32 {
        a.iter().zip(b).map(|(x, y)| x * y).sum()
    }

    #[test]
    fn dot_f32_matches_naive_within_tolerance() {
        let a: Vec<f32> = (0..384).map(|i| (i as f32 * 0.013).sin()).collect();
        let b: Vec<f32> = (0..384).map(|i| (i as f32 * 0.017).cos()).collect();
        let chunked = dot_f32(&a, &b);
        let naive = naive_dot(&a, &b);
        assert!(
            (chunked - naive).abs() < 1e-3,
            "chunked={chunked} naive={naive}"
        );
    }

    #[test]
    fn dot_f32_handles_non_multiple_of_lane_width() {
        // 13 is not a multiple of LANES (8) → exercises the scalar tail.
        let a: Vec<f32> = (0..13).map(|i| i as f32).collect();
        let b: Vec<f32> = (0..13).map(|i| (i * 2) as f32).collect();
        assert!((dot_f32(&a, &b) - naive_dot(&a, &b)).abs() < 1e-4);
    }

    #[test]
    fn quantize_zero_vector_is_zero_scale() {
        let q = quantize(&[0.0, 0.0, 0.0]);
        assert_eq!(q.scale, 0.0);
        assert_eq!(q.code, vec![0, 0, 0]);
        assert_eq!(dot_quant(&[1.0, 1.0, 1.0], &q), 0.0);
    }

    #[test]
    fn quantize_preserves_max_magnitude_at_full_scale() {
        let q = quantize(&[1.0, 0.0, 0.0]);
        assert_eq!(q.code[0], 127);
        assert_eq!(q.code[1], 0);
        // Reconstructed peak is the original max (scale = max/127, code = 127).
        let recon = q.dequantize();
        assert!((recon[0] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn dot_quant_approximates_cosine_for_normalized_vectors() {
        // Two similar L2-normalized vectors: quantized dot must track the true dot.
        let mut a: Vec<f32> = (0..384).map(|i| (i as f32 * 0.011).sin() + 0.3).collect();
        let mut b: Vec<f32> = a.iter().map(|x| x + 0.02).collect();
        l2_normalize(&mut a);
        l2_normalize(&mut b);

        let exact = naive_dot(&a, &b);
        let approx = dot_quant(&a, &quantize(&b));
        assert!(
            (exact - approx).abs() < 5e-3,
            "exact={exact} approx={approx}"
        );
        // Self-similarity stays ~1.0 after quantization.
        let self_sim = dot_quant(&a, &quantize(&a));
        assert!(self_sim > 0.99, "self_sim={self_sim}");
    }

    #[test]
    fn dot_quant_preserves_ranking() {
        // The most similar doc must still score highest after quantization.
        let query = normalized(vec![1.0, 0.2, 0.0, 0.1]);
        let near = quantize(&normalized(vec![0.9, 0.3, 0.0, 0.1]));
        let far = quantize(&normalized(vec![-0.5, 0.8, 0.2, 0.0]));
        assert!(dot_quant(&query, &near) > dot_quant(&query, &far));
    }

    fn l2_normalize(v: &mut [f32]) {
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for x in v.iter_mut() {
                *x /= norm;
            }
        }
    }

    fn normalized(mut v: Vec<f32>) -> Vec<f32> {
        l2_normalize(&mut v);
        v
    }
}
