//! Weekly team-ROI webhook (GL #388) — posts the savings roll-up to Slack,
//! Discord, or any generic JSON webhook once per ISO week.
//!
//! Design:
//! - The team server itself owns the cron (hourly tick): it already holds the
//!   savings store locally, so no control-plane round trip is needed.
//! - Post-once-per-week is enforced through a tiny state file next to the
//!   savings store (`roi_webhook_state.json`). A failed POST does **not**
//!   advance the state, so the next tick retries; a week with zero reporting
//!   members posts nothing (no synthetic numbers, no noise).
//! - Payload shape is detected from the URL: Slack incoming webhooks take
//!   `{"text": …}`, Discord webhooks take `{"content": …}`, anything else
//!   gets both keys so generic receivers can pick.
//! - HTTPS is enforced — `team.json` is operator-controlled, but a webhook
//!   URL is the one field that leaves the box, so it gets the hard gate.

use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{Datelike, Utc};

use super::savings_summary::{TeamSavingsSummary, aggregate, member_drilldown};
use super::team::TeamAppState;

/// Hourly tick: cheap enough to be negligible, frequent enough that a
/// restart or a transient webhook failure delays the weekly post by at most
/// an hour.
const TICK: Duration = Duration::from_hours(1);

/// State file name, stored inside the savings store directory. The summary
/// aggregator only reads `savings_*.jsonl`, so this never pollutes it.
const STATE_FILE: &str = "roi_webhook_state.json";

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct WebhookState {
    /// ISO-week key (`2026-W24`) of the last successful post.
    #[serde(default)]
    last_posted_week: Option<String>,
}

/// Validate the webhook URL at boot: HTTPS only.
pub fn validate_webhook_url(url: &str) -> Result<(), String> {
    if url.starts_with("https://") {
        Ok(())
    } else {
        Err("roiWebhookUrl must be https:// — refusing to post team ROI over plaintext".into())
    }
}

/// Spawn the weekly poster. Call once from `serve_team` when
/// `roiWebhookUrl` is configured and validated.
#[must_use]
pub fn spawn_weekly_roi_webhook(state: TeamAppState, url: String) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tick(&state, &url).await;
            tokio::time::sleep(TICK).await;
        }
    })
}

/// One scheduler tick: post if this ISO week hasn't been posted yet and at
/// least one member has reported.
async fn tick(state: &TeamAppState, url: &str) {
    let week = iso_week_key(Utc::now().date_naive());
    let dir = state.team.savings_store_dir.lock().await.clone();
    let state_path = dir.join(STATE_FILE);

    if load_state(&state_path).last_posted_week.as_deref() == Some(week.as_str()) {
        return;
    }

    let url_owned = url.to_string();
    let posted = tokio::task::spawn_blocking(move || {
        let summary = aggregate(&dir);
        if summary.member_count == 0 {
            // Nothing reported yet — stay quiet and retry next tick, so the
            // very first post happens as soon as real data exists.
            return false;
        }
        let mover = top_mover(&dir, &summary);
        let text = format_roi_message(&summary, &week, mover.as_deref());
        let payload = payload_for(&url_owned, &text);
        match post_webhook(&url_owned, &payload) {
            Ok(()) => {
                save_state(
                    &dir.join(STATE_FILE),
                    &WebhookState {
                        last_posted_week: Some(week),
                    },
                );
                true
            }
            Err(e) => {
                tracing::warn!("team ROI webhook post failed (will retry next tick): {e}");
                false
            }
        }
    })
    .await
    .unwrap_or(false);

    if posted {
        tracing::info!("team ROI webhook posted weekly summary");
    }
}

/// `2026-W24`-style key — flips Monday 00:00 UTC, which is exactly when the
/// new weekly post becomes due.
fn iso_week_key(date: chrono::NaiveDate) -> String {
    let iso = date.iso_week();
    format!("{}-W{:02}", iso.year(), iso.week())
}

fn load_state(path: &Path) -> WebhookState {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_state(path: &Path, state: &WebhookState) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(state)
        && let Err(e) = std::fs::write(path, json)
    {
        tracing::warn!("could not persist ROI webhook state: {e}");
    }
}

/// The member with the largest net-token gain over the trailing 7 days,
/// computed from each member's own carry-forward series (real reported
/// snapshots only). `None` when nobody moved.
fn top_mover(dir: &Path, summary: &TeamSavingsSummary) -> Option<String> {
    let mut best: Option<(String, u64)> = None;
    for m in &summary.by_member {
        let Some(drill) = member_drilldown(dir, &m.signer) else {
            continue;
        };
        let series = &drill.series;
        if series.is_empty() {
            continue;
        }
        let last = series.last().map_or(0, |p| p.net_saved_tokens);
        // 7 days back (series is daily); clamp for short series.
        let base_idx = series.len().saturating_sub(8);
        let base = series[base_idx].net_saved_tokens;
        let delta = last.saturating_sub(base);
        if delta > 0 && best.as_ref().is_none_or(|(_, b)| delta > *b) {
            best = Some((
                format!("{} (+{} tokens 7d)", drill.agent_id, compact(delta)),
                delta,
            ));
        }
    }
    best.map(|(label, _)| label)
}

