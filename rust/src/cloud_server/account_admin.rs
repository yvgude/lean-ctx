//! Account self-service: data export and account deletion (GL #535).
//!
//! - `GET /api/account/export` — one JSON file with everything the account has
//!   synced (GDPR Art. 20 portability, and the honest half of a premium
//!   offboarding: "take your data with you").
//! - `DELETE /api/account` — irreversible erasure (GDPR Art. 17). Billing dies
//!   first (live Stripe subscription cancelled immediately by the private
//!   plane); only then does the `users` row go, taking every synced table with
//!   it via `ON DELETE CASCADE`.

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use base64::Engine;
use serde::Deserialize;
use serde_json::{Value, json};

use super::auth::{AppState, auth_user};

/// Tables exported verbatim (sensitive columns excluded per query). Token
/// stores (`api_keys`, oauth_*, `magic_links`, …) are deliberately absent: hashes
/// of credentials are neither portable nor the user's "data".
const EXPORT_QUERIES: &[(&str, &str)] = &[
    (
        "stats_daily",
        "SELECT date, tokens_original, tokens_compressed, tokens_saved, tool_calls,
                cache_hits, cache_misses, updated_at
         FROM stats_daily WHERE user_id = $1 ORDER BY date",
    ),
    (
        "command_stats",
        "SELECT command, source, count, input_tokens, output_tokens, tokens_saved, updated_at
         FROM command_stats WHERE user_id = $1 ORDER BY command",
    ),
    (
        "cep_scores",
        "SELECT recorded_at, score, cache_hit_rate, mode_diversity, compression_rate,
                tool_calls, tokens_saved, complexity
         FROM cep_scores WHERE user_id = $1 ORDER BY recorded_at",
    ),
    (
        "gain_scores",
        "SELECT recorded_at, total, compression, cost_efficiency, quality, consistency,
                trend, avoided_usd, tool_spend_usd, model_key
         FROM gain_scores WHERE user_id = $1 ORDER BY recorded_at",
    ),
    (
        "knowledge_entries",
        "SELECT category, key, value, updated_at
         FROM knowledge_entries WHERE user_id = $1 ORDER BY category, key",
    ),
    (
        "gotchas",
        "SELECT pattern, fix, severity, category, occurrences, prevented_count,
                confidence, updated_at
         FROM gotchas WHERE user_id = $1 ORDER BY pattern",
    ),
    (
        "buddy_state",
        "SELECT name, species, level, xp, mood, streak, rarity, state_json, updated_at
         FROM buddy_state WHERE user_id = $1",
    ),
    (
        "feedback_thresholds",
        "SELECT language, entropy, jaccard, sample_count, avg_efficiency, updated_at
         FROM feedback_thresholds WHERE user_id = $1 ORDER BY language",
    ),
    (
        "devices",
        "SELECT device_label, first_seen, last_seen, last_surface, sync_count
         FROM devices WHERE user_id = $1 ORDER BY last_seen DESC",
    ),
    (
        "wrapped_cards",
        "SELECT id, payload_json, created_at, view_count, leaderboard_opt_in, tokens_saved
         FROM wrapped_cards WHERE user_id = $1 ORDER BY created_at",
    ),
];

