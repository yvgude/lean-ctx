//! `GET /v1/savings/summary` — the team savings roll-up (the customer-facing
//! "team usage visibility" surface, powering the account ROI dashboard).
//!
//! The savings store holds one append-only JSONL file per signer
//! (`savings_<pubkey>.jsonl`); each line is a [`SignedSavingsBatchV1`] snapshot
//! of that signer's **whole** local ledger (`period = "all"`). Successive batches
//! from the same signer are therefore cumulative re-snapshots, **not** increments
//! — so the honest team total is the sum of each signer's *latest* batch, never
//! the sum of every batch (which would multiply-count). Integrity is enforced at
//! ingest ([`super::savings_ingest`] verifies the Ed25519 signature before
//! storing), so this read path trusts the stored snapshots and parses defensively.
//!
//! Because every snapshot carries its own `created_at`, the cumulative history can
//! be replayed into a **daily time series**: for each signer, the value on a given
//! day is its most recent snapshot on or before that day (carry-forward); summing
//! across signers yields the team's cumulative ROI curve over the trailing window.
//! This is real reported data — no interpolation, no synthetic points.
//!
//! Authorisation: gated by [`TeamScope::Audit`](super::team) in the team auth
//! middleware (owner/admin only) — aggregate savings is sensitive team data.

use std::collections::HashMap;
use std::path::Path;

use axum::{
    Json,
    extract::{Path as AxumPath, State},
    http::StatusCode,
    response::IntoResponse,
};
use chrono::{Days, NaiveDate, Utc};
use serde::Serialize;

use crate::core::savings_ledger::SignedSavingsBatchV1;

use super::team::TeamAppState;

/// Trailing window (days) for the cumulative savings time series.
const SERIES_WINDOW_DAYS: u32 = 90;
/// Cap on per-model / per-tool rows surfaced to the dashboard.
const MAX_BREAKDOWN_ROWS: usize = 10;

/// Team-wide savings roll-up, aggregated from each member's latest signed batch.
#[derive(Debug, Default, Serialize)]
pub struct TeamSavingsSummary {
    pub schema_version: u32,
    pub generated_at: String,
    /// Distinct signers (≈ developers/agents) that have reported savings.
    pub member_count: usize,
    pub totals: SavingsTotals,
    /// One row per signer, descending by net saved tokens.
    pub by_member: Vec<MemberSavings>,
    /// Cross-team model breakdown (summed over each member's latest batch).
    pub by_model: Vec<ModelRow>,
    /// Cross-team tool breakdown (summed over each member's latest batch).
    pub by_tool: Vec<ToolRow>,
    /// Trailing-window cumulative daily series (oldest → newest). Empty until at
    /// least one timestamped batch exists.
    pub series: Vec<SeriesPoint>,
    /// Length of the series window in days (for client-side labelling).
    pub window_days: u32,
}

#[derive(Debug, Default, Serialize)]
pub struct SavingsTotals {
    /// Gross saved tokens (before bounce adjustment).
    pub saved_tokens: u64,
    /// Net saved tokens (gross minus compressed→full re-read bounce).
    pub net_saved_tokens: u64,
    /// Conservative USD upper bound (ignores prompt-cache discounts).
    pub saved_usd: f64,
    /// Measured agent actions across the team (sum of each signer's latest batch).
    pub total_events: u64,
}

#[derive(Debug, Serialize)]
pub struct MemberSavings {
    /// Truncated signer public key — a stable, privacy-preserving member id.
    pub signer: String,
    pub agent_id: String,
    pub saved_tokens: u64,
    pub net_saved_tokens: u64,
    pub saved_usd: f64,
    /// Measured agent actions for this signer (latest batch).
    pub total_events: u64,
    /// `created_at` of the member's most recent batch (RFC 3339).
    pub last_reported: String,
}

#[derive(Debug, Serialize)]
pub struct ModelRow {
    pub model: String,
    pub saved_tokens: u64,
    pub saved_usd: f64,
}

#[derive(Debug, Serialize)]
pub struct ToolRow {
    pub tool: String,
    pub saved_tokens: u64,
}

/// One day of the cumulative team series. Values are team-wide cumulative totals
/// as of the end of `date` (UTC), reconstructed by carrying each signer's latest
/// snapshot forward.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SeriesPoint {
    /// `YYYY-MM-DD` (UTC).
    pub date: String,
    pub net_saved_tokens: u64,
    pub saved_usd: f64,
    pub total_events: u64,
}

