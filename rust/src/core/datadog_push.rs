//! Agentless Datadog export (GL #401, setup path B).
//!
//! Pushes the metrics-contract series straight to the Datadog Metrics API v2
//! (`POST /api/v2/series`, `DD-API-KEY` header) — no local Agent, no `OTel`
//! Collector. Strictly opt-in: **both** `LEAN_CTX_DATADOG_PUSH=1` and
//! `DD_API_KEY` must be set; a stray `DD_API_KEY` from another tool never
//! turns on egress by itself.
//!
//! Counter semantics: Datadog v2 `count` points are per-interval deltas, not
//! cumulative totals. The pusher keeps the last pushed totals in-process and
//! submits deltas; the first cycle only records the baseline (submitting a
//! lifetime total as one interval would spike every graph). Gauges go out on
//! every cycle, including the first.
//!
//! Tag policy is identical to the `lean_ctx_info` series: five bounded tags
//! (`project`, `profile`, `agent_role`, `model`, `version`) attached to every
//! series — bounded values, so Datadog custom-metric cardinality stays flat.

use std::sync::Mutex;
use std::time::Duration;

const ENABLE_ENV: &str = "LEAN_CTX_DATADOG_PUSH";
const API_KEY_ENV: &str = "DD_API_KEY";
const SITE_ENV: &str = "DD_SITE";
const INTERVAL_ENV: &str = "LEAN_CTX_DATADOG_INTERVAL_SECS";

const DEFAULT_INTERVAL_SECS: u64 = 60;
const MIN_INTERVAL_SECS: u64 = 10;

/// Datadog v2 metric intake types.
const TYPE_COUNT: u8 = 1;
const TYPE_GAUGE: u8 = 3;

/// Cumulative totals as of the previous push — source of the count deltas.
#[derive(Default, Clone, Copy)]
struct Baseline {
    tokens_in: u64,
    tokens_out: u64,
    tokens_saved: u64,
    ledger_tokens: u64,
    ledger_usd: f64,
    tool_calls: u64,
    tool_errors: u64,
}

static BASELINE: Mutex<Option<Baseline>> = Mutex::new(None);

/// True when the operator explicitly enabled the push exporter.
#[must_use]
pub fn enabled() -> bool {
    std::env::var(ENABLE_ENV).is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        && std::env::var(API_KEY_ENV).is_ok_and(|v| !v.trim().is_empty())
}

fn interval() -> Duration {
    let secs = std::env::var(INTERVAL_ENV)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DEFAULT_INTERVAL_SECS)
        .max(MIN_INTERVAL_SECS);
    Duration::from_secs(secs)
}

fn intake_url() -> String {
    let site = std::env::var(SITE_ENV).unwrap_or_else(|_| "datadoghq.com".to_string());
    format!("https://api.{site}/api/v2/series")
}

/// Spawn the background push loop if (and only if) the operator opted in.
/// Called from long-running entry points (dashboard server). Returns whether
/// the loop was started.
#[must_use]
pub fn spawn_if_enabled() -> bool {
    if !enabled() {
        return false;
    }
    std::thread::Builder::new()
        .name("dd-push".into())
        .spawn(|| {
            loop {
                match push_once() {
                    Ok(sent) => {
                        tracing::debug!("datadog push: {sent} series sent");
                    }
                    Err(e) => {
                        tracing::warn!("datadog push failed (will retry): {e}");
                    }
                }
                std::thread::sleep(interval());
            }
        })
        .is_ok()
}

/// Build and submit one batch. Returns the number of series sent.
pub fn push_once() -> Result<usize, String> {
    let api_key = std::env::var(API_KEY_ENV).map_err(|_| "DD_API_KEY not set".to_string())?;
    let series = build_series(now_ts());
    if series.is_empty() {
        return Ok(0); // first cycle: baseline recorded, gauges follow next tick
    }
    let count = series.len();
    let payload = serde_json::json!({ "series": series });
    let body = serde_json::to_vec(&payload).map_err(|e| e.to_string())?;

    let agent = ureq::Agent::new_with_config(
        ureq::config::Config::builder()
            .timeout_global(Some(Duration::from_secs(10)))
            .http_status_as_error(false)
            .build(),
    );
    let resp = agent
        .post(&intake_url())
        .header("Content-Type", "application/json")
        .header("DD-API-KEY", api_key.trim())
        .send(body.as_slice())
        .map_err(|e| format!("datadog intake unreachable: {e}"))?;

    let status = resp.status().as_u16();
    if !(200..300).contains(&status) {
        let body = resp.into_body().read_to_string().unwrap_or_default();
        return Err(format!("datadog intake rejected ({status}): {body}"));
    }
    Ok(count)
}

fn now_ts() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// Extra static tags, e.g. `env:prod,team:platform` — the Datadog-side
/// equivalent of `OTel` resource attributes like `deployment.environment`.
const EXTRA_TAGS_ENV: &str = "LEAN_CTX_DD_TAGS";

fn tags() -> Vec<String> {
    let mut tags: Vec<String> = super::telemetry::info_tags()
        .into_iter()
        .map(|(k, v)| format!("{k}:{v}"))
        .collect();
    if let Ok(extra) = std::env::var(EXTRA_TAGS_ENV) {
        tags.extend(
            extra
                .split(',')
                .map(str::trim)
                .filter(|t| !t.is_empty() && t.contains(':'))
                .map(String::from),
        );
    }
    tags
}

