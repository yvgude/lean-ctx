//! Email digests (GL #386): monthly for Pro, weekly for Team.
//!
//! A background job ticks hourly and sends each eligible account at most one
//! digest per period (calendar month for Pro, ISO week for Team), rendered
//! from the same aggregations the dashboards use — synced CEP snapshots for
//! Pro, the hosted team server's savings summary for Team. Real reported
//! numbers only: accounts with no activity in the period get no email.
//!
//! Delivery rules:
//! - Idempotent via `digest_log` (`INSERT … ON CONFLICT DO NOTHING` is the
//!   send gate); a failed SMTP send releases the claim so the next tick
//!   retries.
//! - Periods are *previous* month/week, so a digest is caught up after
//!   downtime instead of skipped.
//! - Every digest carries a one-click opt-out link (no login). The token is
//!   stored hashed and rotated on every send — the newest email's link always
//!   works, older links go stale.

use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse};
use chrono::{Datelike, NaiveDate, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::auth::{AppState, auth_user};
use super::billing_edge;
use crate::core::billing::plans::Plan;

/// Hourly tick — cheap (one users scan + per-candidate work only when a
/// period is due) and frequent enough that digests land within an hour of
/// the period boundary.
const TICK: std::time::Duration = std::time::Duration::from_hours(1);

/// Spawn the digest job. Call once from `run()`; a missing mailer disables
/// the job entirely (no claims are written, so enabling SMTP later starts
/// cleanly from the next unsent period).
pub(super) fn spawn_digest_job(state: AppState) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            if state.mailer.is_some()
                && let Err(e) = tick(&state).await
            {
                tracing::warn!("digest tick failed: {e}");
            }
            tokio::time::sleep(TICK).await;
        }
    })
}

async fn tick(state: &AppState) -> anyhow::Result<()> {
    let client = state.pool.get().await?;
    let users = client
        .query(
            "SELECT id, email FROM users WHERE email_verified_at IS NOT NULL",
            &[],
        )
        .await?;
    drop(client);

    let today = Utc::now().date_naive();
    for row in users {
        let user_id: Uuid = row.get(0);
        let email: String = row.get(1);
        if let Err(e) = process_user(state, user_id, &email, today).await {
            tracing::warn!(user = %user_id, "digest send failed: {e}");
        }
    }
    Ok(())
}

/// Evaluate one account: figure out the due (kind, period), check opt-out and
/// the idempotency ledger, resolve the plan, render, claim, send.
async fn process_user(
    state: &AppState,
    user_id: Uuid,
    email: &str,
    today: NaiveDate,
) -> anyhow::Result<()> {
    let client = state.pool.get().await?;

    // Opt-out first — cheapest gate.
    let opted_out: bool = client
        .query_opt(
            "SELECT digest_opt_out FROM email_prefs WHERE user_id = $1",
            &[&user_id],
        )
        .await?
        .is_some_and(|r| r.get(0));
    if opted_out {
        return Ok(());
    }

    // Probe both cadences against the ledger before paying for the plan
    // lookup (one HTTP call to the billing plane per candidate).
    let month_key = previous_month_key(today);
    let week_key = previous_iso_week_key(today);
    let pro_sent = digest_already_sent(&client, user_id, "pro-monthly", &month_key).await?;
    let team_sent = digest_already_sent(&client, user_id, "team-weekly", &week_key).await?;
    if pro_sent && team_sent {
        return Ok(());
    }
    drop(client);

    let plan = billing_edge::resolve_plan(&state.cfg, user_id).await;
    let (kind, period_key) = match plan {
        Plan::Pro if !pro_sent => ("pro-monthly", month_key),
        Plan::Team | Plan::Enterprise if !team_sent => ("team-weekly", week_key),
        _ => return Ok(()),
    };

    let body = match kind {
        "pro-monthly" => render_pro_digest(state, user_id, &period_key).await?,
        _ => render_team_digest(state, user_id, &period_key).await?,
    };

    let client = state.pool.get().await?;
    let Some(content) = body else {
        // Nothing to report this period — claim it silently so we don't
        // re-evaluate (and never send empty digests).
        claim_digest(&client, user_id, kind, &period_key).await?;
        return Ok(());
    };

    if !claim_digest(&client, user_id, kind, &period_key).await? {
        return Ok(()); // raced by a concurrent tick
    }

    let opt_out_link = rotate_opt_out_link(state, &client, user_id).await?;
    drop(client);

    let subject = content.subject.clone();
    let text = format!(
        "{}\n\n—\n{}\nUnsubscribe (one click): {opt_out_link}\n",
        content.body, content.footer
    );

    let mailer = state
        .mailer
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("mailer disappeared"))?;
    if let Err(e) = mailer.send_digest(email, &subject, &text).await {
        // Release the claim so the next tick retries this period.
        let client = state.pool.get().await?;
        client
            .execute(
                "DELETE FROM digest_log WHERE user_id = $1 AND kind = $2 AND period_key = $3",
                &[&user_id, &kind, &period_key],
            )
            .await?;
        return Err(e);
    }
    tracing::info!(user = %user_id, kind, period = %period_key, "digest sent");
    Ok(())
}

