//! Hosted opt-in Wrapped permalink (`/api/wrapped`) — the public side of the viral loop.
//!
//! Anonymous publish returns a public `id` + one-time `edit_token`; the token authorizes
//! delete and the optional account `claim`. Only a closed whitelist of aggregate fields is
//! accepted (`deny_unknown_fields`); no repo names, paths, code, history or raw IPs are stored.
//!
//! Contract: `docs/contracts/wrapped-permalink-v1.md`.

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use serde::{Deserialize, Serialize};

use super::auth::{AppState, auth_user, constant_time_eq, generate_token, sha256_hex};
use super::helpers::internal_error;

/// Max publishes accepted per `ip_hash` within the rolling rate-limit window.
const MAX_PUBLISH_PER_HOUR: i64 = 20;
/// Documented publish body cap (contract: `413 payload_too_large` over this size).
const MAX_BODY_BYTES: usize = 8 * 1024;
const MAX_TOP_COMMANDS: usize = 12;
const MAX_NAME_LEN: usize = 40;
const MAX_LABEL_LEN: usize = 60;

type ApiResult<T> = Result<T, (StatusCode, String)>;

/// JSON error envelope matching the cloud server convention (`helpers::internal_error`).
fn err(status: StatusCode, code: &str) -> (StatusCode, String) {
    (status, format!(r#"{{"error":"{code}"}}"#))
}

fn bad_payload() -> (StatusCode, String) {
    err(StatusCode::BAD_REQUEST, "invalid_payload")
}

// ─── Whitelisted payload (the ONLY fields that may be published) ──────────────

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TopCommand {
    pub name: String,
    pub pct: f64,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PublishPayload {
    pub period: String,
    pub tokens_saved: i64,
    pub cost_avoided_usd: f64,
    pub pricing_estimated: bool,
    pub compression_rate_pct: f64,
    // The fields below were removed from the publish whitelist (privacy minimalism): current
    // clients no longer send them. They remain declared as optional/defaulted ONLY so that
    // cards published by older clients still deserialize under `deny_unknown_fields`. Nothing
    // public renders them anymore — the hosted card omits any that are zero/empty.
    #[serde(default)]
    pub total_commands: i64,
    #[serde(default)]
    pub sessions_count: i64,
    #[serde(default)]
    pub files_touched: i64,
    #[serde(default)]
    pub top_commands: Vec<TopCommand>,
    #[serde(default)]
    pub model_key: Option<String>,
    #[serde(default)]
    pub display_name: Option<String>,
    /// Opt-in (default off): show this card on the public leaderboard.
    #[serde(default)]
    pub leaderboard_opt_in: bool,
}

impl PublishPayload {
    /// Rejects anything outside the documented bounds. Pure (no I/O) so it is unit-tested.
    fn validate(&self) -> ApiResult<()> {
        if !matches!(self.period.as_str(), "day" | "week" | "month" | "all") {
            return Err(bad_payload());
        }
        if self.tokens_saved < 0 || self.total_commands < 0 {
            return Err(bad_payload());
        }
        if self.sessions_count < 0 || self.files_touched < 0 {
            return Err(bad_payload());
        }
        if !finite_nonneg(self.cost_avoided_usd) {
            return Err(bad_payload());
        }
        if !in_pct(self.compression_rate_pct) {
            return Err(bad_payload());
        }
        if self.top_commands.len() > MAX_TOP_COMMANDS {
            return Err(bad_payload());
        }
        for c in &self.top_commands {
            let len = c.name.chars().count();
            if len == 0 || len > MAX_NAME_LEN || has_markup(&c.name) || !in_pct(c.pct) {
                return Err(bad_payload());
            }
        }
        if let Some(m) = &self.model_key
            && (m.chars().count() > MAX_LABEL_LEN || has_markup(m))
        {
            return Err(bad_payload());
        }
        if let Some(name) = &self.display_name {
            let len = name.chars().count();
            if len == 0 || len > MAX_LABEL_LEN || has_markup(name) {
                return Err(bad_payload());
            }
        }
        Ok(())
    }

    /// Rebuilds a `WrappedReport` for server-side card rendering. Fields outside the privacy
    /// whitelist (sparkline history, bounce, input tokens) take neutral defaults.
    fn to_report(&self) -> crate::core::wrapped::WrappedReport {
        crate::core::wrapped::WrappedReport {
            period: self.period.clone(),
            tokens_saved: u64::try_from(self.tokens_saved).unwrap_or(0),
            tokens_input: 0,
            cost_avoided_usd: self.cost_avoided_usd,
            total_commands: u64::try_from(self.total_commands).unwrap_or(0),
            sessions_count: usize::try_from(self.sessions_count).unwrap_or(0),
            top_commands: self
                .top_commands
                .iter()
                .map(|c| (c.name.clone(), 0u64, c.pct))
                .collect(),
            compression_rate_pct: self.compression_rate_pct,
            files_touched: u64::try_from(self.files_touched).unwrap_or(0),
            daily_savings: Vec::new(),
            bounce_tokens: 0,
            model_key: self.model_key.clone().unwrap_or_default(),
            pricing_estimated: self.pricing_estimated,
            percentile: None,
        }
    }
}

fn finite_nonneg(v: f64) -> bool {
    v.is_finite() && v >= 0.0
}

fn in_pct(v: f64) -> bool {
    v.is_finite() && (0.0..=100.0).contains(&v)
}

/// Rejects markup and control characters — defence against stored XSS in user-chosen text.
fn has_markup(s: &str) -> bool {
    s.chars()
        .any(|c| c == '<' || c == '>' || (c.is_control() && c != '\t'))
}

// ─── Login-less publisher identity (signed publish, VL-3c) ────────────────────

/// Length (hex chars) of the publisher id derived from the public key. 16 bytes of SHA-256 is
/// collision-safe yet compact, and reveals nothing about the key beyond a stable pseudonym.
const PUBLISHER_ID_HEX_LEN: usize = 32;

/// Wraps the whitelisted payload with the publisher's Ed25519 public key and a signature over
/// the exact `payload_json` bytes. The server derives a stable `publisher_id` from the key — no
/// login, no account — and upserts the card, so re-publishing from the same machine refreshes
/// one card instead of piling up duplicates. Old clients still POST the bare payload object.
#[derive(Deserialize)]
struct SignedEnvelope {
    /// The serialized `PublishPayload`, byte-identical to what the client signed.
    payload_json: String,
    /// Hex-encoded Ed25519 public key (32 bytes → 64 hex chars).
    public_key: Option<String>,
    /// Hex-encoded Ed25519 signature over `payload_json.as_bytes()` (64 bytes → 128 hex chars).
    signature: Option<String>,
}

/// Verifies the envelope signature against its public key and returns the parsed payload plus
/// the derived `publisher_id`. A missing key/signature or a bad signature is rejected — there is
/// no way to publish under another machine's identity without holding its private key.
fn verify_signed_envelope(env: &SignedEnvelope) -> ApiResult<(PublishPayload, String)> {
    use crate::core::agent_identity::{hex_decode, verify_signature};
    let (Some(pk_hex), Some(sig_hex)) = (&env.public_key, &env.signature) else {
        return Err(bad_payload());
    };
    let pk_bytes = hex_decode(pk_hex).map_err(|_| bad_payload())?;
    let sig_bytes = hex_decode(sig_hex).map_err(|_| bad_payload())?;
    if !verify_signature(&pk_bytes, env.payload_json.as_bytes(), &sig_bytes) {
        return Err(err(StatusCode::UNAUTHORIZED, "invalid_signature"));
    }
    let payload: PublishPayload =
        serde_json::from_str(&env.payload_json).map_err(|_| bad_payload())?;
    // Stable, non-reversible pseudonym derived from the public key (its hex form). The same key
    // always maps to the same publisher_id, which is the upsert key — no account, no login.
    let publisher_id = sha256_hex(pk_hex)
        .get(..PUBLISHER_ID_HEX_LEN)
        .ok_or_else(internal_error_str)?
        .to_string();
    Ok((payload, publisher_id))
}

fn internal_error_str() -> (StatusCode, String) {
    err(StatusCode::INTERNAL_SERVER_ERROR, "internal_error")
}

// ─── Handlers ─────────────────────────────────────────────────────────────────

/// `POST /api/wrapped` — publish (or refresh) a Wrapped card. Body parsed from raw bytes so
/// unknown/oversized payloads return our own `invalid_payload` / `payload_too_large` instead of
/// axum defaults. Two body shapes are accepted:
///   • a signed envelope `{payload_json, public_key, signature}` — the client proves a login-less
///     identity and the card is UPSERTed by `(publisher_id, period)` (one stable card/URL); or
///   • a bare payload object — legacy anonymous insert (may create duplicates) for old clients.
pub(super) async fn publish(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    if body.len() > MAX_BODY_BYTES {
        return Err(err(StatusCode::PAYLOAD_TOO_LARGE, "payload_too_large"));
    }

    // Signed envelope (login-less identity → upsert) vs. legacy bare payload (anonymous insert).
    // The signed `payload_json` is stored verbatim so the stored card stays signature-verifiable.
    let (payload, payload_json, publisher_id) = if let Ok(env) =
        serde_json::from_slice::<SignedEnvelope>(&body)
    {
        let (payload, pid) = verify_signed_envelope(&env)?;
        (payload, env.payload_json, Some(pid))
    } else {
        let payload: PublishPayload = serde_json::from_slice(&body).map_err(|_| bad_payload())?;
        let json = serde_json::to_string(&payload).map_err(internal_error)?;
        (payload, json, None)
    };
    payload.validate()?;

    let client = state.pool.get().await.map_err(internal_error)?;
    let ip_hash = client_ip_hash(&headers, &state.cfg.ip_hash_salt);

    if let Some(h) = &ip_hash {
        let row = client
            .query_one(
                "SELECT count(*) FROM wrapped_cards \
                 WHERE ip_hash = $1 AND created_at > now() - interval '1 hour'",
                &[h],
            )
            .await
            .map_err(internal_error)?;
        let recent: i64 = row.get(0);
        if recent >= MAX_PUBLISH_PER_HOUR {
            return Err(err(StatusCode::TOO_MANY_REQUESTS, "rate_limited"));
        }
    }

    let id = generate_card_id();
    let edit_token = generate_token();
    let edit_token_hash = sha256_hex(&edit_token);
    let base = state.cfg.public_base_url.trim_end_matches('/');

    // Signed → UPSERT by (publisher_id, period): the same machine refreshes one stable card.
    // Legacy → plain INSERT (anonymous, may duplicate); `period` is still recorded.
    if let Some(pid) = &publisher_id {
        let row = client
            .query_one(
                "INSERT INTO wrapped_cards \
                 (id, edit_token_hash, payload_json, ip_hash, leaderboard_opt_in, tokens_saved, publisher_id, period) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
                 ON CONFLICT (publisher_id, period) WHERE publisher_id IS NOT NULL \
                 DO UPDATE SET payload_json = EXCLUDED.payload_json, \
                               leaderboard_opt_in = EXCLUDED.leaderboard_opt_in, \
                               tokens_saved = EXCLUDED.tokens_saved \
                 RETURNING id, (xmax = 0) AS inserted",
                &[
                    &id,
                    &edit_token_hash,
                    &payload_json,
                    &ip_hash,
                    &payload.leaderboard_opt_in,
                    &payload.tokens_saved,
                    pid,
                    &payload.period,
                ],
            )
            .await
            .map_err(internal_error)?;
        let final_id: String = row.get(0);
        let inserted: bool = row.get(1);
        let url = format!("{base}/w/{final_id}");
        let mut out = serde_json::json!({ "id": final_id, "url": url });
        // The one-time edit_token exists only for a freshly inserted card; on an update the
        // client keeps the token it stored on first publish.
        if inserted {
            out["edit_token"] = serde_json::Value::String(edit_token);
            Ok((StatusCode::CREATED, Json(out)))
        } else {
            Ok((StatusCode::OK, Json(out)))
        }
    } else {
        client
            .execute(
                "INSERT INTO wrapped_cards \
                 (id, edit_token_hash, payload_json, ip_hash, leaderboard_opt_in, tokens_saved, period) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7)",
                &[
                    &id,
                    &edit_token_hash,
                    &payload_json,
                    &ip_hash,
                    &payload.leaderboard_opt_in,
                    &payload.tokens_saved,
                    &payload.period,
                ],
            )
            .await
            .map_err(internal_error)?;
        let url = format!("{base}/w/{id}");
        Ok((
            StatusCode::CREATED,
            Json(serde_json::json!({ "id": id, "edit_token": edit_token, "url": url })),
        ))
    }
}

/// `GET /api/wrapped/:id` — public fetch; increments `view_count` atomically.
pub(super) async fn get_card(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let client = state.pool.get().await.map_err(internal_error)?;
    let row = client
        .query_opt(
            "UPDATE wrapped_cards SET view_count = view_count + 1 \
             WHERE id = $1 RETURNING payload_json, created_at, view_count",
            &[&id],
        )
        .await
        .map_err(internal_error)?;
    let Some(row) = row else {
        return Err(err(StatusCode::NOT_FOUND, "not_found"));
    };

    let payload_json: String = row.get(0);
    let created_at: chrono::DateTime<chrono::Utc> = row.get(1);
    let view_count: i64 = row.get(2);
    let card: serde_json::Value = serde_json::from_str(&payload_json).map_err(internal_error)?;

    Ok(Json(serde_json::json!({
        "id": id,
        "created_at": created_at.to_rfc3339(),
        "view_count": view_count,
        "card": card,
    })))
}

/// `GET /api/wrapped/:id/card.svg` — server-rendered share card (reuses `WrappedReport::to_svg`).
/// Does not count as a view. Cacheable; the card never changes after publish.
pub(super) async fn get_card_svg(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<axum::response::Response> {
    let svg = fetch_card_svg(&state, &id).await?;

    use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE};
    use axum::response::IntoResponse;
    Ok((
        [
            (CONTENT_TYPE, "image/svg+xml; charset=utf-8"),
            (CACHE_CONTROL, "public, max-age=86400"),
        ],
        svg,
    )
        .into_response())
}

/// `GET /api/wrapped/:id/card.png` — rasterized OG image (PNG) for social unfurls, which do
/// not render SVG. Text needs fonts: the server loads system fonts and falls back to a present
/// family, so the container image must ship a sans font (e.g. `fonts-dejavu-core`).
pub(super) async fn get_card_png(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<axum::response::Response> {
    let svg = fetch_card_svg(&state, &id).await?;
    let png = svg_to_png(&svg).map_err(internal_error)?;

    use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE};
    use axum::response::IntoResponse;
    Ok((
        [
            (CONTENT_TYPE, "image/png"),
            (CACHE_CONTROL, "public, max-age=86400"),
        ],
        png,
    )
        .into_response())
}

/// `GET /w/:id` — the public, crawler-friendly permalink page. Server-rendered so Open Graph /
/// Twitter meta carry per-card data (static hosts can proxy `/w/` here). Counts as a view.
pub(super) async fn get_permalink_page(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<axum::response::Response> {
    let client = state.pool.get().await.map_err(internal_error)?;
    let row = client
        .query_opt(
            "UPDATE wrapped_cards SET view_count = view_count + 1 \
             WHERE id = $1 RETURNING payload_json",
            &[&id],
        )
        .await
        .map_err(internal_error)?;
    let Some(row) = row else {
        return Err(err(StatusCode::NOT_FOUND, "not_found"));
    };
    let payload_json: String = row.get(0);
    let payload: PublishPayload = serde_json::from_str(&payload_json).map_err(internal_error)?;

    let html = render_permalink_html(
        &id,
        &payload,
        &state.cfg.public_base_url,
        &state.cfg.api_base_url,
    );

    use axum::http::header::CONTENT_TYPE;
    use axum::response::IntoResponse;
    Ok(([(CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response())
}

/// One row of the public leaderboard.
#[derive(Serialize)]
pub(super) struct LeaderRow {
    rank: usize,
    id: String,
    url: String,
    display_name: Option<String>,
    tokens_saved: i64,
    cost_avoided_usd: f64,
    compression_rate_pct: f64,
    period: String,
    pricing_estimated: bool,
    /// Self-reported figures that look statistically implausible (very high compression over very
    /// large volume). Such cards are de-emphasized and badged rather than removed.
    flagged: bool,
}

#[derive(Serialize)]
pub(super) struct Leaderboard {
    entries: Vec<LeaderRow>,
}

const LEADERBOARD_LIMIT: i64 = 50;

/// Safety cap on how many per-machine rows we pull before account aggregation.
/// Account stacking (#488) sums a user's machines in Rust, so we must fetch all
/// of a user's machines — a pre-aggregation top-N could drop the smaller ones
/// and undercount the account. Comfortably above the current opted-in
/// population; revisit with a SQL-side `GROUP BY` (and a Postgres test harness)
/// if the board ever approaches this many distinct machines.
const LEADERBOARD_FETCH_CAP: i64 = 2_000;

/// Compression rate (percent) at/above which a card is treated as implausible *when paired with
/// high volume*. Organic agent usage compresses reads by ~60-90% on average; a sustained rate
/// this high indicates cache-hit/automation-dominated or fabricated figures, not representative
/// savings. (See `IMPLAUSIBLE_MIN_TOKENS` — a high rate over a tiny sample is normal.)
const IMPLAUSIBLE_RATE_PCT: f64 = 97.0;

/// Saved-token volume above which an extreme `IMPLAUSIBLE_RATE_PCT` is treated as implausible.
/// A near-100% rate over a few thousand tokens is an ordinary small-sample artefact; the same
/// rate sustained across this many tokens is not achievable by real coding work.
const IMPLAUSIBLE_MIN_TOKENS: i64 = 1_000_000_000;

/// Leaderboard figures are **self-reported** from each publisher's local ledger — the server
/// holds no denominator (`tokens_input` is never uploaded; see `PublishPayload`) and therefore
/// cannot recompute the rate. This pure check flags cards whose figures are statistically
/// implausible so the board can de-emphasize and badge them instead of letting a single
/// unverifiable card top the ranking. Pure (no I/O) so it is unit-tested.
fn stats_implausible(tokens_saved: i64, compression_rate_pct: f64) -> bool {
    tokens_saved >= IMPLAUSIBLE_MIN_TOKENS && compression_rate_pct >= IMPLAUSIBLE_RATE_PCT
}

/// Orders leaderboard rows for display: plausible cards first (preserving the incoming
/// `tokens_saved DESC` order from the query), flagged/implausible cards last, then assigns
/// 1-based ranks. `slice::sort_by_key` is stable, so each group keeps its relative order. Pure,
/// so the ordering rule is unit-tested without a database.
fn rank_and_demote_flagged(entries: &mut [LeaderRow]) {
    entries.sort_by_key(|e| e.flagged);
    for (i, e) in entries.iter_mut().enumerate() {
        e.rank = i + 1;
    }
}

/// `GET /api/wrapped/leaderboard` — top opted-in cards by tokens saved. Public; the only
/// person-facing field is the user-chosen `display_name`.
pub(super) async fn leaderboard(State(state): State<AppState>) -> ApiResult<Json<Leaderboard>> {
    Ok(Json(Leaderboard {
        entries: top_cards(&state).await?,
    }))
}

/// `GET /leaderboard` — server-rendered leaderboard page (static hosts proxy `/leaderboard` here).
pub(super) async fn get_leaderboard_page(
    State(state): State<AppState>,
) -> ApiResult<axum::response::Response> {
    let rows = top_cards(&state).await?;
    let html = render_leaderboard_html(&rows, &state.cfg.public_base_url);

    use axum::http::header::CONTENT_TYPE;
    use axum::response::IntoResponse;
    Ok(([(CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response())
}

async fn top_cards(state: &AppState) -> ApiResult<Vec<LeaderRow>> {
    let client = state.pool.get().await.map_err(internal_error)?;
    // One representative row per machine (its highest-saving card); legacy anonymous rows
    // (publisher_id NULL) stay distinct via COALESCE(publisher_id, id). `user_id` is carried
    // through so machines claimed to the same account can be stacked below (#488).
    let rows = client
        .query(
            "SELECT id, payload_json, user_id::text, COALESCE(publisher_id, id) FROM ( \
               SELECT DISTINCT ON (COALESCE(publisher_id, id)) \
                      id, payload_json, tokens_saved, created_at, user_id, publisher_id \
               FROM wrapped_cards \
               WHERE leaderboard_opt_in = TRUE \
               ORDER BY COALESCE(publisher_id, id), tokens_saved DESC, created_at DESC \
             ) t \
             ORDER BY tokens_saved DESC, created_at DESC LIMIT $1",
            &[&LEADERBOARD_FETCH_CAP],
        )
        .await
        .map_err(internal_error)?;

    let raw: Vec<RawLeaderCard> = rows
        .iter()
        .map(|r| RawLeaderCard {
            id: r.get(0),
            payload_json: r.get(1),
            user_id: r.get(2),
            pub_key: r.get(3),
        })
        .collect();

    let base = state.cfg.public_base_url.trim_end_matches('/');
    let mut entries = aggregate_by_account(raw, base);

    // Plausible cards rank first; flagged (implausible, unverifiable) cards sink to the bottom
    // regardless of raw `tokens_saved`, so one unverifiable card can't top the board.
    rank_and_demote_flagged(&mut entries);
    entries.truncate(LEADERBOARD_LIMIT as usize);
    Ok(entries)
}

/// A per-machine leaderboard row as fetched from the DB, before account aggregation.
struct RawLeaderCard {
    id: String,
    payload_json: String,
    /// Account id (`user_id`) as text — present once the card is claimed (`claim_card`).
    user_id: Option<String>,
    /// `COALESCE(publisher_id, id)` — the per-machine identity used for unclaimed cards.
    pub_key: String,
}

/// Collapse a user's machines into one leaderboard entry (#488).
///
/// Reported by a user: publishing from two machines produced two leaderboard rows.
/// The machine identity (`publisher_id`) is derived per device, so each machine is
/// distinct by construction. Once a user claims their cards to one account
/// (`POST /api/wrapped/:id/claim` → `user_id`), this stacks them: cards sharing a
/// `user_id` sum their `tokens_saved` / `cost_avoided_usd`, with the highest-saving
/// machine as the representative (display name + card URL) and a token-weighted
/// average compression rate. Unclaimed / legacy cards (no `user_id`) stay
/// individual, keyed by `pub_key`. Pure (no I/O) so the stacking rule is
/// unit-tested without a database.
fn aggregate_by_account(raw: Vec<RawLeaderCard>, base: &str) -> Vec<LeaderRow> {
    use std::collections::HashMap;

    struct Acc {
        rep_tokens: i64,
        rep_id: String,
        rep_display_name: Option<String>,
        rep_period: String,
        rep_rate: f64,
        sum_tokens: i64,
        sum_cost: f64,
        rate_num: f64,
        rate_den: i64,
        pricing_estimated: bool,
    }

    let mut groups: HashMap<String, Acc> = HashMap::new();
    for c in raw {
        let Ok(p) = serde_json::from_str::<PublishPayload>(&c.payload_json) else {
            continue;
        };
        // Claimed cards group by account; everything else stays per-machine.
        let key = match c.user_id.as_deref() {
            Some(u) if !u.is_empty() => format!("u:{u}"),
            _ => format!("p:{}", c.pub_key),
        };
        let acc = groups.entry(key).or_insert_with(|| Acc {
            rep_tokens: i64::MIN,
            rep_id: String::new(),
            rep_display_name: None,
            rep_period: String::new(),
            rep_rate: 0.0,
            sum_tokens: 0,
            sum_cost: 0.0,
            rate_num: 0.0,
            rate_den: 0,
            pricing_estimated: false,
        });

        let tokens = p.tokens_saved;
        acc.sum_tokens = acc.sum_tokens.saturating_add(tokens);
        acc.sum_cost += p.cost_avoided_usd;
        if tokens > 0 {
            acc.rate_num += p.compression_rate_pct * tokens as f64;
            acc.rate_den = acc.rate_den.saturating_add(tokens);
        }
        acc.pricing_estimated |= p.pricing_estimated;
        // The highest-saving machine represents the account (display name + card URL).
        if tokens > acc.rep_tokens {
            acc.rep_tokens = tokens;
            acc.rep_id = c.id;
            acc.rep_display_name = p.display_name;
            acc.rep_period = p.period;
            acc.rep_rate = p.compression_rate_pct;
        }
    }

    let mut rows: Vec<LeaderRow> = groups
        .into_values()
        .map(|a| {
            // Token-weighted average rate across the account's machines (a plain mean would let a
            // tiny high-rate machine distort the figure); fall back to the representative's rate
            // when there is no positive volume to weight by.
            let rate = if a.rate_den > 0 {
                a.rate_num / a.rate_den as f64
            } else {
                a.rep_rate
            };
            LeaderRow {
                rank: 0, // assigned after reordering by the caller
                url: format!("{base}/w/{}", a.rep_id),
                id: a.rep_id,
                display_name: a.rep_display_name,
                tokens_saved: a.sum_tokens,
                cost_avoided_usd: a.sum_cost,
                compression_rate_pct: rate,
                period: a.rep_period,
                pricing_estimated: a.pricing_estimated,
                flagged: stats_implausible(a.sum_tokens, rate),
            }
        })
        .collect();

    // Deterministic order independent of HashMap iteration: highest stacked savings first,
    // ties broken by the representative card id.
    rows.sort_by(|x, y| {
        y.tokens_saved
            .cmp(&x.tokens_saved)
            .then_with(|| x.id.cmp(&y.id))
    });
    rows
}

fn render_leaderboard_html(rows: &[LeaderRow], public_base: &str) -> String {
    let base = public_base.trim_end_matches('/');
    let mut items = String::new();
    for row in rows {
        let name = row
            .display_name
            .as_deref()
            .map_or_else(|| "anonymous".to_string(), html_escape);
        let tokens_u = u64::try_from(row.tokens_saved).unwrap_or(0);
        let tokens = crate::core::wrapped::format_tokens(tokens_u);
        let energy = crate::core::energy::format_for_tokens(tokens_u);
        let comp = format!("{:.0}%", row.compression_rate_pct);
        let est = if row.pricing_estimated { " est." } else { "" };
        // Flagged cards never get the top-rank highlight; they carry an "unverified" badge instead.
        let rank_class = if row.flagged {
            " lc-flagged"
        } else {
            match row.rank {
                1 => " lc-rank-1",
                2 => " lc-rank-2",
                3 => " lc-rank-3",
                _ => "",
            }
        };
        let flag_badge = if row.flagged {
            r#"<span class="lc-flag" title="Self-reported figures that look statistically implausible (very high compression over very large volume). Not server-verified.">unverified</span>"#
        } else {
            ""
        };
        items.push_str(&format!(
            r#"<li><a class="lc-row{rank_class}" href="{url}"><span class="lc-rank">#{rank}</span><span class="lc-id"><span class="lc-name">{name}</span><span class="lc-period">{period}</span>{flag_badge}</span><span class="lc-stats"><span class="lc-num">{tokens}</span><span class="lc-meta">{comp} compressed · {energy} saved</span><span class="lc-usd">${cost:.0}{est}</span></span></a></li>"#,
            url = row.url,
            rank = row.rank,
            cost = row.cost_avoided_usd,
            period = html_escape(&row.period),
        ));
    }
    let board = if items.is_empty() {
        r#"<div class="lc-empty">No one has opted in yet — be the first:<br/><code>lean-ctx gain --publish --leaderboard</code></div>"#.to_string()
    } else {
        format!(r#"<ol class="lc-board">{items}</ol>"#)
    };

    let head = format!(
        r#"<meta charset="utf-8"/>
<meta name="viewport" content="width=device-width, initial-scale=1"/>
<title>lean-ctx Leaderboard — top realized token savings</title>
<meta name="description" content="Top token savings, opted in by lean-ctx users. Open source — your AI sees only what matters."/>
<link rel="canonical" href="{base}/leaderboard"/>
{fonts}
<style>{css}</style>"#,
        fonts = super::site_theme::FONT_LINKS,
        css = super::site_theme::THEME_CSS,
    );

    format!(
        r#"<!doctype html>
<html lang="en">
<head>
{head}
</head>
<body>
{header}
<main class="lc-container">
<section class="lc-hero">
<span class="lc-label">Self-reported savings</span>
<h1>Leaderboard</h1>
<p>The most realized token savings, opted in by lean-ctx users. Figures are self-reported from each user's local ledger — not server-verified. Cards whose stats look statistically implausible are flagged <span class="lc-flag">unverified</span> and ranked last.</p>
</section>
{board}
<section class="lc-cta-section">
<h2>Put your savings on the board</h2>
<p>Install lean-ctx, then publish your Wrapped recap.</p>
<a class="lc-cta" href="{base}/docs/getting-started/">Install lean-ctx</a>
</section>
</main>
{footer}
</body>
</html>"#,
        header = super::site_theme::header(base),
        footer = super::site_theme::footer(base),
    )
}

/// Loads a card and renders its SVG (shared by `card.svg` and `card.png`). 404 when unknown.
async fn fetch_card_svg(state: &AppState, id: &str) -> ApiResult<String> {
    let client = state.pool.get().await.map_err(internal_error)?;
    let row = client
        .query_opt(
            "SELECT payload_json FROM wrapped_cards WHERE id = $1",
            &[&id],
        )
        .await
        .map_err(internal_error)?;
    let Some(row) = row else {
        return Err(err(StatusCode::NOT_FOUND, "not_found"));
    };
    let payload_json: String = row.get(0);
    let payload: PublishPayload = serde_json::from_str(&payload_json).map_err(internal_error)?;
    Ok(payload.to_report().to_svg())
}

/// Rasterizes an SVG string to PNG bytes via resvg. System fonts are loaded and a present
/// sans family is used as the fallback so headline text renders on headless servers.
fn svg_to_png(svg: &str) -> Result<Vec<u8>, String> {
    use resvg::{tiny_skia, usvg};

    let mut opt = usvg::Options::default();
    // The card SVG declares web-font stacks (`Inter, …, sans-serif` and
    // `ui-monospace, …, monospace`) that don't exist on a headless server. usvg's default
    // generic families point at Windows fonts (Arial / Courier New), so on a slim image the
    // generic tail resolves to nothing and the headline renders blank. Map every generic
    // family onto DejaVu, which the container ships (`fonts-dejavu-core`), so all text
    // always rasterizes. (In usvg 0.47 the generic mappings live on the fontdb, not Options.)
    {
        let db = opt.fontdb_mut();
        db.load_system_fonts();
        db.set_serif_family("DejaVu Serif");
        db.set_sans_serif_family("DejaVu Sans");
        db.set_monospace_family("DejaVu Sans Mono");
    }
    opt.font_family = "DejaVu Sans".to_string();

    let tree = usvg::Tree::from_str(svg, &opt).map_err(|e| format!("svg parse: {e}"))?;
    let size = tree.size().to_int_size();
    let mut pixmap = tiny_skia::Pixmap::new(size.width(), size.height())
        .ok_or_else(|| "pixmap alloc failed".to_string())?;
    resvg::render(&tree, tiny_skia::Transform::default(), &mut pixmap.as_mut());
    pixmap.encode_png().map_err(|e| format!("png encode: {e}"))
}

/// Minimal HTML text escaping for the few user-derived strings on the permalink page.
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// Renders the self-contained permalink page: per-card OG/Twitter meta + the inline card.
fn render_permalink_html(
    id: &str,
    p: &PublishPayload,
    public_base: &str,
    api_base: &str,
) -> String {
    let report = p.to_report();
    let svg = report.to_svg();
    let tokens = crate::core::wrapped::format_tokens(report.tokens_saved);
    let cost = format!("${:.2}", report.cost_avoided_usd);
    let est = if report.pricing_estimated {
        " (est.)"
    } else {
        ""
    };

    let who = p.display_name.as_deref().map(html_escape);
    let title = match &who {
        Some(n) => format!("{n}'s lean-ctx Wrapped"),
        None => "lean-ctx Wrapped".to_string(),
    };
    let description = format!(
        "Saved {tokens} tokens (~{cost}{est}) with lean-ctx — my AI saw only what mattered."
    );

    let page_url = format!("{}/w/{}", public_base.trim_end_matches('/'), id);
    let img_url = format!(
        "{}/api/wrapped/{}/card.png",
        api_base.trim_end_matches('/'),
        id
    );

    let base = public_base.trim_end_matches('/');
    format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8"/>
<meta name="viewport" content="width=device-width, initial-scale=1"/>
<title>{title}</title>
<meta name="description" content="{description}"/>
<link rel="canonical" href="{page_url}"/>
<meta property="og:type" content="website"/>
<meta property="og:site_name" content="lean-ctx"/>
<meta property="og:title" content="{title}"/>
<meta property="og:description" content="{description}"/>
<meta property="og:url" content="{page_url}"/>
<meta property="og:image" content="{img_url}"/>
<meta property="og:image:width" content="1200"/>
<meta property="og:image:height" content="630"/>
<meta name="twitter:card" content="summary_large_image"/>
<meta name="twitter:title" content="{title}"/>
<meta name="twitter:description" content="{description}"/>
<meta name="twitter:image" content="{img_url}"/>
{fonts}
<style>{css}</style>
</head>
<body>
{header}
<main class="lc-container">
<section class="lc-card-wrap">
{svg}
</section>
<section class="lc-cta-section">
<h2>Make your own Wrapped</h2>
<p>Install lean-ctx — your AI sees only what matters.</p>
<a class="lc-cta" href="{base}/docs/getting-started/">Install lean-ctx</a>
</section>
</main>
{footer}
</body>
</html>"#,
        fonts = super::site_theme::FONT_LINKS,
        css = super::site_theme::THEME_CSS,
        header = super::site_theme::header(base),
        footer = super::site_theme::footer(base),
    )
}

/// `DELETE /api/wrapped/:id` — requires the matching `X-Edit-Token`.
pub(super) async fn delete_card(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let token =
        edit_token_header(&headers).ok_or_else(|| err(StatusCode::FORBIDDEN, "forbidden"))?;
    let client = state.pool.get().await.map_err(internal_error)?;

    let stored = fetch_token_hash(&client, &id).await?;
    require_token(&token, &stored)?;

    client
        .execute("DELETE FROM wrapped_cards WHERE id = $1", &[&id])
        .await
        .map_err(internal_error)?;
    Ok(Json(serde_json::json!({ "deleted": true })))
}

/// `POST /api/wrapped/:id/claim` — binds an anonymous card to the authenticated account.
pub(super) async fn claim_card(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let (user_id, _) = auth_user(&state, &headers).await?;
    let token =
        edit_token_header(&headers).ok_or_else(|| err(StatusCode::FORBIDDEN, "forbidden"))?;
    let client = state.pool.get().await.map_err(internal_error)?;

    let stored = fetch_token_hash(&client, &id).await?;
    require_token(&token, &stored)?;

    client
        .execute(
            "UPDATE wrapped_cards SET user_id = $1 WHERE id = $2",
            &[&user_id, &id],
        )
        .await
        .map_err(internal_error)?;
    Ok(Json(serde_json::json!({ "claimed": true })))
}

// ─── Helpers ────────────────────────────────────────────────────────────────

async fn fetch_token_hash(client: &tokio_postgres::Client, id: &str) -> ApiResult<String> {
    let row = client
        .query_opt(
            "SELECT edit_token_hash FROM wrapped_cards WHERE id = $1",
            &[&id],
        )
        .await
        .map_err(internal_error)?;
    match row {
        Some(r) => Ok(r.get(0)),
        None => Err(err(StatusCode::NOT_FOUND, "not_found")),
    }
}

fn require_token(presented: &str, stored_hash: &str) -> ApiResult<()> {
    if constant_time_eq(sha256_hex(presented).as_bytes(), stored_hash.as_bytes()) {
        Ok(())
    } else {
        Err(err(StatusCode::FORBIDDEN, "forbidden"))
    }
}

fn edit_token_header(headers: &HeaderMap) -> Option<String> {
    let v = headers.get("x-edit-token")?.to_str().ok()?.trim();
    (!v.is_empty()).then(|| v.to_string())
}

/// 128-bit unguessable, hex-encoded id (the public `/w/<id>` slug).
fn generate_card_id() -> String {
    let bytes: [u8; 16] = rand::random();
    hex::encode(bytes)
}

/// Salted hash of the client IP (from the front proxy's `X-Forwarded-For`/`X-Real-IP`),
/// for abuse rate-limiting only — the raw IP is never stored.
fn client_ip_hash(headers: &HeaderMap, salt: &str) -> Option<String> {
    let ip = headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.split(',').next())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            headers
                .get("x-real-ip")
                .and_then(|v| v.to_str().ok())
                .map(str::trim)
                .filter(|s| !s.is_empty())
        })?;
    Some(sha256_hex(&format!("{salt}:{ip}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid() -> PublishPayload {
        PublishPayload {
            period: "week".into(),
            tokens_saved: 480_600_000,
            cost_avoided_usd: 1441.79,
            pricing_estimated: true,
            compression_rate_pct: 91.2,
            total_commands: 1234,
            sessions_count: 56,
            files_touched: 789,
            top_commands: vec![TopCommand {
                name: "ctx_search".into(),
                pct: 60.0,
            }],
            model_key: Some("claude-opus".into()),
            display_name: Some("yvesg".into()),
            leaderboard_opt_in: false,
        }
    }

    #[test]
    fn accepts_a_well_formed_payload() {
        assert!(valid().validate().is_ok());
    }

    fn raw_card(
        id: &str,
        user: Option<&str>,
        pub_key: &str,
        tokens: i64,
        name: &str,
        rate: f64,
    ) -> RawLeaderCard {
        let payload = serde_json::json!({
            "period": "all",
            "tokens_saved": tokens,
            "cost_avoided_usd": tokens as f64 / 1000.0,
            "pricing_estimated": false,
            "compression_rate_pct": rate,
            "display_name": name,
        })
        .to_string();
        RawLeaderCard {
            id: id.to_string(),
            payload_json: payload,
            user_id: user.map(str::to_string),
            pub_key: pub_key.to_string(),
        }
    }

    #[test]
    fn machines_claimed_to_one_account_stack() {
        // Two machines, same account → one stacked entry (the #488 fix).
        let raw = vec![
            raw_card("cardA", Some("user-1"), "pubA", 1_000, "Stephen", 80.0),
            raw_card("cardB", Some("user-1"), "pubB", 3_000, "Stephen", 90.0),
        ];
        let out = aggregate_by_account(raw, "https://leanctx.com");
        assert_eq!(
            out.len(),
            1,
            "two machines on one account collapse to one row"
        );
        assert_eq!(out[0].tokens_saved, 4_000, "points stack across machines");
        assert!(
            out[0].url.ends_with("/w/cardB"),
            "the highest-saving machine represents the account"
        );
        // Token-weighted rate: (1000*80 + 3000*90) / 4000 = 87.5
        assert!((out[0].compression_rate_pct - 87.5).abs() < 1e-9);
    }

    #[test]
    fn distinct_accounts_and_unclaimed_cards_stay_separate() {
        let raw = vec![
            raw_card("a", Some("user-1"), "pubA", 1_000, "A", 80.0),
            raw_card("b", Some("user-2"), "pubB", 2_000, "B", 80.0),
            raw_card("c", None, "pubC", 1_500, "C", 80.0), // unclaimed, stays individual
        ];
        let out = aggregate_by_account(raw, "https://x");
        assert_eq!(out.len(), 3);
        // Ordered by stacked tokens, descending.
        assert_eq!(out[0].id, "b");
        assert_eq!(out[1].id, "c");
        assert_eq!(out[2].id, "a");
    }

    #[test]
    fn aggregation_order_is_deterministic_on_ties() {
        let raw = vec![
            raw_card("zzz", Some("u1"), "p1", 1_000, "Z", 50.0),
            raw_card("aaa", Some("u2"), "p2", 1_000, "A", 50.0),
        ];
        let out = aggregate_by_account(raw, "https://x");
        assert_eq!(
            out[0].id, "aaa",
            "equal totals are tie-broken by id, stably"
        );
        assert_eq!(out[1].id, "zzz");
    }

    #[test]
    fn single_machine_is_unchanged_by_aggregation() {
        let raw = vec![raw_card("solo", None, "pSolo", 1_234, "Solo", 73.0)];
        let out = aggregate_by_account(raw, "https://leanctx.com");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].tokens_saved, 1_234);
        assert_eq!(out[0].display_name.as_deref(), Some("Solo"));
        assert!((out[0].compression_rate_pct - 73.0).abs() < 1e-9);
    }

    #[test]
    fn signed_envelope_roundtrips_and_rejects_tampering() {
        use crate::core::agent_identity::hex_encode;
        use ed25519_dalek::{Signer, SigningKey};

        let key = SigningKey::from_bytes(&[7u8; 32]);
        let payload_json = serde_json::to_string(&valid()).unwrap();
        let pubkey_hex = hex_encode(&key.verifying_key().to_bytes());
        let sig_hex = hex_encode(&key.sign(payload_json.as_bytes()).to_bytes());

        // A valid signature parses the payload and yields a stable, fixed-length publisher id.
        let env = SignedEnvelope {
            payload_json: payload_json.clone(),
            public_key: Some(pubkey_hex.clone()),
            signature: Some(sig_hex.clone()),
        };
        let (parsed, publisher_id) = verify_signed_envelope(&env).expect("valid signature");
        assert_eq!(parsed.period, "week");
        assert_eq!(publisher_id.len(), PUBLISHER_ID_HEX_LEN);

        // The same key always maps to the same publisher id — this is the upsert key.
        let again = SignedEnvelope {
            payload_json: payload_json.clone(),
            public_key: Some(pubkey_hex.clone()),
            signature: Some(sig_hex.clone()),
        };
        assert_eq!(verify_signed_envelope(&again).unwrap().1, publisher_id);

        // Tampering with the payload after signing is rejected (signature no longer matches).
        let tampered = SignedEnvelope {
            payload_json: payload_json.replacen("480600000", "999999999", 1),
            public_key: Some(pubkey_hex.clone()),
            signature: Some(sig_hex),
        };
        assert!(verify_signed_envelope(&tampered).is_err());

        // A missing signature cannot slip through the signed path into an unauthenticated upsert.
        let unsigned = SignedEnvelope {
            payload_json,
            public_key: Some(pubkey_hex),
            signature: None,
        };
        assert!(verify_signed_envelope(&unsigned).is_err());
    }

    #[test]
    fn rejects_unknown_fields() {
        let json = r#"{"period":"week","tokens_saved":1,"cost_avoided_usd":0.1,
            "pricing_estimated":false,"compression_rate_pct":50,"total_commands":1,
            "sessions_count":1,"files_touched":1,"repo_path":"/secret/path"}"#;
        assert!(serde_json::from_str::<PublishPayload>(json).is_err());
    }

    #[test]
    fn rejects_bad_period_and_ranges() {
        let mut p = valid();
        p.period = "year".into();
        assert!(p.validate().is_err());

        let mut p = valid();
        p.compression_rate_pct = 150.0;
        assert!(p.validate().is_err());

        let mut p = valid();
        p.tokens_saved = -1;
        assert!(p.validate().is_err());

        let mut p = valid();
        p.cost_avoided_usd = f64::NAN;
        assert!(p.validate().is_err());
    }

    #[test]
    fn rejects_oversized_and_markup_text() {
        let mut p = valid();
        p.display_name = Some("a".repeat(MAX_LABEL_LEN + 1));
        assert!(p.validate().is_err());

        let mut p = valid();
        p.display_name = Some("<script>".into());
        assert!(p.validate().is_err());

        let mut p = valid();
        p.top_commands = (0..=MAX_TOP_COMMANDS)
            .map(|_| TopCommand {
                name: "git".into(),
                pct: 1.0,
            })
            .collect();
        assert!(p.validate().is_err());
    }

    #[test]
    fn png_rasterizes_to_a_valid_image() {
        let svg = valid().to_report().to_svg();
        let png = svg_to_png(&svg).expect("rasterize");
        assert!(
            png.len() > 5000,
            "expected a non-trivial PNG, got {} bytes",
            png.len()
        );
        assert_eq!(&png[1..4], b"PNG", "must have a PNG signature");
        // Written for manual/visual inspection of text rendering during development.
        let _ = std::fs::write("/tmp/lc_card_test.png", &png);
    }

    #[test]
    fn permalink_html_carries_per_card_og_meta() {
        let p = valid();
        let html = render_permalink_html(
            "abc123",
            &p,
            "https://leanctx.com",
            "https://api.leanctx.com",
        );
        assert!(html.contains(
            r#"property="og:image" content="https://api.leanctx.com/api/wrapped/abc123/card.png""#
        ));
        assert!(html.contains(r#"property="og:url" content="https://leanctx.com/w/abc123""#));
        assert!(html.contains("twitter:card"));
        assert!(
            html.contains("yvesg's lean-ctx Wrapped"),
            "display_name personalizes the title"
        );
        assert!(html.contains("<svg"), "card is embedded inline");
    }

    #[test]
    fn leaderboard_html_uses_site_theme_shell() {
        let rows = vec![
            LeaderRow {
                rank: 1,
                id: "a".into(),
                url: "https://leanctx.com/w/a".into(),
                display_name: Some("yvesg".into()),
                tokens_saved: 486_000_000,
                cost_avoided_usd: 1458.0,
                compression_rate_pct: 67.7,
                period: "all".into(),
                pricing_estimated: true,
                flagged: false,
            },
            LeaderRow {
                rank: 2,
                id: "b".into(),
                url: "https://leanctx.com/w/b".into(),
                display_name: None,
                tokens_saved: 12_800_000,
                cost_avoided_usd: 32.0,
                compression_rate_pct: 60.2,
                period: "month".into(),
                pricing_estimated: false,
                flagged: false,
            },
            LeaderRow {
                rank: 3,
                id: "c".into(),
                url: "https://leanctx.com/w/c".into(),
                display_name: Some("roland".into()),
                tokens_saved: 4_200_000,
                cost_avoided_usd: 11.0,
                compression_rate_pct: 55.0,
                period: "week".into(),
                pricing_estimated: false,
                flagged: false,
            },
        ];
        let html = render_leaderboard_html(&rows, "https://leanctx.com");
        // Brand shell + design tokens mirrored from the marketing site.
        assert!(
            html.contains("--accent:#34d399"),
            "carries the site accent token"
        );
        assert!(html.contains("Space Grotesk"), "loads the display font");
        assert!(html.contains("lc-logo-ctx"), "renders the LeanCTX wordmark");
        assert!(
            html.contains(r#"class="lc-row lc-rank-1""#),
            "top row is highlighted"
        );
        assert!(html.contains("lc-footer"), "carries the branded footer");
        assert!(html.contains("yvesg"), "shows opted-in display names");
        assert!(
            html.contains("Self-reported savings"),
            "hero label is honest about provenance"
        );
        assert!(
            !html.contains("Verified savings"),
            "must not imply server-side verification"
        );
        // Written for manual/visual comparison with leanctx.com during development.
        let _ = std::fs::write("/tmp/lc_leaderboard.html", &html);
    }

    #[test]
    fn stats_implausible_thresholds() {
        // Real high-volume usage at organic rates is never flagged.
        assert!(!stats_implausible(5_000_000_000, 71.0));
        assert!(!stats_implausible(9_900_000_000, 90.0));
        // A near-100% rate over a tiny sample is an ordinary small-sample artefact.
        assert!(!stats_implausible(10_000, 100.0));
        // High rate AND high volume together is implausible (the observed #1 anomaly).
        assert!(stats_implausible(9_900_000_000, 100.0));
        // Both thresholds are inclusive at the boundary.
        assert!(stats_implausible(
            IMPLAUSIBLE_MIN_TOKENS,
            IMPLAUSIBLE_RATE_PCT
        ));
        assert!(!stats_implausible(IMPLAUSIBLE_MIN_TOKENS - 1, 100.0));
        assert!(!stats_implausible(
            IMPLAUSIBLE_MIN_TOKENS,
            IMPLAUSIBLE_RATE_PCT - 0.1
        ));
    }

    #[test]
    fn rank_and_demote_flagged_sinks_flagged_and_reranks() {
        let row = |id: &str, tokens: i64, flagged: bool| LeaderRow {
            rank: 0,
            id: id.into(),
            url: format!("https://leanctx.com/w/{id}"),
            display_name: None,
            tokens_saved: tokens,
            cost_avoided_usd: 0.0,
            compression_rate_pct: 0.0,
            period: "all".into(),
            pricing_estimated: false,
            flagged,
        };
        // Incoming order is `tokens_saved DESC` (as the SQL returns it); the flagged top card must
        // sink below the plausible ones while the plausible relative order is preserved.
        let mut rows = vec![
            row("fake", 9_900_000_000, true),
            row("real1", 5_000_000_000, false),
            row("real2", 600_000_000, false),
        ];
        rank_and_demote_flagged(&mut rows);
        assert_eq!(
            rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            vec!["real1", "real2", "fake"]
        );
        assert_eq!(
            rows.iter().map(|r| r.rank).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
    }

    #[test]
    fn flagged_card_renders_unverified_badge_and_no_gold() {
        let rows = vec![LeaderRow {
            rank: 1,
            id: "x".into(),
            url: "https://leanctx.com/w/x".into(),
            display_name: Some("suspicious".into()),
            tokens_saved: 9_900_000_000,
            cost_avoided_usd: 24_763.0,
            compression_rate_pct: 100.0,
            period: "all".into(),
            pricing_estimated: true,
            flagged: true,
        }];
        let html = render_leaderboard_html(&rows, "https://leanctx.com");
        assert!(
            html.contains("lc-flagged"),
            "flagged row carries the muted style"
        );
        assert!(
            html.contains(">unverified<"),
            "flagged row shows the unverified badge"
        );
        assert!(
            !html.contains("lc-row lc-rank-1"),
            "a flagged card never gets the top-rank highlight"
        );
    }

    #[test]
    fn html_escape_neutralizes_markup() {
        assert_eq!(
            html_escape(r#"<b>&"x"</b>"#),
            "&lt;b&gt;&amp;&quot;x&quot;&lt;/b&gt;"
        );
    }

    #[test]
    fn ip_hash_is_salted_and_omitted_without_headers() {
        let mut h = HeaderMap::new();
        // Salts are derived at runtime (not string literals) so this stays a
        // behavioral test and carries no hard-coded cryptographic value.
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        let salt_a = format!("salt-{nonce}-a");
        let salt_b = format!("salt-{nonce}-b");
        assert!(client_ip_hash(&h, &salt_a).is_none());

        h.insert("x-forwarded-for", "203.0.113.7, 10.0.0.1".parse().unwrap());
        let a = client_ip_hash(&h, &salt_a).unwrap();
        let b = client_ip_hash(&h, &salt_b).unwrap();
        assert_ne!(a, b, "different salts must yield different hashes");
        assert!(!a.contains("203.0.113.7"), "raw IP must never appear");
    }
}
