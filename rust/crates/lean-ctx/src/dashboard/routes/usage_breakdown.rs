// SPDX-License-Identifier: Apache-2.0
//! Cockpit usage breakdown (enterprise#20) — `GET /api/usage-breakdown`.
//!
//! The "monitoring × saving" view. Two honest sources, same response shape
//! (Doc 08 §3.3/§3.4):
//!
//! - **central**: `[gateway_server].admin_url` is configured → fetch the
//!   org-wide `GET /api/admin/usage` (person × project × model × provider from
//!   Postgres `usage_events`) and pass it through, tagged `"source":"central"`.
//!   The bearer token comes from `LEAN_CTX_GATEWAY_ADMIN_TOKEN` — never config.
//! - **local-snapshot**: no admin URL → this machine's measured spend
//!   (`proxy_usage.json`, real models + billed tokens) as model-level rows and
//!   the savings-ledger's last-30-days USD as the savings total. A local
//!   snapshot has no person/project dimension and no per-model savings split —
//!   those fields are empty/zero rather than invented.
//!
//! The seat projection uses the same formula as the admin API: per-active-
//! person savings × configured seats, normalized to a 30-day month. Locally
//! `active_persons = 1` (this machine). No `[gateway_server].seats` → no
//! projection.

use lean_ctx::core::config::Config;

pub(super) fn handle(
    path: &str,
    _query_str: &str,
    method: &str,
    _body: &str,
) -> Option<(&'static str, &'static str, String)> {
    if path != "/api/usage-breakdown" {
        return None;
    }
    if !method.eq_ignore_ascii_case("GET") {
        return Some((
            "405 Method Not Allowed",
            "application/json",
            super::helpers::json_err("use GET to read the usage breakdown"),
        ));
    }

    let cfg = Config::load();
    let payload = match cfg.gateway_server.admin_url.as_deref() {
        Some(url) => fetch_central(url, &cfg),
        None => Ok(local_snapshot(&cfg)),
    };
    Some(match payload {
        Ok(json) => ("200 OK", "application/json", json.to_string()),
        Err(e) => (
            "502 Bad Gateway",
            "application/json",
            super::helpers::json_err(&e),
        ),
    })
}

/// Pass-through of the central admin API (same-origin proxy: the cockpit CSP
/// pins `connect-src 'self'`, so the browser cannot call the gateway directly).
fn fetch_central(admin_url: &str, cfg: &Config) -> Result<serde_json::Value, String> {
    let url = format!("{}/api/admin/usage", admin_url.trim_end_matches('/'));
    let mut req = ureq::get(&url)
        .config()
        .timeout_global(Some(std::time::Duration::from_secs(10)))
        .build();
    if let Ok(token) = std::env::var("LEAN_CTX_GATEWAY_ADMIN_TOKEN")
        && !token.trim().is_empty()
    {
        req = req.header("authorization", &format!("Bearer {}", token.trim()));
    }
    let body = req
        .call()
        .map_err(|e| format!("central usage API unreachable ({url}): {e}"))?
        .into_body()
        .read_to_string()
        .map_err(|e| format!("central usage API read failed: {e}"))?;
    let mut json: serde_json::Value = serde_json::from_str(&body)
        .map_err(|e| format!("central usage API returned invalid JSON: {e}"))?;
    annotate(&mut json, "central", cfg);
    Ok(json)
}

/// Model-level spend from this machine's measured proxy usage + ledger savings.
fn local_snapshot(cfg: &Config) -> serde_json::Value {
    let spend = lean_ctx_proxy::usage_meter::persisted_snapshot();
    let rows: Vec<serde_json::Value> = spend
        .iter()
        .map(|m| {
            serde_json::json!({
                // No identity dimension in a local snapshot — empty, not invented.
                "person": "",
                "project": "",
                "model": m.model,
                "provider": "",
                "requests": m.requests,
                "input_tokens": m.input_tokens,
                "output_tokens": m.output_tokens,
                "cost_usd": m.cost_usd,
                "saved_tokens": 0,
                "saved_usd": 0.0,
            })
        })
        .collect();

    let ledger = lean_ctx::core::savings_ledger::summary();
    let now = chrono::Utc::now();
    let cutoff = (now - chrono::Duration::days(30))
        .format("%Y-%m-%d")
        .to_string();
    // Ledger savings over the last 30 days: the projection window matches the
    // admin API's default so central and local numbers are comparable.
    let saved_usd_30d: f64 = ledger
        .by_day
        .iter()
        .filter(|(day, _, _, _)| day.as_str() >= cutoff.as_str())
        .map(|(_, _, usd, _)| usd)
        .sum();

    let cost_usd: f64 = spend.iter().map(|m| m.cost_usd).sum();
    let requests: u64 = spend.iter().map(|m| m.requests).sum();
    let projection = cfg
        .gateway_server
        .seats
        .filter(|_| saved_usd_30d > 0.0)
        .map(|seats| saved_usd_30d * f64::from(seats));

    let mut json = serde_json::json!({
        "from": (now - chrono::Duration::days(30)).to_rfc3339(),
        "to": now.to_rfc3339(),
        "rows": rows,
        "totals": {
            "requests": requests,
            "cost_usd": cost_usd,
            "saved_usd": saved_usd_30d,
            "reference_cost_usd": 0.0,
            "active_persons": 1,
        },
    });
    if let Some(p) = projection
        && let Some(totals) = json["totals"].as_object_mut()
    {
        totals.insert(
            "projection_seats".into(),
            serde_json::json!(cfg.gateway_server.seats),
        );
        totals.insert("projection_usd_per_month".into(), serde_json::json!(p));
    }
    annotate(&mut json, "local-snapshot", cfg);
    json
}

/// Stamps the data source + org label onto the response (cockpit header).
fn annotate(json: &mut serde_json::Value, source: &str, cfg: &Config) {
    if let Some(obj) = json.as_object_mut() {
        obj.insert("source".into(), serde_json::json!(source));
        if let Some(label) = cfg.gateway_server.org_label.as_deref() {
            obj.insert("org_label".into(), serde_json::json!(label));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_matching_paths_pass_through() {
        assert!(handle("/api/other", "", "GET", "").is_none());
    }

    #[test]
    fn post_is_rejected() {
        let (status, _, _) = handle("/api/usage-breakdown", "", "POST", "").expect("handled");
        assert_eq!(status, "405 Method Not Allowed");
    }

    #[test]
    fn local_snapshot_shape_is_stable() {
        // Contract fields the cockpit view binds to (Doc 08 §3.3/§3.4) — the
        // local source must emit the exact same shape as the central API.
        let cfg = Config::default();
        let json = local_snapshot(&cfg);
        assert_eq!(json["source"], "local-snapshot");
        assert_eq!(json["totals"]["active_persons"], 1);
        assert!(json["rows"].is_array());
        assert!(json["totals"]["cost_usd"].is_number());
        assert!(json["totals"]["saved_usd"].is_number());
        // No seats configured → no invented projection.
        assert!(json["totals"].get("projection_usd_per_month").is_none());
    }

    #[test]
    fn org_label_is_annotated_when_configured() {
        let cfg = Config {
            gateway_server: lean_ctx::core::config::GatewayServerConfig {
                org_label: Some("Acme AI Gateway".into()),
                ..Default::default()
            },
            ..Default::default()
        };
        let json = local_snapshot(&cfg);
        assert_eq!(json["org_label"], "Acme AI Gateway");
    }
}
