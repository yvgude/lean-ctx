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

const MAX_LABEL_LEN: usize = 60;

// ─── Whitelisted payload (mirrors the server's accepted fields) ───────────────
//
// Deliberately minimal: only the four aggregate numbers the metrics page & leaderboard use
// (tokens, cost, compression — energy is derived from tokens), plus the period/opt-in needed
// to place the card and the optional display name. We do NOT collect command/session/file
// counts, top command names or the model — they were never used publicly.

#[derive(Serialize, Deserialize)]
struct PublishPayload {
    period: String,
    tokens_saved: i64,
    cost_avoided_usd: f64,
    pricing_estimated: bool,
    compression_rate_pct: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    display_name: Option<String>,
    leaderboard_opt_in: bool,
}

/// Builds the payload, clamping/sanitizing every field so the server's strict validator accepts
/// it. Only the minimal aggregate numbers and the optional chosen name are ever included.
fn build_payload(r: &WrappedReport, name: Option<&str>, leaderboard: bool) -> PublishPayload {
    let display_name = name
        .map(|s| sanitize(s.trim(), MAX_LABEL_LEN))
        .filter(|s| !s.is_empty());

    PublishPayload {
        period: r.period.clone(),
        tokens_saved: clamp_u64(r.tokens_saved),
        cost_avoided_usd: r.cost_avoided_usd.max(0.0),
        pricing_estimated: r.pricing_estimated,
        compression_rate_pct: r.compression_rate_pct.clamp(0.0, 100.0),
        display_name,
        leaderboard_opt_in: leaderboard,
    }
}

