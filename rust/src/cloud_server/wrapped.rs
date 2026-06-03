//! Hosted opt-in Wrapped permalink (`/api/wrapped`) — the public side of the viral loop.
//!
//! Anonymous publish returns a public `id` + one-time `edit_token`; the token authorizes
//! delete and the optional account `claim`. Only a closed whitelist of aggregate fields is
//! accepted (`deny_unknown_fields`); no repo names, paths, code, history or raw IPs are stored.
//!
//! Contract: `docs/contracts/wrapped-permalink-v1.md`.

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use serde::{Deserialize, Serialize};

use super::auth::{auth_user, constant_time_eq, generate_token, sha256_hex, AppState};
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
    pub total_commands: i64,
    pub sessions_count: i64,
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
        if let Some(m) = &self.model_key {
            if m.chars().count() > MAX_LABEL_LEN || has_markup(m) {
                return Err(bad_payload());
            }
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

// ─── Handlers ─────────────────────────────────────────────────────────────────

/// `POST /api/wrapped` — anonymous publish. Body parsed from raw bytes so unknown/oversized
/// payloads return our own `invalid_payload` / `payload_too_large` instead of axum defaults.
pub(super) async fn publish(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    if body.len() > MAX_BODY_BYTES {
        return Err(err(StatusCode::PAYLOAD_TOO_LARGE, "payload_too_large"));
    }
    let payload: PublishPayload = serde_json::from_slice(&body).map_err(|_| bad_payload())?;
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
    let payload_json = serde_json::to_string(&payload).map_err(internal_error)?;

    client
        .execute(
            "INSERT INTO wrapped_cards \
             (id, edit_token_hash, payload_json, ip_hash, leaderboard_opt_in, tokens_saved) \
             VALUES ($1, $2, $3, $4, $5, $6)",
            &[
                &id,
                &edit_token_hash,
                &payload_json,
                &ip_hash,
                &payload.leaderboard_opt_in,
                &payload.tokens_saved,
            ],
        )
        .await
        .map_err(internal_error)?;

    let url = format!(
        "{}/w/{}",
        state.cfg.public_base_url.trim_end_matches('/'),
        id
    );
    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({ "id": id, "edit_token": edit_token, "url": url })),
    ))
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
    period: String,
    pricing_estimated: bool,
}

#[derive(Serialize)]
pub(super) struct Leaderboard {
    entries: Vec<LeaderRow>,
}

const LEADERBOARD_LIMIT: i64 = 50;

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
    let rows = client
        .query(
            "SELECT id, payload_json FROM wrapped_cards \
             WHERE leaderboard_opt_in = TRUE \
             ORDER BY tokens_saved DESC, created_at DESC LIMIT $1",
            &[&LEADERBOARD_LIMIT],
        )
        .await
        .map_err(internal_error)?;

    let base = state.cfg.public_base_url.trim_end_matches('/');
    let entries = rows
        .iter()
        .enumerate()
        .filter_map(|(i, r)| {
            let id: String = r.get(0);
            let payload_json: String = r.get(1);
            let p: PublishPayload = serde_json::from_str(&payload_json).ok()?;
            Some(LeaderRow {
                rank: i + 1,
                url: format!("{base}/w/{id}"),
                id,
                display_name: p.display_name,
                tokens_saved: p.tokens_saved,
                cost_avoided_usd: p.cost_avoided_usd,
                period: p.period,
                pricing_estimated: p.pricing_estimated,
            })
        })
        .collect();
    Ok(entries)
}

