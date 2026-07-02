//! `lean-ctx gateway report` (enterprise#50) — the printable CTO/value report.
//!
//! Renders a **self-contained HTML file** (no external assets, print-to-PDF
//! ready) straight from `usage_events`: executive summary, spend/savings
//! trend (inline SVG), top people/projects/models, routing adoption and the
//! avoided-cost methodology block. Every number is a real aggregate from the
//! store — the report never invents or extrapolates beyond the labeled seat
//! projection (same math as the admin API).

use std::fmt::Write as _;

use super::admin_api::{UsageBreakdownResponse, usage_breakdown};
use super::admin_timeseries::{TimeseriesResponse, timeseries};

/// Inputs assembled by the CLI layer.
#[derive(Debug, Clone)]
pub struct ReportMeta {
    pub org_label: Option<String>,
    pub seats: Option<u32>,
    pub reference_model: Option<String>,
}

/// Queries the store and renders the report HTML.
///
/// # Errors
/// Propagates store/query errors (report generation requires the database —
/// there is no fail-open here, a report with missing data would be dishonest).
pub async fn generate(
    pool: &deadpool_postgres::Pool,
    from: chrono::DateTime<chrono::Utc>,
    to: chrono::DateTime<chrono::Utc>,
    meta: &ReportMeta,
) -> anyhow::Result<String> {
    let usage = usage_breakdown(pool, from, to, meta.seats).await?;
    let series = timeseries(pool, from, to).await?;
    let routed = routed_requests(pool, from, to).await?;
    Ok(render(&usage, &series, routed, meta))
}

/// Requests that were actively re-routed (routing adoption evidence).
async fn routed_requests(
    pool: &deadpool_postgres::Pool,
    from: chrono::DateTime<chrono::Utc>,
    to: chrono::DateTime<chrono::Utc>,
) -> anyhow::Result<i64> {
    let client = pool.get().await?;
    let row = client
        .query_one(
            "SELECT count(*) AS n FROM usage_events \
             WHERE ts >= $1 AND ts <= $2 AND routed_from IS NOT NULL",
            &[&from, &to],
        )
        .await?;
    Ok(row.get("n"))
}

fn usd(v: f64) -> String {
    if v.abs() >= 1_000_000.0 {
        format!("${:.2}M", v / 1_000_000.0)
    } else if v.abs() >= 10_000.0 {
        format!("${:.1}k", v / 1_000.0)
    } else if v.abs() >= 100.0 {
        format!("${v:.0}")
    } else {
        format!("${v:.2}")
    }
}

