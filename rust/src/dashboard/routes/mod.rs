//! HTTP route handlers for the `LeanCTX` dashboard API.

mod agents;
mod context;
mod doctor;
mod graph;
pub mod helpers;
mod knowledge;
mod leaderboard;
mod learning;
mod memory;
mod risk;
mod roi;
mod settings;
mod signals;
mod stats;
mod system;
mod tools;

use std::sync::Arc;

fn match_component_path(path: &str) -> Option<String> {
    let content = match path {
        "/static/components/cockpit-nav.js" => super::COCKPIT_COMPONENT_NAV_JS,
        "/static/components/cockpit-context.js" => super::COCKPIT_COMPONENT_CONTEXT_JS,
        "/static/components/cockpit-overview.js" => super::COCKPIT_COMPONENT_OVERVIEW_JS,
        "/static/components/cockpit-live.js" => super::COCKPIT_COMPONENT_LIVE_JS,
        "/static/components/cockpit-knowledge.js" => super::COCKPIT_COMPONENT_KNOWLEDGE_JS,
        "/static/components/cockpit-agents.js" => super::COCKPIT_COMPONENT_AGENTS_JS,
        "/static/components/cockpit-memory.js" => super::COCKPIT_COMPONENT_MEMORY_JS,
        "/static/components/cockpit-search.js" => super::COCKPIT_COMPONENT_SEARCH_JS,
        "/static/components/cockpit-compression.js" => super::COCKPIT_COMPONENT_COMPRESSION_JS,
        "/static/components/cockpit-tour.js" => super::COCKPIT_COMPONENT_TOUR_JS,
        "/static/components/cockpit-graph.js" => super::COCKPIT_COMPONENT_GRAPH_JS,
        "/static/components/cockpit-architecture.js" => super::COCKPIT_COMPONENT_ARCHITECTURE_JS,
        "/static/components/cockpit-explorer.js" => super::COCKPIT_COMPONENT_EXPLORER_JS,
        "/static/components/cockpit-health.js" => super::COCKPIT_COMPONENT_HEALTH_JS,
        "/static/components/cockpit-remaining.js" => super::COCKPIT_COMPONENT_REMAINING_JS,
        "/static/components/cockpit-commander.js" => super::COCKPIT_COMPONENT_COMMANDER_JS,
        "/static/components/cockpit-palette.js" => super::COCKPIT_COMPONENT_PALETTE_JS,
        "/static/components/cockpit-roi.js" => super::COCKPIT_COMPONENT_ROI_JS,
        "/static/components/cockpit-leaderboard.js" => super::COCKPIT_COMPONENT_LEADERBOARD_JS,
        "/static/components/cockpit-area-tabs.js" => super::COCKPIT_COMPONENT_AREA_TABS_JS,
        "/static/components/cockpit-protection.js" => super::COCKPIT_COMPONENT_PROTECTION_JS,
        "/static/components/cockpit-settings.js" => super::COCKPIT_COMPONENT_SETTINGS_JS,
        _ => return None,
    };
    Some(content.to_string())
}

#[must_use]
pub fn route_response(
    path: &str,
    query_str: &str,
    query_token: Option<&String>,
    token: Option<&Arc<String>>,
    _is_loopback: bool,
    method: &str,
    body: &str,
) -> (&'static str, &'static str, String) {
    if path == "/" || path == "/index.html" || path == "/cockpit" || path == "/cockpit/" {
        let mut html = super::COCKPIT_INDEX_HTML.to_string();
        if let Some(t) = token {
            let expected = t.as_str();
            let valid_query = query_token
                .as_ref()
                .is_some_and(|q| super::constant_time_eq(q.as_bytes(), expected.as_bytes()));
            if valid_query {
                let script = format!(
                    "<script>window.__LEAN_CTX_TOKEN__=\"{expected}\";try{{if(location.search.includes('token=')){{history.replaceState(null,'',location.pathname+location.hash);}}}}catch(e){{}}</script>"
                );
                html = html.replacen("<head>", &format!("<head>{script}"), 1);
            }
        }
        return ("200 OK", "text/html; charset=utf-8", html);
    }
    if path == "/static/style.css" {
        return (
            "200 OK",
            "text/css; charset=utf-8",
            super::COCKPIT_STYLE_CSS.to_string(),
        );
    }
    if path == "/static/lib/api.js" {
        return (
            "200 OK",
            "application/javascript; charset=utf-8",
            super::COCKPIT_LIB_API_JS.to_string(),
        );
    }
    if path == "/static/lib/format.js" {
        return (
            "200 OK",
            "application/javascript; charset=utf-8",
            super::COCKPIT_LIB_FORMAT_JS.to_string(),
        );
    }
    if path == "/static/lib/router.js" {
        return (
            "200 OK",
            "application/javascript; charset=utf-8",
            super::COCKPIT_LIB_ROUTER_JS.to_string(),
        );
    }
    if path == "/static/lib/charts.js" {
        return (
            "200 OK",
            "application/javascript; charset=utf-8",
            super::COCKPIT_LIB_CHARTS_JS.to_string(),
        );
    }
    if path == "/static/lib/shared.js" {
        return (
            "200 OK",
            "application/javascript; charset=utf-8",
            super::COCKPIT_LIB_SHARED_JS.to_string(),
        );
    }
    if path == "/static/lib/doctor.js" {
        return (
            "200 OK",
            "application/javascript; charset=utf-8",
            super::COCKPIT_LIB_DOCTOR_JS.to_string(),
        );
    }
    if let Some(content) = match_component_path(path) {
        return ("200 OK", "application/javascript; charset=utf-8", content);
    }
    if path == "/static/fonts/fonts.css" {
        return (
            "200 OK",
            "text/css; charset=utf-8",
            super::COCKPIT_FONTS_CSS.to_string(),
        );
    }
    if path == "/static/vendor/chart.umd.min.js" {
        return (
            "200 OK",
            "application/javascript; charset=utf-8",
            super::COCKPIT_VENDOR_CHART_JS.to_string(),
        );
    }
    if path == "/static/vendor/d3.min.js" {
        return (
            "200 OK",
            "application/javascript; charset=utf-8",
            super::COCKPIT_VENDOR_D3_JS.to_string(),
        );
    }
    if path == "/favicon.svg" {
        return (
            "200 OK",
            "image/svg+xml; charset=utf-8",
            super::COCKPIT_FAVICON_SVG.to_string(),
        );
    }
    if path == "/favicon.ico" {
        return ("204 No Content", "text/plain", String::new());
    }

    stats::handle(path, query_str, method, body)
        .or_else(|| signals::handle(path, query_str, method, body))
        .or_else(|| context::handle(path, query_str, method, body))
        .or_else(|| risk::handle(path, query_str, method, body))
        .or_else(|| roi::handle(path, query_str, method, body))
        .or_else(|| knowledge::handle(path, query_str, method, body))
        .or_else(|| learning::handle(path, query_str, method, body))
        .or_else(|| memory::handle(path, query_str, method, body))
        .or_else(|| graph::handle(path, query_str, method, body))
        .or_else(|| agents::handle(path, query_str, method, body))
        .or_else(|| tools::handle(path, query_str, method, body))
        .or_else(|| settings::handle(path, query_str, method, body))
        .or_else(|| doctor::handle(path, query_str, method, body))
        .or_else(|| leaderboard::handle(path, query_str, method, body))
        .or_else(|| system::handle(path, query_str, method, body))
        .unwrap_or_else(|| ("404 Not Found", "text/plain", "Not Found".to_string()))
}
