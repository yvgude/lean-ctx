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
    /// Whether this card has been claimed (bound to the logged-in account) on the
    /// server. Prevents redundant claim calls on every dashboard status poll.
    #[serde(default)]
    account_claimed: bool,
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

/// Read the current submission state for the dashboard card. As a side effect,
/// auto-claims any unclaimed cards when the user is logged in (leaderboard
/// consolidation, GH #736).
pub(crate) fn leaderboard_status() -> LeaderboardStatus {
    let cfg = crate::core::config::Config::load_global();
    let mut store = PublishedStore::load();

    // Auto-claim: if the user is logged in and has unclaimed published cards,
    // claim them server-side so the leaderboard union-find can merge entries.
    if crate::cloud_client::is_logged_in() {
        let mut dirty = false;
        for card in &mut store.cards {
            if !card.account_claimed && !card.edit_token.is_empty() {
                match crate::cloud_client::claim_wrapped(&card.id, &card.edit_token) {
                    Ok(()) => {
                        card.account_claimed = true;
                        dirty = true;
                    }
                    Err(e) => tracing::debug!("auto-claim {}: {e}", card.id),
                }
            }
        }
        if dirty {
            if let Err(e) = store.save() {
                tracing::debug!("auto-claim store save: {e}");
            }
        }
    }

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

// ─── Login-less machine linking (GH #736) ─────────────────────────────────────

/// This machine's linkable card: the all-time card if present (the one the
/// leaderboard represents), else the most recent one — with a stored edit_token.
fn linkable_card(store: &PublishedStore) -> Option<&PublishedEntry> {
    store
        .cards
        .iter()
        .filter(|c| !c.edit_token.is_empty())
        .max_by_key(|c| (c.period == "all", c.published_at.clone()))
}

/// `lean-ctx gain --link [CODE]` — merge this machine's leaderboard entry with
/// another machine's, without any account (GH #736).
///
/// Without a code: mints a short-lived pairing code for this machine's card.
/// With a code (from the other machine): joins both cards into one link group;
/// the public leaderboard then shows a single combined entry.
pub(crate) fn link(code: Option<&str>) {
    let store = PublishedStore::load();
    let Some(card) = linkable_card(&store) else {
        eprintln!(
            "No leaderboard card on this machine yet.\n\
             1. Publish:     lean-ctx gain --publish --leaderboard\n\
             2. Then link:   lean-ctx gain --link"
        );
        std::process::exit(1);
    };

    match code {
        None => match cloud_client::link_wrapped_start(&card.id, &card.edit_token) {
            Ok(minted) => {
                let mins = (minted.expires_in_secs / 60).max(1);
                println!("Pairing code:  {}", minted.code);
                println!();
                println!("On your other machine, run within {mins} minutes:");
                println!("  lean-ctx gain --link {}", minted.code);
                println!();
                println!(
                    "Both machines will then appear as one combined leaderboard entry \
                     (tokens summed). No account needed — the code proves card ownership."
                );
            }
            Err(e) => {
                eprintln!("Could not create a pairing code: {e}");
                std::process::exit(1);
            }
        },
        Some(code) => match cloud_client::link_wrapped_complete(&card.id, &card.edit_token, code) {
            Ok(()) => {
                println!(
                    "Linked! This machine and the code's machine now stack as one \
                     leaderboard entry."
                );
                println!(
                    "Link more machines anytime:  lean-ctx gain --link   (on any linked machine)"
                );
            }
            Err(e) => {
                eprintln!("Could not complete the link: {e}");
                std::process::exit(1);
            }
        },
    }
}

/// Dashboard-friendly link start: mint a pairing code (returns Result instead of
/// exiting). Used by the dashboard link proxy endpoint.
pub(crate) fn link_start() -> Result<cloud_client::LinkCode, String> {
    let store = PublishedStore::load();
    let card = linkable_card(&store)
        .ok_or("No published card on this machine yet. Submit to the leaderboard first.")?;
    cloud_client::link_wrapped_start(&card.id, &card.edit_token)
}

/// Dashboard-friendly link complete: join this card into a pairing code's group
/// (returns Result instead of exiting). Used by the dashboard link proxy endpoint.
pub(crate) fn link_complete(code: &str) -> Result<(), String> {
    let store = PublishedStore::load();
    let card = linkable_card(&store)
        .ok_or("No published card on this machine yet. Submit to the leaderboard first.")?;
    cloud_client::link_wrapped_complete(&card.id, &card.edit_token, code)
}

impl PublishedStore {
    fn load() -> Self {
        store_path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    fn save(&self) -> Result<(), String> {
        let Some(path) = store_path() else {
            return Ok(());
        };
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| format!("could not serialize published-card store: {e}"))?;
        crate::config_io::write_atomic(&path, &json)
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

/// Public accessor for dashboard routes that need the same identity resolution.
pub(crate) fn publisher_agent_id_for_dashboard() -> String {
    publisher_agent_id()
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
    let first_publish = agent_identity::stored_recovery_phrase(&agent).is_none()
        && !crate::core::data_dir::lean_ctx_data_dir()
            .map(|d| d.join("keys").join(format!("{agent}.phrase")).exists())
            .unwrap_or(false);
    let signing_key = if first_publish {
        let phrase = agent_identity::generate_recovery_phrase();
        agent_identity::import_phrase_identity(&agent, &phrase)
            .map_err(|e| format!("could not create phrase-based identity: {e}"))?
    } else {
        agent_identity::get_or_create_keypair(&agent)
            .map_err(|e| format!("could not load publisher key: {e}"))?
    };
    let public_key = agent_identity::hex_encode(&signing_key.verifying_key().to_bytes());
    let signature = agent_identity::hex_encode(&agent_identity::sign_bytes_with(
        &signing_key,
        payload_json.as_bytes(),
    ));

    let envelope = serde_json::json!({
        "payload_json": payload_json,
        "public_key": &public_key,
        "signature": signature,
    });
    let mut card = cloud_client::publish_wrapped(&envelope)?;

    if card.edit_token.is_none() {
        let stored = PublishedStore::load()
            .cards
            .into_iter()
            .find(|entry| entry.id == card.id && !entry.edit_token.is_empty())
            .map(|entry| entry.edit_token);
        card.edit_token = if let Some(token) = stored {
            Some(token)
        } else {
            let nonce = card.edit_token_challenge.as_deref().ok_or_else(|| {
                format!(
                    "card {} refreshed, but the server did not provide an ownership-recovery challenge",
                    card.id
                )
            })?;
            let proof = crate::core::wrapped::edit_token_recovery_message(&card.id, nonce);
            let recovery_signature = agent_identity::hex_encode(&agent_identity::sign_bytes_with(
                &signing_key,
                proof.as_bytes(),
            ));
            Some(cloud_client::recover_wrapped_edit_token(
                &card.id,
                nonce,
                &public_key,
                &recovery_signature,
            )?)
        };
    }

    record_published(&mut card, period, auto, leaderboard);
    Ok(card)
}

/// Records the card as the single local entry for its period: stale cards for the same period
/// with a different id are retired server-side (cleaning up any pre-upsert duplicates), and the
/// edit_token is preserved across signed re-publishes (the server returns it only on insert).
fn record_published(
    card: &mut cloud_client::PublishedCard,
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
        account_claimed: cloud_client::is_logged_in(),
    });
    if let Err(e) = store.save() {
        tracing::warn!("Published, but could not save local record: {e}");
        return;
    }
    let persisted = PublishedStore::load()
        .cards
        .iter()
        .any(|entry| entry.id == card.id && entry.edit_token == edit_token);
    if !persisted {
        tracing::warn!("Published, but local ownership record verification failed");
        return;
    }

    // Stack this machine under the user's account on the leaderboard (#488).
    // The CLI reports success only after the claim endpoint confirms it.
    if leaderboard && !edit_token.is_empty() && cloud_client::is_logged_in() {
        match cloud_client::claim_wrapped(&card.id, &edit_token) {
            Ok(()) => card.account_claimed = true,
            Err(e) => {
                tracing::warn!("Published, but could not link this card to your account: {e}");
            }
        }
    }
}

fn experimental_publication_enabled() -> bool {
    std::env::var("LEAN_CTX_EXPERIMENTAL_PUBLICATION").as_deref() == Ok("1")
}

/// Development-only hosted publication evaluation.
pub(crate) fn publish(period: &str, name: Option<&str>, leaderboard: bool) {
    if !experimental_publication_enabled() {
        eprintln!(
            "Hosted publication and public rankings are Research and unavailable in the public LeanCTX Runtime. \\
             Set LEAN_CTX_EXPERIMENTAL_PUBLICATION=1 only for a local development evaluation."
        );
        return;
    }
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
            maybe_show_recovery_phrase(&publisher_agent_id());
            println!("Published: {}", card.url);
            println!("{}", shared_disclosure(effective_name.is_some()));
            if crate::core::share::copy_to_clipboard(&card.url) {
                println!("URL copied to clipboard — paste it anywhere.");
            }
            if leaderboard {
                if let Some(base) = card.url.split("/w/").next() {
                    println!(
                        "Listed in the development-only ranking evaluation: {base}/metrics#leaderboard"
                    );
                }
                if card.account_claimed {
                    println!(
                        "Linked this leaderboard card to your account. Cards published from your other \
                         signed-in machines stack under the same leaderboard entry."
                    );
                } else {
                    if cloud_client::is_logged_in() {
                        println!(
                            "Account link was not confirmed; the card is published and remains under your control."
                        );
                    }
                    println!();
                    println!("  ┌─ Multiple machines? ─────────────────────────────────┐");
                    println!("  │  Combine them into one leaderboard entry:            │");
                    println!("  │  lean-ctx gain --link   (no account needed)          │");
                    println!("  │  Or use the dashboard:  http://localhost:3333        │");
                    println!("  └─────────────────────────────────────────────────────┘");
                }
                // A nameless entry shows as "anonymous" on the board — nudge once toward a handle.
                if effective_name.is_none() {
                    println!(
                        "Development tip: set a handle for the local evaluation instead of \"anonymous\" — \
                         lean-ctx gain --publish --leaderboard --name=\"your handle\""
                    );
                }
            } else {
                // Closes the loop for plain `--publish`: a private permalink never reaches the
                // public board, so spell out the exact opt-in path the metrics page documents.
                println!(
                    "Development-only ranking evaluation at https://leanctx.com/metrics — \
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
    if !experimental_publication_enabled() {
        return;
    }
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
    if !experimental_publication_enabled() {
        return;
    }
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
    if !experimental_publication_enabled() {
        return;
    }
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

/// What `lean-ctx gain --unpublish` was asked to take down.
#[derive(Clone, Copy)]
pub(crate) enum UnpublishTarget<'a> {
    /// `--unpublish` — the most recently published card.
    Latest,
    /// `--unpublish=all` — every card this machine published.
    All,
    /// `--unpublish=<id|url>` — one specific card.
    One(&'a str),
}

/// Extracts a card id from either a bare id or a published permalink.
///
/// The takedown request in #1726 arrived as a URL, because that is the only
/// form a user ever sees — the id is never printed on its own. Accepts
/// `https://leanctx.com/w/<id>`, `leanctx.com/w/<id>`, and the bare `<id>`;
/// query strings and fragments are dropped.
fn card_id_from_target(target: &str) -> &str {
    let target = target.trim();
    let target = target.split(['?', '#']).next().unwrap_or(target);
    target
        .rsplit('/')
        .find(|segment| !segment.is_empty())
        .unwrap_or(target)
}

/// Explains why a card the user can see is not takeable-down from here, instead
/// of leaving them with a bare "not found". The `edit_token` is minted once, at
/// publish time, and stored only on the publishing machine — so a card
/// published from another machine (or from a since-cleared data directory)
/// cannot be deleted from this one.
fn print_unknown_card(target: &str, store: &PublishedStore) {
    println!("No published card matching `{target}` is known on this machine.");
    println!();
    if store.cards.is_empty() {
        println!("  This machine has no publication records at all.");
    } else {
        println!("  Cards published from this machine:");
        for card in &store.cards {
            println!("    {}  {}", card.id, card.url);
        }
        println!();
        println!("  Remove one with: lean-ctx gain --unpublish=<id|url>");
        println!("  Remove all with: lean-ctx gain --unpublish=all");
    }
    println!();
    println!("  A card is deleted with the edit token minted when it was");
    println!("  published, and that token is kept only on the machine that");
    println!("  published it. If that machine or its data directory is gone,");
    println!("  request takedown at https://leanctx.com/support");
}

/// Deletes one card via its stored `edit_token`. Returns `false` on a server
/// error, having already reported it.
fn unpublish_entry(store: &mut PublishedStore, entry: &PublishedEntry) -> bool {
    match cloud_client::unpublish_wrapped(&entry.id, &entry.edit_token) {
        Ok(()) => {
            store.cards.retain(|c| c.id != entry.id);
            let _ = store.save();
            println!("Unpublished {} ({})", entry.id, entry.url);
            true
        }
        Err(e) => {
            eprintln!("Unpublish failed for {}: {e}", entry.id);
            false
        }
    }
}

/// `lean-ctx gain --unpublish[=<id|url|all>]` — take a published card down.
///
/// Deliberately not gated behind `LEAN_CTX_EXPERIMENTAL_PUBLICATION`: turning
/// publishing off must never strand a page that is already public (#1726).
pub(crate) fn unpublish(target: UnpublishTarget<'_>) {
    let mut store = PublishedStore::load();

    let entries = match target {
        UnpublishTarget::All => store.cards.clone(),
        UnpublishTarget::Latest => store.cards.last().cloned().into_iter().collect(),
        UnpublishTarget::One(target) => {
            let id = card_id_from_target(target);
            store
                .cards
                .iter()
                .find(|c| c.id == id)
                .cloned()
                .into_iter()
                .collect()
        }
    };

    if entries.is_empty() {
        match target {
            UnpublishTarget::One(target) => print_unknown_card(target, &store),
            _ => println!("No published cards found on this machine."),
        }
        return;
    }

    let mut failed = 0_usize;
    for entry in &entries {
        if !unpublish_entry(&mut store, entry) {
            failed += 1;
        }
    }
    if failed > 0 {
        eprintln!(
            "{failed} of {} card(s) could not be removed.",
            entries.len()
        );
        std::process::exit(1);
    }
}

/// `lean-ctx gain --rejoin <phrase>` — restore identity from a recovery phrase.
pub(crate) fn rejoin(phrase: Option<&str>) {
    let phrase = match phrase {
        Some(p) if !p.trim().is_empty() => p.trim().to_string(),
        _ => {
            eprintln!("Usage: lean-ctx gain --rejoin WORD1 WORD2 WORD3 WORD4");
            eprintln!("Enter the 4-word recovery phrase you received on first publish.");
            std::process::exit(1);
        }
    };

    let agent_id = publisher_agent_id();
    match crate::core::agent_identity::import_phrase_identity(&agent_id, &phrase) {
        Ok(key) => {
            let pub_hex = crate::core::agent_identity::hex_encode(&key.verifying_key().to_bytes());
            let publisher_id = {
                use sha2::{Digest, Sha256};
                let hash = Sha256::digest(key.verifying_key().as_bytes());
                let mut hex = String::new();
                for b in &hash {
                    use std::fmt::Write;
                    let _ = write!(hex, "{b:02x}");
                }
                hex
            };
            println!();
            println!("  Identity restored!");
            println!("  Publisher ID: {}...", &publisher_id[..16]);
            println!("  Public key:   {}...", &pub_hex[..16]);
            println!();
            println!("  Your leaderboard position is reconnected.");
            println!("  Run: lean-ctx gain --publish --leaderboard");
            println!();
            println!("  \u{2139}  Have entries from a different key? Merge them:");
            println!("     lean-ctx gain --link   (on the old machine)");
            println!();
        }
        Err(e) => {
            eprintln!("Failed to restore identity: {e}");
            std::process::exit(1);
        }
    }
}

/// Show recovery phrase during first publish (when no key exists yet).
pub(crate) fn maybe_show_recovery_phrase(agent_id: &str) {
    use crate::core::agent_identity;
    if let Some(phrase) = agent_identity::stored_recovery_phrase(agent_id) {
        println!();
        println!("  ┌─ Recovery Phrase (save this!) ─────────────────────────┐");
        println!("  │                                                        │");
        let upper = phrase.to_uppercase();
        let padded = format!("{upper:<54}");
        println!("  │  {padded}│");
        println!("  │                                                        │");
        println!("  │  Enter this on any machine to rejoin your position:    │");
        println!("  │  lean-ctx gain --rejoin {upper:<30}│");
        println!("  └────────────────────────────────────────────────────────┘");
        println!();
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

    #[test]
    fn published_token_store_roundtrips_atomically() {
        let _isolated = crate::core::data_dir::isolated_data_dir();
        let store = PublishedStore {
            cards: vec![PublishedEntry {
                id: "card-1".into(),
                edit_token: "secret-token".into(),
                url: "https://leanctx.com/w/card-1".into(),
                period: "all".into(),
                published_at: "2026-01-01T00:00:00Z".into(),
                auto: false,
                leaderboard: true,
                account_claimed: false,
            }],
        };
        store.save().unwrap();
        let loaded = PublishedStore::load();
        assert_eq!(loaded.cards.len(), 1);
        assert_eq!(loaded.cards[0].edit_token, "secret-token");
    }

    #[test]
    fn card_id_is_extracted_from_a_published_permalink() {
        // #1726: the id is never shown on its own, so a takedown request
        // arrives as the URL the user can actually see.
        let id = "196127aaad436a2cd42164cbbbbcd3cb";
        for target in [
            id,
            &format!("https://leanctx.com/w/{id}"),
            &format!("leanctx.com/w/{id}"),
            &format!("https://leanctx.com/w/{id}/"),
            &format!("https://leanctx.com/w/{id}?utm_source=x"),
            &format!("  https://leanctx.com/w/{id}#top  "),
        ] {
            assert_eq!(card_id_from_target(target), id, "target: {target}");
        }
    }
}
