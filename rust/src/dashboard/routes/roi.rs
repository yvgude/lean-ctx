//! `/api/roi` — the user-facing **ROI & Plan** monitoring surface.
//!
//! Aggregates read-only, privacy-preserving data for the dashboard's "ROI" view:
//! - the signed savings ROI report (tokens/$ saved, top models/tools, provenance),
//! - the daily savings trend (for the chart),
//! - the effective commercial plan + entitlements (cache-only — applies offline
//!   grace, never hits the network on a dashboard request),
//! - the metered-usage billable flag.
//!
//! It is **individual + local only**: it shows *your own* signed savings and a
//! read-only plan status. The team roll-up is a separate surface (the web account
//! `/account/team` for managed teams, or `lean-ctx savings team` / the team
//! server for self-hosted ones) — never mixed into this personal cockpit.
//!
//! It never gates anything and never mutates state. The plan shown here is only
//! for display/hosted-surface hints; the local engine has no entitlement checks
//! (Local-Free Invariant).

use serde_json::json;

pub(super) fn handle(
    path: &str,
    _query_str: &str,
    _method: &str,
    _body: &str,
) -> Option<(&'static str, &'static str, String)> {
    match path {
        "/api/roi" => Some(roi()),
        _ => None,
    }
}

fn roi() -> (&'static str, &'static str, String) {
    let agent_id = crate::core::agent_identity::current_agent_id();
    let report = crate::core::savings_ledger::roi_report(agent_id);
    let summary = crate::core::savings_ledger::summary();
    let usage = crate::core::billing::metered_usage(agent_id);

    let eff = crate::cloud_client::resolve_effective_plan_cached();
    let entitlements = eff.plan.entitlements();
    let logged_in = crate::cloud_client::is_logged_in();

    // #895 Track B: measured (A/B holdout) or estimated output-token reduction.
    // Same JSON shape the `lean-ctx output-savings --json` CLI emits.
    let output = crate::proxy::output_savings::to_json(&crate::proxy::output_savings::current());

    let payload = json!({
        "roi": report,
        // [[YYYY-MM-DD, saved_tokens, saved_usd], ...] ascending — drives the trend chart.
        "trend": summary.by_day,
        "output": output,
        "plan": {
            "plan": eff.plan.as_str(),
            "source": plan_source_label(eff.source),
            "verified_at": eff.verified_at,
            "grace_days": eff.grace_days,
            "logged_in": logged_in,
            "entitlements": entitlements,
        },
        "usage": {
            "billable": usage.is_billable(),
            "metered_events": usage.metered_events,
            "net_saved_tokens": usage.net_saved_tokens,
            "saved_usd": usage.saved_usd,
        }
    });
    let body = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string());
    ("200 OK", "application/json", body)
}

/// Stable wire label for the effective-plan provenance.
fn plan_source_label(source: crate::cloud_client::PlanSource) -> &'static str {
    use crate::cloud_client::PlanSource;
    match source {
        PlanSource::Live => "live",
        PlanSource::Cached => "cached",
        PlanSource::Expired => "expired",
        PlanSource::None => "none",
    }
}