fn clamp_u64(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

/// One-line, honest disclosure of exactly what a publish shares. The payload is a fixed,
/// minimal set of aggregate numbers (enforced by `build_payload` + the server whitelist) —
/// never code, paths, repos or prompts. Printed on every publish so the user always sees it.
fn shared_disclosure(has_name: bool) -> String {
    let name = if has_name {
        ", and the display name you chose"
    } else {
        ""
    };
    format!(
        "Shared (aggregate numbers only): tokens saved, estimated USD, compression rate{name}.\n\
         Never shared: your code, file contents, file paths, repo names, prompts or messages."
    )
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
    /// True for cards created by `auto_publish`; these are auto-retired on refresh, while
    /// manual `--publish` cards (false, the default for older records) are never touched.
    #[serde(default)]
    auto: bool,
    /// Whether this card was published with leaderboard opt-in. Used to prevent
    /// auto-publish from accidentally downgrading a leaderboard entry.
    #[serde(default)]
    leaderboard: bool,
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

/// Returns true if the user has ever published at least one Wrapped card.
pub(crate) fn has_published() -> bool {
    let store = PublishedStore::load();
    !store.cards.is_empty()
}

/// Whether any published card is opted into the public leaderboard
/// (`leanctx.com/metrics`). Distinct from `has_published`: a user can hold a
/// private permalink (`/w/<id>`) without ever appearing on the public board, so
/// the recap hint can keep nudging them toward `--leaderboard` until they join.
pub(crate) fn has_leaderboard_entry() -> bool {
    PublishedStore::load().cards.iter().any(|c| c.leaderboard)
}

// ─── Dashboard surface (#466) ─────────────────────────────────────────────────
//
// The dashboard's leaderboard card needs to (a) show the current submission
// state, (b) submit on demand, and (c) flip auto-submit — all without the CLI's
// stdout/`process::exit` side effects. These thin wrappers reuse the exact same
// signed `publish_report` core so a dashboard submit is byte-for-byte the same
// privacy-safe payload as `gain --publish --leaderboard`.

/// Current leaderboard/publish state for the dashboard card.
#[derive(Serialize)]
pub(crate) struct LeaderboardStatus {
    /// Whether this machine has ever published any Wrapped card.
    pub published: bool,
    /// Whether any published card is opted into the public leaderboard.
    pub on_leaderboard: bool,
    /// Whether `[gain] auto_publish` is on (the auto-submit toggle).
    pub auto_submit: bool,
    /// The chosen public handle, if any (else the board shows "anonymous").
    pub display_name: Option<String>,
    /// Permalink of the representative (all-time) card, if published.
    pub url: Option<String>,
    /// RFC3339 timestamp of that card's last publish, if any.
    pub last_published_at: Option<String>,
}

/// Read the current submission state for the dashboard (pure, read-only).
pub(crate) fn leaderboard_status() -> LeaderboardStatus {
    let cfg = crate::core::config::Config::load_global();
    let store = PublishedStore::load();
    // The public board aggregates the all-time per-publisher card; prefer it,
    // falling back to the most recent card for the permalink/timestamp shown.
    let entry = store
        .cards
        .iter()
        .find(|c| c.period == "all")
        .or_else(|| store.cards.last());
    LeaderboardStatus {
        published: !store.cards.is_empty(),
        on_leaderboard: store.cards.iter().any(|c| c.leaderboard),
        auto_submit: cfg.gain.auto_publish,
        display_name: cfg.gain.display_name.clone(),
        url: entry.map(|c| c.url.clone()),
        last_published_at: entry.map(|c| c.published_at.clone()),
    }
}

/// Submit this machine's all-time recap to the public leaderboard on demand.
///
/// Mirrors the `gain --publish --leaderboard` path but returns a `Result` and
/// never prints or exits, so a dashboard route can render success/failure as
/// JSON. A chosen `name` is persisted (so future/auto submits reuse it); when
/// `None`, a previously saved handle is reused.
pub(crate) fn submit_leaderboard(
    name: Option<&str>,
) -> Result<cloud_client::PublishedCard, String> {
    let period = "all";
    let report = WrappedReport::generate(period);
    if report.tokens_saved == 0 {
        return Err("Nothing to publish yet — use lean-ctx for a bit, then try again.".to_string());
    }

    let mut cfg = crate::core::config::Config::load_global();
    if let Some(n) = name.map(str::trim).filter(|n| !n.is_empty())
        && cfg.gain.display_name.as_deref() != Some(n)
    {
        cfg.gain.display_name = Some(n.to_string());
        if let Err(e) = cfg.save() {
            tracing::warn!("Could not save display name: {e}");
        }
    }
    let effective_name = name
        .map(str::to_string)
        .or_else(|| cfg.gain.display_name.clone());

    publish_report(&report, period, effective_name.as_deref(), true, false)
}

/// Flip the auto-submit toggle (`[gain] auto_publish`). Enabling it also opts in
/// to the leaderboard so the next automatic publish actually reaches the board.
pub(crate) fn set_auto_submit(on: bool) -> Result<(), String> {
    crate::core::config::Config::update_global(|c| {
        c.gain.auto_publish = on;
        if on {
            c.gain.leaderboard = true;
        }
    })
    .map(|_| ())
    .map_err(|e| format!("could not save config: {e}"))
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

/// Stable per-machine identity used to sign published cards. It is the same id the savings
/// ledger signs with, so a user has one identity across proof artifacts and the leaderboard.
fn publisher_agent_id() -> String {
    std::env::var("LEAN_CTX_AGENT_ID")
        .or_else(|_| std::env::var("LCTX_AGENT_ID"))
        .unwrap_or_else(|_| "local".to_string())
}

/// Builds the whitelisted payload, signs it with this machine's persistent Ed25519 key, and
/// publishes it. The server derives a stable, login-less `publisher_id` from the public key and
/// upserts the card, so re-publishing the same period refreshes one card instead of duplicating.
fn publish_report(
    report: &WrappedReport,
    period: &str,
    name: Option<&str>,
    leaderboard: bool,
    auto: bool,
) -> Result<cloud_client::PublishedCard, String> {
    use crate::core::agent_identity;

    let payload = build_payload(report, name, leaderboard);
    let payload_json =
        serde_json::to_string(&payload).map_err(|e| format!("could not build payload: {e}"))?;

    let agent = publisher_agent_id();
    let (signature, public_key) =
        agent_identity::sign_with_public_key(&agent, payload_json.as_bytes())
            .map(|(sig, key)| {
                (
                    agent_identity::hex_encode(&sig),
                    agent_identity::hex_encode(&key.to_bytes()),
                )
            })
            .map_err(|e| format!("could not sign payload: {e}"))?;

    let envelope = serde_json::json!({
        "payload_json": payload_json,
        "public_key": public_key,
        "signature": signature,
    });
    let card = cloud_client::publish_wrapped(&envelope)?;
    record_published(&card, period, auto, leaderboard);
    Ok(card)
}

/// Records the card as the single local entry for its period: stale cards for the same period
/// with a different id are retired server-side (cleaning up any pre-upsert duplicates), and the
/// `edit_token` is preserved across signed re-publishes (the server returns it only on insert).
fn record_published(
    card: &cloud_client::PublishedCard,
    period: &str,
    auto: bool,
    leaderboard: bool,
) {
    let mut store = PublishedStore::load();

    for stale in store
        .cards
        .iter()
        .filter(|c| c.period == period && c.id != card.id && !c.edit_token.is_empty())
    {
        let _ = cloud_client::unpublish_wrapped(&stale.id, &stale.edit_token);
    }

    let edit_token = card.edit_token.clone().unwrap_or_else(|| {
        store
            .cards
            .iter()
            .find(|c| c.id == card.id)
            .map(|c| c.edit_token.clone())
            .unwrap_or_default()
    });

    store.cards.retain(|c| c.period != period);
    store.cards.push(PublishedEntry {
        id: card.id.clone(),
        edit_token: edit_token.clone(),
        url: card.url.clone(),
        period: period.to_string(),
        published_at: chrono::Utc::now().to_rfc3339(),
        auto,
        leaderboard,
    });
    if let Err(e) = store.save() {
        tracing::warn!("Published, but could not save local record: {e}");
    }

    // Stack this machine under the user's account on the leaderboard (#488): the board
    // aggregates cards that share a `user_id`, so claiming binds each machine's card to the
    // logged-in account. Best-effort and idempotent (re-publishes preserve `user_id`
    // server-side), and only meaningful for opted-in leaderboard cards.
    if leaderboard
        && !edit_token.is_empty()
        && cloud_client::is_logged_in()
        && let Err(e) = cloud_client::claim_wrapped(&card.id, &edit_token)
    {
        tracing::warn!("Published, but could not link this card to your account: {e}");
    }
}

/// `lean-ctx gain --publish` — generate, publish, record, and copy the permalink.
pub(crate) fn publish(period: &str, name: Option<&str>, leaderboard: bool) {
    let report = WrappedReport::generate(period);
    if report.tokens_saved == 0 {
        println!("Nothing to publish yet — use lean-ctx for a bit, then try again.");
        return;
    }

    // A name chosen here sticks: persist it so future (incl. automatic) publishes reuse it, and
    // fall back to a previously saved name when no `--name` flag is given.
    let mut cfg = crate::core::config::Config::load_global();
    if let Some(n) = name.map(str::trim).filter(|n| !n.is_empty())
        && cfg.gain.display_name.as_deref() != Some(n)
    {
        cfg.gain.display_name = Some(n.to_string());
        if let Err(e) = cfg.save() {
            tracing::warn!("Could not save display name: {e}");
        }
    }
    let effective_name = name
        .map(str::to_string)
        .or_else(|| cfg.gain.display_name.clone());

    match publish_report(
        &report,
        period,
        effective_name.as_deref(),
        leaderboard,
        false,
    ) {
        Ok(card) => {
            println!("Published: {}", card.url);
            println!("{}", shared_disclosure(effective_name.is_some()));
            if crate::core::share::copy_to_clipboard(&card.url) {
                println!("URL copied to clipboard — paste it anywhere.");
            }
            if leaderboard {
                if let Some(base) = card.url.split("/w/").next() {
                    println!("Listed on the community leaderboard: {base}/metrics#leaderboard");
                }
                // Account linking (#488): logged-in machines stack under one entry; otherwise
                // each machine is a separate row. `record_published` did the actual claim — this
                // only reflects the state to the user.
                if cloud_client::is_logged_in() {
                    println!(
                        "Linked to your account — all your machines now stack under one leaderboard entry."
                    );
                } else {
                    println!(
                        "Tip: run  lean-ctx login  so your machines stack under one leaderboard entry instead of separate rows."
                    );
                }
                // A nameless entry shows as "anonymous" on the board — nudge once toward a handle.
                if effective_name.is_none() {
                    println!(
                        "Tip: claim a handle so you're not listed as \"anonymous\" — \
                         lean-ctx gain --publish --leaderboard --name=\"your handle\""
                    );
                }
            } else {
                // Closes the loop for plain `--publish`: a private permalink never reaches the
                // public board, so spell out the exact opt-in path the metrics page documents.
                println!(
                    "Tip: also appear on the public leaderboard at https://leanctx.com/metrics — \
                     re-run with  lean-ctx gain --publish --leaderboard"
                );
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

/// Config-driven automatic publish, invoked from the `lean-ctx gain` recap views.
///
/// Opt-in via `[gain] auto_publish = true`, throttled by `auto_publish_interval_hours`, and
/// fully non-fatal: any failure is logged but never interrupts the recap. Because publishes are
/// signed, the server upserts one card per (machine, period), so refreshing the recap never
/// piles up duplicates on the public leaderboard.
pub(crate) fn maybe_auto_publish(period: &str) {
    let cfg = crate::core::config::Config::load_global();
    let g = &cfg.gain;
    if !g.auto_publish {
        return;
    }
    if !auto_publish_due(
        g.last_auto_publish.as_deref(),
        g.auto_publish_interval_hours,
    ) {
        return;
    }

    let report = WrappedReport::generate(period);
    if report.tokens_saved == 0 {
        return;
    }

    // Capture disclosure input before `cfg` is moved to record the timestamp below.
    let disclose_name = g.display_name.is_some();

    // Never downgrade leaderboard opt-in: if the stored card was on the leaderboard,
    // preserve that even when the config flag is (accidentally) false.
    let stored_leaderboard = PublishedStore::load()
        .cards
        .iter()
        .find(|c| c.period == period)
        .is_some_and(|c| c.leaderboard);
    let leaderboard = g.leaderboard || stored_leaderboard;

    match publish_report(
        &report,
        period,
        g.display_name.as_deref(),
        leaderboard,
        true,
    ) {
        Ok(card) => {
            let mut cfg = cfg;
            cfg.gain.last_auto_publish = Some(chrono::Utc::now().to_rfc3339());
            if let Err(e) = cfg.save() {
                tracing::warn!("Auto-published, but could not record timestamp: {e}");
            }
            println!("\nAuto-published your recap: {}", card.url);
            println!("{}", shared_disclosure(disclose_name));
            println!("  (disable with: lean-ctx config set gain.auto_publish false)");
        }
        Err(e) => tracing::warn!("Auto-publish skipped: {e}"),
    }
}

/// Background-safe auto-publish for long-running hosts (the MCP server).
///
/// Unlike [`maybe_auto_publish`], this is what makes auto-publish truly *automatic*:
/// it runs on MCP-server startup instead of requiring an interactive `lean-ctx gain`.
/// Two properties make that safe:
///   * **Silent** — it never writes to stdout (the MCP server owns stdout for the
///     JSON-RPC protocol; a stray `println!` would corrupt the stream). All output
///     goes through `tracing`.
///   * **Non-blocking** — the cheap gating (opt-in flag + 24h throttle) runs inline
///     so a thread is only spawned when a publish is actually due, and the network
///     call itself happens on a detached thread that never blocks startup.
///
/// Because publishes are signed, the server upserts one card per (machine, period),
/// so even if two sessions start at once the worst case is one idempotent re-publish.
pub(crate) fn maybe_auto_publish_background() {
    let cfg = crate::core::config::Config::load();
    let g = &cfg.gain;
    if !g.auto_publish {
        return;
    }
    if !auto_publish_due(
        g.last_auto_publish.as_deref(),
        g.auto_publish_interval_hours,
    ) {
        return;
    }
    std::thread::spawn(|| publish_in_background("all"));
}

/// The detached publish worker for [`maybe_auto_publish_background`]. Re-checks the
/// throttle right before the network call (cheap defence against a startup race) and
/// records the timestamp on success. Period is fixed to `all` to match the public
/// leaderboard/hero, which aggregate the all-time per-publisher card.
fn publish_in_background(period: &str) {
    let cfg = crate::core::config::Config::load_global();
    let g = &cfg.gain;
    if !g.auto_publish
        || !auto_publish_due(
            g.last_auto_publish.as_deref(),
            g.auto_publish_interval_hours,
        )
    {
        return;
    }

    let report = WrappedReport::generate(period);
    if report.tokens_saved == 0 {
        return;
    }

    // Never silently downgrade a leaderboard entry to a private card.
    let stored_leaderboard = PublishedStore::load()
        .cards
        .iter()
        .find(|c| c.period == period)
        .is_some_and(|c| c.leaderboard);
    let leaderboard = g.leaderboard || stored_leaderboard;

    match publish_report(
        &report,
        period,
        g.display_name.as_deref(),
        leaderboard,
        true,
    ) {
        Ok(card) => {
            let mut cfg = cfg;
            cfg.gain.last_auto_publish = Some(chrono::Utc::now().to_rfc3339());
            if let Err(e) = cfg.save() {
                tracing::warn!("Background auto-publish: could not record timestamp: {e}");
            }
            tracing::info!("Background auto-published recap: {}", card.url);
        }
        Err(e) => tracing::warn!("Background auto-publish skipped: {e}"),
    }
}

/// Whether enough time has elapsed since the last automatic publish. A missing or
/// unparseable timestamp counts as "due" so the first run always publishes.
fn auto_publish_due(last: Option<&str>, interval_hours: u64) -> bool {
    let Some(last) = last else {
        return true;
    };
    let Ok(prev) = chrono::DateTime::parse_from_rfc3339(last) else {
        return true;
    };
    let elapsed = chrono::Utc::now().signed_duration_since(prev.with_timezone(&chrono::Utc));
    let interval = i64::try_from(interval_hours.max(1)).unwrap_or(i64::MAX);
    elapsed.num_hours() >= interval
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
            percentile: Some(99),
        }
    }

    #[test]
    fn payload_carries_only_minimal_aggregates() {
        let p = build_payload(&report(), Some("yvesg"), false);
        let v = serde_json::to_value(&p).unwrap();
        let obj = v.as_object().unwrap();
        // exactly the minimal keys — no counts, top_commands, model_key, tokens_input, bounce…
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec![
                "compression_rate_pct",
                "cost_avoided_usd",
                "display_name",
                "leaderboard_opt_in",
                "period",
                "pricing_estimated",
                "tokens_saved",
            ]
        );
    }

    #[test]
    fn no_name_omits_display_name() {
        let p = build_payload(&report(), None, false);
        assert!(p.display_name.is_none());
        let v = serde_json::to_value(&p).unwrap();
        assert!(v.as_object().unwrap().get("display_name").is_none());
    }

    #[test]
    fn leaderboard_flag_sets_opt_in() {
        assert!(!build_payload(&report(), None, false).leaderboard_opt_in);
        assert!(build_payload(&report(), None, true).leaderboard_opt_in);
    }

    #[test]
    fn auto_publish_due_throttle() {
        // Never published or unparseable → always due (so the first run publishes).
        assert!(auto_publish_due(None, 24));
        assert!(auto_publish_due(Some("not-a-timestamp"), 24));
        // Published just now → not due within the interval.
        let now = chrono::Utc::now().to_rfc3339();
        assert!(!auto_publish_due(Some(&now), 24));
        // Published 48h ago → due again for a 24h interval.
        let two_days_ago = (chrono::Utc::now() - chrono::Duration::hours(48)).to_rfc3339();
        assert!(auto_publish_due(Some(&two_days_ago), 24));
        // A zero interval is clamped to 1h, so a fresh publish is still throttled.
        assert!(!auto_publish_due(Some(&now), 0));
    }

    #[test]
    fn sanitizes_markup_and_truncates() {
        assert_eq!(sanitize("ctx_search", MAX_LABEL_LEN), "ctx_search");
        assert_eq!(sanitize("<script>", MAX_LABEL_LEN), "script");
        assert_eq!(
            sanitize(&"a".repeat(100), MAX_LABEL_LEN).chars().count(),
            MAX_LABEL_LEN
        );
    }

    #[test]
    fn display_name_is_sanitized_and_capped() {
        let p = build_payload(&report(), Some("  <b>hi</b>  "), false);
        let name = p.display_name.unwrap();
        assert!(!name.contains('<') && !name.contains('>'));
        assert!(name.chars().count() <= MAX_LABEL_LEN);
    }

    #[test]
    fn compression_is_clamped_into_range() {
        let mut r = report();
        r.compression_rate_pct = 250.0;
        let p = build_payload(&r, None, false);
        assert!((0.0..=100.0).contains(&p.compression_rate_pct));
    }
}
