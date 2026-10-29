//! HTTP route handlers for the LeanCTX dashboard API.

mod agent_control;
mod agents;
mod analysis_bridge;
mod context;
mod decisions;
mod doctor;
mod feedback;
mod gateway_connect;
mod gateway_receipts;
mod gateway_stages;
mod graph;
pub(crate) mod health;
pub mod helpers;
mod hive_workspace;
mod kernel;
mod knowledge;
mod learning;
mod memory;
mod provenance;
pub(crate) mod response_cache;
mod risk;
mod roi;
mod scope;
mod settings;
mod signals;
mod snapshots;
mod solution;
mod stats;
mod system;
mod telemetry;
mod tools;
#[cfg(feature = "enterprise")]
mod usage_breakdown;

use std::sync::Arc;

pub fn route_response(
    path: &str,
    query_str: &str,
    _query_token: Option<&String>,
    token: Option<&Arc<String>>,
    _is_loopback: bool,
    method: &str,
    body: &str,
) -> (&'static str, &'static str, String) {
    // The HTTP boundary already verified the bearer and CSRF headers. Unlike
    // passive dashboard views, execution control is unavailable in no-auth mode.
    if path == "/api/agents/work-graph" {
        if token.is_none() {
            return (
                "403 Forbidden",
                "application/json",
                r#"{"error":"authenticated dashboard required"}"#.into(),
            );
        }
        return agent_control::handle(method, body);
    }

    if path == analysis_bridge::INPUTS || path == analysis_bridge::APPLY {
        return analysis_bridge::handle(path, token.is_some(), method, body);
    }

    // Response cache: return cached response for expensive read-only routes.
    let cache_key = response_cache::key(path);
    if response_cache::is_cacheable(path, method) {
        if let Some(cached) = response_cache::get(&cache_key) {
            return cached;
        }
    }
    let response = stats::handle(path, query_str, method, body)
        .or_else(|| signals::handle(path, query_str, method, body))
        .or_else(|| context::handle(path, query_str, method, body))
        .or_else(|| risk::handle(path, query_str, method, body))
        .or_else(|| roi::handle(path, query_str, method, body))
        .or_else(|| snapshots::handle(path, query_str, method, body))
        .or_else(|| knowledge::handle(path, query_str, method, body))
        .or_else(|| learning::handle(path, query_str, method, body))
        .or_else(|| memory::handle(path, query_str, method, body))
        .or_else(|| graph::handle(path, query_str, method, body))
        .or_else(|| agents::handle(path, query_str, method, body))
        .or_else(|| tools::handle(path, query_str, method, body))
        .or_else(|| settings::handle(path, query_str, method, body))
        .or_else(|| kernel::handle(path, query_str, method, body))
        .or_else(|| doctor::handle(path, query_str, method, body))
        .or_else(|| feedback::handle(path, query_str, method, body))
        .or_else(|| health::handle(path, query_str, method, body))
        .or_else(|| scope::handle(path, query_str, method, body))
        .or_else(|| provenance::handle(path, query_str, method, body))
        .or_else(|| gateway_receipts::handle(path, query_str, method, body))
        .or_else(|| gateway_connect::handle(path, query_str, method, body))
        .or_else(|| gateway_stages::handle(path, query_str, method, body))
        .or_else(|| hive_workspace::handle(path, query_str, method, body))
        .or_else(|| decisions::handle(path, query_str, method, body));
    #[cfg(feature = "enterprise")]
    let response = response.or_else(|| usage_breakdown::handle(path, query_str, method, body));
    let result = response
        .or_else(|| solution::handle(path, query_str, method, body))
        .or_else(|| telemetry::handle(path, query_str, method, body))
        .or_else(|| system::handle(path, query_str, method, body))
        .unwrap_or_else(|| ("404 Not Found", "text/plain", "Not Found".to_string()));

    // Store in cache for subsequent requests within the TTL window.
    if response_cache::is_cacheable(path, method) && result.0 == "200 OK" {
        response_cache::put(&cache_key, result.1, &result.2);
    }
    result
}