fn n(v: i64) -> String {
    if v >= 1_000_000 {
        format!("{:.1}M", v as f64 / 1e6)
    } else if v >= 10_000 {
        format!("{:.1}k", v as f64 / 1e3)
    } else {
        v.to_string()
    }
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Pure renderer (unit-tested without a database).
fn render(
    usage: &UsageBreakdownResponse,
    series: &TimeseriesResponse,
    routed_requests: i64,
    meta: &ReportMeta,
) -> String {
    let t = &usage.totals;
    let org = meta.org_label.as_deref().unwrap_or("Organization");
    let window = format!("{} → {}", &usage.from[..10], &usage.to[..10]);
    let avoided = (t.reference_cost_usd - t.cost_usd).max(0.0);

    let top_by = |key: fn(&super::admin_api::UsageBreakdownRow) -> &str| {
        let mut agg = std::collections::BTreeMap::<String, (i64, f64, f64)>::new();
        for r in &usage.rows {
            let e = agg.entry(key(r).to_string()).or_default();
            e.0 += r.requests;
            e.1 += r.cost_usd;
            e.2 += r.saved_usd;
        }
        let mut v: Vec<_> = agg.into_iter().collect();
        v.sort_by(|a, b| b.1.1.total_cmp(&a.1.1));
        v.truncate(10);
        v
    };
    let people = top_by(|r| &r.person);
    let projects = top_by(|r| &r.project);
    let models = top_by(|r| &r.model);

    let table = |title: &str, rows: &[(String, (i64, f64, f64))]| -> String {
        let mut out = format!(
            "<h3>{title}</h3><table><thead><tr><th>{title}</th>\
             <th class=num>Requests</th><th class=num>Saved</th><th class=num>Cost</th></tr></thead><tbody>"
        );
        for (name, (req, cost, saved)) in rows {
            let _ = write!(
                out,
                "<tr><td>{}</td><td class=num>{}</td><td class='num saved'>{}</td><td class=num>{}</td></tr>",
                esc(name),
                n(*req),
                usd(*saved),
                usd(*cost)
            );
        }
        out.push_str("</tbody></table>");
        out
    };

    let projection_block = match (t.projection_seats, t.projection_usd_per_month) {
        (Some(seats), Some(p)) => format!(
            "<div class=kpi><div class=kpi-label>Projected org savings</div>\
             <div class=kpi-value>{}/mo</div><div class=kpi-foot>at {seats} seats — extrapolation, not billing</div></div>",
            usd(p)
        ),
        _ => String::new(),
    };

    let methodology = meta.reference_model.as_deref().map_or_else(
        || {
            "<p>No counterfactual reference model is configured; the avoided-cost \
             column reports 0. Configure <code>[proxy.baseline] reference_model</code> \
             to enable avoided-cost accounting.</p>"
                .to_string()
        },
        |m| {
            format!(
                "<p>The <b>baseline</b> prices every request's <i>uncompressed</i> input \
                 at the contract-frozen reference model <code>{}</code>. <b>Avoided cost</b> \
                 is the difference between that counterfactual and the actual spend. Local \
                 inference is booked at a transparent shadow rate — savings are never \
                 measured against a free-of-charge fiction.</p>",
                esc(m)
            )
        },
    );

    format!(
        r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8">
<title>{org_esc} · lean-ctx value report</title>
<style>
:root{{--green:#059669;--ink:#111114;--muted:#6b7280;--line:#e5e7eb;--soft:#f5f6f8}}
*{{margin:0;padding:0;box-sizing:border-box}}
body{{font:14px/1.55 -apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,sans-serif;color:var(--ink);padding:48px;max-width:960px;margin:0 auto}}
header{{display:flex;justify-content:space-between;align-items:baseline;border-bottom:2px solid var(--ink);padding-bottom:14px;margin-bottom:28px}}
h1{{font-size:22px;letter-spacing:-0.02em}}
h2{{font-size:15px;margin:32px 0 12px;letter-spacing:-0.01em}}
h3{{font-size:13px;margin:22px 0 8px;color:var(--muted);text-transform:uppercase;letter-spacing:0.06em}}
.sub{{color:var(--muted);font-size:12px}}
.kpis{{display:grid;grid-template-columns:repeat(auto-fit,minmax(170px,1fr));gap:12px;margin:20px 0}}
.kpi{{border:1px solid var(--line);border-radius:8px;padding:14px 16px;background:var(--soft)}}
.kpi-label{{font-size:10px;text-transform:uppercase;letter-spacing:0.08em;color:var(--muted)}}
.kpi-value{{font-size:24px;font-weight:650;font-variant-numeric:tabular-nums;margin-top:4px}}
.kpi-value.green{{color:var(--green)}}
.kpi-foot{{font-size:11px;color:var(--muted);margin-top:2px}}
table{{width:100%;border-collapse:collapse;font-size:12.5px;margin-bottom:8px}}
th{{text-align:left;font-size:10px;text-transform:uppercase;letter-spacing:0.06em;color:var(--muted);padding:6px 8px;border-bottom:1px solid var(--ink)}}
td{{padding:6px 8px;border-bottom:1px solid var(--line);font-variant-numeric:tabular-nums}}
.num{{text-align:right}}
.saved{{color:var(--green)}}
.chart{{margin:12px 0;border:1px solid var(--line);border-radius:8px;padding:12px;background:#fff}}
.legend{{font-size:11px;color:var(--muted);margin-top:6px}}
.legend b{{font-weight:600}}
footer{{margin-top:40px;padding-top:12px;border-top:1px solid var(--line);font-size:11px;color:var(--muted);display:flex;justify-content:space-between}}
@media print{{body{{padding:24px}}.kpi{{break-inside:avoid}}table{{break-inside:auto}}}}
</style></head><body>
<header><div><h1>{org_esc} — AI gateway value report</h1>
<div class=sub>window {window} · generated by lean-ctx gateway</div></div>
<div class=sub>lean-ctx v{version}</div></header>

<section class=kpis>
<div class=kpi><div class=kpi-label>Actual spend</div><div class=kpi-value>{spend}</div><div class=kpi-foot>{requests} requests</div></div>
<div class=kpi><div class=kpi-label>Verified savings</div><div class="kpi-value green">{saved}</div><div class=kpi-foot>measured per event</div></div>
<div class=kpi><div class=kpi-label>Baseline (counterfactual)</div><div class=kpi-value>{reference}</div><div class=kpi-foot>uncompressed @ reference model</div></div>
<div class=kpi><div class=kpi-label>Avoided cost</div><div class="kpi-value green">{avoided}</div><div class=kpi-foot>baseline − actual</div></div>
{projection_block}
</section>

<h2>Spend &amp; savings per day</h2>
<div class=chart>{svg}
<div class=legend><b>▬</b> spend &nbsp; <b style="color:var(--green)">▬</b> saved &nbsp; <span style="color:#7c3aed"><b>╌</b> baseline</span></div></div>

<h2>Adoption</h2>
<table><tbody>
<tr><td>Active people in window</td><td class=num>{persons}</td></tr>
<tr><td>Requests actively re-routed to cheaper models</td><td class=num>{routed}</td></tr>
</tbody></table>

<h2>Breakdown</h2>
{people_table}
{projects_table}
{models_table}

<h2>Methodology</h2>
{methodology}

<footer><span>lean-ctx — SEE · ROUTE · REMEMBER · PROVE</span><span>numbers sourced from usage_events; projection labeled as extrapolation</span></footer>
</body></html>
"#,
        org_esc = esc(org),
        window = window,
        version = env!("CARGO_PKG_VERSION"),
        spend = usd(t.cost_usd),
        requests = n(t.requests),
        saved = usd(t.saved_usd),
        reference = usd(t.reference_cost_usd),
        avoided = usd(avoided),
        projection_block = projection_block,
        svg = trend_svg(series),
        persons = n(t.active_persons),
        routed = n(routed_requests),
        people_table = table("Top people", &people),
        projects_table = table("Top projects", &projects),
        models_table = table("Top models", &models),
        methodology = methodology,
    )
}

/// Inline SVG: daily spend bars + saved line + dashed baseline line.
/// Pure geometry — no JS, prints crisply.
fn trend_svg(series: &TimeseriesResponse) -> String {
    const W: f64 = 860.0;
    const H: f64 = 180.0;
    const PAD: f64 = 8.0;
    let points = &series.points;
    if points.is_empty() {
        return "<p class=sub>No events in this window.</p>".into();
    }
    let max = points
        .iter()
        .map(|p| p.cost_usd.max(p.saved_usd).max(p.reference_cost_usd))
        .fold(0.0_f64, f64::max)
        .max(1e-9);
    let count = points.len() as f64;
    let step = (W - 2.0 * PAD) / count;
    let bar_w = (step * 0.55).clamp(1.0, 26.0);
    let y = |v: f64| H - PAD - (v / max) * (H - 2.0 * PAD);
    let x = |i: usize| PAD + step * (i as f64) + step / 2.0;

    let mut svg = format!(
        r#"<svg viewBox="0 0 {W} {H}" width="100%" height="{H}" role="img" aria-label="daily spend and savings">"#
    );
    // grid lines at 0/50/100%
    for frac in [0.0_f64, 0.5, 1.0] {
        let gy = y(max * frac);
        let _ = write!(
            svg,
            r##"<line x1="{PAD}" y1="{gy:.1}" x2="{:.1}" y2="{gy:.1}" stroke="#e5e7eb" stroke-width="1"/>"##,
            W - PAD
        );
    }
    for (i, p) in points.iter().enumerate() {
        let _ = write!(
            svg,
            r##"<rect x="{:.1}" y="{:.1}" width="{bar_w:.1}" height="{:.1}" fill="#94a3b8" rx="1.5"><title>{}: spend {}</title></rect>"##,
            x(i) - bar_w / 2.0,
            y(p.cost_usd),
            (H - PAD - y(p.cost_usd)).max(0.0),
            p.day,
            usd(p.cost_usd),
        );
    }
    let polyline = |vals: Vec<f64>, color: &str, dash: &str| -> String {
        let pts: Vec<String> = vals
            .iter()
            .enumerate()
            .map(|(i, v)| format!("{:.1},{:.1}", x(i), y(*v)))
            .collect();
        format!(
            r#"<polyline points="{}" fill="none" stroke="{color}" stroke-width="2"{dash}/>"#,
            pts.join(" ")
        )
    };
    svg.push_str(&polyline(
        points.iter().map(|p| p.saved_usd).collect(),
        "#059669",
        "",
    ));
    if points.iter().any(|p| p.reference_cost_usd > 0.0) {
        svg.push_str(&polyline(
            points.iter().map(|p| p.reference_cost_usd).collect(),
            "#7c3aed",
            r#" stroke-dasharray="5 4""#,
        ));
    }
    svg.push_str("</svg>");
    svg
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway_server::admin_api::{UsageBreakdownRow, UsageTotals};
    use crate::gateway_server::admin_timeseries::TimeseriesPoint;

    fn fixture() -> (UsageBreakdownResponse, TimeseriesResponse) {
        let usage = UsageBreakdownResponse {
            from: "2026-06-01T00:00:00+00:00".into(),
            to: "2026-07-01T00:00:00+00:00".into(),
            rows: vec![
                UsageBreakdownRow {
                    person: "alice@zuehlke.com".into(),
                    project: "checkout".into(),
                    model: "claude-sonnet-4-5".into(),
                    provider: "anthropic".into(),
                    requests: 900,
                    input_tokens: 8_000_000,
                    output_tokens: 400_000,
                    cost_usd: 210.0,
                    saved_tokens: 2_500_000,
                    saved_usd: 65.0,
                },
                UsageBreakdownRow {
                    person: "bob@zuehlke.com".into(),
                    project: "platform".into(),
                    model: "phi-4".into(),
                    provider: "foundry".into(),
                    requests: 300,
                    input_tokens: 1_000_000,
                    output_tokens: 90_000,
                    cost_usd: 12.0,
                    saved_tokens: 400_000,
                    saved_usd: 4.0,
                },
            ],
            totals: UsageTotals {
                requests: 1200,
                cost_usd: 222.0,
                saved_usd: 69.0,
                reference_cost_usd: 410.0,
                active_persons: 2,
                projection_seats: Some(800),
                projection_usd_per_month: Some(27_600.0),
            },
        };
        let series = TimeseriesResponse {
            from: usage.from.clone(),
            to: usage.to.clone(),
            points: vec![
                TimeseriesPoint {
                    day: "2026-06-01".into(),
                    requests: 600,
                    cost_usd: 111.0,
                    saved_usd: 30.0,
                    reference_cost_usd: 205.0,
                },
                TimeseriesPoint {
                    day: "2026-06-02".into(),
                    requests: 600,
                    cost_usd: 111.0,
                    saved_usd: 39.0,
                    reference_cost_usd: 205.0,
                },
            ],
        };
        (usage, series)
    }

    #[test]
    fn report_contains_real_numbers_and_no_external_assets() {
        let (usage, series) = fixture();
        let html = render(
            &usage,
            &series,
            42,
            &ReportMeta {
                org_label: Some("Zühlke Engineering AG".into()),
                seats: Some(800),
                reference_model: Some("claude-opus-4.5".into()),
            },
        );
        // Executive numbers present.
        assert!(html.contains("$222"));
        assert!(html.contains("$69.00"));
        assert!(html.contains("$410"));
        assert!(html.contains("$188")); // avoided = 410 - 222
        assert!(html.contains("$27.6k/mo"));
        assert!(html.contains("alice@zuehlke.com"));
        assert!(html.contains("claude-opus-4.5"));
        assert!(html.contains("42")); // routed requests
        // Self-contained: no http(s) loads, no scripts.
        assert!(!html.contains("src=\"http"));
        assert!(!html.contains("<script"));
        assert!(html.contains("<svg"));
        // XSS hygiene on org label.
        let hostile = render(
            &usage,
            &series,
            0,
            &ReportMeta {
                org_label: Some("<script>alert(1)</script>".into()),
                seats: None,
                reference_model: None,
            },
        );
        assert!(!hostile.contains("<script>alert"));
    }

    #[test]
    fn empty_window_renders_gracefully() {
        let usage = UsageBreakdownResponse {
            from: "2026-06-01T00:00:00+00:00".into(),
            to: "2026-06-02T00:00:00+00:00".into(),
            rows: vec![],
            totals: UsageTotals {
                requests: 0,
                cost_usd: 0.0,
                saved_usd: 0.0,
                reference_cost_usd: 0.0,
                active_persons: 0,
                projection_seats: None,
                projection_usd_per_month: None,
            },
        };
        let series = TimeseriesResponse {
            from: usage.from.clone(),
            to: usage.to.clone(),
            points: vec![],
        };
        let html = render(
            &usage,
            &series,
            0,
            &ReportMeta {
                org_label: None,
                seats: None,
                reference_model: None,
            },
        );
        assert!(html.contains("No events in this window"));
        assert!(html.contains("reference_model"));
    }
}