fn render_leaderboard_html(rows: &[LeaderRow], public_base: &str) -> String {
    let mut items = String::new();
    for row in rows {
        let name = row
            .display_name
            .as_deref()
            .map_or_else(|| "anonymous".to_string(), html_escape);
        let tokens =
            crate::core::wrapped::format_tokens(u64::try_from(row.tokens_saved).unwrap_or(0));
        let est = if row.pricing_estimated { " (est.)" } else { "" };
        items.push_str(&format!(
            r#"<li><a href="{url}"><span class="rank">#{rank}</span><span class="name">{name}</span><span class="num">{tokens} tokens · ${cost:.0}{est}</span><span class="period">{period}</span></a></li>"#,
            url = row.url,
            rank = row.rank,
            cost = row.cost_avoided_usd,
            period = html_escape(&row.period),
        ));
    }
    if items.is_empty() {
        items.push_str(r#"<li class="empty">No one has opted in yet — be the first: <code>lean-ctx gain --publish --leaderboard</code></li>"#);
    }

    format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8"/>
<meta name="viewport" content="width=device-width, initial-scale=1"/>
<title>lean-ctx Leaderboard</title>
<meta name="description" content="Top token savings, opted in by lean-ctx users."/>
<style>
  :root {{ color-scheme: dark; }}
  body {{ margin:0; background:#0b1020; color:#e5e7eb;
         font-family:Inter,system-ui,-apple-system,"Segoe UI",Roboto,sans-serif; padding:40px 16px; }}
  main {{ max-width:760px; margin:0 auto; }}
  h1 {{ font-size:34px; }} h1 span {{ color:#34d399; }}
  p.sub {{ color:#94a3b8; margin-top:-8px; }}
  ol {{ list-style:none; padding:0; }}
  li a {{ display:flex; gap:14px; align-items:baseline; text-decoration:none; color:inherit;
          padding:14px 16px; border-radius:10px; background:#131a2e; margin-bottom:10px; }}
  li a:hover {{ background:#1b2540; }}
  .rank {{ color:#34d399; font-weight:700; width:46px; }}
  .name {{ font-weight:700; flex:1; }}
  .num {{ color:#22d3ee; font-variant-numeric:tabular-nums; }}
  .period {{ color:#64748b; width:60px; text-align:right; }}
  .empty {{ color:#94a3b8; }} code {{ color:#34d399; }}
  a.cta {{ color:#34d399; }}
</style>
</head>
<body>
<main>
<h1>lean-ctx <span>Leaderboard</span></h1>
<p class="sub">Top realized token savings — opt in with <code>--leaderboard</code>.</p>
<ol>{items}</ol>
<p><a class="cta" href="{public_base}">Install lean-ctx — your AI sees only what matters</a></p>
</main>
</body>
</html>"#,
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
<style>
  :root {{ color-scheme: dark; }}
  body {{ margin:0; background:#0b1020; color:#e5e7eb;
         font-family:Inter,system-ui,-apple-system,"Segoe UI",Roboto,sans-serif;
         display:flex; min-height:100vh; align-items:center; justify-content:center; padding:24px; }}
  .wrap {{ width:100%; max-width:1200px; text-align:center; }}
  .card {{ width:100%; height:auto; border-radius:16px; box-shadow:0 20px 60px rgba(0,0,0,.45); }}
  .cta {{ margin-top:28px; }}
  .cta a {{ display:inline-block; background:#34d399; color:#06281d; font-weight:700;
            text-decoration:none; padding:14px 22px; border-radius:10px; }}
  .sub {{ margin-top:14px; color:#94a3b8; font-size:15px; }}
  .sub a {{ color:#34d399; }}
</style>
</head>
<body>
<main class="wrap">
{svg}
<div class="cta"><a href="{public_base}">Make your own — install lean-ctx</a></div>
<p class="sub">Open source · your AI sees only what matters · <a href="{public_base}">leanctx.com</a></p>
</main>
</body>
</html>"#,
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
    fn html_escape_neutralizes_markup() {
        assert_eq!(
            html_escape(r#"<b>&"x"</b>"#),
            "&lt;b&gt;&amp;&quot;x&quot;&lt;/b&gt;"
        );
    }

    #[test]
    fn ip_hash_is_salted_and_omitted_without_headers() {
        let mut h = HeaderMap::new();
        assert!(client_ip_hash(&h, "salt").is_none());

        h.insert("x-forwarded-for", "203.0.113.7, 10.0.0.1".parse().unwrap());
        let a = client_ip_hash(&h, "salt-a").unwrap();
        let b = client_ip_hash(&h, "salt-b").unwrap();
        assert_ne!(a, b, "different salts must yield different hashes");
        assert!(!a.contains("203.0.113.7"), "raw IP must never appear");
    }
}