struct DigestContent {
    subject: String,
    body: String,
    footer: String,
}

/// Pro monthly digest from synced CEP snapshots. `None` when the account had
/// no synced activity in the period.
async fn render_pro_digest(
    state: &AppState,
    user_id: Uuid,
    period_key: &str,
) -> anyhow::Result<Option<DigestContent>> {
    let (start, end) = month_bounds(period_key)
        .ok_or_else(|| anyhow::anyhow!("bad month period key: {period_key}"))?;
    let client = state.pool.get().await?;
    let row = client
        .query_one(
            "SELECT COALESCE(SUM(tokens_saved),0)::bigint, \
                    COALESCE(SUM(tool_calls),0)::bigint, \
                    COUNT(*)::bigint, \
                    COALESCE(AVG(score),0)::float8 \
             FROM cep_scores \
             WHERE user_id = $1 AND recorded_at::date >= $2 AND recorded_at::date < $3",
            &[&user_id, &start, &end],
        )
        .await?;
    let (tokens, calls, sessions, score): (i64, i64, i64, f64) =
        (row.get(0), row.get(1), row.get(2), row.get(3));
    if sessions == 0 {
        return Ok(None);
    }
    let all_time: i64 = client
        .query_one(
            "SELECT COALESCE(SUM(tokens_saved),0)::bigint FROM cep_scores WHERE user_id = $1",
            &[&user_id],
        )
        .await?
        .get(0);

    Ok(Some(format_pro_digest(
        period_key, tokens, calls, sessions, score, all_time,
    )))
}

/// Team weekly digest from the hosted server's savings summary (proxied via
/// the billing plane with the audit-only control token). `None` when no
/// member has reported yet.
async fn render_team_digest(
    state: &AppState,
    user_id: Uuid,
    period_key: &str,
) -> anyhow::Result<Option<DigestContent>> {
    let Some((status, payload)) = billing_edge::forward_for_digest(
        &state.cfg,
        format!("/api/billing/team/{user_id}/savings"),
    )
    .await
    else {
        // Billing plane unreachable: error (no claim) so the next tick retries,
        // instead of silently burning the period.
        anyhow::bail!("billing plane unreachable for team digest");
    };
    if status != 200 {
        anyhow::bail!("billing plane returned HTTP {status} for team digest");
    }
    let available = payload
        .get("savings_available")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let summary = payload.get("summary");
    let (Some(summary), true) = (summary, available) else {
        return Ok(None);
    };
    let totals = &summary["totals"];
    let members = summary["member_count"].as_u64().unwrap_or(0);
    if members == 0 {
        return Ok(None);
    }
    Ok(Some(format_team_digest(
        period_key,
        totals["net_saved_tokens"].as_u64().unwrap_or(0),
        totals["saved_usd"].as_f64().unwrap_or(0.0),
        totals["total_events"].as_u64().unwrap_or(0),
        members,
        summary["by_model"].get(0).and_then(|m| m["model"].as_str()),
        summary["by_tool"].get(0).and_then(|t| t["tool"].as_str()),
    )))
}