/// Human-compact token count (`78.0M`, `4.2k`).
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

/// Render the weekly message — totals, 7-day window, top mover, top
/// model/tool. Plain text by design: it renders identically in Slack,
/// Discord and any generic receiver; no per-vendor block kits to maintain.
fn format_roi_message(summary: &TeamSavingsSummary, week: &str, top_mover: Option<&str>) -> String {
    let t = &summary.totals;
    let mut lines = vec![
        format!("lean-ctx team ROI — {week}"),
        format!(
            "Net saved: {} tokens (~${:.2}) · {} measured actions · {} reporting member{}",
            compact(t.net_saved_tokens),
            t.saved_usd,
            compact(t.total_events),
            summary.member_count,
            if summary.member_count == 1 { "" } else { "s" },
        ),
    ];

    // Trailing-7d window from the team series (cumulative ⇒ delta of ends).
    if summary.series.len() >= 2 {
        let last = summary.series.last().unwrap();
        let base_idx = summary.series.len().saturating_sub(8);
        let base = &summary.series[base_idx];
        let d_tokens = last.net_saved_tokens.saturating_sub(base.net_saved_tokens);
        let d_usd = (last.saved_usd - base.saved_usd).max(0.0);
        lines.push(format!(
            "Last 7 days: +{} tokens (~${d_usd:.2})",
            compact(d_tokens)
        ));
    }

    if let Some(mover) = top_mover {
        lines.push(format!("Top mover: {mover}"));
    }
    if let Some(m) = summary.by_model.first() {
        lines.push(format!(
            "Top model: {} ({} tokens)",
            m.model,
            compact(m.saved_tokens)
        ));
    }
    if let Some(t) = summary.by_tool.first() {
        lines.push(format!(
            "Top tool: {} ({} tokens)",
            t.tool,
            compact(t.saved_tokens)
        ));
    }
    lines.join("\n")
}

/// Choose the payload shape from the webhook URL.
fn payload_for(url: &str, text: &str) -> serde_json::Value {
    let is_discord =
        url.contains("discord.com/api/webhooks") || url.contains("discordapp.com/api/webhooks");
    let is_slack = url.contains("hooks.slack.com");
    if is_discord {
        serde_json::json!({ "content": text })
    } else if is_slack {
        serde_json::json!({ "text": text })
    } else {
        // Generic receiver: send both common keys.
        serde_json::json!({ "text": text, "content": text })
    }
}

/// POST the payload. Synchronous (callers run it inside `spawn_blocking`).
fn post_webhook(url: &str, payload: &serde_json::Value) -> Result<(), String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(15)))
        .build()
        .into();
    let resp = agent
        .post(url)
        .header("Content-Type", "application/json")
        .send(payload.to_string().as_bytes())
        .map_err(|e| e.to_string())?;
    let code = resp.status().as_u16();
    // Slack returns 200, Discord 204 — accept the whole 2xx class.
    if (200..300).contains(&code) {
        Ok(())
    } else {
        Err(format!("webhook returned HTTP {code}"))
    }
}