/// A single signer's cumulative snapshot on a given day.
#[derive(Debug, Clone, Copy)]
struct DayPoint {
    date: NaiveDate,
    net_saved_tokens: u64,
    saved_usd: f64,
    total_events: u64,
}

/// Per-member drilldown (GL #389) — one signer's full picture: latest totals,
/// model/tool breakdowns from the latest batch, and a 90-day cumulative series
/// replayed from that signer's snapshot history alone.
#[derive(Debug, Serialize)]
pub struct MemberDrilldown {
    pub schema_version: u32,
    pub generated_at: String,
    /// Truncated signer public key — matches `by_member[].signer` in the summary.
    pub signer: String,
    pub agent_id: String,
    /// `created_at` of the member's most recent batch (RFC 3339).
    pub last_reported: String,
    pub totals: SavingsTotals,
    /// This member's model breakdown (latest batch, top rows).
    pub by_model: Vec<ModelRow>,
    /// This member's tool breakdown (latest batch, top rows).
    pub by_tool: Vec<ToolRow>,
    /// Trailing-window cumulative daily series for this member only.
    pub series: Vec<SeriesPoint>,
    pub window_days: u32,
}

pub async fn v1_savings_summary(State(state): State<TeamAppState>) -> impl IntoResponse {
    let dir = state.team.savings_store_dir.lock().await.clone();
    let summary = tokio::task::spawn_blocking(move || aggregate(&dir))
        .await
        .unwrap_or_default();
    (StatusCode::OK, Json(summary))
}

/// `GET /v1/savings/member/{signer}` — drilldown for one member (GL #389).
/// `signer` is the truncated public key from `by_member[].signer`. Audit-scoped
/// like the summary (same sensitivity class). 404 when the signer has never
/// reported; 400 when the id can't be a signer prefix (defense-in-depth: the
/// id is also used to derive a store filename).
pub async fn v1_savings_member(
    State(state): State<TeamAppState>,
    AxumPath(signer): AxumPath<String>,
) -> axum::response::Response {
    if !is_valid_signer_prefix(&signer) {
        return super::json_error(
            StatusCode::BAD_REQUEST,
            "invalid_signer",
            "signer must be 1-64 chars of [A-Za-z0-9_-]",
        );
    }
    let dir = state.team.savings_store_dir.lock().await.clone();
    let drill = tokio::task::spawn_blocking(move || member_drilldown(&dir, &signer))
        .await
        .ok()
        .flatten();
    match drill {
        Some(d) => (StatusCode::OK, Json(d)).into_response(),
        None => super::json_error(
            StatusCode::NOT_FOUND,
            "unknown_member",
            "no savings batches reported for this signer",
        ),
    }
}

