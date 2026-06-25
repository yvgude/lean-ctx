//! Pooling strategies for transformer hidden states.
//!
//! Converts per-token hidden states `[seq_len × dim]` into a single
//! fixed-size embedding vector `[dim]`.

/// Mean pooling over token positions, weighted by attention mask.
///
/// Takes the raw hidden state output `[1 × seq_len × dim]` flattened to a Vec,
/// and produces a single embedding by averaging across attended positions.
#[must_use]
pub fn mean_pool(
    hidden_states: &[f32],
    attention_mask: &[i32],
    seq_len: usize,
    dim: usize,
) -> Vec<f32> {
    let mut sum = vec![0.0f32; dim];
    let mut count = 0.0f32;

    for pos in 0..seq_len {
        if attention_mask.get(pos).copied().unwrap_or(0) > 0 {
            let offset = pos * dim;
            for (d, sum_val) in sum.iter_mut().enumerate().take(dim) {
                if let Some(&val) = hidden_states.get(offset + d) {
                    *sum_val += val;
                }
            }
            count += 1.0;
        }
    }

    if count > 0.0 {
        for val in &mut sum {
            *val /= count;
        }
    }

    sum
}

/// Batched mean pooling over multiple sequences in a single output tensor.
///
/// The model output is `[batch, max_seq_len, dim]` flattened row-major. Each
/// sequence is mean-pooled using its own attention mask to exclude padding.
#[must_use]
pub fn mean_pool_batch(
    hidden_states: &[f32],
    masks: &[&[i32]],
    max_seq_len: usize,
    dim: usize,
) -> Vec<Vec<f32>> {
    let batch = masks.len();
    let expected_len = batch * max_seq_len * dim;
    if hidden_states.len() < expected_len {
        return vec![vec![0.0; dim]; batch];
    }
    let mut results = Vec::with_capacity(batch);
    for (b, m) in masks.iter().enumerate().take(batch) {
        let offset = b * max_seq_len * dim;
        let h = &hidden_states[offset..][..max_seq_len * dim];
        results.push(mean_pool(h, m, max_seq_len, dim));
    }
    results
}

/// L2-normalize a vector in-place.
pub fn normalize_l2(vec: &mut [f32]) {
    let norm: f32 = vec.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > f32::EPSILON {
        for x in vec.iter_mut() {
            *x /= norm;
        }
    }
}

/// Compute the L2 norm of a vector.
#[must_use]
pub fn l2_norm(vec: &[f32]) -> f32 {
    vec.iter().map(|x| x * x).sum::<f32>().sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mean_pool_basic() {
        // 2 tokens, 3 dimensions, all attended
        let hidden = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0];
        let mask = vec![1, 1];
        let result = mean_pool(&hidden, &mask, 2, 3);
        assert_eq!(result.len(), 3);
        assert!((result[0] - 2.5).abs() < 1e-6);
        assert!((result[1] - 3.5).abs() < 1e-6);
        assert!((result[2] - 4.5).abs() < 1e-6);
    }

    #[test]
    fn mean_pool_with_padding() {
        // 3 tokens, 2 dimensions, last token is padding
        let hidden = vec![1.0, 2.0, 3.0, 4.0, 0.0, 0.0];
        let mask = vec![1, 1, 0];
        let result = mean_pool(&hidden, &mask, 3, 2);
        assert!((result[0] - 2.0).abs() < 1e-6);
        assert!((result[1] - 3.0).abs() < 1e-6);
    }

    #[test]
    fn mean_pool_single_token() {
        let hidden = vec![5.0, 10.0];
        let mask = vec![1];
        let result = mean_pool(&hidden, &mask, 1, 2);
        assert!((result[0] - 5.0).abs() < 1e-6);
        assert!((result[1] - 10.0).abs() < 1e-6);
    }

    #[test]
    fn mean_pool_all_masked() {
        let hidden = vec![1.0, 2.0, 3.0, 4.0];
        let mask = vec![0, 0];
        let result = mean_pool(&hidden, &mask, 2, 2);
        assert!(result.iter().all(|&v| v == 0.0));
    }

    #[test]
    fn normalize_l2_basic() {
        let mut vec = vec![3.0, 4.0];
        normalize_l2(&mut vec);
        assert!((vec[0] - 0.6).abs() < 1e-6);
        assert!((vec[1] - 0.8).abs() < 1e-6);

        let norm: f32 = vec.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5);
    }

    #[test]
    fn normalize_l2_already_normalized() {
        let mut vec = vec![1.0, 0.0, 0.0];
        normalize_l2(&mut vec);
        assert!((vec[0] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn normalize_l2_zero_vector() {
        let mut vec = vec![0.0, 0.0, 0.0];
        normalize_l2(&mut vec);
        assert!(vec.iter().all(|&v| v == 0.0));
    }

    #[test]
    fn l2_norm_basic() {
        assert!((l2_norm(&[3.0, 4.0]) - 5.0).abs() < 1e-6);
    }

    #[test]
    fn l2_norm_unit() {
        assert!((l2_norm(&[1.0, 0.0, 0.0]) - 1.0).abs() < 1e-6);
    }
}
