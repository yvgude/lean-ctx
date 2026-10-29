use std::sync::{LazyLock, Mutex};
use std::time::Instant;

use axum::{Json, extract::State, response::Html};
use serde::Serialize;

use crate::proxy::{ProxyState, value_gate_proxy};

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

/// `GET /dashboard`: the proxy's own page gave way to the Cockpit. It names
/// the command and links the running Cockpit of this data dir when one is
/// recorded — never with the token (the Cockpit asks for it, or
/// `lean-ctx cockpit` opens it signed in).
pub(crate) async fn page() -> Html<String> {
    let cockpit = crate::dashboard::launch::read_record()
        .filter(crate::dashboard::launch::LaunchRecord::process_alive)
        .map(|record| (record.port, record.base_path));
    Html(cockpit_pointer_page(
        cockpit.as_ref().map(|(p, b)| (*p, b.as_str())),
    ))
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The pointer page; `cockpit` is the running Cockpit's port and subpath.
pub(crate) fn cockpit_pointer_page(cockpit: Option<(u16, &str)>) -> String {
    let link = cockpit.map_or_else(String::new, |(port, base_path)| {
        let url = escape(&format!("http://localhost:{port}{base_path}/#spend"));
        format!("<p>Running now: <a href=\"{url}\">{url}</a></p>\n")
    });
    format!(
        r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>LeanCTX Cockpit</title>
<style>
:root{{color-scheme:light dark;--fg:#1d1d1f;--muted:#5f6368;--bg:#fafafa;--card:#fff;--line:#dadce0}}
@media (prefers-color-scheme:dark){{:root{{--fg:#e8eaed;--muted:#9aa0a6;--bg:#151517;--card:#1f1f22;--line:#3c4043}}}}
body{{margin:0;background:var(--bg);color:var(--fg);font:16px/1.55 system-ui,-apple-system,"Segoe UI",sans-serif}}
main{{max-width:36rem;margin:12vh auto;padding:0 16px}}
pre{{background:var(--card);border:1px solid var(--line);border-radius:8px;padding:.75rem 1rem;overflow-x:auto}}
p.tag{{color:var(--muted)}}
</style></head>
<body><main>
<h1>LeanCTX Cockpit</h1>
<p class="tag">Spend, savings and proof of the context this proxy carries are in the local Cockpit.</p>
<pre><code>lean-ctx cockpit</code></pre>
{link}<p class="tag">Proxy counters stay available as JSON at <code>/api/stats</code>.</p>
</main></body></html>
"#
    )
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

#[cfg(test)]
mod tests {
    use super::cockpit_pointer_page;

    #[test]
    fn the_pointer_page_names_the_command_and_links_the_running_cockpit_without_a_token() {
        let without = cockpit_pointer_page(None);
        assert!(without.contains("lean-ctx cockpit"));
        assert!(without.contains("/api/stats"));
        assert!(!without.contains("http://localhost"));

        let with = cockpit_pointer_page(Some((3334, "/c")));
        assert!(with.contains("href=\"http://localhost:3334/c/#spend\""));
        assert!(!with.contains("token"));
        assert!(!with.contains("<script"));
    }
}