/// Signer ids are truncated Ed25519 public keys (hex or base64url) — anything
/// outside `[A-Za-z0-9_-]{1,64}` is rejected before touching the filesystem.
fn is_valid_signer_prefix(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Build the drilldown for one signer from its JSONL snapshot history.
/// Returns `None` when the signer file doesn't exist or holds no parseable batch.
pub(super) fn member_drilldown(dir: &Path, signer: &str) -> Option<MemberDrilldown> {
    // Ingest stores files under the *truncated* signer key, so the id from
    // `by_member[].signer` maps 1:1 onto a filename.
    let truncated: String = signer.chars().take(16).collect();
    let path = dir.join(format!("savings_{truncated}.jsonl"));

    let batches = read_all_batches(&path);
    let latest = batches.last()?;

    let mut by_model: Vec<ModelRow> = latest
        .totals
        .by_model
        .iter()
        .map(|(model, tokens, usd)| ModelRow {
            model: model.clone(),
            saved_tokens: *tokens,
            saved_usd: round_usd(*usd),
        })
        .collect();
    by_model.sort_by_key(|r| std::cmp::Reverse(r.saved_tokens));
    by_model.truncate(MAX_BREAKDOWN_ROWS);

    let mut by_tool: Vec<ToolRow> = latest
        .totals
        .by_tool
        .iter()
        .map(|(tool, tokens)| ToolRow {
            tool: tool.clone(),
            saved_tokens: *tokens,
        })
        .collect();
    by_tool.sort_by_key(|r| std::cmp::Reverse(r.saved_tokens));
    by_tool.truncate(MAX_BREAKDOWN_ROWS);

    let mut points: Vec<DayPoint> = batches
        .iter()
        .filter_map(|b| {
            parse_date(&b.created_at).map(|date| DayPoint {
                date,
                net_saved_tokens: b.totals.net_saved_tokens,
                saved_usd: b.totals.saved_usd,
                total_events: b.totals.total_events as u64,
            })
        })
        .collect();
    points.sort_by_key(|p| p.date);
    let series = build_series(
        std::slice::from_ref(&points),
        Utc::now().date_naive(),
        SERIES_WINDOW_DAYS,
    );

    Some(MemberDrilldown {
        schema_version: 1,
        generated_at: Utc::now().to_rfc3339(),
        signer: truncated,
        agent_id: latest.agent_id.clone(),
        last_reported: latest.created_at.clone(),
        totals: SavingsTotals {
            saved_tokens: latest.totals.saved_tokens,
            net_saved_tokens: latest.totals.net_saved_tokens,
            saved_usd: round_usd(latest.totals.saved_usd),
            total_events: latest.totals.total_events as u64,
        },
        by_model,
        by_tool,
        series,
        window_days: SERIES_WINDOW_DAYS,
    })
}

/// Aggregate the savings store: latest batch per signer (totals/breakdowns) plus
/// a carry-forward daily series replayed from every signer's full snapshot history.
/// Also feeds the `/v1/usage` snapshot ([`super::team_billing`]).
pub(super) fn aggregate(dir: &Path) -> TeamSavingsSummary {
    let mut members: Vec<MemberSavings> = Vec::new();
    let mut model_totals: HashMap<String, (u64, f64)> = HashMap::new();
    let mut tool_totals: HashMap<String, u64> = HashMap::new();
    let mut totals = SavingsTotals::default();
    let mut signer_points: Vec<Vec<DayPoint>> = Vec::new();

    let Ok(entries) = std::fs::read_dir(dir) else {
        return finalize(totals, members, model_totals, tool_totals, &signer_points);
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let named_savings = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("savings_"));
        let is_jsonl = path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("jsonl"));
        if !(named_savings && is_jsonl) {
            continue;
        }

        let batches = read_all_batches(&path);
        let Some(batch) = batches.last() else {
            continue;
        };

        totals.saved_tokens = totals
            .saved_tokens
            .saturating_add(batch.totals.saved_tokens);
        totals.net_saved_tokens = totals
            .net_saved_tokens
            .saturating_add(batch.totals.net_saved_tokens);
        totals.saved_usd += batch.totals.saved_usd;
        totals.total_events = totals
            .total_events
            .saturating_add(batch.totals.total_events as u64);

        for (model, tokens, usd) in &batch.totals.by_model {
            let acc = model_totals.entry(model.clone()).or_default();
            acc.0 = acc.0.saturating_add(*tokens);
            acc.1 += *usd;
        }
        for (tool, tokens) in &batch.totals.by_tool {
            let acc = tool_totals.entry(tool.clone()).or_default();
            *acc = acc.saturating_add(*tokens);
        }

        let signer = batch.signer_public_key.as_deref().unwrap_or("unknown");
        members.push(MemberSavings {
            signer: signer.chars().take(16).collect(),
            agent_id: batch.agent_id.clone(),
            saved_tokens: batch.totals.saved_tokens,
            net_saved_tokens: batch.totals.net_saved_tokens,
            saved_usd: round_usd(batch.totals.saved_usd),
            total_events: batch.totals.total_events as u64,
            last_reported: batch.created_at.clone(),
        });

        let mut points: Vec<DayPoint> = batches
            .iter()
            .filter_map(|b| {
                parse_date(&b.created_at).map(|date| DayPoint {
                    date,
                    net_saved_tokens: b.totals.net_saved_tokens,
                    saved_usd: b.totals.saved_usd,
                    total_events: b.totals.total_events as u64,
                })
            })
            .collect();
        points.sort_by_key(|p| p.date);
        signer_points.push(points);
    }

    finalize(totals, members, model_totals, tool_totals, &signer_points)
}

