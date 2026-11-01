//! Enhanced health dashboard for the Context Kernel.

/// Combined snapshot of kernel health, adaptation, search, evidence, and live state.
#[derive(Debug, Clone, serde::Serialize)]
pub struct EnhancedDashboard {
    /// Aggregated kernel health and activity.
    pub kernel_health: super::health::HealthReport,
    /// Current adaptive-compression state.
    pub adaptive: super::adaptive_bridge::AdaptiveSummary,
    /// Search activity recorded for the current session.
    pub search: crate::tools::search_kernel::SearchSummary,
    /// Evidence totals observed at dispatch boundaries.
    pub evidence_dispatch: super::evidence_wiring::DispatchSummary,
    /// Existing live dashboard snapshot.
    pub live: serde_json::Value,
}

/// Collects an enhanced dashboard snapshot from all kernel reporting surfaces.
#[must_use]
pub fn enhanced_dashboard() -> EnhancedDashboard {
    let live = serde_json::from_str(&super::live_dashboard::snapshot_json())
        .unwrap_or_else(|error| serde_json::json!({ "error": error.to_string() }));
    EnhancedDashboard {
        kernel_health: super::health::kernel_health(),
        adaptive: super::adaptive_bridge::adaptive_summary(),
        search: crate::tools::search_kernel::search_summary(),
        evidence_dispatch: super::evidence_wiring::dispatch_summary(),
        live,
    }
}

/// Serializes the enhanced dashboard, returning a JSON error object on failure.
#[must_use]
pub fn health_json() -> String {
    serde_json::to_string(&enhanced_dashboard())
        .unwrap_or_else(|error| serde_json::json!({ "error": error.to_string() }).to_string())
}

#[cfg(test)]
pub mod tests {
    use super::{enhanced_dashboard, health_json};
    use crate::core::context_kernel::{adaptive_bridge, evidence_wiring, kernel_config};
    use crate::tools::search_kernel;

    fn isolated() -> std::sync::MutexGuard<'static, ()> {
        let guard = kernel_config::KERNEL_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        kernel_config::reset_features();
        adaptive_bridge::reset();
        search_kernel::reset();
        evidence_wiring::reset();
        guard
    }

    #[test]
    fn enhanced_includes_all() {
        let _guard = isolated();
        adaptive_bridge::update_bounce_signal(0.15);
        search_kernel::record_search("health dashboard", 2, 8);
        evidence_wiring::record_from_tool_dispatch("ctx_search", 2, 8, 4);

        let dashboard = enhanced_dashboard();

        assert!(dashboard.kernel_health.subsystem_count > 0);
        assert!(dashboard.adaptive.signals_received > 0);
        assert!(dashboard.search.total_searches > 0);
        assert!(dashboard.evidence_dispatch.tool_dispatches > 0);
        assert!(dashboard.live.is_object());
    }

    #[test]
    fn health_json_valid() {
        let _guard = isolated();
        assert!(serde_json::from_str::<serde_json::Value>(&health_json()).is_ok());
    }
}
