//! `/api/roi` — local evidence and context-usage observations for the dashboard.
//!
//! This route is local-only. It exposes the local ledger report, daily trend,
//! and observed output shaping data. Hosted plans, entitlements, organization
//! usage, and billing surfaces are intentionally absent from the public Runtime.

use crate::core::savings_ledger::ProjectScope;
use serde_json::json;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Instant;

/// One answer per project a read named (`None`: every project).
static ROI_CACHE: Mutex<Vec<(Option<PathBuf>, Instant, String)>> = Mutex::new(Vec::new());
/// The report re-walks the savings ledger, so polls are answered from this
/// cache; an explicit refresh (`_refresh=1`, the Cockpit's reload) recomputes.
const ROI_TTL_SECS: u64 = 300;
const ROI_CACHE_LIMIT: usize = 8;

pub(super) fn handle(
    path: &str,
    query_str: &str,
    _method: &str,
    _body: &str,
) -> Option<(&'static str, &'static str, String)> {
    match path {
        "/api/roi" => Some(roi_cached(super::helpers::wants_refresh(query_str))),
        _ => None,
    }
}

/// Forget every cached answer (after a write the report reflects).
pub(super) fn invalidate() {
    if let Ok(mut guard) = ROI_CACHE.lock() {
        guard.clear();
    }
}

fn roi_cached(refresh: bool) -> (&'static str, &'static str, String) {
    let scope = super::helpers::request_ledger_scope();
    let key = scope.as_ref().map(|s| s.project().to_path_buf());
    if let Ok(mut guard) = ROI_CACHE.lock() {
        guard.retain(|(k, ts, _)| ts.elapsed().as_secs() < ROI_TTL_SECS && !(refresh && *k == key));
        if let Some((_, _, body)) = guard.iter().find(|(k, _, _)| *k == key) {
            return ("200 OK", "application/json", body.clone());
        }
    }
    let result = roi(scope.as_ref());
    if let Ok(mut guard) = ROI_CACHE.lock() {
        // A concurrent read may have stored this project's answer meanwhile.
        guard.retain(|(k, _, _)| *k != key);
        if guard.len() >= ROI_CACHE_LIMIT {
            guard.remove(0);
        }
        guard.push((key, Instant::now(), result.2.clone()));
    }
    result
}

fn roi(scope: Option<&ProjectScope>) -> (&'static str, &'static str, String) {
    let agent_id = crate::core::agent_identity::current_agent_id();
    let report = crate::core::savings_ledger::roi_report_for(agent_id, scope);
    let summary = crate::core::savings_ledger::summary_for(scope);
    let holdout_fraction = crate::core::config::Config::load()
        .proxy
        .output_holdout_fraction();
    let output = output_json(&crate::proxy::output_savings::current(), holdout_fraction);

    let payload = json!({
        "roi": report,
        "trend": summary.by_day,
        "output": output,
    });
    let body = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string());
    ("200 OK", "application/json", body)
}

/// The output-savings object plus whether the holdout is running right now
/// (`proxy.output_holdout` > 0), so the dashboard can tell "not measured —
/// holdout off" apart from "holdout on, collecting turns" (#895).
fn output_json(
    savings: &crate::proxy::output_savings::Savings,
    holdout_fraction: f64,
) -> serde_json::Value {
    let mut output = crate::proxy::output_savings::to_json(savings);
    if let Some(obj) = output.as_object_mut() {
        obj.insert("holdout_active".into(), json!(holdout_fraction > 0.0));
        obj.insert("holdout_fraction".into(), json!(holdout_fraction));
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proxy::output_savings::Savings;

    #[test]
    fn an_explicit_refresh_recomputes_the_cached_report() {
        let _data = crate::core::data_dir::isolated_data_dir();
        let stale = r#"{"stale":true}"#.to_string();
        let seed = || {
            invalidate();
            ROI_CACHE
                .lock()
                .unwrap()
                .push((None, Instant::now(), stale.clone()));
        };
        seed();
        let (_, _, polled) = handle("/api/roi", "", "GET", "").unwrap();
        assert_eq!(polled, stale, "a poll is answered from the cache");

        let (status, _, fresh) = handle("/api/roi", "_refresh=1", "GET", "").unwrap();
        assert_eq!(status, "200 OK");
        assert_ne!(fresh, stale, "a refresh recomputes");
        let fresh: serde_json::Value = serde_json::from_str(&fresh).unwrap();
        assert!(fresh.get("roi").is_some() && fresh.get("output").is_some());

        seed();
        invalidate();
        let (_, _, after) = handle("/api/roi", "", "GET", "").unwrap();
        assert_ne!(after, stale, "an invalidated cache is recomputed");
    }

    #[test]
    fn output_reports_whether_the_holdout_is_active() {
        let pending = Savings::Pending {
            control_n: 3,
            treatment_n: 4,
            needed: 30,
        };
        let on = output_json(&pending, 0.1);
        assert_eq!(on["status"], "pending");
        assert_eq!(on["holdout_active"], true);
        assert_eq!(on["holdout_fraction"], 0.1);

        let estimated = Savings::Estimated {
            point_pct: 33.0,
            low_pct: 21.0,
            high_pct: 45.0,
        };
        let off = output_json(&estimated, 0.0);
        assert_eq!(off["status"], "estimated");
        assert_eq!(off["holdout_active"], false);
    }
}