// ─── Pure rendering (unit-tested) ────────────────────────────────────────────

fn compact(n: u64) -> String {
    if n >= 1_000_000_000 {
        format!("{:.1}B", n as f64 / 1e9)
    } else if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1e6)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1e3)
    } else {
        n.to_string()
    }
}

/// `2026-05` → human label `May 2026`.
fn month_label(period_key: &str) -> String {
    let Some((y, m)) = period_key.split_once('-') else {
        return period_key.to_string();
    };
    const NAMES: [&str; 12] = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    m.parse::<usize>()
        .ok()
        .filter(|m| (1..=12).contains(m))
        .map_or_else(
            || period_key.to_string(),
            |m| format!("{} {y}", NAMES[m - 1]),
        )
}

fn format_pro_digest(
    period_key: &str,
    tokens: i64,
    calls: i64,
    sessions: i64,
    score: f64,
    all_time_tokens: i64,
) -> DigestContent {
    let label = month_label(period_key);
    DigestContent {
        subject: format!(
            "Your LeanCTX month — {} tokens saved",
            compact(tokens.max(0) as u64)
        ),
        body: format!(
            "{label} in numbers:\n\n\
             - Tokens saved: {} (all-time: {})\n\
             - Agent actions measured: {}\n\
             - Sessions synced: {sessions}\n\
             - Mean CEP score: {score:.0}\n\n\
             Full picture: https://leanctx.com/account/cloud/",
            compact(tokens.max(0) as u64),
            compact(all_time_tokens.max(0) as u64),
            compact(calls.max(0) as u64),
        ),
        footer: "You receive this monthly digest because cloud sync is enabled on your LeanCTX Pro account.".into(),
    }
}

#[allow(clippy::too_many_arguments)]
fn format_team_digest(
    period_key: &str,
    net_tokens: u64,
    usd: f64,
    events: u64,
    members: u64,
    top_model: Option<&str>,
    top_tool: Option<&str>,
) -> DigestContent {
    let mut body = format!(
        "Team ROI for {period_key}:\n\n\
         - Net tokens saved: {} (~${usd:.2})\n\
         - Measured agent actions: {}\n\
         - Reporting members: {members}\n",
        compact(net_tokens),
        compact(events),
    );
    if let Some(m) = top_model {
        body.push_str(&format!("- Top model: {m}\n"));
    }
    if let Some(t) = top_tool {
        body.push_str(&format!("- Top tool: {t}\n"));
    }
    body.push_str("\nFull dashboard: https://leanctx.com/account/team/");
    DigestContent {
        subject: format!(
            "Your team saved {} tokens (~${usd:.2}) — {period_key}",
            compact(net_tokens)
        ),
        body,
        footer: "You receive this weekly digest as the owner of a LeanCTX Team server.".into(),
    }
}

// ─── Period keys ─────────────────────────────────────────────────────────────

/// The *previous* calendar month as `YYYY-MM` (the period a monthly digest
/// reports on).
fn previous_month_key(today: NaiveDate) -> String {
    let (y, m) = if today.month() == 1 {
        (today.year() - 1, 12)
    } else {
        (today.year(), today.month() - 1)
    };
    format!("{y}-{m:02}")
}

/// `[first day, first day of next month)` for a `YYYY-MM` key.
fn month_bounds(period_key: &str) -> Option<(NaiveDate, NaiveDate)> {
    let (y, m) = period_key.split_once('-')?;
    let (y, m): (i32, u32) = (y.parse().ok()?, m.parse().ok()?);
    let start = NaiveDate::from_ymd_opt(y, m, 1)?;
    let end = if m == 12 {
        NaiveDate::from_ymd_opt(y + 1, 1, 1)?
    } else {
        NaiveDate::from_ymd_opt(y, m + 1, 1)?
    };
    Some((start, end))
}

/// The *previous* ISO week as `YYYY-Www` (the period a weekly digest reports
/// on).
fn previous_iso_week_key(today: NaiveDate) -> String {
    let prev = today - chrono::Days::new(7);
    let iso = prev.iso_week();
    format!("{}-W{:02}", iso.year(), iso.week())
}

