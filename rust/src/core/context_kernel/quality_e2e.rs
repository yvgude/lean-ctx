//! Quality and integration end-to-end conformance tests.

#[cfg(test)]
mod tests {
    use super::super::accounting_fix;

    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < 1e-9,
            "expected {expected}, got {actual}"
        );
    }

    #[test]
    fn honest_accounting_vs_phantom() {
        let accounting = accounting_fix::compute_honest_accounting(1_000, 300, 100, 50);

        assert_eq!(accounting.delivered_tokens, 450);
        assert_close(accounting.actual_compression_ratio, 0.55);
        assert_close(accounting.reported_compression_ratio, 0.70);
        assert_close(accounting.phantom_savings_pct, 0.15);
    }
}