/// Expose the state path for ops/debugging (`lean-ctx team … status` later).
#[allow(dead_code)]
#[must_use]
pub fn state_path(savings_dir: &Path) -> PathBuf {
    savings_dir.join(STATE_FILE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http_server::savings_summary::{
        MemberSavings, ModelRow, SavingsTotals, SeriesPoint, ToolRow,
    };

    fn summary() -> TeamSavingsSummary {
        TeamSavingsSummary {
            schema_version: 2,
            generated_at: "2026-06-10T00:00:00Z".into(),
            member_count: 2,
            totals: SavingsTotals {
                saved_tokens: 80_000_000,
                net_saved_tokens: 78_000_000,
                saved_usd: 196.42,
                total_events: 36_001,
            },
            by_member: vec![MemberSavings {
                signer: "aaaaaaaaaaaaaaaa".into(),
                agent_id: "dev-laptop".into(),
                saved_tokens: 50_000_000,
                net_saved_tokens: 48_000_000,
                saved_usd: 120.0,
                total_events: 20_000,
                last_reported: "2026-06-09T00:00:00Z".into(),
            }],
            by_model: vec![ModelRow {
                model: "claude-opus".into(),
                saved_tokens: 41_200_000,
                saved_usd: 150.0,
            }],
            by_tool: vec![ToolRow {
                tool: "ctx_read".into(),
                saved_tokens: 28_900_000,
            }],
            series: vec![
                SeriesPoint {
                    date: "2026-06-01".into(),
                    net_saved_tokens: 70_000_000,
                    saved_usd: 180.0,
                    total_events: 30_000,
                },
                SeriesPoint {
                    date: "2026-06-10".into(),
                    net_saved_tokens: 78_000_000,
                    saved_usd: 196.42,
                    total_events: 36_001,
                },
            ],
            window_days: 90,
        }
    }

    #[test]
    fn iso_week_key_flips_on_monday() {
        // 2026-06-07 is a Sunday (W23), 2026-06-08 a Monday (W24).
        let sun = chrono::NaiveDate::from_ymd_opt(2026, 6, 7).unwrap();
        let mon = chrono::NaiveDate::from_ymd_opt(2026, 6, 8).unwrap();
        assert_eq!(iso_week_key(sun), "2026-W23");
        assert_eq!(iso_week_key(mon), "2026-W24");
    }

    #[test]
    fn payload_shape_follows_vendor() {
        let slack = payload_for("https://hooks.slack.com/services/T/B/X", "hi");
        assert_eq!(slack["text"], "hi");
        assert!(slack.get("content").is_none());

        let discord = payload_for("https://discord.com/api/webhooks/1/x", "hi");
        assert_eq!(discord["content"], "hi");
        assert!(discord.get("text").is_none());

        let generic = payload_for("https://example.com/hook", "hi");
        assert_eq!(generic["text"], "hi");
        assert_eq!(generic["content"], "hi");
    }

    #[test]
    fn message_carries_totals_window_and_movers() {
        let msg = format_roi_message(&summary(), "2026-W24", Some("dev-laptop (+8.0M tokens 7d)"));
        assert!(msg.contains("2026-W24"));
        assert!(msg.contains("78.0M tokens"));
        assert!(msg.contains("$196.42"));
        assert!(msg.contains("36.0k measured actions"));
        assert!(msg.contains("2 reporting members"));
        assert!(msg.contains("Last 7 days: +8.0M tokens"));
        assert!(msg.contains("Top mover: dev-laptop"));
        assert!(msg.contains("Top model: claude-opus (41.2M tokens)"));
        assert!(msg.contains("Top tool: ctx_read (28.9M tokens)"));
        // Discord hard limit is 2000 chars — stay far below.
        assert!(msg.len() < 1000, "message must stay compact: {}", msg.len());
    }

    #[test]
    fn state_roundtrip_and_default() {
        let dir =
            std::env::temp_dir().join(format!("leanctx_roi_webhook_state_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(STATE_FILE);

        assert_eq!(load_state(&path).last_posted_week, None);
        save_state(
            &path,
            &WebhookState {
                last_posted_week: Some("2026-W24".into()),
            },
        );
        assert_eq!(
            load_state(&path).last_posted_week.as_deref(),
            Some("2026-W24")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn webhook_url_must_be_https() {
        assert!(validate_webhook_url("https://hooks.slack.com/services/x").is_ok());
        assert!(validate_webhook_url("http://hooks.slack.com/services/x").is_err());
        assert!(validate_webhook_url("ftp://example.com").is_err());
    }

    #[test]
    fn compact_formatting() {
        assert_eq!(compact(950), "950");
        assert_eq!(compact(4_200), "4.2k");
        assert_eq!(compact(78_000_000), "78.0M");
        assert_eq!(compact(1_500_000_000), "1.5B");
    }

    /// Real HTTP round trip against a local listener: proves the POST body,
    /// content type and 2xx/5xx handling without any external service.
    #[test]
    fn post_webhook_roundtrip_against_local_listener() {
        use std::io::{Read, Write};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        let handle = std::thread::spawn(move || {
            let mut bodies = Vec::new();
            for status in ["204 No Content", "500 Internal Server Error"] {
                let (mut stream, _) = listener.accept().unwrap();
                // Read until headers + declared body length are complete
                // (header and body may arrive in separate TCP writes).
                let mut raw = Vec::new();
                let mut buf = [0u8; 4096];
                loop {
                    let n = stream.read(&mut buf).unwrap();
                    if n == 0 {
                        break;
                    }
                    raw.extend_from_slice(&buf[..n]);
                    let text = String::from_utf8_lossy(&raw);
                    if let Some(head_end) = text.find("\r\n\r\n") {
                        let content_len = text
                            .to_ascii_lowercase()
                            .lines()
                            .find_map(|l| {
                                l.strip_prefix("content-length:")
                                    .map(str::trim)
                                    .map(String::from)
                            })
                            .and_then(|v| v.parse::<usize>().ok())
                            .unwrap_or(0);
                        if raw.len() >= head_end + 4 + content_len {
                            break;
                        }
                    }
                }
                bodies.push(String::from_utf8_lossy(&raw).to_string());
                let resp =
                    format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                stream.write_all(resp.as_bytes()).unwrap();
            }
            bodies
        });

        let url = format!("http://{addr}/hook");
        let payload = serde_json::json!({ "content": "lean-ctx team ROI — 2026-W24" });

        // 204 → Ok.
        assert!(post_webhook(&url, &payload).is_ok());
        // 500 → Err mentioning the code (state must not advance on this).
        let err = post_webhook(&url, &payload).unwrap_err();
        assert!(err.contains("500"), "got: {err}");

        let bodies = handle.join().unwrap();
        assert!(bodies[0].contains("POST /hook"));
        // ureq normalizes header casing — compare case-insensitively.
        assert!(
            bodies[0]
                .to_ascii_lowercase()
                .contains("content-type: application/json")
        );
        assert!(bodies[0].contains("lean-ctx team ROI"));
    }
}