// ─── Ledger + opt-out plumbing ───────────────────────────────────────────────

async fn digest_already_sent(
    client: &deadpool_postgres::Client,
    user_id: Uuid,
    kind: &str,
    period_key: &str,
) -> anyhow::Result<bool> {
    Ok(client
        .query_opt(
            "SELECT 1 FROM digest_log WHERE user_id = $1 AND kind = $2 AND period_key = $3",
            &[&user_id, &kind, &period_key],
        )
        .await?
        .is_some())
}

/// Claim a (user, kind, period) slot. `false` when another tick already holds it.
async fn claim_digest(
    client: &deadpool_postgres::Client,
    user_id: Uuid,
    kind: &str,
    period_key: &str,
) -> anyhow::Result<bool> {
    let n = client
        .execute(
            "INSERT INTO digest_log (user_id, kind, period_key) VALUES ($1, $2, $3) \
             ON CONFLICT DO NOTHING",
            &[&user_id, &kind, &period_key],
        )
        .await?;
    Ok(n == 1)
}

/// Mint a fresh opt-out token for this send (stored hashed) and return the
/// full unsubscribe URL.
async fn rotate_opt_out_link(
    state: &AppState,
    client: &deadpool_postgres::Client,
    user_id: Uuid,
) -> anyhow::Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| anyhow::anyhow!("getrandom: {e}"))?;
    let token = hex_lower(&bytes);
    let sha = sha256_hex(token.as_bytes());
    client
        .execute(
            "INSERT INTO email_prefs (user_id, opt_out_token_sha256) VALUES ($1, $2) \
             ON CONFLICT (user_id) DO UPDATE \
               SET opt_out_token_sha256 = EXCLUDED.opt_out_token_sha256, updated_at = NOW()",
            &[&user_id, &sha],
        )
        .await?;
    Ok(format!(
        "{}/api/digest/opt-out?token={token}",
        state.cfg.api_base_url.trim_end_matches('/')
    ))
}

fn hex_lower(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut acc, byte| {
            let _ = write!(acc, "{byte:02x}");
            acc
        })
}

fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(data);
    hex_lower(&h.finalize())
}

// ─── HTTP handlers ───────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub(super) struct OptOutQuery {
    #[serde(default)]
    token: String,
}

/// `GET /api/digest/opt-out?token=…` — one-click unsubscribe straight from the
/// email; no login. Idempotent; unknown tokens get the same neutral page so
/// the endpoint can't be used to probe accounts.
pub(super) async fn opt_out(
    State(state): State<AppState>,
    Query(q): Query<OptOutQuery>,
) -> impl IntoResponse {
    const PAGE: &str = "<!doctype html><meta charset=\"utf-8\">\
        <title>LeanCTX digests</title>\
        <body style=\"font-family:system-ui;max-width:36rem;margin:4rem auto;line-height:1.6\">\
        <h1>You're unsubscribed</h1>\
        <p>You won't receive LeanCTX email digests anymore. You can re-enable them \
        anytime from your <a href=\"https://leanctx.com/account/\">account dashboard</a>.</p>";

    if q.token.len() == 64 && q.token.bytes().all(|b| b.is_ascii_hexdigit()) {
        let sha = sha256_hex(q.token.as_bytes());
        if let Ok(client) = state.pool.get().await {
            let _ = client
                .execute(
                    "UPDATE email_prefs SET digest_opt_out = TRUE, updated_at = NOW() \
                     WHERE opt_out_token_sha256 = $1",
                    &[&sha],
                )
                .await;
        }
    }
    Html(PAGE)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct DigestPrefBody {
    opt_out: bool,
}

/// `PUT /api/account/digest` — authenticated toggle (re-enable after an email
/// opt-out, or opt out without digging up an email).
pub(super) async fn put_digest_pref(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(body): Json<DigestPrefBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let (user_id, _email) = auth_user(&state, &headers).await?;
    let client = state
        .pool
        .get()
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    // Fresh rows need a token hash; reuse the rotation helper's shape with a
    // throwaway token (a real one is minted on the next send anyway).
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("getrandom: {e}")))?;
    let sha = sha256_hex(hex_lower(&bytes).as_bytes());
    client
        .execute(
            "INSERT INTO email_prefs (user_id, digest_opt_out, opt_out_token_sha256) \
             VALUES ($1, $2, $3) \
             ON CONFLICT (user_id) DO UPDATE \
               SET digest_opt_out = EXCLUDED.digest_opt_out, updated_at = NOW()",
            &[&user_id, &body.opt_out, &sha],
        )
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(json!({ "optOut": body.opt_out })))
}