fn finalize(
    mut totals: SavingsTotals,
    mut members: Vec<MemberSavings>,
    model_totals: HashMap<String, (u64, f64)>,
    tool_totals: HashMap<String, u64>,
    signer_points: &[Vec<DayPoint>],
) -> TeamSavingsSummary {
    totals.saved_usd = round_usd(totals.saved_usd);
    members.sort_by_key(|m| std::cmp::Reverse(m.net_saved_tokens));

    let mut by_model: Vec<ModelRow> = model_totals
        .into_iter()
        .map(|(model, (saved_tokens, usd))| ModelRow {
            model,
            saved_tokens,
            saved_usd: round_usd(usd),
        })
        .collect();
    by_model.sort_by_key(|r| std::cmp::Reverse(r.saved_tokens));
    by_model.truncate(MAX_BREAKDOWN_ROWS);

    let mut by_tool: Vec<ToolRow> = tool_totals
        .into_iter()
        .map(|(tool, saved_tokens)| ToolRow { tool, saved_tokens })
        .collect();
    by_tool.sort_by_key(|r| std::cmp::Reverse(r.saved_tokens));
    by_tool.truncate(MAX_BREAKDOWN_ROWS);

    let series = build_series(signer_points, Utc::now().date_naive(), SERIES_WINDOW_DAYS);

    TeamSavingsSummary {
        schema_version: 2,
        generated_at: Utc::now().to_rfc3339(),
        member_count: members.len(),
        totals,
        by_member: members,
        by_model,
        by_tool,
        series,
        window_days: SERIES_WINDOW_DAYS,
    }
}

/// Replay each signer's snapshot history into a team-wide cumulative daily series
/// over the trailing `window_days` ending at `today`. For each day, a signer
/// contributes its most recent snapshot on or before that day (carry-forward);
/// the per-day team value is the sum across signers. Returns an empty series when
/// no signer has any timestamped batch.
fn build_series(
    signer_points: &[Vec<DayPoint>],
    today: NaiveDate,
    window_days: u32,
) -> Vec<SeriesPoint> {
    if window_days == 0 || signer_points.iter().all(Vec::is_empty) {
        return Vec::new();
    }
    let start = today - Days::new(u64::from(window_days.saturating_sub(1)));

    // Per-signer cursor into its (date-ascending) snapshot list and the
    // carried-forward cumulative value as of the current day.
    let mut cursor = vec![0usize; signer_points.len()];
    let mut carried = vec![(0u64, 0f64, 0u64); signer_points.len()];

    let mut out: Vec<SeriesPoint> = Vec::with_capacity(window_days as usize);
    let mut day = start;
    while day <= today {
        let mut net = 0u64;
        let mut usd = 0f64;
        let mut events = 0u64;
        for (si, points) in signer_points.iter().enumerate() {
            while cursor[si] < points.len() && points[cursor[si]].date <= day {
                let p = points[cursor[si]];
                carried[si] = (p.net_saved_tokens, p.saved_usd, p.total_events);
                cursor[si] += 1;
            }
            net = net.saturating_add(carried[si].0);
            usd += carried[si].1;
            events = events.saturating_add(carried[si].2);
        }
        out.push(SeriesPoint {
            date: day.format("%Y-%m-%d").to_string(),
            net_saved_tokens: net,
            saved_usd: round_usd(usd),
            total_events: events,
        });
        match day.succ_opt() {
            Some(next) => day = next,
            None => break,
        }
    }
    out
}

/// All parseable batches in a signer's JSONL file, in file (append/chronological)
/// order. The last element is the signer's latest cumulative snapshot.
fn read_all_batches(path: &Path) -> Vec<SignedSavingsBatchV1> {
    let Ok(content) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    content
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() {
                return None;
            }
            serde_json::from_str::<SignedSavingsBatchV1>(line).ok()
        })
        .collect()
}

/// Parse an RFC 3339 `created_at` into a UTC calendar date.
fn parse_date(created_at: &str) -> Option<NaiveDate> {
    chrono::DateTime::parse_from_rfc3339(created_at)
        .ok()
        .map(|dt| dt.with_timezone(&Utc).date_naive())
}

