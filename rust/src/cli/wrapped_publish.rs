//! `gain --publish` / `--unpublish` — the client half of the hosted Wrapped permalink (VL-3b).
//!
//! Builds a privacy-safe, whitelisted payload from a local `WrappedReport` (a dedicated struct,
//! so a forbidden field cannot be serialized by construction), publishes it anonymously, and
//! records `{id, edit_token, url}` in `~/.lean-ctx/wrapped/published.json` so the same machine
//! can later delete the card. Server contract: `docs/contracts/wrapped-permalink-v1.md`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::cloud_client;
use crate::core::wrapped::WrappedReport;

const MAX_TOP_COMMANDS: usize = 12;
const MAX_NAME_LEN: usize = 40;
const MAX_LABEL_LEN: usize = 60;

// ─── Whitelisted payload (mirrors the server's accepted fields) ───────────────

#[derive(Serialize, Deserialize)]
struct TopCommand {
    name: String,
    pct: f64,
}

#[derive(Serialize, Deserialize)]
struct PublishPayload {
    period: String,
    tokens_saved: i64,
    cost_avoided_usd: f64,
    pricing_estimated: bool,
    compression_rate_pct: f64,
    total_commands: i64,
    sessions_count: i64,
    files_touched: i64,
    top_commands: Vec<TopCommand>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    display_name: Option<String>,
    leaderboard_opt_in: bool,
}

/// Builds the payload, clamping/sanitizing every field so the server's strict validator accepts
/// it. Only aggregate, non-identifying values are ever included.
fn build_payload(
    r: &WrappedReport,
    name: Option<&str>,
    no_model: bool,
    leaderboard: bool,
) -> PublishPayload {
    let top_commands = r
        .top_commands
        .iter()
        .filter_map(|(cmd, _count, pct)| {
            let name = sanitize(cmd, MAX_NAME_LEN);
            (!name.is_empty()).then(|| TopCommand {
                name,
                pct: pct.clamp(0.0, 100.0),
            })
        })
        .take(MAX_TOP_COMMANDS)
        .collect();

    let model_key =
        (!no_model && !r.model_key.is_empty()).then(|| sanitize(&r.model_key, MAX_LABEL_LEN));
    let display_name = name
        .map(|s| sanitize(s.trim(), MAX_LABEL_LEN))
        .filter(|s| !s.is_empty());

    PublishPayload {
        period: r.period.clone(),
        tokens_saved: clamp_u64(r.tokens_saved),
        cost_avoided_usd: r.cost_avoided_usd.max(0.0),
        pricing_estimated: r.pricing_estimated,
        compression_rate_pct: r.compression_rate_pct.clamp(0.0, 100.0),
        total_commands: clamp_u64(r.total_commands),
        sessions_count: i64::try_from(r.sessions_count).unwrap_or(i64::MAX),
        files_touched: clamp_u64(r.files_touched),
        top_commands,
        model_key,
        display_name,
        leaderboard_opt_in: leaderboard,
    }
}

fn clamp_u64(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

/// Strips control/markup characters and truncates to `max` chars (char-safe), matching the
/// server's `has_markup` + length rules so a publish never round-trips into a 400.
fn sanitize(s: &str, max: usize) -> String {
    s.chars()
        .filter(|c| !c.is_control() && *c != '<' && *c != '>')
        .take(max)
        .collect()
}

// ─── Local record of published cards ──────────────────────────────────────────

#[derive(Serialize, Deserialize, Clone)]
struct PublishedEntry {
    id: String,
    edit_token: String,
    url: String,
    period: String,
    published_at: String,
}

#[derive(Serialize, Deserialize, Default)]
struct PublishedStore {
    cards: Vec<PublishedEntry>,
}

fn store_path() -> Option<PathBuf> {
    let base = std::env::var("LEAN_CTX_DATA_DIR")
        .map(PathBuf::from)
        .ok()
        .or_else(|| dirs::home_dir().map(|h| h.join(".lean-ctx")))?;
    Some(base.join("wrapped").join("published.json"))
}

impl PublishedStore {
    fn load() -> Self {
        store_path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    fn save(&self) -> std::io::Result<()> {
        let Some(path) = store_path() else {
            return Ok(());
        };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let json = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(path, json)
    }
}

// ─── Commands ───────────────────────────────────────────────────────────────

/// `lean-ctx gain --publish` — generate, publish, record, and copy the permalink.
pub(crate) fn publish(period: &str, name: Option<&str>, no_model: bool, leaderboard: bool) {
    let report = WrappedReport::generate(period);
    if report.tokens_saved == 0 {
        println!("Nothing to publish yet — use lean-ctx for a bit, then try again.");
        return;
    }

    let payload = build_payload(&report, name, no_model, leaderboard);
    let value = match serde_json::to_value(&payload) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("Could not build payload: {e}");
            std::process::exit(1);
        }
    };

    match cloud_client::publish_wrapped(&value) {
        Ok(card) => {
            let mut store = PublishedStore::load();
            store.cards.push(PublishedEntry {
                id: card.id.clone(),
                edit_token: card.edit_token.clone(),
                url: card.url.clone(),
                period: period.to_string(),
                published_at: chrono::Utc::now().to_rfc3339(),
            });
            if let Err(e) = store.save() {
                tracing::warn!("Published, but could not save local record: {e}");
            }

            println!("Published: {}", card.url);
            if crate::core::share::copy_to_clipboard(&card.url) {
                println!("URL copied to clipboard — paste it anywhere.");
            }
            if leaderboard {
                if let Some(base) = card.url.split("/w/").next() {
                    println!("Listed on the leaderboard: {base}/leaderboard");
                }
            }
            println!(
                "Remove anytime with:  lean-ctx gain --unpublish={}",
                card.id
            );
        }
        Err(e) => {
            eprintln!("Publish failed: {e}");
            std::process::exit(1);
        }
    }
}

