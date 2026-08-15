use std::sync::{LazyLock, Mutex};
use std::time::Instant;

use axum::{Json, extract::State, response::Html};
use serde::Serialize;

use crate::proxy::{ProxyState, value_gate_proxy};

const DASHBOARD_HTML: &str = include_str!("dashboard.html");
static PROXY_STARTED: LazyLock<Mutex<Option<Instant>>> = LazyLock::new(|| Mutex::new(None));

#[derive(Serialize)]
pub(crate) struct ToolStat {
    tool: String,
    tokens_saved: u64,
    calls: u64,
}

#[derive(Serialize)]
pub(crate) struct DashboardStats {
    session_tokens_saved: u64,
    session_requests: u64,
    total_tokens_saved: u64,
    total_requests: u64,
    compression_ratio: f64,
    estimated_cost_saved_usd: f64,
    per_tool_stats: Vec<ToolStat>,
    uptime_seconds: u64,
}

pub(crate) fn mark_proxy_started() {
    let mut started = PROXY_STARTED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *started = Some(Instant::now());
}

pub(crate) async fn page() -> Html<&'static str> {
    Html(DASHBOARD_HTML)
}

pub(crate) async fn stats(State(state): State<ProxyState>) -> Json<DashboardStats> {
    let session = value_gate_proxy::session_metrics();
    let all_time = crate::core::stats::load_for_display();
    let proxy = &state.stats;
    let proxy_saved = proxy
        .tokens_saved
        .load(std::sync::atomic::Ordering::Relaxed);
    let proxy_requests = proxy
        .requests_total
        .load(std::sync::atomic::Ordering::Relaxed);
    let total_saved = all_time
        .total_input_tokens
        .saturating_sub(all_time.total_output_tokens)
        .max(proxy_saved);
    let total_requests = all_time.total_commands.max(proxy_requests);
    let mut per_tool_stats = all_time
        .commands
        .iter()
        .map(|(tool, stats)| ToolStat {
            tool: tool.clone(),
            tokens_saved: stats.input_tokens.saturating_sub(stats.output_tokens),
            calls: stats.count,
        })
        .filter(|tool| tool.tokens_saved > 0 || tool.calls > 0)
        .collect::<Vec<_>>();
    per_tool_stats.sort_by_key(|tool| std::cmp::Reverse(tool.tokens_saved));
    per_tool_stats.truncate(8);

    let uptime_seconds = PROXY_STARTED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
        .map_or(0, |started| started.elapsed().as_secs());
    let compression_ratio = if session.total_original_tokens > 0 {
        value_gate_proxy::compression_ratio()
    } else {
        proxy.compression_ratio()
    };
    let estimated_cost_saved_usd = if session.cost_micros_estimate > 0 {
        session.cost_micros_estimate as f64 / 1_000_000.0
    } else {
        (total_saved as f64 / 1_000_000.0) * 2.50
    };

    Json(DashboardStats {
        session_tokens_saved: session.total_tokens_pruned,
        session_requests: session.request_count,
        total_tokens_saved: total_saved,
        total_requests,
        compression_ratio,
        estimated_cost_saved_usd,
        per_tool_stats,
        uptime_seconds,
    })
}
