// SPDX-License-Identifier: Apache-2.0
//! `/api/kernel` — Context Kernel metrics for the cockpit dashboard.
//!
//! Aggregates kernel health, provider stats, evidence, and ETPAO into
//! a single JSON response consumed by the cockpit UI.

use serde_json::{Value, json};

/// Handles `/api/kernel` GET requests.
pub(crate) fn handle(
    path: &str,
    _query_str: &str,
    _method: &str,
    _body: &str,
) -> Option<(&'static str, &'static str, String)> {
    if path != "/api/kernel" {
        return None;
    }
    Some(("200 OK", "application/json; charset=utf-8", kernel_json()))
}

fn kernel_json() -> String {
    let report = lean_ctx::core::context_kernel::dashboard_report::generate_report();
    let provider_stats = lean_ctx::core::context_kernel::envelope_bridge::provider_stats();
    let health_report = lean_ctx::core::context_kernel::health::kernel_health();
    let evidence = lean_ctx::core::context_kernel::evidence_wiring::dispatch_summary();

    let providers: Vec<Value> = provider_stats
        .iter()
        .map(|p| {
            json!({
                "provider": lean_ctx::core::context_kernel::provider_parity::provider_display_name(p.provider),
                "requests": p.request_count,
                "input_tokens": p.total_input,
                "output_tokens": p.total_output,
                "cache_read_tokens": p.total_cache_read,
                "avg_input": p.avg_input,
            })
        })
        .collect();

    let subsystems: Vec<Value> = report
        .subsystems
        .iter()
        .map(|s| {
            json!({
                "name": s.name,
                "active": s.active,
                "detail": s.detail,
            })
        })
        .collect();

    let body = json!({
        "enabled": report.kernel_enabled,
        "health": report.health_status,
        "version": report.version,
        "subsystems": subsystems,
        "savings": {
            "dedup_hits": report.savings.dedup_hits,
            "schema_tokens_saved": report.savings.schema_tokens_saved,
            "evidence_dispatches": report.savings.evidence_dispatches,
            "response_cached": report.savings.response_cached,
        },
        "providers": providers,
        "evidence": {
            "proxy_dispatches": evidence.proxy_dispatches,
            "tool_dispatches": evidence.tool_dispatches,
            "total_input": evidence.total_input_tokens,
            "total_output": evidence.total_output_tokens,
        },
        "health_details": {
            "initialized": health_report.initialized,
            "dedup_hit_rate": health_report.dedup_hit_rate,
            "schema_optimizations": health_report.schema_optimizations,
        },
    });

    serde_json::to_string(&body).unwrap_or_else(|_| "{}".to_string())
}
