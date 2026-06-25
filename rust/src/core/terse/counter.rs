//! Token counting utilities for terse compression.
//!
//! Wraps the existing `core::tokens` module to provide compression-specific
//! counting with before/after tracking.

use crate::core::tokens::{self, TokenizerFamily};

#[must_use]
pub fn count(text: &str) -> u32 {
    tokens::count_tokens(text) as u32
}

#[must_use]
pub fn count_with_family(text: &str, family: TokenizerFamily) -> u32 {
    tokens::count_tokens_for(text, family) as u32
}

/// Calculates savings percentage from before/after token counts.
#[must_use]
pub fn savings_pct(before: u32, after: u32) -> f32 {
    if before == 0 || after >= before {
        return 0.0;
    }
    ((before - after) as f32 / before as f32) * 100.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn count_non_empty() {
        let n = count("Hello, world! This is a test.");
        assert!(n > 0);
    }

    #[test]
    fn count_empty_is_zero() {
        assert_eq!(count(""), 0);
    }

    #[test]
    fn savings_pct_zero_before() {
        assert_eq!(savings_pct(0, 0), 0.0);
    }

    #[test]
    fn savings_pct_half() {
        let pct = savings_pct(100, 50);
        assert!((pct - 50.0).abs() < 0.01);
    }

    #[test]
    fn savings_pct_full() {
        let pct = savings_pct(100, 0);
        assert!((pct - 100.0).abs() < 0.01);
    }

    #[test]
    fn savings_pct_none() {
        let pct = savings_pct(100, 100);
        assert!(pct.abs() < 0.01);
    }
}
