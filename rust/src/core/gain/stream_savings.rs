//! Provider-stream economics for gain reporting (#1191).

use serde::{Deserialize, Serialize};

use super::model_pricing::ModelCost;

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
pub struct StreamSavings {
    pub first_inject_tokens_saved: u64,
    pub reread_tokens_saved: u64,
    pub input_tokens_saved: u64,
    pub output_tokens_saved: u64,
    pub first_inject_overhead_tokens: u64,
    pub reread_overhead_tokens: u64,
    pub bounce_tokens: u64,
    pub cache_write_usd_saved: f64,
    pub cache_read_usd_saved: f64,
    pub input_usd_saved: f64,
    pub output_usd_saved: f64,
    pub gross_usd_saved: f64,
    pub overhead_usd: f64,
    pub net_usd_saved: f64,
}

impl StreamSavings {
    /// `native_baseline` is the native prefix lean-ctx's prefix replaces
    /// (`context_overhead::native_baseline_tokens_per_turn`: Replace mode only).
    #[must_use]
    pub fn calculate(
        first_inject_tokens_saved: u64,
        reread_tokens_saved: u64,
        overhead_per_turn: u64,
        native_baseline: u64,
        turns: u64,
        bounce_tokens: u64,
        cost: ModelCost,
    ) -> Self {
        let overhead_delta = overhead_per_turn.saturating_sub(native_baseline);
        let first_inject_overhead_tokens = if turns > 0 { overhead_delta } else { 0 };
        let reread_overhead_tokens = overhead_delta.saturating_mul(turns.saturating_sub(1));
        let cache_write_usd_saved = usd(first_inject_tokens_saved, cost.cache_write_per_m);
        let cache_read_usd_saved = usd(reread_tokens_saved, cost.cache_read_per_m);
        let gross_usd_saved = cache_write_usd_saved + cache_read_usd_saved;
        let overhead_usd = usd(
            first_inject_overhead_tokens.saturating_add(bounce_tokens),
            cost.cache_write_per_m,
        ) + usd(reread_overhead_tokens, cost.cache_read_per_m);

        Self {
            first_inject_tokens_saved,
            reread_tokens_saved,
            input_tokens_saved: 0,
            output_tokens_saved: 0,
            first_inject_overhead_tokens,
            reread_overhead_tokens,
            bounce_tokens,
            cache_write_usd_saved,
            cache_read_usd_saved,
            input_usd_saved: 0.0,
            output_usd_saved: 0.0,
            gross_usd_saved,
            overhead_usd,
            net_usd_saved: gross_usd_saved - overhead_usd,
        }
    }
}

fn usd(tokens: u64, rate_per_m: f64) -> f64 {
    tokens as f64 / 1_000_000.0 * rate_per_m
}

#[cfg(test)]
pub mod tests {
    use super::*;

    const REAL_RATES: ModelCost = ModelCost {
        input_per_m: 5.0,
        output_per_m: 25.0,
        cache_write_per_m: 6.25,
        cache_read_per_m: 0.5,
    };

    #[test]
    fn million_first_inject_tokens_save_cache_write_rate() {
        let s = StreamSavings::calculate(1_000_000, 0, 2_400, 2_400, 1, 0, REAL_RATES);
        assert_eq!(s.cache_write_usd_saved, 6.25);
        assert_eq!(s.net_usd_saved, 6.25);
    }

    #[test]
    fn million_reread_tokens_save_only_cache_read_rate() {
        let s = StreamSavings::calculate(0, 1_000_000, 2_400, 2_400, 1, 0, REAL_RATES);
        assert_eq!(s.cache_read_usd_saved, 0.5);
        assert_eq!(s.net_usd_saved, 0.5);
    }

    #[test]
    fn same_token_count_is_twelve_and_half_times_more_valuable_on_first_inject() {
        let s = StreamSavings::calculate(100_000, 100_000, 2_400, 2_400, 1, 0, REAL_RATES);
        assert_eq!(s.cache_write_usd_saved / s.cache_read_usd_saved, 12.5);
    }

    #[test]
    fn fixed_overhead_splits_into_write_then_reads() {
        let s = StreamSavings::calculate(0, 0, 3_400, 2_400, 4, 0, REAL_RATES);
        assert_eq!(s.first_inject_overhead_tokens, 1_000);
        assert_eq!(s.reread_overhead_tokens, 3_000);
        assert!((s.overhead_usd - 0.00775).abs() < 1e-12);
    }

    #[test]
    fn native_baseline_is_subtracted_only_when_it_is_replaced() {
        let replace = StreamSavings::calculate(10_000, 0, 2_000, 2_400, 50, 0, REAL_RATES);
        assert_eq!(replace.first_inject_overhead_tokens, 0);
        assert_eq!(replace.reread_overhead_tokens, 0);
        assert_eq!(replace.overhead_usd, 0.0);
        // Hybrid/MCP keep the native tools, so the whole prefix is charged.
        let hybrid = StreamSavings::calculate(10_000, 0, 2_000, 0, 50, 0, REAL_RATES);
        assert_eq!(hybrid.first_inject_overhead_tokens, 2_000);
        assert_eq!(hybrid.reread_overhead_tokens, 98_000);
    }

    #[test]
    fn no_observed_turn_means_no_guessed_overhead() {
        let s = StreamSavings::calculate(10_000, 0, 9_000, 2_400, 0, 0, REAL_RATES);
        assert_eq!(s.first_inject_overhead_tokens, 0);
        assert_eq!(s.reread_overhead_tokens, 0);
    }

    #[test]
    fn bounce_waste_can_make_net_savings_negative() {
        let s = StreamSavings::calculate(100, 0, 2_400, 2_400, 1, 1_000, REAL_RATES);
        assert!(s.net_usd_saved < 0.0);
        assert!((s.overhead_usd - 0.00625).abs() < 1e-12);
    }

    #[test]
    fn output_and_new_input_are_zero_without_measured_evidence() {
        let s = StreamSavings::calculate(1_000, 2_000, 2_400, 2_400, 2, 0, REAL_RATES);
        assert_eq!(s.input_tokens_saved, 0);
        assert_eq!(s.output_tokens_saved, 0);
        assert_eq!(s.input_usd_saved, 0.0);
        assert_eq!(s.output_usd_saved, 0.0);
    }
}