fn series_entry(metric: &str, ty: u8, value: f64, ts: i64, tags: &[String]) -> serde_json::Value {
    serde_json::json!({
        "metric": metric,
        "type": ty,
        "points": [{ "timestamp": ts, "value": value }],
        "tags": tags,
    })
}

/// Assemble the batch: gauges always, counts as deltas vs. the baseline.
/// First call returns an empty batch (baseline only) by design.
fn build_series(ts: i64) -> Vec<serde_json::Value> {
    let snap = super::telemetry::global_metrics().snapshot();
    let (ledger_tokens, ledger_usd) = super::telemetry::ledger_totals_cached();
    let current = Baseline {
        tokens_in: snap.tokens_input,
        tokens_out: snap.tokens_output,
        tokens_saved: snap.tokens_saved,
        ledger_tokens,
        ledger_usd,
        tool_calls: snap.tool_calls_total,
        tool_errors: snap.tool_calls_error,
    };

    let Ok(mut guard) = BASELINE.lock() else {
        return Vec::new();
    };
    let Some(prev) = *guard else {
        *guard = Some(current);
        return Vec::new();
    };
    *guard = Some(current);
    drop(guard);

    let t = tags();
    let d = |cur: u64, old: u64| cur.saturating_sub(old) as f64;
    let mut series = vec![
        series_entry(
            "leanctx.tokens.in",
            TYPE_COUNT,
            d(current.tokens_in, prev.tokens_in),
            ts,
            &t,
        ),
        series_entry(
            "leanctx.tokens.out",
            TYPE_COUNT,
            d(current.tokens_out, prev.tokens_out),
            ts,
            &t,
        ),
        series_entry(
            "leanctx.tokens.saved",
            TYPE_COUNT,
            d(current.tokens_saved, prev.tokens_saved),
            ts,
            &t,
        ),
        series_entry(
            "leanctx.tokens.saved_verified",
            TYPE_COUNT,
            d(current.ledger_tokens, prev.ledger_tokens),
            ts,
            &t,
        ),
        series_entry(
            "leanctx.cost.saved_usd",
            TYPE_COUNT,
            (current.ledger_usd - prev.ledger_usd).max(0.0),
            ts,
            &t,
        ),
        series_entry(
            "leanctx.tools.calls",
            TYPE_COUNT,
            d(current.tool_calls, prev.tool_calls),
            ts,
            &t,
        ),
        series_entry(
            "leanctx.tools.errors",
            TYPE_COUNT,
            d(current.tool_errors, prev.tool_errors),
            ts,
            &t,
        ),
    ];

    let slo = crate::core::slo::evaluate_quiet();
    let verify = crate::core::output_verification::stats_snapshot();
    series.extend([
        series_entry(
            "leanctx.cache.hit_ratio",
            TYPE_GAUGE,
            snap.cache_hit_rate,
            ts,
            &t,
        ),
        series_entry(
            "leanctx.compression.ratio",
            TYPE_GAUGE,
            snap.compression_ratio,
            ts,
            &t,
        ),
        series_entry(
            "leanctx.session.uptime_seconds",
            TYPE_GAUGE,
            snap.session_uptime_secs as f64,
            ts,
            &t,
        ),
        series_entry(
            "leanctx.slo.violations",
            TYPE_GAUGE,
            slo.violations.len() as f64,
            ts,
            &t,
        ),
        series_entry(
            "leanctx.verification.pass_ratio",
            TYPE_GAUGE,
            verify.pass_rate,
            ts,
            &t,
        ),
    ]);
    series
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Single sequential test: `BASELINE` is process-global, so the
    /// baseline → delta → tags assertions must not run as parallel tests.
    #[test]
    fn baseline_then_deltas_then_tags() {
        *BASELINE.lock().unwrap() = None;

        // Cycle 1: baseline only, nothing submitted.
        assert!(build_series(1000).is_empty());

        // Cycle 2: full batch with counts as deltas, not lifetime totals.
        let m = super::super::telemetry::global_metrics();
        m.record_tokens(100, 10, 500);
        let batch = build_series(1060);
        assert!(
            batch.len() >= 12,
            "expected full batch, got {}",
            batch.len()
        );
        let saved = batch
            .iter()
            .find(|s| s["metric"] == "leanctx.tokens.saved")
            .expect("tokens.saved present");
        let v = saved["points"][0]["value"].as_f64().unwrap();
        // Other lib tests may record tokens concurrently (global metrics), so
        // ≥ the 500 just recorded — but never absent or typed as gauge.
        assert!(
            v >= 500.0,
            "delta should include the 500 just recorded: {v}"
        );
        assert_eq!(saved["type"], TYPE_COUNT);

        // Every series carries the five bounded info tags.
        for s in &batch {
            let tags: Vec<String> = s["tags"]
                .as_array()
                .unwrap()
                .iter()
                .map(|t| t.as_str().unwrap().to_string())
                .collect();
            for key in ["project:", "profile:", "agent_role:", "model:", "version:"] {
                assert!(
                    tags.iter().any(|t| t.starts_with(key)),
                    "{} missing tag {key}",
                    s["metric"]
                );
            }
        }
    }

    #[test]
    fn disabled_without_explicit_opt_in() {
        // Neither env set in the test environment → off.
        assert!(!enabled());
    }

    #[test]
    fn site_routing_defaults_to_us() {
        assert_eq!(intake_url(), "https://api.datadoghq.com/api/v2/series");
    }
}