/// `GET /api/account/digest` — current digest preference for the dashboard.
pub(super) async fn get_digest_pref(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> Result<Json<Value>, (StatusCode, String)> {
    let (user_id, _email) = auth_user(&state, &headers).await?;
    let client = state
        .pool
        .get()
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let opted_out: bool = client
        .query_opt(
            "SELECT digest_opt_out FROM email_prefs WHERE user_id = $1",
            &[&user_id],
        )
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .is_some_and(|r| r.get(0));
    Ok(Json(json!({ "optOut": opted_out })))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    #[test]
    fn previous_month_key_handles_january() {
        assert_eq!(previous_month_key(day("2026-06-10")), "2026-05");
        assert_eq!(previous_month_key(day("2026-01-03")), "2025-12");
    }

    #[test]
    fn month_bounds_cover_full_month() {
        let (start, end) = month_bounds("2026-05").unwrap();
        assert_eq!(start, day("2026-05-01"));
        assert_eq!(end, day("2026-06-01"));
        let (start, end) = month_bounds("2025-12").unwrap();
        assert_eq!(start, day("2025-12-01"));
        assert_eq!(end, day("2026-01-01"));
        assert!(month_bounds("garbage").is_none());
        assert!(month_bounds("2026-13").is_none());
    }

    #[test]
    fn previous_iso_week_key_is_last_week() {
        // 2026-06-10 is in ISO week 24 → previous is W23.
        assert_eq!(previous_iso_week_key(day("2026-06-10")), "2026-W23");
        // Year boundary: Jan 1 2026 (W01) → previous week is 2025-W52.
        assert_eq!(previous_iso_week_key(day("2026-01-01")), "2025-W52");
    }

    #[test]
    fn pro_digest_reads_like_a_report() {
        let d = format_pro_digest("2026-05", 4_200_000, 3_401, 18, 87.3, 78_000_000);
        assert_eq!(d.subject, "Your LeanCTX month — 4.2M tokens saved");
        assert!(d.body.contains("May 2026 in numbers"));
        assert!(d.body.contains("Tokens saved: 4.2M (all-time: 78.0M)"));
        assert!(d.body.contains("Agent actions measured: 3.4k"));
        assert!(d.body.contains("Sessions synced: 18"));
        assert!(d.body.contains("Mean CEP score: 87"));
        assert!(d.body.contains("https://leanctx.com/account/cloud/"));
        assert!(d.footer.contains("monthly digest"));
    }

    #[test]
    fn team_digest_reads_like_a_report() {
        let d = format_team_digest(
            "2026-W23",
            78_000_000,
            196.42,
            36_001,
            4,
            Some("claude-opus"),
            Some("ctx_read"),
        );
        assert!(d.subject.contains("78.0M tokens"));
        assert!(d.subject.contains("$196.42"));
        assert!(d.subject.contains("2026-W23"));
        assert!(d.body.contains("Reporting members: 4"));
        assert!(d.body.contains("Top model: claude-opus"));
        assert!(d.body.contains("Top tool: ctx_read"));
        assert!(d.body.contains("https://leanctx.com/account/team/"));
    }

    #[test]
    fn team_digest_omits_missing_breakdowns() {
        let d = format_team_digest("2026-W23", 1_000, 0.01, 10, 1, None, None);
        assert!(!d.body.contains("Top model"));
        assert!(!d.body.contains("Top tool"));
    }

    #[test]
    fn month_label_renders() {
        assert_eq!(month_label("2026-05"), "May 2026");
        assert_eq!(month_label("2025-12"), "December 2025");
        assert_eq!(month_label("garbage"), "garbage");
    }
}
