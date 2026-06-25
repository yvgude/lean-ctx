//! Deterministic pseudo-random number generation via splitmix64.
//!
//! Used for LSH hyperplane generation and any other context that
//! needs reproducible randomness without pulling in the `rand` crate.

/// Splitmix64 — deterministic 64-bit PRNG.
///
/// Standard algorithm from Numerical Recipes.  Pure function: given
/// the same `state` it always returns the same result.
///
/// ```
/// let x = lean_ctx_core::prng::splitmix64(42);
/// assert_eq!(x, 2407408988495057637);
/// ```
#[inline]
#[must_use]
pub fn splitmix64(state: u64) -> u64 {
    let mut z = state.wrapping_add(0x9e3779b97f4a7c15u64);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9u64);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111ebu64);
    z ^ (z >> 31)
}

/// Deterministic `f32` in `[0, 1)` from a `splitmix64` value.
///
/// Uses the high 23 bits (24-bit mantissa precision) to produce a
/// uniform float.
#[inline]
#[must_use]
pub fn splitmix64_f32(state: u64) -> f32 {
    let value = splitmix64(state);
    (value >> 40) as f32 / (1u64 << 24) as f32
}

/// Deterministic `f32` in `[-0.5, 0.5]` for hyperplane generation.
#[inline]
#[must_use]
pub fn splitmix64_f32_signed(state: u64) -> f32 {
    splitmix64_f32(state) - 0.5
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic() {
        assert_eq!(splitmix64(42), 13679457532755275413);
        assert_eq!(splitmix64(0), 16294208416658607535);
    }

    #[test]
    fn different_seeds_different_results() {
        assert_ne!(splitmix64(1), splitmix64(2));
    }

    #[test]
    fn f32_range() {
        for seed in [0u64, 1, 42, 999, u64::MAX] {
            let v = splitmix64_f32(seed);
            assert!((0.0..1.0).contains(&v), "seed={seed} yielded {v}");
        }
    }

    #[test]
    fn signed_range() {
        for seed in [0u64, 1, 42, 999, u64::MAX] {
            let v = splitmix64_f32_signed(seed);
            assert!((-0.5..=0.5).contains(&v), "seed={seed} yielded {v}");
        }
    }

    #[test]
    fn signed_is_f32_minus_half() {
        for seed in [0u64, 1, 42, 999, u64::MAX] {
            assert_eq!(
                splitmix64_f32_signed(seed),
                splitmix64_f32(seed) - 0.5,
                "seed={seed}",
            );
        }
    }
}
