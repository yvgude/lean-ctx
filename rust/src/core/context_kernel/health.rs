//! Aggregated Context Kernel health reporting.

/// Snapshot of health and activity across Context Kernel subsystems.
#[derive(Debug, Clone, serde::Serialize)]
pub struct HealthReport {
    /// Whether kernel startup initialization completed.
    pub initialized: bool,
    /// Whether the kernel master switch is enabled.
    pub kernel_enabled: bool,
    /// Fraction of deduplication checks that were cache hits.
    pub dedup_hit_rate: f64,
    /// Number of deduplication checks performed.
    pub dedup_total_checks: usize,
    /// Number of schema optimizations applied.
    pub schema_optimizations: usize,
    /// Estimated tokens saved by schema optimization.
    pub schema_tokens_saved: usize,
    /// Number of canonical evidence envelopes recorded.
    pub evidence_total_envelopes: usize,
    /// Number of evidence receipt-chain entries recorded.
    pub evidence_chain_entries: usize,
    /// Source of the effective kernel configuration.
    pub config_source: String,
    /// Number of subsystems represented by this report.
    pub subsystem_count: usize,
}

/// Returns an aggregated snapshot of Context Kernel health and activity.
#[must_use]
pub fn kernel_health() -> HealthReport {
    let startup_status = super::startup::status();
    let initialized = super::startup::is_initialized() && startup_status.initialized;
    let configured_features = super::kernel_config::features();
    let kernel_enabled = super::kernel_config::is_enabled()
        && startup_status.kernel_enabled
        && configured_features.enabled;
    let dedup = super::dedup_wiring::dedup_stats();
    let schema = super::schema_wiring::schema_savings();
    let evidence = super::envelope_wiring::evidence_summary();
    let (effective_features, config_source) = super::config_bridge::effective_config();

    HealthReport {
        initialized,
        kernel_enabled: kernel_enabled && effective_features.enabled,
        dedup_hit_rate: dedup.hit_rate,
        dedup_total_checks: dedup.total_checks,
        schema_optimizations: schema.optimizations_applied,
        schema_tokens_saved: schema.total_tokens_saved,
        evidence_total_envelopes: evidence.total_envelopes,
        evidence_chain_entries: evidence.chain_entries,
        config_source: format!("{config_source:?}"),
        subsystem_count: subsystem_names().len(),
    }
}

/// Returns whether the initialized Context Kernel is enabled and error-free.
#[must_use]
pub fn is_healthy() -> bool {
    let report = kernel_health();
    report.initialized && report.kernel_enabled
}

/// Returns the names of subsystems represented by [`HealthReport`].
#[must_use]
pub fn subsystem_names() -> &'static [&'static str] {
    &[
        "startup",
        "kernel_config",
        "dedup",
        "schema",
        "evidence",
        "config_bridge",
    ]
}

#[cfg(test)]
pub mod tests {
    use super::{is_healthy, kernel_health};
    use crate::core::context_kernel::{
        dedup_wiring, envelope_wiring, kernel_config, schema_wiring, startup,
    };

    fn isolated() -> std::sync::MutexGuard<'static, ()> {
        let guard = kernel_config::KERNEL_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        kernel_config::reset_features();
        startup::reset();
        dedup_wiring::reset_dedup();
        schema_wiring::reset_schema_state();
        envelope_wiring::reset_evidence();
        guard
    }

    #[test]
    fn health_after_init() {
        let _guard = isolated();
        startup::initialize();
        assert!(kernel_health().initialized);
    }

    #[test]
    fn health_shows_dedup_stats() {
        let _guard = isolated();
        let _ = dedup_wiring::check_content("health.rs", "same", false);
        let _ = dedup_wiring::check_content("health.rs", "same", false);
        let report = kernel_health();
        assert!(report.dedup_hit_rate > 0.0);
        assert_eq!(report.dedup_total_checks, 2);
    }

    #[test]
    fn healthy_when_enabled() {
        let _guard = isolated();
        startup::initialize();
        assert!(is_healthy());
    }

    #[test]
    fn health_reports_uninitialized_until_startup() {
        let _guard = isolated();
        assert!(!kernel_health().initialized);
        startup::initialize();
        assert!(kernel_health().initialized);
    }
}