/// `GET /api/account/export` — the account's full synced footprint as a
/// downloadable JSON document.
pub(super) async fn export_account(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, (StatusCode, String)> {
    let (user_id, email) = auth_user(&state, &headers).await?;
    let client = state.pool.get().await.map_err(internal)?;

    let mut data = serde_json::Map::new();
    for (name, sql) in EXPORT_QUERIES {
        let rows = client
            .query(
                &format!(
                    "SELECT COALESCE(json_agg(row_to_json(t)), '[]'::json)::text FROM ({sql}) t"
                ),
                &[&user_id],
            )
            .await
            .map_err(internal)?;
        let json_text: String = rows[0].get(0);
        data.insert(
            (*name).to_string(),
            serde_json::from_str(&json_text).unwrap_or(Value::Array(vec![])),
        );
    }

    // Zero-knowledge vaults: client-side-encrypted blobs, exported as base64
    // ciphertext. Only the account's sync key (never on the server) opens them.
    let mut vaults = serde_json::Map::new();
    for (label, table) in [
        ("knowledge", "knowledge_blobs"),
        ("gotchas", "gotcha_blobs"),
    ] {
        let row = client
            .query_opt(
                &format!("SELECT blob, entry_count, sha256, updated_at::text FROM {table} WHERE user_id = $1"),
                &[&user_id],
            )
            .await
            .map_err(internal)?;
        if let Some(r) = row {
            let blob: Vec<u8> = r.get(0);
            vaults.insert(
                label.to_string(),
                json!({
                    "ciphertext_base64": base64::engine::general_purpose::STANDARD.encode(blob),
                    "entry_count": r.get::<_, i64>(1),
                    "sha256": r.get::<_, String>(2),
                    "updated_at": r.get::<_, String>(3),
                    "encryption": "XChaCha20-Poly1305, client-side key — decrypt with `lean-ctx cloud pull` on a signed-in machine",
                }),
            );
        }
    }

    // Hosted index bundles can be 64 MB of ciphertext each — metadata only.
    let bundles = client
        .query(
            "SELECT project_hash, size_bytes, sha256, updated_at::text
             FROM index_bundles WHERE user_id = $1 ORDER BY updated_at DESC",
            &[&user_id],
        )
        .await
        .map_err(internal)?
        .iter()
        .map(|r| {
            json!({
                "project_hash": r.get::<_, String>(0),
                "size_bytes": r.get::<_, i64>(1),
                "sha256": r.get::<_, String>(2),
                "updated_at": r.get::<_, String>(3),
            })
        })
        .collect::<Vec<_>>();

    let account = client
        .query_one(
            "SELECT id::text, email, email_verified_at::text, created_at::text
             FROM users WHERE id = $1",
            &[&user_id],
        )
        .await
        .map_err(internal)?;

    let export = json!({
        "format": "leanctx-account-export",
        "version": 1,
        "generated_at": chrono::Utc::now().to_rfc3339(),
        "account": {
            "user_id": account.get::<_, String>(0),
            "email": account.get::<_, String>(1),
            "email_verified_at": account.get::<_, Option<String>>(2),
            "created_at": account.get::<_, String>(3),
        },
        "data": data,
        "encrypted_vaults": vaults,
        "hosted_index_bundles": bundles,
        "notes": [
            "Encrypted vaults and index bundles are client-side encrypted; restore them with `lean-ctx cloud pull` while signed in.",
            "Invoices live in the Stripe billing portal (Account -> Manage billing) and are retained as legal records.",
        ],
    });

    tracing::info!(%user_id, email, "account export generated");
    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/json".to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!(
                    "attachment; filename=\"leanctx-export-{}.json\"",
                    chrono::Utc::now().format("%Y-%m-%d")
                ),
            ),
        ],
        Json(export),
    )
        .into_response())
}

#[derive(Deserialize)]
pub(super) struct DeleteAccountBody {
    /// Must equal the account email — a deliberate speed bump so a stray API
    /// call or a half-filled dialog can never erase an account.
    pub confirm: String,
}

/// `DELETE /api/account` — irreversible. Billing first, data second, so a paid
/// subscription can never outlive its account.
pub(super) async fn delete_account(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<DeleteAccountBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let (user_id, email) = auth_user(&state, &headers).await?;

    if !body.confirm.trim().eq_ignore_ascii_case(&email) {
        return Err((
            StatusCode::BAD_REQUEST,
            "confirmation does not match the account email".to_string(),
        ));
    }

    // 1. Billing plane: cancel any live subscription *now* and purge billing
    // rows. A reachable-but-failing billing plane aborts the deletion —
    // never leave a paid subscription running for an account we just erased.
    let billing = super::billing_edge::billing_delete_account(&state.cfg, user_id).await?;
    let plan = billing
        .as_ref()
        .and_then(|v| v.get("plan").and_then(Value::as_str))
        .map(str::to_string);

    // 2. Community plane: one DELETE — every synced table cascades off users.
    let client = state.pool.get().await.map_err(internal)?;
    client
        .execute("DELETE FROM users WHERE id = $1", &[&user_id])
        .await
        .map_err(internal)?;

    // 3. Goodbye mail (best-effort — the account is already gone).
    if let Some(mailer) = &state.mailer {
        let plan_note = plan
            .as_deref()
            .filter(|p| *p != "free")
            .map(|p| {
                format!("Your {p} subscription was cancelled immediately — no further charges.\n")
            })
            .unwrap_or_default();
        let body = format!(
            "Your LeanCTX account ({email}) and all synced data have been permanently deleted.\n\n\
             {plan_note}\
             What remains:\n\
             - Everything on your machines keeps working — the local engine never needed an account.\n\
             - Past invoices stay available from Stripe's receipt emails (legal bookkeeping records).\n\n\
             If anything about LeanCTX pushed you away, reply and tell us — a human reads every answer.\n\n\
             Thanks for trying it. You're welcome back anytime: https://leanctx.com\n\n\
             — The LeanCTX team"
        );
        if let Err(e) = mailer
            .send_digest(&email, "Your LeanCTX account has been deleted", &body)
            .await
        {
            tracing::warn!(error = %e, "goodbye email failed");
        }
    }

    tracing::info!(%user_id, email, ?plan, "account deleted");
    Ok(Json(json!({ "deleted": true })))
}

fn internal<E: std::fmt::Display>(e: E) -> (StatusCode, String) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        format!("internal error: {e}"),
    )
}