fn round_usd(v: f64) -> f64 {
    (v * 1_000_000.0).round() / 1_000_000.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::savings_ledger::signed_batch::BatchTotals;

    fn batch(signer: &str, net: u64, usd: f64, created_at: &str) -> SignedSavingsBatchV1 {
        SignedSavingsBatchV1 {
            schema_version: 1,
            kind: "lean-ctx.savings-batch".into(),
            created_at: created_at.into(),
            lean_ctx_version: "test".into(),
            agent_id: format!("agent-{signer}"),
            period: "all".into(),
            first_entry_hash: "genesis".into(),
            last_entry_hash: "head".into(),
            chain_valid: true,
            totals: BatchTotals {
                total_events: 1,
                saved_tokens: net,
                net_saved_tokens: net,
                saved_usd: usd,
                bounce_tokens: 0,
                bounce_events: 0,
                tokenizers: vec!["o200k_base".into()],
                by_model: vec![("claude-opus".into(), net, usd)],
                by_tool: vec![("ctx_read".into(), net)],
            },
            signer_public_key: Some(signer.into()),
            signature: Some("sig".into()),
        }
    }

    fn write_lines(dir: &Path, file: &str, batches: &[SignedSavingsBatchV1]) {
        let body = batches
            .iter()
            .map(|b| serde_json::to_string(b).unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(dir.join(file), body + "\n").unwrap();
    }

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "leanctx_savings_summary_{tag}_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn day(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    fn points(raw: &[(&str, u64, f64, u64)]) -> Vec<DayPoint> {
        raw.iter()
            .map(|(d, net, usd, ev)| DayPoint {
                date: day(d),
                net_saved_tokens: *net,
                saved_usd: *usd,
                total_events: *ev,
            })
            .collect()
    }

    #[test]
    fn latest_batch_per_signer_is_not_double_counted() {
        let dir = temp_dir("nodouble");
        // Signer A re-snapshots twice (1000 → 3000); only the latest must count.
        write_lines(
            &dir,
            "savings_aaaaaaaaaaaaaaaa.jsonl",
            &[
                batch("aaaaaaaaaaaaaaaa", 1000, 0.01, "2026-06-01T00:00:00Z"),
                batch("aaaaaaaaaaaaaaaa", 3000, 0.03, "2026-06-08T00:00:00Z"),
            ],
        );
        // Signer B has a single snapshot.
        write_lines(
            &dir,
            "savings_bbbbbbbbbbbbbbbb.jsonl",
            &[batch(
                "bbbbbbbbbbbbbbbb",
                2000,
                0.02,
                "2026-06-07T00:00:00Z",
            )],
        );

        let s = aggregate(&dir);
        assert_eq!(s.schema_version, 2);
        assert_eq!(s.member_count, 2);
        // 3000 (A latest) + 2000 (B) = 5000 — NOT 1000+3000+2000.
        assert_eq!(s.totals.net_saved_tokens, 5000);
        // total_events = 1 (A latest) + 1 (B) = 2.
        assert_eq!(s.totals.total_events, 2);
        // by_member sorted descending by net tokens.
        assert_eq!(s.by_member[0].net_saved_tokens, 3000);
        assert_eq!(s.by_member[1].net_saved_tokens, 2000);
        assert_eq!(s.by_member[0].total_events, 1);
        // model + tool breakdowns summed over members' latest batches.
        assert_eq!(s.by_model[0].model, "claude-opus");
        assert_eq!(s.by_model[0].saved_tokens, 5000);
        assert_eq!(s.by_tool[0].tool, "ctx_read");
        assert_eq!(s.by_tool[0].saved_tokens, 5000);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_or_missing_store_is_zeroed() {
        let missing = std::env::temp_dir().join("leanctx_savings_summary_does_not_exist_xyz");
        let _ = std::fs::remove_dir_all(&missing);
        let s = aggregate(&missing);
        assert_eq!(s.member_count, 0);
        assert_eq!(s.totals.net_saved_tokens, 0);
        assert!(s.by_member.is_empty());
        assert!(s.series.is_empty());
        assert_eq!(s.window_days, SERIES_WINDOW_DAYS);
    }

    #[test]
    fn non_savings_files_are_ignored() {
        let dir = temp_dir("ignore");
        std::fs::write(dir.join("audit.jsonl"), "{\"not\":\"a batch\"}\n").unwrap();
        std::fs::write(dir.join("README.md"), "hello\n").unwrap();
        write_lines(
            &dir,
            "savings_cccccccccccccccc.jsonl",
            &[batch(
                "cccccccccccccccc",
                700,
                0.007,
                "2026-06-08T00:00:00Z",
            )],
        );
        let s = aggregate(&dir);
        assert_eq!(s.member_count, 1);
        assert_eq!(s.totals.net_saved_tokens, 700);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn series_carries_each_signer_snapshot_forward_and_sums() {
        // A: 1000 on day 1, re-snapshots to 3000 on day 3.
        // B: 2000 on day 2 (single snapshot).
        let a = points(&[
            ("2026-06-01", 1000, 0.01, 10),
            ("2026-06-03", 3000, 0.03, 30),
        ]);
        let b = points(&[("2026-06-02", 2000, 0.02, 20)]);
        let series = build_series(&[a, b], day("2026-06-04"), 4);

        // 4-day window: 06-01 .. 06-04.
        assert_eq!(series.len(), 4);
        // day 1: A=1000, B=0 → 1000.
        assert_eq!(series[0].date, "2026-06-01");
        assert_eq!(series[0].net_saved_tokens, 1000);
        assert_eq!(series[0].total_events, 10);
        // day 2: A=1000 (carried), B=2000 → 3000.
        assert_eq!(series[1].net_saved_tokens, 3000);
        assert_eq!(series[1].total_events, 30);
        // day 3: A=3000 (re-snapshot), B=2000 → 5000.
        assert_eq!(series[2].net_saved_tokens, 5000);
        // day 4: both carried forward → 5000.
        assert_eq!(series[3].net_saved_tokens, 5000);
        assert_eq!(series[3].total_events, 50);
        assert!((series[3].saved_usd - 0.05).abs() < 1e-9);
    }

    #[test]
    fn series_window_clips_to_recent_days_only() {
        // A snapshot well before the window must still be carried in as the
        // opening value (not dropped) so the curve starts at the true baseline.
        let a = points(&[("2026-01-01", 5000, 0.5, 100)]);
        let series = build_series(&[a], day("2026-06-03"), 3);
        assert_eq!(series.len(), 3);
        assert_eq!(series[0].date, "2026-06-01");
        // Carried forward from January.
        assert_eq!(series[0].net_saved_tokens, 5000);
        assert_eq!(series[2].net_saved_tokens, 5000);
    }

    #[test]
    fn series_is_empty_without_points() {
        let series = build_series(&[Vec::new(), Vec::new()], day("2026-06-03"), 30);
        assert!(series.is_empty());
    }

    #[test]
    fn member_drilldown_returns_latest_breakdowns_and_own_series() {
        let dir = temp_dir("drill");
        // Two snapshots: drilldown totals/breakdowns must come from the LATEST,
        // the series from the full history (1000 → 3000).
        write_lines(
            &dir,
            "savings_aaaaaaaaaaaaaaaa.jsonl",
            &[
                batch("aaaaaaaaaaaaaaaa", 1000, 0.01, "2026-06-01T00:00:00Z"),
                batch("aaaaaaaaaaaaaaaa", 3000, 0.03, "2026-06-03T00:00:00Z"),
            ],
        );
        // A second signer must NOT leak into A's drilldown.
        write_lines(
            &dir,
            "savings_bbbbbbbbbbbbbbbb.jsonl",
            &[batch(
                "bbbbbbbbbbbbbbbb",
                9999,
                0.99,
                "2026-06-02T00:00:00Z",
            )],
        );

        let d = member_drilldown(&dir, "aaaaaaaaaaaaaaaa").expect("drilldown");
        assert_eq!(d.signer, "aaaaaaaaaaaaaaaa");
        assert_eq!(d.agent_id, "agent-aaaaaaaaaaaaaaaa");
        assert_eq!(d.totals.net_saved_tokens, 3000);
        assert_eq!(d.last_reported, "2026-06-03T00:00:00Z");
        assert_eq!(d.by_model.len(), 1);
        assert_eq!(d.by_model[0].model, "claude-opus");
        assert_eq!(d.by_model[0].saved_tokens, 3000);
        assert_eq!(d.by_tool[0].tool, "ctx_read");
        assert_eq!(d.window_days, SERIES_WINDOW_DAYS);
        // Series is member-only: its last value equals the member's latest
        // snapshot, not the team total (which would include signer B).
        let last = d.series.last().expect("series");
        assert_eq!(last.net_saved_tokens, 3000);
        assert_eq!(last.total_events, 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn member_drilldown_unknown_signer_is_none() {
        let dir = temp_dir("drillmissing");
        assert!(member_drilldown(&dir, "cccccccccccccccc").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn signer_prefix_validation_rejects_path_chars() {
        assert!(is_valid_signer_prefix("aaaaaaaaaaaaaaaa"));
        assert!(is_valid_signer_prefix("AbC123_-"));
        assert!(!is_valid_signer_prefix(""));
        assert!(!is_valid_signer_prefix("../../etc/passwd"));
        assert!(!is_valid_signer_prefix("a/b"));
        assert!(!is_valid_signer_prefix("a.b"));
        assert!(!is_valid_signer_prefix(&"a".repeat(65)));
    }
}