/// `lean-ctx gain --unpublish[=<id>]` — delete a published card via its stored `edit_token`.
/// With no id, removes the most recently published card.
pub(crate) fn unpublish(id: Option<&str>) {
    let mut store = PublishedStore::load();
    let entry = match id {
        Some(id) => store.cards.iter().find(|c| c.id == id).cloned(),
        None => store.cards.last().cloned(),
    };

    let Some(entry) = entry else {
        match id {
            Some(id) => println!("No published card with id {id} found locally."),
            None => println!("No published cards found. Publish one with: lean-ctx gain --publish"),
        }
        return;
    };

    match cloud_client::unpublish_wrapped(&entry.id, &entry.edit_token) {
        Ok(()) => {
            store.cards.retain(|c| c.id != entry.id);
            let _ = store.save();
            println!("Unpublished {} ({})", entry.id, entry.url);
        }
        Err(e) => {
            eprintln!("Unpublish failed: {e}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report() -> WrappedReport {
        WrappedReport {
            period: "week".into(),
            tokens_saved: 480_600_000,
            tokens_input: 600_000_000,
            cost_avoided_usd: 1441.79,
            total_commands: 1234,
            sessions_count: 56,
            top_commands: vec![
                ("ctx_search".into(), 100, 60.0),
                ("ctx_read".into(), 80, 40.0),
            ],
            compression_rate_pct: 91.2,
            files_touched: 789,
            daily_savings: vec![1, 2, 3],
            bounce_tokens: 100,
            model_key: "claude-opus".into(),
            pricing_estimated: true,
        }
    }

    #[test]
    fn payload_carries_only_whitelisted_aggregates() {
        let p = build_payload(&report(), Some("yvesg"), false, false);
        let v = serde_json::to_value(&p).unwrap();
        let obj = v.as_object().unwrap();
        // exactly the whitelisted keys, nothing more (no daily_savings, tokens_input, bounce…)
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec![
                "compression_rate_pct",
                "cost_avoided_usd",
                "display_name",
                "files_touched",
                "leaderboard_opt_in",
                "model_key",
                "period",
                "pricing_estimated",
                "sessions_count",
                "tokens_saved",
                "top_commands",
                "total_commands",
            ]
        );
    }

    #[test]
    fn no_model_omits_model_key() {
        let p = build_payload(&report(), None, true, false);
        assert!(p.model_key.is_none());
        assert!(p.display_name.is_none());
        let v = serde_json::to_value(&p).unwrap();
        assert!(v.as_object().unwrap().get("model_key").is_none());
    }

    #[test]
    fn leaderboard_flag_sets_opt_in() {
        assert!(!build_payload(&report(), None, false, false).leaderboard_opt_in);
        assert!(build_payload(&report(), None, false, true).leaderboard_opt_in);
    }

    #[test]
    fn sanitizes_markup_and_truncates() {
        assert_eq!(sanitize("ctx_search", MAX_NAME_LEN), "ctx_search");
        assert_eq!(sanitize("<script>", MAX_NAME_LEN), "script");
        assert_eq!(
            sanitize(&"a".repeat(100), MAX_NAME_LEN).chars().count(),
            MAX_NAME_LEN
        );
    }

    #[test]
    fn display_name_is_sanitized_and_capped() {
        let p = build_payload(&report(), Some("  <b>hi</b>  "), false, false);
        let name = p.display_name.unwrap();
        assert!(!name.contains('<') && !name.contains('>'));
        assert!(name.chars().count() <= MAX_LABEL_LEN);
    }

    #[test]
    fn top_commands_are_capped_and_clamped() {
        let mut r = report();
        r.top_commands = (0..20).map(|i| (format!("cmd{i}"), 1, 250.0)).collect();
        let p = build_payload(&r, None, false, false);
        assert!(p.top_commands.len() <= MAX_TOP_COMMANDS);
        assert!(p.top_commands.iter().all(|c| c.pct <= 100.0));
    }
}
