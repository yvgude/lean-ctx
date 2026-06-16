use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
use serde::Serialize;
use std::path::PathBuf;
use tokio::sync::Mutex;

use crate::core::savings_ledger::SignedSavingsBatchV1;

use super::team::TeamAppState;

#[derive(Debug, Serialize)]
struct IngestResponse {
    accepted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    signer_public_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    net_saved_tokens: Option<u64>,
}

/// `POST /api/v1/savings/ingest` — accepts a `SignedSavingsBatchV1` JSON body.
/// Verifies the Ed25519 signature, rejects on INVALID, and appends to the team's
/// savings store if valid.
pub async fn v1_savings_ingest(
    State(state): State<TeamAppState>,
    Json(batch): Json<SignedSavingsBatchV1>,
) -> impl IntoResponse {
    if batch.kind != "lean-ctx.savings-batch" {
        return (
            StatusCode::BAD_REQUEST,
            Json(IngestResponse {
                accepted: false,
                error: Some("invalid kind — expected \"lean-ctx.savings-batch\"".to_string()),
                signer_public_key: None,
                net_saved_tokens: None,
            }),
        );
    }

    let result = batch.verify();
    if !result.signature_valid {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(IngestResponse {
                accepted: false,
                error: Some(
                    result
                        .error
                        .unwrap_or_else(|| "signature verification failed".to_string()),
                ),
                signer_public_key: None,
                net_saved_tokens: None,
            }),
        );
    }

    let signer = result.signer_public_key.clone();
    let net_saved = batch.totals.net_saved_tokens;

    if let Err(e) = append_batch(&state.team.savings_store_dir, &batch).await {
        tracing::error!("savings ingest write error: {e}");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(IngestResponse {
                accepted: false,
                error: Some(format!("storage error: {e}")),
                signer_public_key: signer,
                net_saved_tokens: Some(net_saved),
            }),
        );
    }

    (
        StatusCode::OK,
        Json(IngestResponse {
            accepted: true,
            error: None,
            signer_public_key: signer,
            net_saved_tokens: Some(net_saved),
        }),
    )
}

/// Append a verified batch to the team savings store (one JSONL file per signer).
async fn append_batch(
    store_dir: &Mutex<PathBuf>,
    batch: &SignedSavingsBatchV1,
) -> anyhow::Result<()> {
    let dir = store_dir.lock().await.clone();
    tokio::fs::create_dir_all(&dir).await?;

    let signer = batch.signer_public_key.as_deref().unwrap_or("unknown");
    let filename = format!("savings_{}.jsonl", &signer[..signer.len().min(16)]);
    let path = dir.join(filename);

    let line = serde_json::to_string(batch)?;
    use tokio::io::AsyncWriteExt;
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .await?;
    file.write_all(line.as_bytes()).await?;
    file.write_all(b"\n").await?;

    tracing::info!(
        signer = signer,
        net_saved_tokens = batch.totals.net_saved_tokens,
        "savings batch ingested"
    );
    Ok(())
}
