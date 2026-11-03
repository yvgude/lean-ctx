//! Structured dashboard reporting across Context Kernel subsystems.

use serde::Serialize;

/// Activity status and human-readable detail for one kernel subsystem.
#[derive(Debug, Clone, Serialize)]
pub struct SubsystemStatus {
    /// Display name of the subsystem.
    pub name: String,
    /// Whether the subsystem has recorded activity.
    pub active: bool,
    /// One-line summary of the subsystem's current metrics.
    pub detail: String,
}

/// Token and evidence savings summarized across kernel subsystems.
#[derive(Debug, Clone, Default, Serialize)]
pub struct TokenSavingsSummary {
    /// Number of content-deduplication cache hits.
    pub dedup_hits: usize,
    /// Estimated tokens saved by schema optimization.
    pub schema_tokens_saved: usize,
    /// Number of evidence dispatches recorded.
    pub evidence_dispatches: usize,
    /// Number of responses served from cache.
    pub response_cached: usize,
}

/// Aggregated report for the Context Kernel dashboard.
#[derive(Debug, Clone, Serialize)]
pub struct DashboardReport {
    /// Version of lean-ctx that generated the report.
    pub version: &'static str,
    /// Whether the Context Kernel master switch is enabled.
    pub kernel_enabled: bool,
    /// Overall status: `healthy`, `degraded`, or `disabled`.
    pub health_status: String,
    /// Activity summaries for each represented subsystem.
    pub subsystems: Vec<SubsystemStatus>,
    /// Combined token and evidence savings.
    pub savings: TokenSavingsSummary,
    /// Per-provider request distribution.
    pub provider_distribution: Vec<super::envelope_bridge::ProviderStat>,
}

/// Generates a point-in-time report from all dashboard subsystems.
#[must_use]
pub fn generate_report() -> DashboardReport {
    let kernel_enabled = super::kernel_config::is_enabled();
    let dedup = super::ctx_read_dedup::dedup_summary();
    let schema = super::schema_wiring::schema_savings();
    let evidence = super::evidence_wiring::dispatch_summary();
    let adaptive = super::adaptive_bridge::adaptive_summary();
    let search = crate::core::search_kernel::search_summary();
    let response = super::response_evidence::response_summary();
    let dispatches = evidence.tool_dispatches + evidence.proxy_dispatches;
    let health_status = if !kernel_enabled {
        "disabled"
    } else if super::health::is_healthy() {
        "healthy"
    } else {
        "degraded"
    };

    DashboardReport {
        version: env!("CARGO_PKG_VERSION"),
        kernel_enabled,
        health_status: health_status.to_owned(),
        subsystems: vec![
            status(
                "Content Dedup",
                dedup.total_reads > 0,
                format!(
                    "{} hits, {} tokens saved",
                    dedup.dedup_hits, dedup.tokens_saved
                ),
            ),
            status(
                "Schema Opt",
                schema.optimizations_applied > 0,
                format!(
                    "{} optimizations, {} tokens saved",
                    schema.optimizations_applied, schema.total_tokens_saved
                ),
            ),
            status(
                "Evidence",
                dispatches > 0,
                format!("{dispatches} dispatches recorded"),
            ),
            status(
                "Adaptive",
                adaptive.signals_received > 0,
                format!(
                    "bounce rate {:.2}, advice: {:?}",
                    adaptive.current_bounce_rate, adaptive.advice
                ),
            ),
            status(
                "Search",
                search.total_searches > 0,
                format!(
                    "{} searches, {} repeats detected",
                    search.total_searches, search.repeated_queries
                ),
            ),
            status(
                "Response",
                response.total_responses > 0,
                format!(
                    "{} responses, {:.0}% cache hit",
                    response.total_responses,
                    response.cache_hit_rate * 100.0
                ),
            ),
        ],
        savings: TokenSavingsSummary {
            dedup_hits: dedup.dedup_hits,
            schema_tokens_saved: schema.total_tokens_saved,
            evidence_dispatches: dispatches,
            response_cached: response.cached_responses,
        },
        provider_distribution: super::envelope_bridge::provider_stats(),
    }
}

fn status(name: &str, active: bool, detail: String) -> SubsystemStatus {
    SubsystemStatus {
        name: name.to_owned(),
        active,
        detail,
    }
}

/// Formats a dashboard report for terminal display.
#[must_use]
pub fn format_report(report: &DashboardReport) -> String {
    let kernel = if report.kernel_enabled { "ON" } else { "OFF" };
    let subsystems = report
        .subsystems
        .iter()
        .map(|subsystem| {
            let marker = if subsystem.active { '✓' } else { '○' };
            format!("  {marker} {:<15} — {}", subsystem.name, subsystem.detail)
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "═══ lean-ctx Kernel Dashboard ═══\n\
         Version: {} | Status: {} | Kernel: {kernel}\n\n\
         Subsystems:\n{subsystems}\n\n\
         Savings: dedup={} schema={}tok evidence={} cached={}",
        report.version,
        report.health_status,
        report.savings.dedup_hits,
        report.savings.schema_tokens_saved,
        report.savings.evidence_dispatches,
        report.savings.response_cached,
    )
}

/// Serializes a dashboard report as pretty-printed JSON.
#[must_use]
pub fn report_json(report: &DashboardReport) -> String {
    serde_json::to_string_pretty(report)
        .unwrap_or_else(|_| r#"{"error":"dashboard serialization failed"}"#.to_owned())
}

#[cfg(test)]
pub mod tests {
    use super::{format_report, generate_report, report_json};

    #[test]
    fn report_populated() {
        assert!(!generate_report().subsystems.is_empty());
    }

    #[test]
    fn format_contains_sections() {
        let formatted = format_report(&generate_report());
        assert!(formatted.contains("Dashboard"));
        assert!(formatted.contains("Subsystems"));
        assert!(formatted.contains("Savings"));
    }

    #[test]
    fn json_valid() {
        let json = report_json(&generate_report());
        assert!(serde_json::from_str::<serde_json::Value>(&json).is_ok());
    }
}
